import { ItemField, ListItemsRequestSchema, Operator, create } from "@todo-tui/client";
import { describe, expect, it } from "vitest";
import { matchText, searchQuery, searchTab, tabQuery } from "./tabs.js";

const targets = (clauses: readonly { kind: { case?: string; value?: unknown } }[] | undefined) =>
  (clauses ?? []).map((c) => (c.kind.value as { target: ItemField; op: Operator }).target);

describe("tab queries", () => {
  it("names the Search tab by its query", () => {
    expect(searchTab("milk")).toBe("search:milk");
    expect(searchQuery("search:milk shake")).toBe("milk shake");
    expect(searchQuery("01JVIEW")).toBeUndefined();
  });

  it("matches only text with a letter or digit, trimmed (hard case 50)", () => {
    expect(matchText("  milk ")).toBe("milk");
    expect(matchText("   ")).toBeUndefined();
    expect(matchText("-- ( ) *")).toBeUndefined();
    expect(matchText("é")).toBe("é");
    expect(matchText("#42")).toBe("#42");
  });

  it("narrows a saved view with title, labels or remote key MATCHES", () => {
    expect(tabQuery("V1")).toEqual({ viewId: "V1" });
    const q = tabQuery("V1", "milk");
    expect(q.viewId).toBe("V1");
    expect(targets(q.filter?.anyOf as never)).toEqual([ItemField.TITLE, ItemField.LABELS, ItemField.REMOTE_LINK_KEY]);
    // A valid request.
    expect(() => create(ListItemsRequestSchema, q)).not.toThrow();
  });

  it("searches everything but deleted Items, the description too, newest change first; / narrows it further", () => {
    const q = tabQuery(searchTab("milk"));
    expect(q.viewId).toBeUndefined();
    expect(q.filter).toMatchObject({ includeArchived: true, includeDone: true, includeDeferred: true });
    expect(q.filter?.includeDeleted).toBeUndefined();
    expect(targets(q.filter?.anyOf as never)).toEqual([ItemField.TITLE, ItemField.LABELS, ItemField.REMOTE_LINK_KEY, ItemField.DESCRIPTION]);
    expect(q.sort).toEqual([{ key: ItemField.UPDATED_AT, descending: true }]);
    const narrowed = tabQuery(searchTab("milk"), "oat");
    const group = narrowed.filter?.allOf?.[0]?.kind as { case: string; value: { anyOf: never } };
    expect(group.case).toBe("group");
    expect(targets(group.value.anyOf)).toEqual([ItemField.TITLE, ItemField.LABELS, ItemField.REMOTE_LINK_KEY]);
    expect(narrowed.filter?.anyOf).toHaveLength(4);
  });
});
