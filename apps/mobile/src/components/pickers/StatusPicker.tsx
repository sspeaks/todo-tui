import { Status } from "@todo-tui/client/portable";
import { Pressable, ScrollView, StyleSheet, Text } from "react-native";
import { type Theme, useTheme } from "../../theme";

export const OPEN_STATUSES: { value: Status; label: string }[] = [
  { value: Status.INBOX, label: "Inbox" },
  { value: Status.NEXT, label: "Next" },
  { value: Status.SOMEDAY, label: "Someday" },
];

export const ALL_STATUSES: { value: Status; label: string }[] = [
  ...OPEN_STATUSES,
  { value: Status.IN_PROGRESS, label: "Doing" },
  { value: Status.DONE, label: "Done" },
  { value: Status.CANCELLED, label: "Cancelled" },
];

/** A row of segments, one per status. */
export function StatusPicker({
  value,
  options,
  onChange,
}: {
  value: Status;
  options: readonly { value: Status; label: string }[];
  onChange: (s: Status) => void;
}) {
  const s = styles(useTheme());
  return (
    <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={s.row}>
      {options.map((o) => {
        const on = o.value === value;
        return (
          <Pressable key={o.value} style={[s.seg, on && s.on]} onPress={() => onChange(o.value)} accessibilityRole="button" accessibilityState={{ selected: on }}>
            <Text style={[s.text, on && s.onText]}>{o.label}</Text>
          </Pressable>
        );
      })}
    </ScrollView>
  );
}

const styles = (t: Theme) =>
  StyleSheet.create({
    row: { gap: 6 },
    seg: { paddingHorizontal: 12, paddingVertical: 8, borderRadius: 8, backgroundColor: t.surface },
    on: { backgroundColor: t.accent },
    text: { color: t.text, fontWeight: "500" },
    onText: { color: t.onAccent },
  });
