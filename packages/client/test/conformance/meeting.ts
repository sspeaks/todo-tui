// BuildMeetingLink (design 2.9) as a suite any daemon runs:
// test/meeting-conformance.test.ts runs it against FakeDaemon (whose
// builder is the port in src/meeting.ts), uds.integration.test.ts against
// the real daemon ($TODO_BIN). Each test takes a fresh daemon.
import { Code, ConnectError } from "@connectrpc/connect";
import { afterEach, describe, expect, it } from "vitest";
import { type ItemsClient, captureItem, deleteItem, setStatus, updateItem } from "../../src/mutations.js";
import { buildMeetingLink } from "../../src/queries.js";
import { CreatedVia, Status } from "../../src/gen/todo/v1/item_pb.js";

export interface MeetingTarget {
  items: ItemsClient;
  close?: () => void | Promise<void>;
}

const code = (p: Promise<unknown>) => p.then(() => undefined, (e: unknown) => ConnectError.from(e).code);

export function meetingConformance(open: () => Promise<MeetingTarget>): void {
  describe("meeting links", () => {
    let close: (() => void | Promise<void>) | undefined;
    afterEach(async () => {
      await close?.();
      close = undefined;
    });

    async function target() {
      const t = await open();
      close = t.close;
      return t.items;
    }

    const capture = async (items: ItemsClient, text: string) => (await captureItem(items, text, { createdVia: CreatedVia.CLI })).item!;

    it("fills the subject, content and attendees, encoded as Teams takes them", async () => {
      const items = await target();
      const a = await capture(items, "Plan the Q4 offsite ~dana@example.com");
      await updateItem(items, { id: a.id, description: "Agenda & budget\nfor the team.\n\nMore later." }, ["description"]);
      const b = await capture(items, "Café \\#1 ~Dana@Example.com");
      const c = await capture(items, "c ~\"the vendor\"");
      const one = await buildMeetingLink(items, [a.id]);
      expect(one).toMatchObject({ subject: "Plan the Q4 offsite", content: "Agenda & budget for the team.", attendees: ["dana@example.com"] });
      expect(one.url).toBe(
        "https://teams.microsoft.com/l/meeting/new?subject=Plan%20the%20Q4%20offsite&content=Agenda%20%26%20budget%20for%20the%20team.&attendees=dana%40example.com",
      );
      const all = await buildMeetingLink(items, [b.id, a.id, c.id]);
      expect(all).toMatchObject({
        subject: "Café #1 + 2 more",
        content: "Café #1\n\nPlan the Q4 offsite\nAgenda & budget for the team.\n\nc",
        attendees: ["Dana@Example.com"],
      });
      expect(all.url.startsWith("https://teams.microsoft.com/l/meeting/new?subject=Caf%C3%A9%20%231%20%2B%202%20more&content=")).toBe(true);
      const given = await buildMeetingLink(items, [c.id], { subject: "  Kickoff\n" });
      expect(given.url).toBe("https://teams.microsoft.com/l/meeting/new?subject=Kickoff");
    });

    it("reads whitespace and case as the daemon's Rust does", async () => {
      const items = await target();
      // U+0085 is whitespace (U+FEFF is not, but a transport may drop a
      // leading one); case folds in ASCII only.
      const a = await capture(items, "Plan\u0085offsite ~JOSÉ@example.com");
      const b = await capture(items, "Kick\ufeffoff ~josé@example.com");
      const c = await capture(items, "c");
      await updateItem(items, { id: c.id, waiting: { on: { ref: { case: "freeText", value: "dana@example.com\u0085" } }, reason: "" } }, ["waiting"]);
      const m = await buildMeetingLink(items, [a.id, b.id, c.id]);
      expect(m.subject).toBe("Plan offsite + 2 more");
      expect(m.content.split("\n\n")[1]).toBe("Kick\ufeffoff");
      expect(m.attendees).toEqual(["JOSÉ@example.com", "josé@example.com", "dana@example.com"]);
    });

    it("works on a done Item; refuses a deleted one and no ids", async () => {
      const items = await target();
      const a = await capture(items, "post-mortem");
      await setStatus(items, a.id, Status.DONE);
      expect((await buildMeetingLink(items, [a.id])).subject).toBe("post-mortem");
      await deleteItem(items, a.id);
      expect(await code(buildMeetingLink(items, [a.id]))).toBe(Code.NotFound);
      expect(await code(buildMeetingLink(items, []))).toBe(Code.InvalidArgument);
      expect(await code(buildMeetingLink(items, ["nope"]))).toBe(Code.InvalidArgument);
    });
  });
}
