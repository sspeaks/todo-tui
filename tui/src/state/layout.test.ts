import { describe, expect, it } from "vitest";
import { MIN_TITLE, ROW_PREFIX, layout, rowColumns, tabLabels } from "./layout.js";

describe("layout", () => {
  it("splits at 110 columns, sizes the body, and is too small below 40×8", () => {
    expect(layout(120, 30)).toMatchObject({ wide: true, bodyHeight: 27, detailWidth: 48, listWidth: 71, tooSmall: false });
    expect(layout(109, 30)).toMatchObject({ wide: false, listWidth: 109, detailWidth: 109 });
    expect(layout(39, 30).tooSmall).toBe(true);
    expect(layout(40, 7).tooSmall).toBe(true);
    expect(layout(40, 8)).toMatchObject({ tooSmall: false, bodyHeight: 5 });
  });

  it("drops row columns in order (markers, waiting, key, due) to keep the title at least 20", () => {
    const natural = { key: 8, due: 7, waiting: 10, markers: 6 };
    expect(rowColumns(100, natural)).toEqual({ ...natural, title: 100 - ROW_PREFIX - (8 + 7 + 10 + 6 + 4 * 2) });
    expect(rowColumns(60, natural)).toEqual({ markers: 0, waiting: 10, key: 8, due: 7, title: 21 });
    expect(rowColumns(50, natural)).toEqual({ markers: 0, waiting: 0, key: 8, due: 7, title: 23 });
    expect(rowColumns(40, natural)).toEqual({ markers: 0, waiting: 0, key: 0, due: 7, title: 23 });
    expect(rowColumns(30, natural)).toEqual({ markers: 0, waiting: 0, key: 0, due: 0, title: 30 - ROW_PREFIX });
    expect(rowColumns(60, natural).title).toBeGreaterThanOrEqual(MIN_TITLE);
    expect(rowColumns(80, { key: 0, due: 0, waiting: 0, markers: 0 }).title).toBe(80 - ROW_PREFIX);
  });

  it("labels tabs with non-zero counts, and shortens the others when they do not fit", () => {
    const views = ["Inbox", "Today", "Next", "Waiting", "Someday", "Archive"].map((name) => ({ id: name, name }));
    const counts = new Map([
      ["Inbox", 3],
      ["Today", 0],
      ["Next", 12],
    ]);
    expect(tabLabels(views, counts, "Inbox", 200).map((l) => l.text)).toEqual(["1 Inbox 3", "2 Today", "3 Next 12", "4 Waiting", "5 Someday", "6 Archive"]);
    const short = tabLabels(views, counts, "Next", 40);
    expect(short.map((l) => l.text)).toEqual(["1 I3", "2 T", "3 Next 12", "4 W", "5 S", "6 A"]);
    expect(short.find((l) => l.current)?.id).toBe("Next");
  });
});
