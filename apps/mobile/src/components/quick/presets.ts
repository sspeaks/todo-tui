// The one-tap choices: days counted from today, priorities, repeat rules.
import { Priority, type RecurrenceRule, Weekday } from "@todo-tui/client/portable";
import { civil } from "@todo-tui/present";

/** Today, tomorrow, the weekend (Saturday, or today on one), next Monday. */
export function dayPresets(today: number): { label: string; day: number }[] {
  const wd = civil(today).weekday; // 0 = Sunday
  const weekend = wd === 6 || wd === 0 ? today : today + (6 - wd);
  const monday = today + ((1 - wd + 7) % 7 || 7);
  const out = [
    { label: "Today", day: today },
    { label: "Tomorrow", day: today + 1 },
    { label: "Weekend", day: weekend },
    { label: "Next week", day: monday },
  ];
  // The weekend can be today or tomorrow: keep one pill per day.
  return out.filter((p, i) => out.findIndex((q) => q.day === p.day) === i);
}

export const PRIORITY_PILLS: { value: Priority; label: string }[] = [
  { value: Priority.HIGHEST, label: "+1" },
  { value: Priority.HIGH, label: "+2" },
  { value: Priority.MEDIUM, label: "+3" },
  { value: Priority.LOW, label: "+4" },
  { value: Priority.LOWEST, label: "+5" },
];

/** Repeat rules one tap sets, as ParseRecurrence takes them. */
export const REPEAT_PRESETS = [
  { label: "Daily", text: "daily" },
  { label: "Weekdays", text: "weekdays" },
  { label: "Weekly", text: "weekly" },
  { label: "Monthly", text: "monthly" },
] as const;

export type RepeatPreset = (typeof REPEAT_PRESETS)[number]["text"];

const WEEKDAYS: readonly Weekday[] = [Weekday.MONDAY, Weekday.TUESDAY, Weekday.WEDNESDAY, Weekday.THURSDAY, Weekday.FRIDAY];

/** Which preset an Item's rule is, if any: a weekly on one day is "weekly", on Mon–Fri "weekdays". */
export function presetOf(rule: RecurrenceRule | undefined): RepeatPreset | undefined {
  const k = rule?.kind;
  switch (k?.case) {
    case "daily":
      return "daily";
    case "weekly": {
      const days = k.value.days;
      if (days.length === 1) return "weekly";
      return days.length === WEEKDAYS.length && days.every((d, i) => d === WEEKDAYS[i]) ? "weekdays" : undefined;
    }
    case "monthly":
      return "monthly";
    default:
      return undefined;
  }
}
