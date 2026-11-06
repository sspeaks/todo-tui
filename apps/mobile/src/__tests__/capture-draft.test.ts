import { Priority } from "@todo-tui/client/portable";
import { dayNumber } from "@todo-tui/present";
import { NO_CHOICES, captureText, cleanLabel, waitingToken } from "../capture-draft";

test("the choices become tokens after the title", () => {
  expect(captureText("call dentist", NO_CHOICES)).toBe("call dentist");
  expect(
    captureText("  call dentist ", {
      due: dayNumber(2026, 11, 6),
      priority: Priority.HIGH,
      labels: ["home", "health care"],
      waiting: "the vendor",
      status: "next",
      repeat: "@mon, thu",
    }),
  ).toBe('call dentist +2 ^2026-11-06 #home #health-care ~"the vendor" >next @mon,thu');
});

test("labels are one word, waiting names are quoted only when needed", () => {
  expect(cleanLabel(" #a b\u0007 ")).toBe("a-b");
  expect(cleanLabel("   ")).toBe("");
  expect(waitingToken("dana")).toBe("~dana");
  expect(waitingToken(' the "big" vendor ')).toBe('~"the big vendor"');
  expect(waitingToken('""')).toBeUndefined();
});

test("empty choices add nothing", () => {
  expect(captureText("x", { labels: ["", "  "], waiting: " ", repeat: " " })).toBe("x");
});
