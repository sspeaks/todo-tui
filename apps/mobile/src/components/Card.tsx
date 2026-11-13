// Grouped cards, as iOS settings: a mono caption, then a card of rows with
// hairlines between them. A row is an icon, a label and a mono value; a
// field's one-tap choices sit under it.
import { Ionicons } from "@expo/vector-icons";
import { Children, isValidElement, type ReactNode } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { size, space, type Theme, useTheme } from "../theme";

type Icon = keyof typeof Ionicons.glyphMap;

/** A caption (none for a card of its own, as the title's) and the card, its rows parted by hairlines. */
export function CardGroup({ title, children }: { title?: string; children: ReactNode }) {
  const s = styles(useTheme());
  const rows = Children.toArray(children).filter(Boolean);
  return (
    <View style={s.group}>
      {title ? <Text style={s.caption}>{title}</Text> : null}
      <View style={s.card}>
        {rows.map((row, i) => (
          // The row's own key, so that a row coming or going leaves the others mounted.
          <View key={isValidElement(row) && row.key !== null ? row.key : i}>
            {i > 0 ? <View style={s.hairline} /> : null}
            {row}
          </View>
        ))}
      </View>
    </View>
  );
}

/**
 * One row: `icon` and `label`, `value` on the right in mono (red with
 * `danger`, or `valueColor`), and `children` under them. With `onPress` it
 * is a button; `accent` and `danger` colour its label, `center` centres it
 * (a lone action, as Delete).
 */
export function CardRow({
  icon,
  label,
  value,
  valueColor,
  danger,
  accent,
  center,
  onPress,
  children,
}: {
  icon?: Icon;
  label?: string;
  value?: string | undefined;
  valueColor?: string;
  danger?: boolean;
  accent?: boolean;
  center?: boolean;
  onPress?: () => void;
  children?: ReactNode;
}) {
  const t = useTheme();
  const s = styles(t);
  const color = danger && onPress ? t.danger : accent ? t.accent : t.text;
  const head = label !== undefined ? (
    <View style={[s.head, center && s.center]}>
      {icon ? <Ionicons name={icon} size={18} color={danger && onPress ? t.danger : accent ? t.accent : t.dim} /> : null}
      <Text style={[s.label, center && s.centerLabel, { color }]} numberOfLines={2}>
        {label}
      </Text>
      {value ? (
        <Text style={[s.value, danger && !onPress && s.danger, valueColor !== undefined && { color: valueColor }]} numberOfLines={3}>
          {value}
        </Text>
      ) : null}
    </View>
  ) : null;
  const body = children ? <View style={[head && s.body, label !== undefined && icon !== undefined && s.indent]}>{children}</View> : null;
  if (!onPress)
    return (
      // A label and its value are one stop for VoiceOver; a row of choices keeps them apart.
      <View style={s.row} accessible={!children && label !== undefined}>
        {head}
        {body}
      </View>
    );
  return (
    <Pressable style={({ pressed }) => [s.row, pressed && s.pressed]} onPress={onPress} accessibilityRole="button">
      {head}
      {body}
    </Pressable>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    group: { gap: 6 },
    caption: { ...t.mono, color: t.dim, fontSize: size.caption, letterSpacing: 0.8, textTransform: "uppercase", paddingHorizontal: space.xs },
    card: { backgroundColor: t.card, borderRadius: 12, overflow: "hidden" },
    hairline: { height: StyleSheet.hairlineWidth, backgroundColor: t.border, marginLeft: 14 },
    row: { paddingHorizontal: 14, paddingVertical: 12 },
    pressed: { opacity: 0.6 },
    head: { flexDirection: "row", alignItems: "center", gap: space.m },
    center: { justifyContent: "center" },
    label: { flex: 1, fontSize: 15 },
    centerLabel: { flex: 0, textAlign: "center", fontWeight: "600" },
    value: { ...t.mono, flexShrink: 1, maxWidth: "60%", color: t.dim, fontSize: size.meta, textAlign: "right" },
    danger: { ...t.monoBold, color: t.danger },
    body: { marginTop: 10 },
    // Under the label, past the icon.
    indent: { marginLeft: 30 },
  });
