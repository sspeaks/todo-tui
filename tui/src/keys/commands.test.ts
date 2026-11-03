import { describe, expect, it } from "vitest";
import { completeCommand, resolveCommand } from "./commands.js";

describe("command line", () => {
  it("runs commands by name, alias or unique prefix", () => {
    expect(resolveCommand("q")).toEqual({ kind: "run", command: "quit", args: "" });
    expect(resolveCommand(" quit ")).toEqual({ kind: "run", command: "quit", args: "" });
    expect(resolveCommand("h")).toEqual({ kind: "run", command: "help", args: "" });
    expect(resolveCommand("ref")).toEqual({ kind: "run", command: "refresh", args: "" });
    expect(resolveCommand("search  milk  shake")).toEqual({ kind: "run", command: "search", args: "milk shake" });
    expect(resolveCommand("s milk")).toEqual({ kind: "run", command: "search", args: "milk" });
    expect(resolveCommand("")).toEqual({ kind: "none" });
  });

  it("says what is not a command, and when a planned one arrives", () => {
    expect(resolveCommand("zz")).toEqual({ kind: "message", level: "error", text: "Not a command: zz" });
    expect(resolveCommand("sync")).toEqual({ kind: "message", level: "info", text: ":sync arrives with Jira sync in P2" });
    expect(resolveCommand("export ~/x.jsonl")).toEqual({ kind: "run", command: "export", args: "~/x.jsonl" });
    expect(resolveCommand("view new x is:next")).toEqual({ kind: "run", command: "view", args: "new x is:next" });
    expect(resolveCommand("review")).toMatchObject({ text: ":review arrives with the weekly review in P4" });
  });

  it("completes the command name to the longest shared prefix", () => {
    expect(completeCommand("qu")).toBe("quit");
    expect(completeCommand("re")).toBe("refresh");
    expect(completeCommand("se")).toBe("search ");
    expect(completeCommand("zz")).toBe("zz");
    expect(completeCommand("search mi")).toBe("search mi");
  });

  it("completes :view's subcommand, then a view name, quoting one with a space", () => {
    const views = ["Inbox", "Deep work", "Deferred"];
    expect(completeCommand("vi")).toBe("view ");
    expect(completeCommand("view e")).toBe("view edit ");
    expect(completeCommand("view edit in", views)).toBe("view edit Inbox");
    expect(completeCommand("view delete de", views)).toBe("view delete de");
    expect(completeCommand("view delete d", views)).toBe("view delete De");
    expect(completeCommand("view delete dee", views)).toBe('view delete "Deep work"');
    expect(completeCommand("view new de", views)).toBe("view new de");
    // A quote or backslash in a name is escaped, as viewWords reads it.
    expect(completeCommand("view edit sa", ['Say "hi" now'])).toBe('view edit "Say \\"hi\\" now"');
  });
});
