// Port of crates/todo-daemon/src/paths.rs (design 7.1, ADR-0001 D7/D8).
// Resolution is a pure function of PathsEnv so both sides share testdata/paths.json.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { userInfo } from "node:os";

/** Unix socket paths longer than this fall back to /tmp (7.1). */
export const MAX_SOCKET_PATH_BYTES = 100;

export type Os = "macos" | "linux";

export interface PathsEnv {
  os: Os;
  home?: string | undefined;
  todoTuiHome?: string | undefined;
  xdgConfigHome?: string | undefined;
  xdgDataHome?: string | undefined;
  xdgStateHome?: string | undefined;
  xdgRuntimeDir?: string | undefined;
  tmpdir?: string | undefined;
  uid: number;
}

export interface Paths {
  config: string;
  data: string;
  logs: string;
  runtime: string;
  socket: string;
  lock: string;
}

export class PathsError extends Error {
  constructor(
    readonly kind: "NoHome" | "RelativeTodoTuiHome",
    message: string,
  ) {
    super(message);
    this.name = "PathsError";
  }
}

/** Joins like Rust's PathBuf::push: no normalisation, one separator added if missing. */
function join(base: string, ...parts: string[]): string {
  let out = base;
  for (const part of parts) {
    if (part.startsWith("/")) out = part;
    else out = out.endsWith("/") ? out + part : `${out}/${part}`;
  }
  return out;
}

/** Lexical normalisation of an absolute path (no file-system access). */
export function normalizeAbsolute(path: string): string {
  const out: string[] = [];
  for (const c of path.split("/")) {
    if (c === "" || c === ".") continue;
    if (c === "..") out.pop();
    else out.push(c);
  }
  return `/${out.join("/")}`;
}

/** Empty and relative values count as unset; others are cleaned as Rust's Path::components would. */
function absOrUnset(v: string | undefined): string | undefined {
  if (v === undefined || !v.startsWith("/")) return undefined;
  return `/${v.split("/").filter((c) => c !== "" && c !== ".").join("/")}`;
}

function sha8(s: string): string {
  return createHash("sha256").update(s, "utf8").digest("hex").slice(0, 8);
}

export function resolvePaths(env: PathsEnv): Paths {
  const tmpFallback = `/tmp/todo-tui-${env.uid}`;
  let config: string;
  let data: string;
  let logs: string;
  let runtime: string;
  let longFallback = tmpFallback;

  const tth = env.todoTuiHome === "" ? undefined : env.todoTuiHome;
  if (tth !== undefined) {
    if (!tth.startsWith("/")) {
      throw new PathsError("RelativeTodoTuiHome", `TODO_TUI_HOME must be an absolute path: ${JSON.stringify(tth)}`);
    }
    const dir = normalizeAbsolute(tth);
    config = join(dir, "config.toml");
    data = join(dir, "data");
    logs = join(dir, "logs");
    runtime = join(dir, "run");
    longFallback = join(tmpFallback, `h-${sha8(dir)}`);
  } else {
    const home = absOrUnset(env.home);
    const fromHome = (...parts: string[]) => {
      if (home === undefined) throw new PathsError("NoHome", "HOME is not set and TODO_TUI_HOME is not set");
      return join(home, ...parts);
    };
    config = join(absOrUnset(env.xdgConfigHome) ?? fromHome(".config"), "todo-tui", "config.toml");
    if (env.os === "macos") {
      data = fromHome("Library", "Application Support", "todo-tui");
      logs = fromHome("Library", "Logs", "todo-tui");
      const tmp = absOrUnset(env.tmpdir);
      runtime = tmp !== undefined ? join(tmp, "todo-tui") : tmpFallback;
    } else {
      data = join(absOrUnset(env.xdgDataHome) ?? fromHome(".local", "share"), "todo-tui");
      logs = join(absOrUnset(env.xdgStateHome) ?? fromHome(".local", "state"), "todo-tui");
      const rt = absOrUnset(env.xdgRuntimeDir);
      runtime = rt !== undefined ? join(rt, "todo-tui") : tmpFallback;
    }
  }

  if (Buffer.byteLength(join(runtime, "daemon.sock"), "utf8") > MAX_SOCKET_PATH_BYTES) {
    runtime = longFallback;
  }
  return {
    config,
    data,
    logs,
    runtime,
    socket: join(runtime, "daemon.sock"),
    lock: join(data, "daemon.lock"),
  };
}

let darwinTempDir: string | null | undefined;

/** macOS's per-user temp dir, used when TMPDIR is unset (as launchd jobs often are). */
function darwinUserTempDir(): string | undefined {
  if (darwinTempDir === undefined) {
    try {
      darwinTempDir = execFileSync("getconf", ["DARWIN_USER_TEMP_DIR"], { encoding: "utf8" }).trim() || null;
    } catch {
      darwinTempDir = null;
    }
  }
  return darwinTempDir ?? undefined;
}

/** Reads the current process environment and uid. */
export function envFromProcess(vars: NodeJS.ProcessEnv = process.env): PathsEnv {
  const os: Os = process.platform === "darwin" ? "macos" : "linux";
  const get = (k: string) => (vars[k] === "" ? undefined : vars[k]);
  let todoTuiHome = get("TODO_TUI_HOME");
  if (todoTuiHome !== undefined && !todoTuiHome.startsWith("/")) {
    todoTuiHome = join(process.cwd(), todoTuiHome);
  }
  let tmpdir = get("TMPDIR");
  if (os === "macos" && absOrUnset(tmpdir) === undefined) tmpdir = darwinUserTempDir();
  return {
    os,
    home: get("HOME"),
    todoTuiHome,
    xdgConfigHome: get("XDG_CONFIG_HOME"),
    xdgDataHome: get("XDG_DATA_HOME"),
    xdgStateHome: get("XDG_STATE_HOME"),
    xdgRuntimeDir: get("XDG_RUNTIME_DIR"),
    tmpdir,
    uid: process.getuid?.() ?? userInfo().uid,
  };
}

/** The daemon socket for the current process environment. */
export function defaultSocketPath(vars: NodeJS.ProcessEnv = process.env): string {
  return resolvePaths(envFromProcess(vars)).socket;
}
