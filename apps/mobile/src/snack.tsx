// One snackbar for every screen, under the header: clear of the keyboard,
// the quick-add bar and Expo Go's tools button. Undo is built in.
import { type Item, undo } from "@todo-tui/client/portable";
import { useClients, useLiveStore } from "@todo-tui/client/react";
import { createContext, type ReactNode, useCallback, useContext, useEffect, useMemo, useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { describeError } from "./errors";
import { type Theme, useTheme } from "./theme";

export interface SnackAction {
  label: string;
  onPress: () => void;
}

interface Snack {
  text: string;
  error?: boolean;
  actions: SnackAction[];
}

interface SnackApi {
  /** A message; with `opId`, an Undo for that op. */
  show(text: string, opts?: { opId?: string; actions?: SnackAction[] }): void;
  /** A failed call, in words the person can act on. */
  fail(err: unknown): void;
  /** Applies a mutation's Items at its revision, then shows `text` with Undo. */
  done(text: string, res: { result?: { opId: string; revision: bigint }; items: readonly (Item | undefined)[] }, actions?: SnackAction[]): void;
}

const Ctx = createContext<SnackApi | null>(null);

export function useSnack(): SnackApi {
  const api = useContext(Ctx);
  if (!api) throw new Error("useSnack outside <SnackProvider>");
  return api;
}

export function SnackProvider({ children }: { children: ReactNode }) {
  const s = styles(useTheme());
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
              (err: unknown) => setSnack({ text: describeError(err), error: true, actions: [] }),
            );
          },
        });
      }
      setSnack({ text, actions });
    };
    return {
      show,
      fail: (err) => setSnack({ text: describeError(err), error: true, actions: [] }),
      done: (text, res, actions) => {
        const items = res.items.filter((i): i is Item => i !== undefined);
        if (res.result) store.applyItems(items, res.result.revision);
        show(text, { ...(res.result && { opId: res.result.opId }), ...(actions && { actions }) });
      },
    };
  }, [clients, store]);

  const dismiss = useCallback(() => setSnack(undefined), []);
  useEffect(() => {
    if (!snack) return;
    const timer = setTimeout(dismiss, snack.error ? 8_000 : 5_000);
    return () => clearTimeout(timer);
  }, [snack, dismiss]);

  return (
    <Ctx.Provider value={api}>
      {children}
      {snack ? (
        <View style={[s.wrap, { top: insets.top + HEADER }]} pointerEvents="box-none">
          <View style={s.snack}>
            <Text style={[s.text, snack.error && s.error]} numberOfLines={3}>
              {snack.text}
            </Text>
            {snack.actions.map((a) => (
              <Pressable
                key={a.label}
                hitSlop={8}
                onPress={() => {
                  dismiss();
                  a.onPress();
                }}
              >
                <Text style={s.action}>{a.label}</Text>
              </Pressable>
            ))}
          </View>
        </View>
      ) : null}
    </Ctx.Provider>
  );
}

/** The navigation header's height, below the status bar. */
const HEADER = 52;

const styles = (t: Theme) =>
  StyleSheet.create({
    wrap: { position: "absolute", left: 12, right: 12 },
    snack: {
      flexDirection: "row",
      alignItems: "center",
      gap: 16,
      backgroundColor: t.text,
      borderRadius: 10,
      paddingHorizontal: 14,
      paddingVertical: 12,
      shadowColor: "#000",
      shadowOpacity: 0.2,
      shadowRadius: 8,
      shadowOffset: { width: 0, height: 2 },
      elevation: 4,
    },
    text: { flex: 1, color: t.bg },
    error: { color: t.danger },
    action: { color: t.accent, fontWeight: "700" },
  });
