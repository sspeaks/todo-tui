// One snackbar for every screen: a dark toast at the bottom, above the tab
// bar where there is one, and short of the + button on the list. Undo is
// built in.
import { type Item, undo } from "@todo-tui/client/portable";
import { useClients, useLiveStore } from "@todo-tui/client/react";
import { createContext, type ReactNode, useCallback, useContext, useEffect, useMemo, useState } from "react";
import { Ionicons } from "@expo/vector-icons";
import { usePathname } from "expo-router";
import { Animated, Pressable, StyleSheet, Text, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { describeError } from "./errors";
import { type Theme, useTheme } from "./theme";

export interface SnackAction {
  label: string;
  onPress: () => void;
}

type Icon = keyof typeof Ionicons.glyphMap;

interface Snack {
  text: string;
  icon: Icon;
  error?: boolean;
  actions: SnackAction[];
}

interface SnackApi {
  /** A message; with `opId`, an Undo for that op. */
  show(text: string, opts?: { opId?: string; actions?: SnackAction[]; icon?: Icon }): void;
  /** A failed call, in words the person can act on. */
  fail(err: unknown): void;
  /** Applies a mutation's Items at its revision, then shows `text` with Undo. */
  done(
    text: string,
    res: { result?: { opId: string; revision: bigint }; items: readonly (Item | undefined)[] },
    actions?: SnackAction[],
    icon?: Icon,
  ): void;
}

const Ctx = createContext<SnackApi | null>(null);

export function useSnack(): SnackApi {
  const api = useContext(Ctx);
  if (!api) throw new Error("useSnack outside <SnackProvider>");
  return api;
}

export function SnackProvider({ children }: { children: ReactNode }) {
  const insets = useSafeAreaInsets();
  const clients = useClients();
  const store = useLiveStore();
  const [snack, setSnack] = useState<Snack>();

  const api = useMemo<SnackApi>(() => {
    const show: SnackApi["show"] = (text, opts = {}) => {
      const actions = [...(opts.actions ?? [])];
      if (opts.opId) {
        const opId = opts.opId;
        actions.unshift({
          label: "Undo",
          onPress: () => {
            undo(clients.items, { expectedOpId: opId, timeoutMs: 15_000 }).then(
              (res) => {
                if (res.result) store.applyItems(res.items, res.result.revision);
                show(res.entry ? `Undid: ${res.entry.description}` : "Nothing to undo");
              },
              (err: unknown) => setSnack({ text: describeError(err), icon: "alert-circle", error: true, actions: [] }),
            );
          },
        });
      }
      setSnack({ text, icon: opts.icon ?? (opts.opId ? "checkmark-circle" : "information-circle"), actions });
    };
    return {
      show,
      fail: (err) => setSnack({ text: describeError(err), icon: "alert-circle", error: true, actions: [] }),
      done: (text, res, actions, icon) => {
        const items = res.items.filter((i): i is Item => i !== undefined);
        if (res.result) store.applyItems(items, res.result.revision);
        show(text, { ...(res.result && { opId: res.result.opId }), ...(actions && { actions }), icon: icon ?? "checkmark-circle" });
      },
    };
  }, [clients, store]);

  const dismiss = useCallback(() => setSnack(undefined), []);
  useEffect(() => {
    if (!snack) return;
    const timer = setTimeout(dismiss, snack.error ? 8_000 : 5_000);
    return () => clearTimeout(timer);
  }, [snack, dismiss]);

  // Above the tab bar on the tabs, and short of the + button on the list.
  const path = usePathname();
  const onTabs = TAB_PATHS.has(path);
  const bottom = insets.bottom + (onTabs ? TAB_BAR : 0) + 12;
  const right = path === "/" ? 92 : 12;

  return (
    <Ctx.Provider value={api}>
      {children}
      {snack ? <Toast snack={snack} bottom={bottom} right={right} onAction={dismiss} /> : null}
    </Ctx.Provider>
  );
}

/** The tab screens, which have the tab bar under them. */
const TAB_PATHS = new Set(["/", "/search", "/settings"]);
/** The bottom tab bar's height above the home indicator. */
const TAB_BAR = 49;

function Toast({ snack, bottom, right, onAction }: { snack: Snack; bottom: number; right: number; onAction: () => void }) {
  const t = useTheme();
  const s = styles(t);
  const [shown] = useState(() => new Animated.Value(0));
  useEffect(() => {
    shown.setValue(0);
    Animated.timing(shown, { toValue: 1, duration: 180, useNativeDriver: true }).start();
  }, [snack, shown]);
  const lift = shown.interpolate({ inputRange: [0, 1], outputRange: [12, 0] });
  return (
    <Animated.View
      style={[s.wrap, { bottom, right, opacity: shown, transform: [{ translateY: lift }] }]}
      pointerEvents="box-none"
      accessibilityLiveRegion="polite"
    >
      <View style={s.snack}>
        <Ionicons name={snack.icon} size={18} color={snack.error ? "#fca5a5" : snack.icon === "information-circle" ? "#cbd5e1" : "#86efac"} />
        <Text style={s.text} numberOfLines={2}>
          {snack.text}
        </Text>
        {snack.actions.map((a) => (
          <Pressable
            key={a.label}
            hitSlop={10}
            onPress={() => {
              onAction();
              a.onPress();
            }}
            accessibilityRole="button"
          >
            <Text style={s.action}>{a.label}</Text>
          </Pressable>
        ))}
      </View>
    </Animated.View>
  );
}

const styles = (_t: Theme) =>
  StyleSheet.create({
    wrap: { position: "absolute", left: 12 },
    // Dark in both themes, as iOS toasts are.
    snack: {
      flexDirection: "row",
      alignItems: "center",
      gap: 10,
      backgroundColor: "#1f2329",
      borderRadius: 12,
      paddingLeft: 12,
      paddingRight: 14,
      paddingVertical: 11,
      shadowColor: "#000",
      shadowOpacity: 0.18,
      shadowRadius: 10,
      shadowOffset: { width: 0, height: 4 },
      elevation: 5,
    },
    text: { flex: 1, color: "#f3f4f6", fontSize: 14, lineHeight: 19 },
    // The accent's cyan, light enough for the dark snackbar in both themes.
    action: { color: "#67e8f9", fontWeight: "700", fontSize: 15 },
  });
