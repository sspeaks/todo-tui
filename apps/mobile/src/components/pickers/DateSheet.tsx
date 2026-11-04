import DateTimePicker from "@react-native-community/datetimepicker";
import { civil, dayLabel } from "@todo-tui/present";
import { useState } from "react";
import { Platform, Text } from "react-native";
import { dayOfDate, jsDate } from "../../dates";
import { useTheme } from "../../theme";
import { Option, Sheet, sheetStyles } from "./Sheet";

/** Quick choices counted from `today`: today, tomorrow, the weekend, next Monday, a week out. */
function quick(today: number): { label: string; day: number }[] {
  const wd = civil(today).weekday; // 0 = Sunday
  const weekend = wd === 6 || wd === 0 ? today : today + (6 - wd);
  const monday = today + (((1 - wd + 7) % 7) || 7);
  return [
    { label: "Today", day: today },
    { label: "Tomorrow", day: today + 1 },
    { label: wd === 6 || wd === 0 ? "This weekend (today)" : "This weekend", day: weekend },
    { label: "Next week", day: monday },
    { label: "In a week", day: today + 7 },
  ];
}

/** Picks a day: quick choices, then a calendar. `onChange(undefined)` clears it. */
export function DateSheet({
  title,
  visible,
  value,
  today,
  onChange,
  onClose,
}: {
  title: string;
  visible: boolean;
  value: number | undefined;
  today: number;
  onChange: (day: number | undefined) => void;
  onClose: () => void;
}) {
  const t = useTheme();
  const s = sheetStyles(t);
  const [androidPicker, setAndroidPicker] = useState(false);
  const pick = (day: number | undefined) => {
    onChange(day);
    onClose();
  };
  return (
    <Sheet title={title} visible={visible} onClose={onClose} {...(value !== undefined && { onClear: () => pick(undefined) })}>
      {quick(today).map((q) => (
        <Option key={q.label} label={q.label} detail={dayLabel(q.day, today)} on={value === q.day} onPress={() => pick(q.day)} />
      ))}
      {Platform.OS === "ios" ? (
        <DateTimePicker
          value={jsDate(value ?? today)}
          mode="date"
          display="inline"
          accentColor={t.accent}
          onValueChange={(_, date) => onChange(dayOfDate(date))}
        />
      ) : (
        <>
          <Option label="Pick a date…" {...(value !== undefined && { detail: dayLabel(value, today) })} onPress={() => setAndroidPicker(true)} />
          {androidPicker ? (
            <DateTimePicker
              value={jsDate(value ?? today)}
              mode="date"
              onValueChange={(_, date) => {
                setAndroidPicker(false);
                pick(dayOfDate(date));
              }}
              onDismiss={() => setAndroidPicker(false)}
            />
          ) : null}
        </>
      )}
      {value !== undefined ? <Text style={s.hint}>{dayLabel(value, today)}</Text> : null}
    </Sheet>
  );
}
