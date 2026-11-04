import { type Item, Status } from "@todo-tui/client/portable";
import { priorityLabel, rowCells, singleLine, statusGlyph } from "@todo-tui/present";
import { memo } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { type Theme, useTheme } from "../theme";

const closed = (s: Status) => s === Status.DONE || s === Status.CANCELLED;

/** One Item as the TUI's row reads: glyph, priority, title, then due, waiting, progress and markers. */
export const ItemRow = memo(function ItemRow({
  item,
  today,
  onToggle,
  onOpen,
}: {
  item: Item;
  today: number;
  onToggle: (item: Item) => void;
  onOpen: (item: Item) => void;
}) {
  const t = useTheme();
  const s = styles(t);
  const cells = rowCells(item, today);
  const priority = priorityLabel(item);
  const done = closed(item.status);
  const meta = [cells.waiting, cells.progress, cells.markers, item.labels.map((l) => `#${singleLine(l)}`).join(" ")].filter(Boolean);
  return (
    <View style={s.row}>
      <Pressable
        onPress={() => onToggle(item)}
        hitSlop={12}
        accessibilityRole="checkbox"
        accessibilityState={{ checked: done }}
        accessibilityLabel={done ? "Reopen" : "Complete"}
      >
        <Text style={[s.glyph, done && s.dim]}>{statusGlyph(item.status)}</Text>
      </Pressable>
      <Pressable style={s.body} onPress={() => onOpen(item)} accessibilityRole="button" accessibilityHint="Opens the details">
        <Text style={[s.title, done && s.doneTitle]} numberOfLines={2}>
          {priority ? <Text style={s.priority}>{priority} </Text> : null}
          {singleLine(item.title)}
          {cells.parent ? <Text style={s.dim}>{`  ↳ ${cells.parent}`}</Text> : null}
        </Text>
        {meta.length > 0 ? (
          <Text style={[s.meta, cells.followUpDue && s.warn]} numberOfLines={1}>
            {meta.join("  ")}
          </Text>
        ) : null}
      </Pressable>
      {cells.due ? <Text style={[s.due, cells.overdue && s.overdue]}>{cells.due}</Text> : null}
    </View>
  );
});

const styles = (t: Theme) =>
  StyleSheet.create({
    // Opaque, to cover the swipe actions under it.
    row: { flexDirection: "row", alignItems: "flex-start", paddingHorizontal: 16, paddingVertical: 12, gap: 12, backgroundColor: t.bg },
    glyph: { fontSize: 20, color: t.accent, width: 24, textAlign: "center", lineHeight: 22 },
    body: { flex: 1, gap: 2 },
    title: { color: t.text, fontSize: 16, lineHeight: 22 },
    doneTitle: { color: t.dim, textDecorationLine: "line-through" },
    priority: { color: "#c026d3", fontWeight: "600" },
    meta: { color: t.dim, fontSize: 13 },
    warn: { color: t.warn },
    due: { color: t.dim, fontSize: 13, lineHeight: 22 },
    overdue: { color: t.danger, fontWeight: "600" },
    dim: { color: t.dim },
  });
