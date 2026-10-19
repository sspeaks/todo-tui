//! File-system locations (design 7.1, with ADR-0001 D7 and D8).
//!
//! Resolution is a pure function of [`Env`], so the same vectors
//! (`testdata/paths.json`) can test this module and the TypeScript port.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// Unix socket paths longer than this fall back to `/tmp` (7.1).
pub const MAX_SOCKET_PATH_BYTES: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
}

/// The inputs to path resolution. Empty or relative XDG variables, `HOME`
/// and `TMPDIR` count as unset. `TODO_TUI_HOME` never falls back that way:
/// see [`Env::todo_tui_home`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    pub os: Os,
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// `$TODO_TUI_HOME` (unset when empty). [`Env::from_process`] makes a
    /// relative value absolute against the current directory; [`Paths::resolve`]
    /// rejects a relative value rather than use the default layout.
    pub todo_tui_home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub xdg_runtime_dir: Option<PathBuf>,
    /// `$TMPDIR` (per-user on macOS).
    pub tmpdir: Option<PathBuf>,
    /// The real uid, for `/tmp/todo-tui-$UID`.
    pub uid: u32,
}

/// macOS's per-user temp dir, used when `TMPDIR` is unset (as launchd jobs
/// often are). Must agree with the TS client's `envFromProcess`.
fn darwin_user_temp_dir() -> Option<PathBuf> {
    let out = std::process::Command::new("getconf")
        .arg("DARWIN_USER_TEMP_DIR")
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let dir = String::from_utf8(out.stdout).ok()?;
    let dir = dir.trim();
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

impl Env {
    /// Reads the current process environment and uid.
    pub fn from_process() -> Env {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let todo_tui_home = var("TODO_TUI_HOME").map(|p| {
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().map(|cwd| cwd.join(&p)).unwrap_or(p)
            }
        });
        Env {
            os: if cfg!(target_os = "macos") {
                Os::MacOs
            } else {
                Os::Linux
            },
            home: var("HOME"),
            todo_tui_home,
            xdg_config_home: var("XDG_CONFIG_HOME"),
            xdg_data_home: var("XDG_DATA_HOME"),
            xdg_state_home: var("XDG_STATE_HOME"),
            xdg_runtime_dir: var("XDG_RUNTIME_DIR"),
            tmpdir: var("TMPDIR").filter(|p| p.is_absolute()).or_else(|| {
                cfg!(target_os = "macos")
                    .then(darwin_user_temp_dir)
                    .flatten()
            }),
            uid: rustix::process::getuid().as_raw(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The config file, `.../config.toml`. The daemon never writes it.
    pub config: PathBuf,
    /// Database, backups and spool.
    pub data: PathBuf,
    pub logs: PathBuf,
    /// Directory holding the socket (mode 0700).
    pub runtime: PathBuf,
    /// `runtime/daemon.sock`, at most [`MAX_SOCKET_PATH_BYTES`] bytes.
    pub socket: PathBuf,
    /// `data/daemon.lock` (D7: next to the database, not in `runtime`).
    pub lock: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathsError {
    #[error("HOME is not set and TODO_TUI_HOME is not set")]
    NoHome,
    #[error("TODO_TUI_HOME must be an absolute path: {0:?}")]
    RelativeTodoTuiHome(PathBuf),
}

impl Paths {
    /// Resolves all locations. Nothing is created on disk.
    ///
    /// - `TODO_TUI_HOME=<dir>`: `<dir>` is first normalised lexically (no
    ///   file-system access): repeated and trailing separators are removed,
    ///   `.` components dropped, and `..` removes the preceding component
    ///   (`..` at `/` stays `/`). Then `<dir>/config.toml`, `<dir>/data`,
    ///   `<dir>/logs`, runtime `<dir>/run`. A relative `<dir>` fails with
    ///   [`PathsError::RelativeTodoTuiHome`].
    /// - Otherwise per OS (7.1). Config: `$XDG_CONFIG_HOME/todo-tui/config.toml`,
    ///   default `~/.config/todo-tui/config.toml`. macOS: data
    ///   `~/Library/Application Support/todo-tui`, logs `~/Library/Logs/todo-tui`,
    ///   runtime `$TMPDIR/todo-tui`. Linux: data `$XDG_DATA_HOME/todo-tui`
    ///   (default `~/.local/share/todo-tui`), logs `$XDG_STATE_HOME/todo-tui`
    ///   (default `~/.local/state/todo-tui`), runtime `$XDG_RUNTIME_DIR/todo-tui`.
    ///   With no `TMPDIR` / `XDG_RUNTIME_DIR`, runtime is `/tmp/todo-tui-$UID`.
    /// - If `runtime/daemon.sock` exceeds [`MAX_SOCKET_PATH_BYTES`], runtime
    ///   becomes `/tmp/todo-tui-$UID`, or with `TODO_TUI_HOME` set
    ///   `/tmp/todo-tui-$UID/h-<sha8>` where `sha8` is the first 8 hex digits
    ///   of SHA-256 over the normalised `TODO_TUI_HOME` path bytes (D8), so
    ///   `/x`, `/x/` and `/y/../x` share one socket.
    pub fn resolve(env: &Env) -> Result<Paths, PathsError> {
        let fallback = PathBuf::from(format!("/tmp/todo-tui-{}", env.uid));
        let (config, data, logs, runtime, home_key) = match non_empty(&env.todo_tui_home) {
            Some(dir) => {
                if !dir.is_absolute() {
                    return Err(PathsError::RelativeTodoTuiHome(dir.to_path_buf()));
                }
                let dir = normalize(dir);
                (
                    dir.join("config.toml"),
                    dir.join("data"),
                    dir.join("logs"),
                    dir.join("run"),
                    Some(dir),
                )
            }
            None => {
                let home = || absolute(&env.home).ok_or(PathsError::NoHome);
                let xdg_or =
                    |var: &Option<PathBuf>, default: &str| -> Result<PathBuf, PathsError> {
                        match absolute(var) {
                            Some(dir) => Ok(dir),
                            None => Ok(home()?.join(default)),
                        }
                    };
                let config = xdg_or(&env.xdg_config_home, ".config")?.join("todo-tui/config.toml");
                let (data, logs, runtime_base) = match env.os {
                    Os::MacOs => (
                        home()?.join("Library/Application Support/todo-tui"),
                        home()?.join("Library/Logs/todo-tui"),
                        &env.tmpdir,
                    ),
                    Os::Linux => (
                        xdg_or(&env.xdg_data_home, ".local/share")?.join("todo-tui"),
                        xdg_or(&env.xdg_state_home, ".local/state")?.join("todo-tui"),
                        &env.xdg_runtime_dir,
                    ),
                };
                let runtime = absolute(runtime_base)
                    .map(|dir| dir.join("todo-tui"))
                    .unwrap_or_else(|| fallback.clone());
                (config, data, logs, runtime, None)
            }
        };

        let mut runtime = runtime;
        let mut socket = runtime.join(SOCKET_NAME);
        if socket.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
            runtime = match &home_key {
                Some(home) => fallback.join(format!("h-{}", sha8(home))),
                None => fallback,
            };
            socket = runtime.join(SOCKET_NAME);
        }
        let lock = data.join(LOCK_NAME);
        Ok(Paths {
            config,
            data,
            logs,
            runtime,
            socket,
            lock,
        })
    }

    /// The SQLite database in [`Paths::data`].
    pub fn database(&self) -> PathBuf {
        self.data.join("todo.db")
    }

    /// The daemon log file in [`Paths::logs`].
    pub fn daemon_log(&self) -> PathBuf {
        self.logs.join("daemon.log")
    }
}

const SOCKET_NAME: &str = "daemon.sock";
const LOCK_NAME: &str = "daemon.lock";

fn non_empty(p: &Option<PathBuf>) -> Option<&Path> {
    p.as_deref().filter(|p| !p.as_os_str().is_empty())
}

/// An absolute path with repeated and trailing separators and `.` removed;
/// `None` for unset, empty or relative values.
fn absolute(p: &Option<PathBuf>) -> Option<PathBuf> {
    non_empty(p)
        .filter(|p| p.is_absolute())
        .map(|p| p.components().collect())
}

/// Lexical normalisation of an absolute path, resolving `.` and `..`.
fn normalize(p: &Path) -> PathBuf {
    let mut parts: Vec<OsString> = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    let mut out = PathBuf::from("/");
    out.extend(parts);
    out
}

fn sha8(p: &Path) -> String {
    let digest = Sha256::digest(p.as_os_str().as_bytes());
    hex::encode(&digest[..4])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_is_lexical() {
        for (input, want) in [
            ("/", "/"),
            ("/a//b/", "/a/b"),
            ("/a/./b/../c", "/a/c"),
            ("/..", "/"),
            ("/a/../../b", "/b"),
        ] {
            assert_eq!(normalize(Path::new(input)), PathBuf::from(want), "{input}");
        }
    }
}
