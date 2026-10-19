//! End-to-end tests of the `todo` binary on temp `TODO_TUI_HOME`s.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A temp home whose daemon is stopped on drop, even if the test fails.
struct Home {
    _dir: TempDir,
    home: PathBuf,
}

impl Home {
    fn new() -> Home {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_owned();
        Home { _dir: dir, home }
    }

    /// A home deep enough that its socket path exceeds 100 bytes (D8).
    fn long() -> Home {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("x".repeat(100));
        Home { _dir: dir, home }
    }

    fn todo(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_todo"))
            .args(args)
            .env("TODO_TUI_HOME", &self.home)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.todo(args);
        assert!(
            out.status.success(),
            "todo {args:?} failed: {}\nlog:\n{}",
            String::from_utf8_lossy(&out.stderr),
            std::fs::read_to_string(self.home.join("logs/daemon.log")).unwrap_or_default()
        );
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.todo(&["daemon", "stop"]);
    }
}

fn socket_in(dir: &Path) -> bool {
    dir.join("daemon.sock").exists()
}

#[test]
fn add_auto_spawns_the_daemon_then_stop_stops_it() {
    let h = Home::new();
    let out = h.ok(&["add", "call", "the", "dentist"]);
    let (id, title) = out.trim_end().split_once("  ").unwrap();
    assert_eq!(id.len(), 8, "{out:?}");
    assert_eq!(title, "call the dentist");
    assert!(socket_in(&h.home.join("run")));

    // The alias reuses the running daemon.
    assert!(
        h.ok(&["a", "-v", "second"])
            .trim_end()
            .ends_with("  -v second")
    );

    assert_eq!(h.ok(&["daemon", "stop"]).trim(), "todo daemon: stopped");
    assert!(!socket_in(&h.home.join("run")));
    assert_eq!(h.ok(&["daemon", "stop"]).trim(), "todo daemon: not running");
}

#[test]
fn parallel_detach_leaves_one_daemon() {
    let h = Home::new();
    std::thread::scope(|s| {
        let runs: Vec<_> = (0..5)
            .map(|_| s.spawn(|| h.ok(&["daemon", "--detach"])))
            .collect();
        for r in runs {
            r.join().unwrap();
        }
    });
    let pid = std::fs::read_to_string(h.home.join("data/daemon.lock")).unwrap();
    assert!(pid.trim().parse::<u32>().is_ok(), "{pid:?}");
    assert_eq!(
        h.ok(&["daemon", "--detach"]).trim(),
        "todo daemon: already running"
    );
    h.ok(&["add", "still one daemon"]);
    assert_eq!(h.ok(&["daemon", "stop"]).trim(), "todo daemon: stopped");
}

#[test]
fn long_home_uses_the_hashed_tmp_fallback() {
    let h = Home::long();
    h.ok(&["add", "long path"]);
    assert!(!socket_in(&h.home.join("run")));
    let uid = rustix::process::getuid().as_raw();
    let fallback = PathBuf::from(format!("/tmp/todo-tui-{uid}"));
    let found = std::fs::read_dir(&fallback)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("h-") && socket_in(p))
        .unwrap_or_else(|| panic!("no socket under {}", fallback.display()));
    h.ok(&["daemon", "stop"]);
    assert!(!socket_in(&found));
    std::fs::remove_dir(found).unwrap();
}
