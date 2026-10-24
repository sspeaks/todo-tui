//! `todo` with no command (design 4.2, 4.6): exec the bundled TUI with Node.

use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail};

/// The source checkout this binary was built from (`crates/todo-cli/../..`).
fn checkout() -> &'static Path {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().and_then(Path::parent).unwrap_or(manifest)
}

/// Replaces this process with `node <tui.js>`, so the TUI owns the terminal
/// and its exit status is ours. The environment passes through, with
/// `TODO_BIN` set to this binary unless it is set already, so the TUI starts
/// this build's daemon. Returns only on failure.
pub(crate) fn launch() -> anyhow::Error {
    let exe = std::env::current_exe();
    let entry = match find_entry(
        std::env::var_os("TODO_TUI_ENTRY"),
        exe.as_deref().ok(),
        checkout(),
    ) {
        Ok(entry) => entry,
        Err(e) => return e,
    };
    let mut node = Command::new("node");
    node.arg(&entry);
    if std::env::var_os("TODO_BIN").is_none_or(|bin| bin.is_empty())
        && let Ok(exe) = exe
    {
        node.env("TODO_BIN", exe);
    }
    let e = node.exec();
    if e.kind() == io::ErrorKind::NotFound {
        anyhow!("cannot run the TUI: node is not on PATH; install Node 24 or newer")
    } else {
        anyhow!("cannot run node {}: {e}", entry.display())
    }
}

/// The TUI bundle to run, in order (design 4.6):
///
/// 1. `$TODO_TUI_ENTRY`, when set and not empty. It must name a file: a
///    missing one is an error rather than a fall-through to another build.
/// 2. `<directory of exe>/../share/todo-tui/tui.js`, the packaged layout
///    (`..` taken lexically).
/// 3. `tui/dist/tui.js` in `checkout`, the source checkout `exe` was built
///    from (`target/debug/todo`, `cargo install --path crates/todo-cli`).
fn find_entry(
    env_entry: Option<OsString>,
    exe: Option<&Path>,
    checkout: &Path,
) -> anyhow::Result<PathBuf> {
    if let Some(entry) = env_entry.filter(|e| !e.is_empty()).map(PathBuf::from) {
        if entry.is_file() {
            return Ok(entry);
        }
        bail!("TODO_TUI_ENTRY is {}, which is not a file", entry.display());
    }
    let packaged = exe
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|prefix| prefix.join("share/todo-tui/tui.js"));
    let built = checkout.join("tui/dist/tui.js");
    if let Some(entry) = packaged.iter().chain([&built]).find(|e| e.is_file()) {
        return Ok(entry.clone());
    }
    if checkout.join("tui/package.json").is_file() {
        bail!(
            "the TUI is not built; run `pnpm -C {} build` (or set TODO_TUI_ENTRY)",
            checkout.join("tui").display()
        );
    }
    let looked = packaged.as_deref().unwrap_or(&built);
    bail!(
        "the TUI is not installed: no {}; set TODO_TUI_ENTRY to a built tui.js",
        looked.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_is_found_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = |path: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
            path
        };
        let exe = root.join("prefix/bin/todo");
        let checkout = root.join("src");
        let find = |env: Option<&Path>| {
            find_entry(env.map(OsString::from), Some(&exe), &checkout).map_err(|e| format!("{e:#}"))
        };

        let custom = file("custom/tui.js");
        assert_eq!(
            find(None),
            Err(format!(
                "the TUI is not installed: no {}; set TODO_TUI_ENTRY to a built tui.js",
                root.join("prefix/share/todo-tui/tui.js").display()
            ))
        );
        file("src/tui/package.json");
        assert_eq!(
            find(None),
            Err(format!(
                "the TUI is not built; run `pnpm -C {} build` (or set TODO_TUI_ENTRY)",
                checkout.join("tui").display()
            ))
        );
        let built = file("src/tui/dist/tui.js");
        assert_eq!(find(None), Ok(built.clone()));
        // An empty TODO_TUI_ENTRY counts as unset.
        assert_eq!(find(Some(Path::new(""))), Ok(built));
        let packaged = file("prefix/share/todo-tui/tui.js");
        assert_eq!(find(None), Ok(packaged));
        assert_eq!(find(Some(&custom)), Ok(custom));
        // A TODO_TUI_ENTRY that is not a file does not fall through.
        let missing = root.join("missing.js");
        assert_eq!(
            find(Some(&missing)),
            Err(format!(
                "TODO_TUI_ENTRY is {}, which is not a file",
                missing.display()
            ))
        );
        assert_eq!(
            find(Some(root)),
            Err(format!(
                "TODO_TUI_ENTRY is {}, which is not a file",
                root.display()
            ))
        );
    }
}
