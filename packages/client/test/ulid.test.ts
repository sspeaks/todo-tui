import { afterEach, describe, expect, it, vi } from "vitest";
import { monotonicUlid, ulid } from "../src/ulid.js";

/** When `byte` is set, getRandomValues fills buffers with it instead of random bytes. */
const fixedRandom = vi.hoisted(() => ({ byte: undefined as number | undefined }));
vi.mock("node:crypto", async (importOriginal) => {
  const crypto = await importOriginal<typeof import("node:crypto")>();
  return {
    ...crypto,
    getRandomValues: (buf: Uint8Array) => (fixedRandom.byte === undefined ? crypto.getRandomValues(buf) : buf.fill(fixedRandom.byte)),
  };
});
afterEach(() => {
  fixedRandom.byte = undefined;
});

const ULID = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/;

describe("ulid", () => {
  it("is 26 Crockford base32 characters", () => {
    const ids = Array.from({ length: 100 }, () => ulid());
    for (const id of ids) expect(id).toMatch(ULID);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("encodes the millisecond timestamp first", () => {
    // The example from the ULID spec.
    expect(monotonicUlid(() => 1469918176385)().slice(0, 10)).toBe("01ARYZ6S41");
    expect(monotonicUlid(() => 0)().slice(0, 10)).toBe("0000000000");
    expect(monotonicUlid(() => 2 ** 48 - 1)().slice(0, 10)).toBe("7ZZZZZZZZZ");
  });

  it("increases within one millisecond and when the clock goes back", () => {
    let now = 1_700_000_000_000;
    const next = monotonicUlid(() => now);
    const ids = Array.from({ length: 1000 }, next);
    now -= 5_000;
    ids.push(next(), next());
    now += 10_000;
    ids.push(next());
    for (let i = 1; i < ids.length; i++) expect(ids[i]! > ids[i - 1]!).toBe(true);
    expect(new Set(ids.slice(0, 1002).map((id) => id.slice(0, 10))).size).toBe(1);
    expect(ids.at(-1)!.slice(0, 10)).not.toBe(ids[0]!.slice(0, 10));
  });

  it("encodes the 80 random bits big-endian after the timestamp", () => {
    fixedRandom.byte = 0xa5;
    const radix32 = BigInt(`0x${"a5".repeat(10)}`).toString(32).padStart(16, "0");
    const crockford = [...radix32].map((d) => "0123456789ABCDEFGHJKMNPQRSTVWXYZ"[parseInt(d, 32)]).join("");
    expect(monotonicUlid(() => 0)()).toBe(`0000000000${crockford}`);
  });

  it("adds one to the 80-bit random part within a millisecond, carrying across bytes", () => {
    fixedRandom.byte = 0;
    const next = monotonicUlid(() => 1_700_000_000_000);
    const tails = Array.from({ length: 257 }, () => next().slice(10));
    expect(tails[0]).toBe("0000000000000000");
    expect(tails[1]).toBe("0000000000000001");
    // 256 = 8 * 32: the carry out of the last byte.
    expect(tails[256]).toBe("0000000000000080");
  });

  it("moves on to the next millisecond when the random part overflows", () => {
    const now = 1_700_000_000_000;
    fixedRandom.byte = 0xff;
    const next = monotonicUlid(() => now);
    const last = next();
    expect(last.slice(10)).toBe("ZZZZZZZZZZZZZZZZ");
    const after = next();
    expect(after > last).toBe(true);
    expect(after.slice(0, 10)).toBe(monotonicUlid(() => now + 1)().slice(0, 10));
    expect(after).toMatch(ULID);
  });

  it("sorts by time across milliseconds", () => {
    let now = 1_700_000_000_000;
    const next = monotonicUlid(() => now++);
    const ids = Array.from({ length: 50 }, next);
    expect([...ids].sort()).toEqual(ids);
  });
});
