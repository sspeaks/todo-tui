import type { ReactNode } from "react";
import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
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

/** A thin divider between groups in a row. */
export function Gap() {
  const s = styles(useTheme());
  return <View style={s.gap} />;
}

/** A horizontally scrolling row of pills. */
export function PillRow({ children }: { children: ReactNode }) {
  const s = styles(useTheme());
  return (
    <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={s.row} keyboardShouldPersistTaps="always">
      {children}
    </ScrollView>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    row: { gap: 6, paddingHorizontal: 8, alignItems: "center" },
    pill: { paddingHorizontal: 11, paddingVertical: 6, borderRadius: 14, backgroundColor: t.surface, maxWidth: 200 },
    on: { backgroundColor: t.accent },
    text: { color: t.text, fontSize: 14 },
    onText: { color: t.onAccent, fontWeight: "600" },
    gap: { width: StyleSheet.hairlineWidth, height: 20, backgroundColor: t.border, marginHorizontal: 4 },
  });
