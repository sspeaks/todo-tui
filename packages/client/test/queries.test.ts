import { createClient, createRouterTransport } from "@connectrpc/connect";
import { describe, expect, it } from "vitest";
import { ItemService, type ParseCaptureRequest } from "../src/gen/todo/v1/item_service_pb.js";
import { parseCapture } from "../src/queries.js";

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
});
