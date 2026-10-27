import { describe, expect, it } from "vitest";
import { dayLabel, dayNumber, isoDay, localDay, longDay, relativeDays, shortDay } from "./dates.js";
import { rowCells } from "./item.js";
import { create, ItemSchema, Status } from "@todo-tui/client";

const today = dayNumber(2026, 10, 26); // a Monday

describe("date labels", () => {
  it("labels days relative to today", () => {
    expect(shortDay(today, today)).toBe("Today");
    expect(shortDay(today + 1, today)).toBe("Tom");
    expect(shortDay(today + 4, today)).toBe("Fri");
    expect(shortDay(today + 7, today)).toBe("Nov 2");
    expect(shortDay(today - 2, today)).toBe("Oct 24");
    expect(shortDay(dayNumber(2027, 1, 5), today)).toBe("2027-01-05");
    expect(longDay(today + 4)).toBe("Fri Oct 30 2026");
    expect(dayLabel(today + 4, today)).toBe("Fri Oct 30");
    expect(dayLabel(dayNumber(2027, 1, 5), today)).toBe("Tue Jan 5 2027");
    expect(isoDay(today)).toBe("2026-10-26");
    expect([relativeDays(today, today), relativeDays(today + 3, today), relativeDays(today - 2, today)]).toEqual([
      "today",
      "in 3 days",
      "2 days ago",
    ]);
  });

  it("takes the local date of a time", () => {
    expect(localDay(new Date(Date.UTC(2026, 9, 26, 23, 59)))).toBe(today); // TZ=UTC in the test env
  });

  it("builds a row's cells from today: due, overdue, waiting age and follow-up, markers", () => {
    const item = create(ItemSchema, {
      status: Status.INBOX,
      dueDate: { year: 2026, month: 10, day: 25 },
      isOverdue: true,
      waiting: { on: { ref: { case: "freeText", value: "dana\x1b[31m" } }, since: { year: 2026, month: 10, day: 23 }, followUpOn: { year: 2026, month: 10, day: 26 } },
      deferUntil: { year: 2026, month: 11, day: 3 },
      pendingFields: [1],
      hasConflict: true,
      recurrence: {},
      remoteLink: { key: "PLAT-1" },
    });
    expect(rowCells(item, today)).toEqual({
      key: "PLAT-1",
      due: "^Oct 25",
      overdue: true,
      waiting: "⧗dana 3d",
      followUpDue: true,
      markers: "⏸Nov 3 ↑ ! 🔁",
    });
    expect(rowCells(create(ItemSchema, { deferUntil: { year: 2026, month: 10, day: 26 } }), today)).toMatchObject({ due: "", waiting: "", markers: "" });
  });
});
