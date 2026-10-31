import { createClient, createRouterTransport } from "@connectrpc/connect";
import { describe, expect, it } from "vitest";
import { ItemService, type ParseCaptureRequest, type ParseRecurrenceRequest } from "../src/gen/todo/v1/item_service_pb.js";
import { parseCapture, parseRecurrence } from "../src/queries.js";

describe("query helpers", () => {
  it("ParseCapture sends the text, and reference_time only when given", async () => {
    const got: ParseCaptureRequest[] = [];
    const transport = createRouterTransport(({ service }) => {
      service(ItemService, {
        parseCapture: (req) => {
          got.push(req);
          return { draft: { title: req.text.replace(/ \+\d$/, "") } };
        },
      });
    });
    const client = createClient(ItemService, transport);
    const res = await parseCapture(client, "call dentist +2");
    expect(res.draft?.title).toBe("call dentist");
    const at = new Date(Date.UTC(2026, 9, 26));
    await parseCapture(client, "x ^fri", { referenceTime: at });
    expect(got.map((r) => r.text)).toEqual(["call dentist +2", "x ^fri"]);
    expect(got[0]?.referenceTime).toBeUndefined();
    expect(got[1]?.referenceTime?.seconds).toBe(BigInt(at.getTime() / 1000));
  });

  it("ParseRecurrence sends the text, and the anchor and reference_time only when given", async () => {
    const got: ParseRecurrenceRequest[] = [];
    const transport = createRouterTransport(({ service }) => {
      service(ItemService, {
        parseRecurrence: (req) => {
          got.push(req);
          return { warning: `${req.text}: not a repeat rule` };
        },
      });
    });
    const client = createClient(ItemService, transport);
    expect((await parseRecurrence(client, "home")).warning).toBe("home: not a repeat rule");
    const at = new Date(Date.UTC(2026, 9, 26));
    await parseRecurrence(client, "@weekly", { anchor: { year: 2026, month: 10, day: 30 }, referenceTime: at });
    expect(got.map((r) => r.text)).toEqual(["home", "@weekly"]);
    expect([got[0]?.anchor, got[0]?.referenceTime]).toEqual([undefined, undefined]);
    expect(got[1]?.anchor).toMatchObject({ year: 2026, month: 10, day: 30 });
    expect(got[1]?.referenceTime?.seconds).toBe(BigInt(at.getTime() / 1000));
  });
});
