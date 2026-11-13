import { Ionicons } from "@expo/vector-icons";
import { type ReactNode, createContext, useContext } from "react";
import { Keyboard, Pressable, StyleSheet, Text, View } from "react-native";
import { tick } from "../../haptics";
import { type Theme, useTheme } from "../../theme";

type Icon = keyof typeof Ionicons.glyphMap;

/**
 * How the pills inside look and behave: `compact` (squarer, shorter, for a
 * sheet that must fit on one screen) and `dismissKeyboard` (a choice closes
 * the keyboard, so every section is in sight). Off by default.
 */
export interface PillStyle {
  compact: boolean;
  dismissKeyboard: boolean;
}
export const PillStyleContext = createContext<PillStyle>({ compact: false, dismissKeyboard: false });

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
  mono,
  accessibilityLabel,
}: {
  label: string;
  /** In mono, as the list prints it (`#work`). */
  mono?: boolean;
  on?: boolean;
  onPress: () => void;
  icon?: Icon;
  dot?: string;
  accessibilityLabel?: string;
}) {
  const t = useTheme();
  const s = styles(t);
  const { compact, dismissKeyboard } = useContext(PillStyleContext);
  const fg = on ? t.onAccent : t.text;
  return (
    <Pressable
      style={({ pressed }) => [s.pill, compact && s.compact, on && s.on, pressed && s.pressed]}
      onPress={() => {
        tick();
        if (dismissKeyboard) Keyboard.dismiss();
        onPress();
      }}
      hitSlop={4}
      accessibilityRole="button"
      accessibilityState={{ selected: !!on }}
      accessibilityLabel={accessibilityLabel ?? label}
    >
      {on ? <Ionicons name="checkmark" size={15} color={fg} /> : dot ? <View style={[s.dot, { backgroundColor: dot }]} /> : icon ? <Ionicons name={icon} size={15} color={t.dim} /> : null}
      <Text style={[s.text, compact && s.compactText, mono && s.mono, { color: fg }, on && s.onText]} numberOfLines={1}>
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
  const { compact } = useContext(PillStyleContext);
  // Compact: the icon alone, beside the pills, saves the heading's line.
  if (compact)
    return (
      <View style={[s.group, s.compactGroup]} accessibilityLabel={label}>
        <View style={s.compactIcon}>
          <Ionicons name={icon} size={17} color={t.dim} />
        </View>
        <View style={[s.row, s.grow]}>{children}</View>
      </View>
    );
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
    // Squarer and shorter: the add sheet fits on one screen.
    compact: { borderRadius: 8, minHeight: 32, paddingHorizontal: 10, gap: 5 },
    compactText: { fontSize: 14 },
    compactGroup: { flexDirection: "row", alignItems: "flex-start", gap: 10, paddingVertical: 8, paddingHorizontal: 12 },
    compactIcon: { width: 20, height: 32, alignItems: "center", justifyContent: "center" },
    grow: { flex: 1 },
    pressed: { opacity: 0.7 },
    dot: { width: 9, height: 9, borderRadius: 5 },
    text: { fontSize: 15 },
    mono: { ...t.mono, fontSize: 14 },
    onText: { fontWeight: "600" },
  });
