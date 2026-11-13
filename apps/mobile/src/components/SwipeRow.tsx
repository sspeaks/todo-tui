// A row you can swipe: right past the threshold for the right action
// (complete or reopen, on green), left for the left one (delete, on red).
// Only a clearly sideways drag is taken, so the list still scrolls. The
// same two actions are VoiceOver / TalkBack actions on the row.
//
// Only the side being dragged toward shows, muted until the threshold;
// past it the action's colour comes in, its icon jumps and a tick is felt,
// so it is plain when letting go will act. Let go there and the row slides
// away and stays away while the action runs: it comes back only if the
// action fails, or if the row is still listed a moment after its Item
// changed (a view that keeps done items); otherwise the list drops it.
import { Ionicons } from "@expo/vector-icons";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { Animated, PanResponder, StyleSheet, View, useWindowDimensions } from "react-native";
import { tick } from "../haptics";
import { size, type Theme, useTheme } from "../theme";

const THRESHOLD = 88;
/** Over how many points before the threshold the action's colour comes in. */
const ARMING = 16;
/** A row slid away whose Item neither left nor changed by then comes back (a slow or lost answer). */
const COME_BACK_MS = 4_000;
/**
 * After its Item changed, a row slid away waits this long before it comes
 * back: the Item's change reaches the row a moment before its view drops
 * it, and a row the view drops must not flash back in between.
 */
const STILL_LISTED_MS = 700;

export interface SwipeAction {
  label: string;
  icon: keyof typeof Ionicons.glyphMap;
}

/**
 * `onRight` and `onLeft` resolve to whether the action happened, and
 * should be stable (useCallback): the gesture is rebuilt when they change.
 * `settled` changes when the row's Item does (its status, its revision).
 */
export function SwipeRow({
  right,
  left,
  onRight,
  onLeft,
  settled,
  children,
}: {
  right: SwipeAction;
  left: SwipeAction;
  onRight: () => Promise<boolean>;
  onLeft: () => Promise<boolean>;
  settled?: unknown;
  children: ReactNode;
}) {
  const t = useTheme();
  const s = styles(t);
  const { width } = useWindowDimensions();
  const [x] = useState(() => new Animated.Value(0));
  /** The actions' layer: faded once the row has slid away, so its slot is empty until the list closes it. */
  const [under] = useState(() => new Animated.Value(1));

  /** The row's motion, kept across gesture rebuilds (new callbacks with each change of the Item). */
  const motion = useMemo(() => {
    /** Not render state: past the threshold (the tick is once per crossing), slid away, the come-back timer and when it fires. */
    const drag: { armed: boolean; away: boolean; timer: ReturnType<typeof setTimeout> | undefined; due: number } = {
      armed: false,
      away: false,
      timer: undefined,
      due: Infinity,
    };
    const back = () => {
      clearTimeout(drag.timer);
      drag.timer = undefined;
      drag.due = Infinity;
      drag.away = false;
      under.setValue(1);
      Animated.spring(x, { toValue: 0, useNativeDriver: false, bounciness: 4 }).start();
    };
    const away = (to: number, run: () => Promise<boolean>) => {
      drag.away = true;
      Animated.sequence([
        Animated.timing(x, { toValue: to, duration: 170, useNativeDriver: false }),
        Animated.timing(under, { toValue: 0, duration: 140, useNativeDriver: false }),
      ]).start();
      void run().then((done) => (done ? backIn(COME_BACK_MS) : back()));
    };
    /**
     * Comes back in `ms`, unless the row is gone by then, or sooner if
     * already due: the Item's change (Subscribe) can arrive before the
     * call's own answer, whose longer fallback must not push it back.
     */
    const backIn = (ms: number) => {
      const due = Date.now() + ms;
      if (due >= drag.due) return;
      clearTimeout(drag.timer);
      drag.due = due;
      drag.timer = setTimeout(back, ms);
    };
    /** Moved to `dx`: whether that crossed the threshold outward (the tick). */
    const move = (dx: number) => {
      x.setValue(dx);
      const past = Math.abs(dx) > THRESHOLD;
      const crossed = past && !drag.armed;
      drag.armed = past;
      return crossed;
    };
    /** Let go (or taken away): the next crossing ticks again. */
    const settle = () => {
      drag.armed = false;
    };
    return { drag, back, backIn, away, move, settle };
  }, [x, under]);

  const responder = useMemo(
    () =>
      PanResponder.create({
        onMoveShouldSetPanResponder: (_, g) => !motion.drag.away && Math.abs(g.dx) > 12 && Math.abs(g.dx) > Math.abs(g.dy) * 2,
        onPanResponderMove: (_, g) => {
          if (motion.move(g.dx)) tick();
        },
        onPanResponderTerminationRequest: () => false,
        onPanResponderRelease: (_, g) => {
          motion.settle();
          if (g.dx > THRESHOLD) motion.away(width, onRight);
          else if (g.dx < -THRESHOLD) motion.away(-width, onLeft);
          else motion.back();
        },
        onPanResponderTerminate: () => {
          motion.settle();
          motion.back();
        },
      }),
    [motion, width, onRight, onLeft],
  );

  // The Item changed while the row was away: if it is still listed a moment later, it comes back, showing the change.
  useEffect(() => {
    if (motion.drag.away) motion.backIn(STILL_LISTED_MS);
  }, [settled, motion]);
  useEffect(() => () => clearTimeout(motion.drag.timer), [motion]);

  return (
    <View
      accessibilityHint={`Swipe right: ${right.label}. Swipe left: ${left.label}.`}
      accessibilityActions={[
        { name: "right", label: right.label },
        { name: "left", label: left.label },
      ]}
      onAccessibilityAction={(e) => void (e.nativeEvent.actionName === "right" ? onRight : onLeft)()}
    >
      <Animated.View style={[s.under, { opacity: under }]}>
        <Side action={right} dir={1} x={x} color={t.ok} t={t} />
        <Side action={left} dir={-1} x={x} color={t.danger} t={t} />
      </Animated.View>
      <Animated.View style={{ transform: [{ translateX: x }] }} {...responder.panHandlers}>
        {children}
      </Animated.View>
    </View>
  );
}

/**
 * One side's action, the whole row wide, shown only while the row is
 * dragged toward it: muted, then in `color` from just before the threshold.
 */
function Side({ action, dir, x, color, t }: { action: SwipeAction; dir: 1 | -1; x: Animated.Value; color: string; t: Theme }) {
  const s = styles(t);
  const shown = x.interpolate({ inputRange: dir === 1 ? [0, 1] : [-1, 0], outputRange: dir === 1 ? [0, 1] : [1, 0], extrapolate: "clamp" });
  const armed = x.interpolate({
    inputRange: dir === 1 ? [THRESHOLD - ARMING, THRESHOLD] : [-THRESHOLD, -THRESHOLD + ARMING],
    outputRange: dir === 1 ? [0, 1] : [1, 0],
    extrapolate: "clamp",
  });
  const scale = x.interpolate({
    inputRange: dir === 1 ? [0, THRESHOLD - 1, THRESHOLD] : [-THRESHOLD, -THRESHOLD + 1, 0],
    outputRange: dir === 1 ? [0.7, 0.9, 1.15] : [1.15, 0.9, 0.7],
    extrapolate: "clamp",
  });
  const content = (fg: string) => (
    <>
      {dir === -1 ? <Animated.Text style={[s.label, { color: fg }]}>{action.label}</Animated.Text> : null}
      <Animated.View style={{ transform: [{ scale }] }}>
        <Ionicons name={action.icon} size={20} color={fg} />
      </Animated.View>
      {dir === 1 ? <Animated.Text style={[s.label, { color: fg }]}>{action.label}</Animated.Text> : null}
    </>
  );
  const align = dir === 1 ? s.start : s.end;
  return (
    <Animated.View style={[s.fill, { opacity: shown }]} pointerEvents="none">
      <View style={[s.fill, s.side, align, s.idle]}>{content(t.dim)}</View>
      <Animated.View style={[s.fill, s.side, align, { backgroundColor: color, opacity: armed }]}>{content(t.onAction)}</Animated.View>
    </Animated.View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    under: { position: "absolute", top: 0, right: 0, bottom: 0, left: 0 },
    fill: { position: "absolute", top: 0, right: 0, bottom: 0, left: 0 },
    side: { flexDirection: "row", alignItems: "center", gap: 8, paddingHorizontal: 22 },
    start: { justifyContent: "flex-start" },
    end: { justifyContent: "flex-end" },
    idle: { backgroundColor: t.chip },
    label: { ...t.monoBold, fontSize: size.meta },
  });
