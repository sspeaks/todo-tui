import { type ClauseSchema, ItemField, type MessageInitShape, Operator, type ViewQuery, useView } from "@todo-tui/client";
import { Box, Text } from "ink";
import { useMemo } from "react";
import { WEEK_ROWS, firstDay, lastDay, monthOf, monthTitle, monthWeeks, nextMonth } from "../format/calendar.js";
import { civil, dayOf } from "../format/dates.js";
import { fit } from "../format/text.js";
import { CALENDAR_ROWS } from "../state/layout.js";
import { useToday } from "./hooks.js";

const WEEKDAYS = "Su Mo Tu We Th Fr Sa";
/** Seven two-cell days and the spaces between them. */
const MONTH_WIDTH = WEEKDAYS.length;
const GAP = "  ";

type ClauseInit = MessageInitShape<typeof ClauseSchema>;

const dueOn = (op: Operator, n: number): ClauseInit => {
  const { year, month, day } = civil(n);
  return {
    kind: {
      case: "predicate",
      value: { target: ItemField.DUE_DATE, op, operands: [{ kind: { case: "value", value: { kind: { case: "dateValue", value: { year, month, day } } } } }] },
    },
  };
};

/** Open Items due from `from` to `to`: the implicit rules leave out done, archived and deferred ones, as the views do. */
const dueBetween = (from: number, to: number): ViewQuery => ({ filter: { allOf: [dueOn(Operator.GE, from), dueOn(Operator.LE, to)] } });

/** One week of a month: today in inverse, days with Items due in bold (red when past). */
function Week({ days, today, due }: { days: readonly (number | undefined)[]; today: number; due: ReadonlySet<number> }) {
  return days.map((n, i) => (
    <Text key={i}>
      {i ? " " : ""}
      {n === undefined ? (
        "  "
      ) : (
        <Text inverse={n === today} bold={due.has(n)} color={due.has(n) && n < today ? "red" : undefined}>
          {String(civil(n).day).padStart(2)}
        </Text>
      )}
    </Text>
  ));
}

/** This month and the next, side by side, under the detail pane. */
export function Calendar({ width }: { width: number }) {
  const today = useToday();
  const [first, second] = useMemo(() => {
    const m = monthOf(today);
    return [m, nextMonth(m)] as const;
  }, [today]);
  const snap = useView(dueBetween(firstDay(first), lastDay(second)));
  const due = useMemo(() => new Set(snap.items.flatMap((i) => (i.dueDate ? [dayOf(i.dueDate)] : []))), [snap.items]);
  const weeks = [monthWeeks(first), monthWeeks(second)];
  return (
    <Box flexDirection="column" width={width} height={CALENDAR_ROWS} flexShrink={0} paddingLeft={1} paddingTop={1}>
      <Text bold wrap="truncate-end">
        {fit(monthTitle(first), MONTH_WIDTH)}
        {GAP}
        {monthTitle(second)}
      </Text>
      <Text dimColor wrap="truncate-end">
        {WEEKDAYS}
        {GAP}
        {WEEKDAYS}
      </Text>
      {Array.from({ length: WEEK_ROWS }, (_, w) => (
        <Text key={w} wrap="truncate-end">
          <Week days={weeks[0]?.[w] ?? []} today={today} due={due} />
          {GAP}
          <Week days={weeks[1]?.[w] ?? []} today={today} due={due} />
        </Text>
      ))}
    </Box>
  );
}
