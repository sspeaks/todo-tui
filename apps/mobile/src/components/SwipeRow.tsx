// A row you can swipe: right past the threshold for the right action
// (complete or reopen, on green), left for the left one (delete, on red).
// Only a clearly sideways drag is taken, so the list still scrolls. The
// same two actions are VoiceOver / TalkBack actions on the row.
import { Ionicons } from "@expo/vector-icons";
import { type ReactNode, useMemo, useState } from "react";
import { Animated, PanResponder, StyleSheet, Text, View, useWindowDimensions } from "react-native";
import { type Theme, useTheme } from "../theme";

const THRESHOLD = 88;

export interface SwipeAction {
  label: string;
  icon: keyof typeof Ionicons.glyphMap;
}

/**
 * `onRight` and `onLeft` should be stable (useCallback): the gesture is
 * rebuilt when they change.
 */
export function SwipeRow({
  right,
  left,
  onRight,
  onLeft,
  children,
}: {
  right: SwipeAction;
  left: SwipeAction;
  onRight: () => void;
  onLeft: () => void;
  children: ReactNode;
}) {
  const t = useTheme();
  const s = styles(t);
  const { width } = useWindowDimensions();
  const [x] = useState(() => new Animated.Value(0));

  const responder = useMemo(() => {
    const back = () => Animated.spring(x, { toValue: 0, useNativeDriver: false, bounciness: 4 }).start();
    const away = (to: number, run: () => void) =>
      Animated.timing(x, { toValue: to, duration: 160, useNativeDriver: false }).start(() => {
        run();
        x.setValue(0);
      });
    return PanResponder.create({
      onMoveShouldSetPanResponder: (_, g) => Math.abs(g.dx) > 12 && Math.abs(g.dx) > Math.abs(g.dy) * 2,
      onPanResponderMove: (_, g) => x.setValue(g.dx),
      onPanResponderTerminationRequest: () => false,
      onPanResponderRelease: (_, g) => {
        if (g.dx > THRESHOLD) away(width, onRight);
        else if (g.dx < -THRESHOLD) away(-width, onLeft);
        else back();
      },
      onPanResponderTerminate: back,
    });
  }, [x, width, onRight, onLeft]);

  return (
    <View
      accessibilityHint={`Swipe right: ${right.label}. Swipe left: ${left.label}.`}
      accessibilityActions={[
        { name: "right", label: right.label },
        { name: "left", label: left.label },
      ]}
      onAccessibilityAction={(e) => (e.nativeEvent.actionName === "right" ? onRight : onLeft)()}
    >
      <View style={s.under}>
        <View style={[s.half, s.rightAction]}>
          <Ionicons name={right.icon} size={20} color="#fff" />
          <Text style={s.actionText}>{right.label}</Text>
        </View>
        <View style={[s.half, s.leftAction]}>
          <Text style={s.actionText}>{left.label}</Text>
          <Ionicons name={left.icon} size={20} color="#fff" />
        </View>
      </View>
      <Animated.View style={{ transform: [{ translateX: x }] }} {...responder.panHandlers}>
        {children}
      </Animated.View>
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    under: { position: "absolute", top: 0, right: 0, bottom: 0, left: 0, flexDirection: "row" },
    half: { flex: 1, flexDirection: "row", alignItems: "center", gap: 8, paddingHorizontal: 20 },
    rightAction: { backgroundColor: t.ok, justifyContent: "flex-start" },
    leftAction: { backgroundColor: t.danger, justifyContent: "flex-end" },
    actionText: { color: "#fff", fontWeight: "600" },
  });
