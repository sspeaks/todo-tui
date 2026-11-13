// People (design 3.5) as a suite any daemon runs:
// test/people-conformance.test.ts runs it against FakeDaemon (whose matcher
// is the port in src/people.ts), uds.integration.test.ts against the real
// daemon ($TODO_BIN), which is given its People through an import. Each
// test takes a fresh daemon.
import { type Client, Code, ConnectError } from "@connectrpc/connect";
import { afterEach, describe, expect, it } from "vitest";
import { CreatedVia } from "../../src/gen/todo/v1/item_pb.js";
import { PeopleService } from "../../src/gen/todo/v1/people_service_pb.js";
import { type ItemsClient, captureItem, updateItem } from "../../src/mutations.js";
import { buildMeetingLink } from "../../src/queries.js";

/** A person to seed: display name, username, address, Jira key. */
export interface SeedPerson {
  readonly id: string;
  readonly displayName: string;
  readonly username: string;
  readonly email: string;
  readonly accountId: string;
}

/** Three people, two of them Alex. */
export const SEED: readonly SeedPerson[] = [
  { id: "01JBP00000000000000000DA0A", displayName: "Dana Example", username: "dana", email: "dana@example.com", accountId: "JIRAUSER1" },
  { id: "01JBP00000000000000000A001", displayName: "Alex One", username: "aone", email: "", accountId: "JIRAUSER2" },
  { id: "01JBP00000000000000000A002", displayName: "Alex Two", username: "atwo", email: "alex.two@example.com", accountId: "JIRAUSER3" },
];

export interface PeopleTarget {
  items: ItemsClient;
  people: Client<typeof PeopleService>;
  close?: () => void | Promise<void>;
}

const code = (p: Promise<unknown>) => p.then(() => undefined, (e: unknown) => ConnectError.from(e).code);

/** `open` gives a fresh daemon that knows SEED. */
export function peopleConformance(open: () => Promise<PeopleTarget>): void {
  describe("people", () => {
    let close: (() => void | Promise<void>) | undefined;
    afterEach(async () => {
      await close?.();
      close = undefined;
    });

    async function target() {
      const t = await open();
      close = t.close;
      return t;
    }

    const [dana, alexOne, alexTwo] = SEED as [SeedPerson, SeedPerson, SeedPerson];
    const capture = async (items: ItemsClient, text: string) => captureItem(items, text, { createdVia: CreatedVia.CLI });

    it("lists the People best first, and gets one", async () => {
      const { people } = await target();
      const all = await people.listPeople({});
      expect(all.people.map((p) => p.displayName)).toEqual(["Alex One", "Alex Two", "Dana Example"]);
      const dan = await people.listPeople({ query: "dan" });
      expect(dan.people.map((p) => [p.displayName, p.emails, p.username])).toEqual([["Dana Example", ["dana@example.com"], "dana"]]);
      expect((await people.listPeople({ query: "two" })).people.map((p) => p.id)).toEqual([alexTwo.id]);
      expect((await people.listPeople({ query: "ex", limit: 1 })).people).toHaveLength(1);
      const got = await people.getPerson({ id: dana.id });
      expect(got.person?.identities.map((i) => i.accountId)).toEqual([dana.accountId]);
      expect(await code(people.getPerson({ id: "01JBP00000000000000000N0NE" }))).toBe(Code.NotFound);
      expect(await code(people.getPerson({ id: "nope" }))).toBe(Code.InvalidArgument);
      expect(await code(people.listPeople({ limit: 1001 }))).toBe(Code.InvalidArgument);
    });

    it("links ~ to the one person it names, by username, address, name or first name", async () => {
      const { items } = await target();
      for (const text of ["~dana", "~DANA@example.com", '~"Dana Example"']) {
        const res = await capture(items, `ping ${text}`);
        expect(res.item?.waiting?.on?.ref, text).toEqual({ case: "personId", value: dana.id });
        expect(res.draft?.waitingPerson?.displayName, text).toBe("Dana Example");
      }
      const first = await items.parseCapture({ text: "ping ~Dana" });
      expect(first.draft?.waitingPerson?.id).toBe(dana.id);
      expect((await capture(items, "ping ~aone")).item?.waiting?.on?.ref).toEqual({ case: "personId", value: alexOne.id });
    });

    it("keeps ~ as text when two people or none answer to it, and says how many", async () => {
      const { items } = await target();
      const two = await capture(items, "ping ~alex");
      expect(two.item?.waiting?.on?.ref).toEqual({ case: "freeText", value: "alex" });
      expect(two.draft?.waitingMatches).toBe(2);
      expect(two.draft?.waitingPerson).toBeUndefined();
      const none = await items.parseCapture({ text: 'ping ~"the vendor"' });
      expect(none.draft?.waitingOn?.ref).toEqual({ case: "freeText", value: "the vendor" });
      expect(none.draft?.waitingMatches).toBe(0);
    });

    it("takes a known person's id for waiting on, and refuses an unknown one", async () => {
      const { items } = await target();
      const x = (await capture(items, "x")).item!;
      const set = await updateItem(items, { id: x.id, waiting: { on: { ref: { case: "personId", value: alexTwo.id } } } }, ["waiting"]);
      expect(set.item?.waiting?.on?.ref).toEqual({ case: "personId", value: alexTwo.id });
      const unknown = updateItem(items, { id: x.id, waiting: { on: { ref: { case: "personId", value: "01JBP00000000000000000N0NE" } } } }, ["waiting"]);
      expect(await code(unknown)).toBe(Code.InvalidArgument);
    });

    it("invites People by their address in a meeting link", async () => {
      const { items } = await target();
      const a = (await capture(items, "a ~dana")).item!;
      const b = (await capture(items, "b ~aone")).item!;
      const c = (await capture(items, "c ~atwo")).item!;
      const m = await buildMeetingLink(items, [a.id, b.id, c.id]);
      // Alex One's address is hidden: none.
      expect(m.attendees).toEqual(["dana@example.com", "alex.two@example.com"]);
    });
  });
}

/**
 * SEED as export lines, for a real daemon's import: the header, then a
 * person record each.
 */
export function seedExport(schemaVersion: number): string {
  const header = {
    format: "todo-tui-export",
    formatVersion: 1,
    schemaVersion,
    exportedAt: "2026-11-13T00:00:00Z",
    daemonVersion: "test",
    revision: "1",
    history: false,
  };
  const people = SEED.map((p) =>
    JSON.stringify({
      type: "person",
      data: {
        id: p.id,
        displayName: p.displayName,
        username: p.username,
        ...(p.email && { emails: [p.email] }),
        identities: [{ instanceId: "work-jira", accountId: p.accountId }],
      },
    }),
  );
  return [JSON.stringify(header), ...people].join("\n") + "\n";
}
