import type { View as SavedView } from "@todo-tui/client/portable";
import { Pressable, ScrollView, StyleSheet, Text } from "react-native";
import { type Theme, useTheme } from "../theme";

/** The saved views as tabs, each with its count when the view shows one. */
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
      {views.map((v) => {
        const on = v.id === selected;
        const count = counts.get(v.id);
        return (
          <Pressable key={v.id} onPress={() => onSelect(v.id)} style={[s.tab, on && s.on]} accessibilityRole="tab" accessibilityState={{ selected: on }}>
            <Text style={[s.name, on && s.onText]}>
              {v.name}
              {count ? <Text style={[s.count, on && s.onText]}> {count}</Text> : null}
            </Text>
          </Pressable>
        );
      })}
    </ScrollView>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    strip: { flexGrow: 0, borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: t.border },
    content: { paddingHorizontal: 8, paddingVertical: 8, gap: 6 },
    tab: { paddingHorizontal: 12, paddingVertical: 6, borderRadius: 16, backgroundColor: t.surface },
    on: { backgroundColor: t.accent },
    name: { color: t.text, fontWeight: "500" },
    count: { color: t.dim, fontWeight: "400" },
    onText: { color: t.onAccent },
  });
