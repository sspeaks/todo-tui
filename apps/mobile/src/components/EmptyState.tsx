// What a list shows with no rows: an icon, the line in mono (as the TUI
// would print it), and a dim hint of what to do.
import { Ionicons } from "@expo/vector-icons";
import { StyleSheet, Text, View } from "react-native";
import { size, space, type Theme, useTheme } from "../theme";

export type EmptyIcon = keyof typeof Ionicons.glyphMap;

export function EmptyState({ icon, text, hint, tone }: { icon: EmptyIcon; text: string; hint?: string | undefined; tone?: "warn" }) {
  const t = useTheme();
  const s = styles(t);
  return (
    <View style={s.wrap} accessibilityRole="summary">
      <Ionicons name={icon} size={36} color={tone === "warn" ? t.warn : t.dim} />
      <Text style={[s.text, tone === "warn" && s.warn]}>{text}</Text>
      {hint ? <Text style={s.hint}>{hint}</Text> : null}
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    wrap: { alignItems: "center", gap: space.s, marginTop: 72, paddingHorizontal: space.xl },
    text: { ...t.mono, color: t.dim, fontSize: 14, textAlign: "center" },
    warn: { color: t.warn },
    hint: { color: t.dim, fontSize: size.caption, textAlign: "center", opacity: 0.8 },
  });
