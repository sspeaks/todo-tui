//! File-system locations (design 7.1, with ADR-0001 D7 and D8).
//!
//! Resolution is a pure function of [`Env`], so the same vectors
//! (`testdata/paths.json`) can test this module and the TypeScript port.

use std::path::PathBuf;

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

impl Env {
    /// Reads the current process environment and uid.
    pub fn from_process() -> Env {
        todo!()
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
    #[allow(unused_variables)]
    pub fn resolve(env: &Env) -> Result<Paths, PathsError> {
        todo!()
    }
}
