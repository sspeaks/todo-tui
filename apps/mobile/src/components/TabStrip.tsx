import type { View as SavedView } from "@todo-tui/client/portable";
import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { size, space, type Theme, useTheme } from "../theme";

/**
 * The saved views as tabs, as the TUI's tab bar reads (`1 Inbox 4`): the
 * position dim, the name, the count when the view shows one. The selected
 * one is tinted, in the accent; the name keeps its weight, so the tabs
 * beside it do not shift.
 */
export function TabStrip({
  views,
  counts,
  selected,
  onSelect,
}: {
  views: readonly SavedView[];
  counts: ReadonlyMap<string, number>;
  selected: string | undefined;
  onSelect: (id: string) => void;
}) {
  const s = styles(useTheme());
  return (
    <ScrollView horizontal showsHorizontalScrollIndicator={false} style={s.strip} contentContainerStyle={s.content}>
      {views.map((v, i) => {
        const on = v.id === selected;
        const count = counts.get(v.id);
        return (
          <Pressable
            key={v.id}
            onPress={() => onSelect(v.id)}
            style={({ pressed }) => [s.tab, on && s.on, pressed && s.pressed]}
            accessibilityRole="tab"
            accessibilityState={{ selected: on }}
            accessibilityLabel={count ? `${v.name}, ${count}` : v.name}
          >
            <Text style={s.index}>{i + 1}</Text>
            <Text style={[s.name, on && s.onText]} numberOfLines={1}>
              {v.name}
            </Text>
            {count ? (
              <View style={[s.badge, on && s.onBadge]}>
                <Text style={[s.count, on && s.onCount]}>{count}</Text>
              </View>
            ) : null}
          </Pressable>
        );
      })}
    </ScrollView>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    strip: { flexGrow: 0, borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: t.border },
    content: { paddingHorizontal: space.s, paddingVertical: space.s, gap: space.xs },
    tab: { flexDirection: "row", alignItems: "center", gap: 6, paddingHorizontal: 10, paddingVertical: 7, borderRadius: 6 },
    on: { backgroundColor: t.accentSoft },
    pressed: { opacity: 0.6 },
    index: { ...t.mono, color: t.dim, fontSize: size.caption },
    name: { color: t.text, fontSize: 15, fontWeight: "500", maxWidth: 180 },
    onText: { color: t.accent },
    badge: { borderRadius: 4, paddingHorizontal: 5, paddingVertical: 1, backgroundColor: t.chip },
    onBadge: { backgroundColor: t.accent },
    count: { ...t.mono, color: t.dim, fontSize: size.caption },
    onCount: { color: t.onAccent },
  });
