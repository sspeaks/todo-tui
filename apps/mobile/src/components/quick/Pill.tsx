import { Ionicons } from "@expo/vector-icons";
import type { ReactNode } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { tick } from "../../haptics";
import { type Theme, useTheme } from "../../theme";

type Icon = keyof typeof Ionicons.glyphMap;

/**
 * One tap: `on` shows it chosen (filled, with a check). `icon` leads the
 * label; `dot` is a coloured dot instead (the priorities).
 */
export function Pill({
  label,
  on,
  onPress,
  icon,
  dot,
  accessibilityLabel,
}: {
  label: string;
  on?: boolean;
  onPress: () => void;
  icon?: Icon;
  dot?: string;
  accessibilityLabel?: string;
}) {
  const t = useTheme();
  const s = styles(t);
  const fg = on ? t.onAccent : t.text;
  return (
    <Pressable
      style={({ pressed }) => [s.pill, on && s.on, pressed && s.pressed]}
      onPress={() => {
        tick();
        onPress();
      }}
      hitSlop={4}
      accessibilityRole="button"
      accessibilityState={{ selected: !!on }}
      accessibilityLabel={accessibilityLabel ?? label}
    >
      {on ? <Ionicons name="checkmark" size={15} color={fg} /> : dot ? <View style={[s.dot, { backgroundColor: dot }]} /> : icon ? <Ionicons name={icon} size={15} color={t.dim} /> : null}
      <Text style={[s.text, { color: fg }, on && s.onText]} numberOfLines={1}>
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

/** A section of choices: a small icon and heading, then the pills, wrapping. */
export function PillGroup({ label, icon, children }: { label: string; icon: Icon; children: ReactNode }) {
  const t = useTheme();
  const s = styles(t);
  return (
    <View style={s.group}>
      <View style={s.heading}>
        <Ionicons name={icon} size={14} color={t.dim} />
        <Text style={s.label}>{label}</Text>
      </View>
      <View style={s.row}>{children}</View>
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    row: { flexDirection: "row", flexWrap: "wrap", gap: 8, alignItems: "center" },
    group: { gap: 10, paddingHorizontal: 16, paddingVertical: 14 },
    heading: { flexDirection: "row", alignItems: "center", gap: 6 },
    label: { color: t.dim, fontSize: 12, fontWeight: "600", letterSpacing: 0.6, textTransform: "uppercase" },
    pill: {
      flexDirection: "row",
      alignItems: "center",
      gap: 6,
      minHeight: 36,
      paddingHorizontal: 14,
      borderRadius: 18,
      backgroundColor: t.chip,
      maxWidth: 240,
    },
    on: { backgroundColor: t.accent },
    pressed: { opacity: 0.7 },
    dot: { width: 9, height: 9, borderRadius: 5 },
    text: { fontSize: 15 },
    onText: { fontWeight: "600" },
  });
