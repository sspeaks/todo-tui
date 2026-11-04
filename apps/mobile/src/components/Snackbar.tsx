import { useEffect } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { type Theme, useTheme } from "../theme";

export interface Snack {
  /** Bumped per message, so the same text twice still restarts the timer. */
  key: number;
  text: string;
  /** The op to undo, when the message offers Undo. */
  opId?: string;
  error?: boolean;
}

export function Snackbar({ snack, onUndo, onDismiss }: { snack: Snack | undefined; onUndo: (opId: string) => void; onDismiss: () => void }) {
  const s = styles(useTheme());
  useEffect(() => {
    if (!snack) return;
    const timer = setTimeout(onDismiss, snack.error ? 8_000 : 5_000);
    return () => clearTimeout(timer);
  }, [snack, onDismiss]);
  if (!snack) return null;
  return (
    <View style={s.snack}>
      <Text style={[s.text, snack.error && s.error]} numberOfLines={3}>
        {snack.text}
      </Text>
      {snack.opId ? (
        <Pressable onPress={() => onUndo(snack.opId!)} hitSlop={8}>
          <Text style={s.undo}>Undo</Text>
        </Pressable>
      ) : null}
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    snack: {
      position: "absolute",
      left: 12,
      right: 12,
      bottom: 8,
      flexDirection: "row",
      alignItems: "center",
      gap: 12,
      backgroundColor: t.text,
      borderRadius: 8,
      paddingHorizontal: 14,
      paddingVertical: 12,
    },
    text: { flex: 1, color: t.bg },
    error: { color: t.danger },
    undo: { color: t.accent, fontWeight: "700" },
  });
