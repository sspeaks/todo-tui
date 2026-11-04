import type { ReactNode } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { type Theme, useTheme } from "../../theme";

/** One tap: `on` shows it chosen. */
export function Pill({ label, on, onPress, accessibilityLabel }: { label: string; on?: boolean; onPress: () => void; accessibilityLabel?: string }) {
  const s = styles(useTheme());
  return (
    <Pressable
      style={[s.pill, on && s.on]}
      onPress={onPress}
      hitSlop={4}
      accessibilityRole="button"
      accessibilityState={{ selected: !!on }}
      accessibilityLabel={accessibilityLabel ?? label}
    >
      <Text style={[s.text, on && s.onText]} numberOfLines={1}>
        {label}
      </Text>
    </Pressable>
  );
}

/** Pills that wrap onto as many lines as they need: every one in sight. */
export function PillRow({ children }: { children: ReactNode }) {
  const s = styles(useTheme());
  return <View style={s.row}>{children}</View>;
}

/** A labelled group of pills: the label on the left, the pills wrapping beside it. */
export function PillGroup({ label, children }: { label: string; children: ReactNode }) {
  const s = styles(useTheme());
  return (
    <View style={s.group}>
      <Text style={s.label}>{label}</Text>
      <View style={[s.row, s.grow]}>{children}</View>
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    row: { flexDirection: "row", flexWrap: "wrap", gap: 6, alignItems: "center" },
    grow: { flex: 1 },
    group: { flexDirection: "row", alignItems: "flex-start", gap: 8, paddingHorizontal: 12 },
    label: { width: 64, color: t.dim, fontSize: 12, paddingTop: 7 },
    pill: { paddingHorizontal: 11, paddingVertical: 6, borderRadius: 14, backgroundColor: t.surface, maxWidth: 200 },
    on: { backgroundColor: t.accent },
    text: { color: t.text, fontSize: 14 },
    onText: { color: t.onAccent, fontWeight: "600" },
  });
