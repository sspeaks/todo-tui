import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { type PathsEnv, PathsError, normalizeAbsolute, resolvePaths } from "../src/paths.js";

const base: PathsEnv = { os: "linux", home: "/home/u", uid: 501 };

describe("resolvePaths", () => {
  it("uses the Linux XDG defaults", () => {
    expect(resolvePaths(base)).toEqual({
      config: "/home/u/.config/todo-tui/config.toml",
      data: "/home/u/.local/share/todo-tui",
      logs: "/home/u/.local/state/todo-tui",
      runtime: "/tmp/todo-tui-501",
      socket: "/tmp/todo-tui-501/daemon.sock",
      lock: "/home/u/.local/share/todo-tui/daemon.lock",
    });
  });

  it("honours absolute XDG variables and ignores relative or empty ones", () => {
    const p = resolvePaths({
      ...base,
      xdgConfigHome: "/c",
      xdgDataHome: "rel",
      xdgStateHome: "",
      xdgRuntimeDir: "/run/user/501",
    });
    expect(p.config).toBe("/c/todo-tui/config.toml");
    expect(p.data).toBe("/home/u/.local/share/todo-tui");
    expect(p.logs).toBe("/home/u/.local/state/todo-tui");
    expect(p.socket).toBe("/run/user/501/todo-tui/daemon.sock");
  });

  it("uses the macOS layout with TMPDIR", () => {
    const p = resolvePaths({ ...base, os: "macos", home: "/Users/u", tmpdir: "/var/folders/xy/T/" });
    expect(p.data).toBe("/Users/u/Library/Application Support/todo-tui");
    expect(p.logs).toBe("/Users/u/Library/Logs/todo-tui");
    expect(p.runtime).toBe("/var/folders/xy/T/todo-tui");
    expect(p.lock).toBe("/Users/u/Library/Application Support/todo-tui/daemon.lock");
  });

  it("falls back to /tmp/todo-tui-$UID without TMPDIR", () => {
    expect(resolvePaths({ ...base, os: "macos" }).runtime).toBe("/tmp/todo-tui-501");
  });

  it("uses the fixed TODO_TUI_HOME layout after lexical normalisation", () => {
    const p = resolvePaths({ ...base, home: undefined, todoTuiHome: "/x//y/./z/../" });
    expect(p).toEqual({
      config: "/x/y/config.toml",
      data: "/x/y/data",
      logs: "/x/y/logs",
      runtime: "/x/y/run",
      socket: "/x/y/run/daemon.sock",
      lock: "/x/y/data/daemon.lock",
    });
  });

  it("falls back to a hashed /tmp dir for long TODO_TUI_HOME sockets (D8)", () => {
    const long = `/${"a".repeat(120)}`;
    const p1 = resolvePaths({ ...base, todoTuiHome: long });
    const p2 = resolvePaths({ ...base, todoTuiHome: `${long}/` });
    const p3 = resolvePaths({ ...base, todoTuiHome: `/b/..${long}` });
    expect(p1.runtime).toMatch(/^\/tmp\/todo-tui-501\/h-[0-9a-f]{8}$/);
    expect(p1.socket).toBe(`${p1.runtime}/daemon.sock`);
    expect(p2.socket).toBe(p1.socket);
    expect(p3.socket).toBe(p1.socket);
    expect(p1.data).toBe(`${long}/data`);
  });

  it("falls back to /tmp/todo-tui-$UID for long default runtime dirs", () => {
    const p = resolvePaths({ ...base, xdgRuntimeDir: `/${"r".repeat(100)}` });
    expect(p.runtime).toBe("/tmp/todo-tui-501");
  });

  it("keeps a socket path of exactly 100 bytes", () => {
    // "/<n>/todo-tui/daemon.sock" = n + 22 bytes
    const p = resolvePaths({ ...base, xdgRuntimeDir: `/${"r".repeat(78)}` });
    expect(Buffer.byteLength(p.socket)).toBe(100);
    expect(p.runtime).not.toBe("/tmp/todo-tui-501");
  });

  it("rejects a relative TODO_TUI_HOME and a missing HOME", () => {
    expect(() => resolvePaths({ ...base, todoTuiHome: "rel/dir" })).toThrow(PathsError);
    expect(() => resolvePaths({ ...base, home: undefined })).toThrow(/HOME is not set/);
    expect(resolvePaths({ ...base, home: undefined, xdgConfigHome: "/c", xdgDataHome: "/d", xdgStateHome: "/s" }).data).toBe(
      "/d/todo-tui",
    );
    expect(() => resolvePaths({ ...base, home: "" })).toThrow(PathsError);
  });

  it("compares env paths component-wise like Rust", () => {
    expect(resolvePaths({ ...base, xdgStateHome: "/s//x/./" }).logs).toBe("/s/x/todo-tui");
  });

  it("normalises like the Rust side", () => {
    expect(normalizeAbsolute("/")).toBe("/");
    expect(normalizeAbsolute("/..")).toBe("/");
    expect(normalizeAbsolute("/a/b/../../..")).toBe("/");
    expect(normalizeAbsolute("/a//b/")).toBe("/a/b");
  });
});

// Shared vectors written by the daemon side (crates/todo-daemon/src/paths.rs).
const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "../../..");
const vectorsPath = [join(repoRoot, "testdata/paths.json"), join(repoRoot, "crates/todo-daemon/testdata/paths.json")].find(
  existsSync,
);

type Json = Record<string, unknown>;
const pick = (o: Json, ...keys: string[]): unknown => keys.map((k) => o[k]).find((v) => v !== undefined && v !== null);
const str = (v: unknown) => (typeof v === "string" ? v : undefined);

function envFromVector(e: Json): PathsEnv {
  const os = String(pick(e, "os") ?? "linux").toLowerCase();
  return {
    os: os.startsWith("mac") || os === "darwin" ? "macos" : "linux",
    home: str(pick(e, "home", "HOME")),
    todoTuiHome: str(pick(e, "todo_tui_home", "todoTuiHome", "TODO_TUI_HOME")),
    xdgConfigHome: str(pick(e, "xdg_config_home", "xdgConfigHome", "XDG_CONFIG_HOME")),
    xdgDataHome: str(pick(e, "xdg_data_home", "xdgDataHome", "XDG_DATA_HOME")),
    xdgStateHome: str(pick(e, "xdg_state_home", "xdgStateHome", "XDG_STATE_HOME")),
    xdgRuntimeDir: str(pick(e, "xdg_runtime_dir", "xdgRuntimeDir", "XDG_RUNTIME_DIR")),
    tmpdir: str(pick(e, "tmpdir", "TMPDIR")),
    uid: Number(pick(e, "uid", "UID") ?? 0),
  };
}

describe.skipIf(!vectorsPath)("shared testdata/paths.json vectors", () => {
  const raw = vectorsPath ? (JSON.parse(readFileSync(vectorsPath, "utf8")) as unknown) : [];
  const cases = (Array.isArray(raw) ? raw : ((raw as Json).cases ?? (raw as Json).vectors ?? [])) as Json[];

  it("has cases", () => {
    expect(cases.length).toBeGreaterThan(0);
  });

  for (const [i, c] of cases.entries()) {
    it(String(c.name ?? c.description ?? `case ${i}`), () => {
      const env = envFromVector((c.env ?? c.input) as Json);
      const expected = pick(c, "expected", "expect", "want", "paths", "output") as Json | string | undefined;
      const err = pick(c, "error", "err") ?? (expected && typeof expected === "object" ? expected.error : undefined);
      if (err !== undefined || typeof expected === "string") {
        const kind = String(err ?? expected);
        expect(() => resolvePaths(env)).toThrow(PathsError);
        try {
          resolvePaths(env);
        } catch (e) {
          const k = (e as PathsError).kind;
          expect(kind.replace(/[_\s]/g, "").toLowerCase()).toContain(k.toLowerCase());
        }
        return;
      }
      expect(resolvePaths(env)).toEqual(expected);
    });
  }
});
