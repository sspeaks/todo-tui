import { shortDay } from "@todo-tui/present";
import { useState } from "react";
import { DateSheet } from "../pickers/DateSheet";
import { dayPresets } from "./presets";
import { Pill } from "./Pill";

/**
 * A day in one tap: Today, Tomorrow, Weekend, Next week; tapping the chosen
 * one clears it. "Pick…" opens the calendar for any other day, which then
 * shows as its own chosen pill.
 */
export function QuickDate({
  title,
  value,
  today,
  onChange,
}: {
  /** For the calendar sheet: "Due", "Hide until". */
  title: string;
  value: number | undefined;
  today: number;
  onChange: (day: number | undefined) => void;
}) {
  const [calendar, setCalendar] = useState(false);
  const presets = dayPresets(today);
  const other = value !== undefined && !presets.some((p) => p.day === value);
  return (
    <>
      {presets.map((p) => (
        <Pill key={p.label} label={p.label} on={value === p.day} onPress={() => onChange(value === p.day ? undefined : p.day)} accessibilityLabel={`${title}: ${p.label}`} />
      ))}
      <Pill
        icon="calendar-outline"
        label={other ? shortDay(value, today) : "Pick…"}
        on={other}
        onPress={() => setCalendar(true)}
        accessibilityLabel={`${title}: pick a date`}
      />
      {calendar ? <DateSheet title={title} visible value={value} today={today} onChange={onChange} onClose={() => setCalendar(false)} /> : null}
    </>
  );
}
