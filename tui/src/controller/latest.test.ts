import { describe, expect, it } from "vitest";
import { Latest } from "./latest.js";

describe("Latest", () => {
  it("keeps one request in flight and sends only the newest input after it", async () => {
    const sent: string[] = [];
    const applied: [string, string][] = [];
    let open: () => void = () => {};
    const latest = new Latest(
      async (input: string) => {
        sent.push(input);
        if (input === "a") await new Promise<void>((r) => (open = r));
        return input.toUpperCase();
      },
      (input, out) => applied.push([input, out]),
    );
    latest.request("a");
    latest.request("ab");
    latest.request("abc");
    expect(latest.busy).toBe(true);
    expect(sent).toEqual(["a"]);
    open();
    await latest.idle();
    expect(sent).toEqual(["a", "abc"]);
    // Every answer goes to apply with its input; apply drops the stale ones.
    expect(applied).toEqual([
      ["a", "A"],
      ["abc", "ABC"],
    ]);
    expect(latest.busy).toBe(false);
  });

  it("reports failures and goes on", async () => {
    const errors: string[] = [];
    const latest = new Latest(
      async (input: string) => {
        if (input === "bad") throw new Error("no");
        return input;
      },
      () => {},
      (input) => errors.push(input),
    );
    latest.request("bad");
    latest.request("good");
    await latest.idle();
    expect(errors).toEqual(["bad"]);
  });
});
