import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Priority, deleteItem, updateItem } from "@todo-tui/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeDaemon, item } from "../fake.test-util.js";
import { type EditorOutcome, type EditorRunner, homeShort, systemEditor } from "./editor.js";
import { setup, stopControllers } from "./setup.test-util.js";

const dirs: string[] = [];
const scratch = () => {
  const dir = mkdtempSync(join(tmpdir(), "todo-tui-test-"));
  dirs.push(dir);
  return dir;
};

afterEach(async () => {
  await stopControllers();
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

/** An editor that hands back `edit(text)`, after `meanwhile()`, and records what it was given. */
function fakeEditor(edit: (text: string) => EditorOutcome, meanwhile: () => Promise<void> = async () => {}) {
  const given: string[] = [];
  const runner: EditorRunner = async (text) => {
    given.push(text);
    await meanwhile();
    return edit(text);
  };
  return { runner, given };
}

const described = () => fakeDaemon([item("pay rent", { description: "on the 1st" }), item("call mum")]);

describe("E", () => {
  it("saves the edited description with expected_revision; one final newline is not part of it (hard case 35)", async () => {
    const ed = fakeEditor((text) => ({ ok: true, text: `${text}via the app\n` }));
    const t = await setup(described(), { controller: { editor: ed.runner, stateDir: scratch() } });
    const rev = t.byTitle("pay rent")!.revision;
    await t.c.press("E");
    expect(ed.given).toEqual(["on the 1st\n"]);
    expect(t.byTitle("pay rent")?.description).toBe("on the 1st\nvia the app");
    expect(t.byTitle("pay rent")!.revision).toBeGreaterThan(rev);
    expect(t.toast()).toBe('edit "pay rent" · u undo');
    // An empty description starts as an empty file.
    await t.c.press("j");
    ed.given.length = 0;
    await t.c.press("E");
    expect(ed.given).toEqual([""]);
  });

  it("sends nothing for unchanged text, or when the editor failed (hard case 35)", async () => {
    let outcome: EditorOutcome = { ok: true, text: "on the 1st\n" };
    const ed = fakeEditor(() => outcome);
    const t = await setup(described(), { controller: { editor: ed.runner, stateDir: scratch() } });
    await t.c.press("E");
    expect(t.toast()).toBe("Description unchanged");
    outcome = { ok: true, text: "on the 1st" };
    await t.c.press("E");
    expect(t.toast()).toBe("Description unchanged");
    outcome = { ok: false, reason: "Editor exited 1, not saved" };
    await t.c.press("E");
    expect(t.state().toast).toMatchObject({ level: "error", text: "Editor exited 1, not saved" });
    expect(t.userOps()).toEqual([]);
  });

  it("rebases silently when only another field changed meanwhile (hard case 35)", async () => {
    let t!: Awaited<ReturnType<typeof setup>>;
    const ed = fakeEditor(
      () => ({ ok: true, text: "on the 2nd\n" }),
      async () => {
        await updateItem(t.clients().items, { id: t.byTitle("pay rent")!.id, priority: Priority.HIGH }, ["priority"]);
      },
    );
    t = await setup(described(), { controller: { editor: ed.runner, stateDir: scratch() } });
    await t.c.press("E");
    expect(t.byTitle("pay rent")).toMatchObject({ description: "on the 2nd", priority: Priority.HIGH });
    expect(t.toast()).toBe('edit "pay rent" · u undo');
  });

  it("keeps the text in <state dir>/recovered when the description changed elsewhere, and says where (hard case 35)", async () => {
    const stateDir = scratch();
    let t!: Awaited<ReturnType<typeof setup>>;
    const ed = fakeEditor(
      () => ({ ok: true, text: "mine\n" }),
      async () => {
        await updateItem(t.clients().items, { id: t.byTitle("pay rent")!.id, description: "theirs" }, ["description"]);
      },
    );
    t = await setup(described(), { controller: { editor: ed.runner, stateDir } });
    const id = t.byTitle("pay rent")!.id;
    await t.c.press("E");
    expect(t.byTitle("pay rent")?.description).toBe("theirs");
    const files = readdirSync(join(stateDir, "recovered"));
    expect(files).toHaveLength(1);
    expect(files[0]).toMatch(new RegExp(`^${id}-\\d{8}T\\d{6}Z\\.md$`));
    const path = join(stateDir, "recovered", files[0] ?? "");
    expect(readFileSync(path, "utf8")).toBe("mine\n");
    expect(t.state().toast).toMatchObject({ level: "error", text: `Description changed elsewhere; your text is in ${path}` });
  });

  it("keeps the text when the Item was deleted meanwhile", async () => {
    const stateDir = scratch();
    let t!: Awaited<ReturnType<typeof setup>>;
    const ed = fakeEditor(
      () => ({ ok: true, text: "mine" }),
      async () => {
        await deleteItem(t.clients().items, t.byTitle("pay rent")!.id);
      },
    );
    t = await setup(described(), { controller: { editor: ed.runner, stateDir } });
    await t.c.press("E");
    expect(t.toast()).toMatch(/^“pay rent” was deleted elsewhere; your text is in .*\/recovered\/.*\.md$/);
    expect(readdirSync(join(stateDir, "recovered"))).toHaveLength(1);
  });

  it("edits the selected Item, never the marked ones", async () => {
    const ed = fakeEditor(() => ({ ok: true, text: "x" }));
    const t = await setup(described(), { controller: { editor: ed.runner, stateDir: scratch() } });
    await t.c.press("mE");
    expect(t.byTitle("call mum")?.description).toBe("x");
    expect(t.byTitle("pay rent")?.description).toBe("on the 1st");
    expect(t.state().marks.size).toBe(1);
  });
});

describe("the system editor", () => {
  /** A shell editor script that appends `text` and notes the file it was given. */
  function script(dir: string, body: string) {
    const path = join(dir, "editor.sh");
    writeFileSync(path, `#!/bin/sh\necho "$1" >> ${join(dir, "files")}\n${body}\n`, { mode: 0o755 });
    return path;
  }

  it("runs $VISUAL, else $EDITOR, through sh on a temp .md file it removes afterwards", async () => {
    const dir = scratch();
    const editor = script(dir, `printf ' edited' >> "$1"`);
    const run = systemEditor(() => undefined, { EDITOR: editor });
    expect(await run("text\n")).toEqual({ ok: true, text: "text\n edited" });
    const [file] = readFileSync(join(dir, "files"), "utf8").trim().split("\n");
    expect(file).toMatch(/todo-edit-.*\/description\.md$/);
    expect(existsSync(file ?? "")).toBe(false);
    // $VISUAL first; a command with arguments and quotes, as git runs it.
    const visual = systemEditor(() => undefined, { VISUAL: `sh -c 'printf "%s!" "$(cat "$1")" > "$1"' sh`, EDITOR: "false" });
    expect(await visual("hi")).toEqual({ ok: true, text: "hi!" });
  });

  it("says a non-zero exit, or a missing editor, saved nothing, naming the editor it could not run", async () => {
    expect(await systemEditor(() => undefined, { EDITOR: "false" })("x")).toEqual({ ok: false, reason: "Editor exited 1, not saved" });
    // (sh's "not found" kept out of the test output.)
    const missing = systemEditor(() => undefined, { EDITOR: "/nonexistent/editor 2>/dev/null" });
    expect(await missing("x")).toEqual({
      ok: false,
      reason: "Could not run the editor /nonexistent/editor 2>/dev/null (not found; set $VISUAL or $EDITOR), not saved",
    });
    const dir = scratch();
    const notExecutable = join(dir, "editor.sh");
    writeFileSync(notExecutable, "#!/bin/sh\n", { mode: 0o644 });
    expect(await systemEditor(() => undefined, { EDITOR: `${notExecutable} 2>/dev/null` })("x")).toEqual({
      ok: false,
      reason: `Could not run the editor ${notExecutable} 2>/dev/null (not executable), not saved`,
    });
  });

  it("shortens a path under the home directory to ~ for the toast", () => {
    expect(homeShort("/home/u/.local/state/todo-tui/recovered/x.md", "/home/u")).toBe("~/.local/state/todo-tui/recovered/x.md");
    expect(homeShort("/home/u2/x.md", "/home/u")).toBe("/home/u2/x.md");
    expect(homeShort("/tmp/x.md", "/")).toBe("/tmp/x.md");
  });

  it("hands the terminal over around the editor when the shell lends it", async () => {
    const order: string[] = [];
    const dir = scratch();
    const editor = script(dir, "true");
    const suspend = vi.fn(async (run: () => void | Promise<void>) => {
      order.push("suspend");
      await run();
      order.push("resume");
    });
    expect(await systemEditor(() => suspend, { EDITOR: editor })("x")).toEqual({ ok: true, text: "x" });
    expect(order).toEqual(["suspend", "resume"]);
    expect(readFileSync(join(dir, "files"), "utf8")).toMatch(/description\.md/);
  });
});

describe("ctrl-c while the editor runs", () => {
  // The editor has the terminal in cooked mode, so a ctrl-c meant for it
  // (`code --wait`) goes to the whole process group: the TUI must live on.
  // Ink's signal-exit re-raises a signal no other listener handles, which
  // the child copies, so ignoring it only until spawnSync returns is not enough.
  it("ignores SIGINT and SIGQUIT around the editor, in the process group that gets them", async () => {
    const job = join(dirname(fileURLToPath(import.meta.url)), "job.ts");
    const script = `
      const { withoutInterrupts } = await import(${JSON.stringify(job)});
      const { spawnSync } = await import("node:child_process");
      for (const sig of ["SIGINT", "SIGQUIT"]) {
        process.on(sig, function exitLike() {
          if (process.listeners(sig).length === 1) { process.off(sig, exitLike); process.kill(process.pid, sig); }
        });
      }
      const r = await withoutInterrupts(() => spawnSync("sh", ["-c", "kill -INT 0; kill -QUIT 0; exit 3"], { stdio: "ignore" }));
      await new Promise((resolve) => setTimeout(resolve, 50));
      process.stdout.write("alive " + (r.signal ?? r.status));
    `;
    const child = spawn(process.execPath, ["--input-type=module", "-e", script], { detached: true, stdio: ["ignore", "pipe", "inherit"] });
    const kill = setTimeout(() => child.kill("SIGKILL"), 5_000);
    let out = "";
    child.stdout.on("data", (d: Buffer) => (out += d));
    const [code, signal] = await new Promise<[number | null, NodeJS.Signals | null]>((resolve) => child.on("exit", (c, s) => resolve([c, s])));
    clearTimeout(kill);
    expect({ code, signal, out }).toEqual({ code: 0, signal: null, out: "alive SIGINT" });
  });
});

describe("ctrl-z", () => {
  it("stops the process with the terminal handed back, and carries on after", async () => {
    const stopProcess = vi.fn(async () => true);
    const t = await setup(described(), { controller: { stopProcess } });
    await t.c.press("<ctrl+z>");
    // No terminal to hand back (as in tests): it says so, and does not stop.
    expect(t.toast()).toBe("Cannot suspend here");
    expect(stopProcess).not.toHaveBeenCalled();
    const suspended: string[] = [];
    t.c.suspendTerminal = async (run) => {
      suspended.push("out");
      await run();
      suspended.push("back");
    };
    await t.c.press("<ctrl+z>j");
    expect(stopProcess).toHaveBeenCalledTimes(1);
    expect(suspended).toEqual(["out", "back"]);
    expect(t.selected()).toBe("call mum");
  });

  it("takes the terminal back and says so when nothing stopped it (no job control)", async () => {
    const stopProcess = vi.fn(async () => false);
    const t = await setup(described(), { controller: { stopProcess } });
    const suspended: string[] = [];
    t.c.suspendTerminal = async (run) => {
      suspended.push("out");
      await run();
      suspended.push("back");
    };
    await t.c.press("<ctrl+z>j");
    expect(suspended).toEqual(["out", "back"]);
    expect(t.toast()).toBe("Cannot suspend: no job control here");
    expect(t.selected()).toBe("call mum");
  });
});

describe("stopSelf", () => {
  // A process group with no job control (a session of its own, as under
  // `tmux new-session cmd` or `xterm -e todo`) is orphaned: the kernel
  // discards its SIGTSTP, so no SIGCONT ever comes. stopSelf must give up
  // rather than leave the TUI waiting with the terminal handed back.
  it("resolves false, without stopping, in a process group with no job control", async () => {
    const job = join(dirname(fileURLToPath(import.meta.url)), "job.ts");
    const child = spawn(
      process.execPath,
      ["--input-type=module", "-e", `const { stopSelf } = await import(${JSON.stringify(job)}); process.stdout.write(String(await stopSelf()))`],
      { detached: true, stdio: ["ignore", "pipe", "inherit"] },
    );
    // A stopped child would never exit: kill it, so the test fails rather than hangs.
    const kill = setTimeout(() => child.kill("SIGKILL"), 5_000);
    let out = "";
    child.stdout.on("data", (d: Buffer) => (out += d));
    const code = await new Promise<number | null>((resolve) => child.on("exit", (c) => resolve(c)));
    clearTimeout(kill);
    expect({ code, out: out.trim() }).toEqual({ code: 0, out: "false" });
  });
});
