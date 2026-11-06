import { Code, ConnectError } from "@connectrpc/connect";
import { describe, expect, it } from "vitest";
import { isConnectionFailure } from "../src/errors.js";
import { isUnreachable, parsePairingLink } from "../src/remote.js";

describe("parsePairingLink", () => {
  it("reads the link `todo remote pair` prints", () => {
    expect(parsePairingLink("todo-tui://pair?url=https%3A%2F%2Fmac.tail1.ts.net&token=todo_ab")).toEqual({
      url: "https://mac.tail1.ts.net",
      token: "todo_ab",
    });
    expect(parsePairingLink("  todo-tui://pair?token=t&url=http%3A%2F%2F127.0.0.1%3A7743%2F ")).toEqual({ url: "http://127.0.0.1:7743", token: "t" });
  });

  it("refuses anything else", () => {
    for (const bad of [
      "",
      "https://mac.tail1.ts.net",
      "todo-tui://pair?url=https%3A%2F%2Fmac",
      "todo-tui://pair?token=t",
      "todo-tui://pair?url=ftp%3A%2F%2Fmac&token=t",
      "todo-tui://pair?url=https%3A%2F%2Fmac%2Fpath&token=t",
      "todo-tui://pair?url=%E0%A4%A&token=t",
    ]) {
      expect(parsePairingLink(bad), bad).toBeUndefined();
    }
  });
});

describe("isUnreachable", () => {
  it("is a network failure or a proxy without its backend, not a refusal", () => {
    expect(isUnreachable(new ConnectError("fetch failed", Code.Unknown, undefined, undefined, new TypeError("fetch failed")))).toBe(true);
    expect(isUnreachable(new TypeError("Network request failed"))).toBe(true);
    expect(isUnreachable(new ConnectError("bad gateway", Code.Unavailable))).toBe(true);
    expect(isUnreachable(new ConnectError("no", Code.Unauthenticated))).toBe(false);
    expect(isUnreachable(new ConnectError("boom", Code.Unknown))).toBe(false);
    expect(isUnreachable(new Error("x"))).toBe(false);
  });
});

describe("isConnectionFailure over fetch", () => {
  it("is a dropped network, so the call is retried with its request id", () => {
    const fetchFailed = (code: string) =>
      new ConnectError("fetch failed", Code.Unknown, undefined, undefined, Object.assign(new TypeError("fetch failed"), { cause: Object.assign(new Error(code), { code }) }));
    for (const code of ["ENOTFOUND", "UND_ERR_CONNECT_TIMEOUT", "UND_ERR_SOCKET", "ECONNREFUSED"]) {
      expect(isConnectionFailure(fetchFailed(code)), code).toBe(true);
    }
    // fetch's TypeError with no cause: still the network.
    expect(isConnectionFailure(new ConnectError("fetch failed", Code.Unknown, undefined, undefined, new TypeError("fetch failed")))).toBe(true);
    // An answer is not.
    expect(isConnectionFailure(new ConnectError("no", Code.Unauthenticated))).toBe(false);
    expect(isConnectionFailure(new ConnectError("boom", Code.Unknown))).toBe(false);
  });
});
