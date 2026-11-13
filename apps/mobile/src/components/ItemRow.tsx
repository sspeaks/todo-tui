import { Ionicons } from "@expo/vector-icons";
import { type Item, Status } from "@todo-tui/client/portable";
import { dayOf, personName, priorityLabel, priorityName, rowCells, singleLine } from "@todo-tui/present";
import { memo, useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { priorityColor, size, space, type Theme, useTheme } from "../theme";
import { StatusMark } from "./StatusMark";

const closed = (s: Status) => s === Status.DONE || s === Status.CANCELLED;

/**
 * One Item as the TUI's row reads, in a phone's shape: the priority as the
 * row's coloured edge, the status mark, the title (and its parent), then a
 * mono line of what the TUI prints in columns (waiting, labels, progress,
 * the Jira key, markers) and the due date on the right. Tapping the mark
 * shows the change at once, and takes it back if it fails.
 */
export const ItemRow = memo(function ItemRow({
  item,
  today,
  onToggle,
  onOpen,
}: {
  item: Item;
  today: number;
  onToggle: (item: Item) => Promise<boolean>;
  onOpen: (item: Item) => void;
}) {
  const t = useTheme();
  const s = styles(t);
  /** The status a tap asked for, shown until the Item's own status moves off the one it was asked from. */
  const [asked, setAsked] = useState<{ status: Status; from: Status }>();
  // Moved (the change landed, or another): forgotten, so that an Undo back to `from` shows as it is.
  if (asked && asked.from !== item.status) setAsked(undefined);
  const status = asked?.from === item.status ? asked.status : item.status;
  const done = closed(status);
  const cells = rowCells(item, today);
  const edge = priorityColor(t, item.effectivePriority);
  const priority = priorityLabel(item);
  const dueToday = item.dueDate !== undefined && dayOf(item.dueDate) === today;
  const waitingOn = item.waiting ? personName(item.waiting.on) : "";

  const toggle = async () => {
    // One change at a time: a second tap while the first is on its way (or the row lingers, done) would send the same change again.
    if (asked) return;
    setAsked({ status: closed(item.status) ? Status.NEXT : Status.DONE, from: item.status });
    if (!(await onToggle(item))) setAsked(undefined);
  };

  const spoken = [
    singleLine(item.title),
    cells.parent && `in ${cells.parent}`,
    priority && `priority ${priorityName(item.effectivePriority)}`,
    cells.due && `${cells.overdue ? "overdue, was due" : "due"} ${cells.due.slice(1)}`,
    waitingOn && `waiting on ${waitingOn}${cells.followUpDue ? ", follow-up due" : ""}`,
    item.labels.length ? item.labels.map((l) => `#${l}`).join(" ") : "",
    item.subtasksTotal ? `${item.subtasksDone} of ${item.subtasksTotal} subtasks done` : "",
  ]
    .filter(Boolean)
    .join(", ");

  const hasMeta = waitingOn || item.labels.length || cells.progress || cells.key || cells.markers || (priority.endsWith("*") && priority);
  return (
    <View style={s.row}>
      {edge ? <View style={[s.edge, { backgroundColor: edge }]} testID="priority-edge" /> : null}
      <Pressable
        onPress={() => void toggle()}
        hitSlop={12}
        style={s.mark}
        accessibilityRole="checkbox"
        accessibilityState={{ checked: done }}
        accessibilityLabel={done ? "Reopen" : "Complete"}
      >
        <StatusMark status={status} />
      </Pressable>
      <Pressable style={s.body} onPress={() => onOpen(item)} accessibilityRole="button" accessibilityLabel={spoken} accessibilityHint="Opens the details">
        <Text style={[s.title, done && s.doneTitle]} numberOfLines={2}>
          {singleLine(item.title)}
          {cells.parent ? <Text style={s.dim}>{`  ↳ ${cells.parent}`}</Text> : null}
        </Text>
        {hasMeta ? (
          <View style={s.meta}>
            {waitingOn ? (
              <View style={s.segment}>
                <Ionicons name="hourglass-outline" size={12} color={cells.followUpDue ? t.warn : t.dim} />
                <Text style={[s.metaText, cells.followUpDue && s.warn]} numberOfLines={1}>
                  {singleLine(waitingOn)}
                  {cells.waitingAge}
                </Text>
              </View>
            ) : null}
            {item.labels.map((l) => (
              <Text key={l} style={s.label} numberOfLines={1}>
                #{singleLine(l)}
              </Text>
            ))}
            {cells.progress ? <Text style={s.metaText}>{cells.progress}</Text> : null}
            {cells.key ? <Text style={s.metaText}>{cells.key}</Text> : null}
            {priority.endsWith("*") ? <Text style={s.metaText}>{priority}</Text> : null}
            {cells.markers ? <Text style={s.metaText}>{cells.markers}</Text> : null}
          </View>
        ) : null}
      </Pressable>
      {cells.due ? <Text style={[s.due, dueToday && s.today, cells.overdue && s.overdue]}>{cells.due}</Text> : null}
    </View>
  );
});

/** The row's left padding, the mark and the gap: the list's separators line up with the title. */
export const ROW_INSET = space.l + 24 + space.m;

const styles = (t: Theme) =>
  StyleSheet.create({
    // Opaque, to cover the swipe actions under it.
    row: { flexDirection: "row", alignItems: "flex-start", paddingLeft: space.l, paddingRight: space.l, paddingVertical: space.m, gap: space.m, backgroundColor: t.bg },
    edge: { position: "absolute", left: 0, top: 0, bottom: 0, width: 3 },
    // Centred in the row, however many lines the title and meta take.
    mark: { alignSelf: "center" },
    body: { flex: 1, gap: 5 },
    title: { color: t.text, fontSize: size.body, lineHeight: 22 },
    doneTitle: { color: t.dim, textDecorationLine: "line-through" },
    meta: { flexDirection: "row", flexWrap: "wrap", alignItems: "center", columnGap: space.s, rowGap: space.xs },
    segment: { flexDirection: "row", alignItems: "center", gap: 3 },
    metaText: { ...t.mono, color: t.dim, fontSize: size.caption },
    label: { ...t.mono, color: t.accent, fontSize: size.caption, backgroundColor: t.accentSoft, borderRadius: 4, paddingHorizontal: 5, paddingVertical: 1, overflow: "hidden" },
    warn: { color: t.warn },
    due: { ...t.mono, color: t.dim, fontSize: size.meta, lineHeight: 22 },
    today: { ...t.monoBold, color: t.accent },
    overdue: { ...t.monoBold, color: t.danger },
    dim: { color: t.dim },
  });
