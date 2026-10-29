// The two-month calendar's grid: months as weeks of day numbers, Monday
// first (ISO weeks, as ANCHOR_START_OF_WEEK and the review day).

import { civil, dayNumber } from "./dates.js";

const MONTH_NAMES = [
  "January",
  "February",
  "March",
  "April",
  "May",
  "June",
  "July",
  "August",
  "September",
  "October",
  "November",
  "December",
];

/** Always six, so the calendar's height does not change from month to month. */
export const WEEK_ROWS = 6;

export interface Month {
  readonly year: number;
  /** 1–12. */
  readonly month: number;
}

/** The month holding day `n`. */
export const monthOf = (n: number): Month => {
  const { year, month } = civil(n);
  return { year, month };
};

export const nextMonth = ({ year, month }: Month): Month => (month === 12 ? { year: year + 1, month: 1 } : { year, month: month + 1 });

export const firstDay = ({ year, month }: Month): number => dayNumber(year, month, 1);

export const lastDay = (m: Month): number => firstDay(nextMonth(m)) - 1;

/** "October 2026" */
export const monthTitle = ({ year, month }: Month): string => `${MONTH_NAMES[month - 1]} ${year}`;

/** WEEK_ROWS weeks of seven day numbers, Monday first; undefined outside the month. */
export function monthWeeks(m: Month): (number | undefined)[][] {
  const first = firstDay(m);
  const last = lastDay(m);
  // Days since the Monday on or before the 1st.
  const lead = (civil(first).weekday + 6) % 7;
  return Array.from({ length: WEEK_ROWS }, (_, w) =>
    Array.from({ length: 7 }, (_, d) => {
      const n = first - lead + w * 7 + d;
      return n >= first && n <= last ? n : undefined;
    }),
  );
}
