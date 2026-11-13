// What a capture will make, as ParseCapture read it: the title, then a tag
// per field it set, in the order the TUI's summary line gives them.
import { Ionicons } from "@expo/vector-icons";
import { type CaptureDraft, Status } from "@todo-tui/client/portable";
import { dayLabel, dayOf, describeRule, personName, singleLine } from "@todo-tui/present";
import { StyleSheet, Text, View } from "react-native";
import { PRIORITY_PILLS } from "./quick/presets";
import { type Theme, useTheme } from "../theme";

type Icon = keyof typeof Ionicons.glyphMap;

export function previewTags(draft: CaptureDraft, today: number): { icon: Icon; text: string; dot?: number }[] {
  const tags: { icon: Icon; text: string; dot?: number }[] = [];
  const p = PRIORITY_PILLS.findIndex((x) => x.value === draft.priority);
  if (p >= 0) tags.push({ icon: "flag-outline", text: PRIORITY_PILLS[p]!.label, dot: p });
  const due = draft.dueDate ?? draft.recurrenceFirst;
  if (due) tags.push({ icon: "calendar-outline", text: dayLabel(dayOf(due), today) });
  if (draft.recurrence) {
    const first = draft.recurrenceFirst;
    const from = first && due && dayOf(first) !== dayOf(due) ? `, first ${dayLabel(dayOf(first), today)}` : "";
    tags.push({ icon: "repeat", text: `${describeRule(draft.recurrence)}${from}` });
  }
  for (const l of draft.labels) tags.push({ icon: "pricetag-outline", text: singleLine(l) });
  // The person `~` named, by name (design 2.3); free text as typed.
  const waiting = draft.waitingPerson ? singleLine(draft.waitingPerson.displayName) : personName(draft.waitingOn);
  if (waiting) tags.push({ icon: "hourglass-outline", text: draft.waitingMatches > 1 ? `${waiting} (${draft.waitingMatches} match)` : waiting });
  if (draft.status === Status.NEXT) tags.push({ icon: "arrow-forward-circle-outline", text: "Next" });
  if (draft.status === Status.SOMEDAY) tags.push({ icon: "moon-outline", text: "Someday" });
  return tags;
}

/** `typed` is the title field: the parsed title shows only when tokens typed in it changed it. */
export function CapturePreview({ draft, today, typed }: { draft: CaptureDraft; today: number; typed: string }) {
  const t = useTheme();
  const s = styles(t);
  const dots = [t.p1, t.p2, t.p3, t.p4, t.p5];
  const tags = previewTags(draft, today);
  if (tags.length === 0) return null;
  return (
    <View style={s.wrap} accessibilityLabel={`Will add ${singleLine(draft.title)}: ${tags.map((x) => x.text).join(", ")}`}>
      {singleLine(draft.title) !== singleLine(typed) ? (
        <Text style={s.title} numberOfLines={2}>
          Title: {singleLine(draft.title)}
        </Text>
      ) : null}
      <View style={s.tags}>
        {tags.map((tag, i) => (
          <View key={`${tag.icon}${i}`} style={s.tag}>
            {tag.dot !== undefined ? <View style={[s.dot, { backgroundColor: dots[tag.dot] }]} /> : <Ionicons name={tag.icon} size={13} color={t.accent} />}
            <Text style={s.tagText}>{tag.text}</Text>
          </View>
        ))}
      </View>
    </View>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    wrap: { gap: 8, paddingTop: 12, borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: t.border },
    title: { color: t.dim, fontSize: 13 },
    tags: { flexDirection: "row", flexWrap: "wrap", gap: 6 },
    tag: { flexDirection: "row", alignItems: "center", gap: 5, paddingHorizontal: 9, paddingVertical: 4, borderRadius: 8, backgroundColor: t.grouped },
    dot: { width: 8, height: 8, borderRadius: 4 },
    tagText: { color: t.text, fontSize: 13 },
  });
