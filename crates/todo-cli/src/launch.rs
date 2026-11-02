//! `todo` with no command and `todo mcp` (design 4.2, 4.6): exec a bundled
//! JavaScript client with Node.

use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail};

/// A JavaScript client the binary ships alongside it.
#[derive(Clone, Copy)]
pub(crate) struct Bundle {
    /// What it is called in messages.
    name: &'static str,
    /// The file under `share/todo-tui/` and `<package>/dist/`.
    file: &'static str,
    /// Its workspace package directory, relative to the checkout.
    package: &'static str,
    /// The variable that overrides where it is.
    env: &'static str,
}

pub(crate) const TUI: Bundle = Bundle {
    name: "the TUI",
    file: "tui.js",
    package: "tui",
    env: "TODO_TUI_ENTRY",
};

pub(crate) const MCP: Bundle = Bundle {
    name: "the MCP server",
    file: "mcp.js",
    package: "mcp",
    env: "TODO_MCP_ENTRY",
};

/// The source checkout this binary was built from (`crates/todo-cli/../..`).
fn checkout() -> &'static Path {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().and_then(Path::parent).unwrap_or(manifest)
}

/// Replaces this process with `node <bundle> <args>`, so the client owns
/// stdio and its exit status is ours. The environment passes through, with
/// `TODO_BIN` set to this binary unless it is set already, so the client
/// starts this build's daemon. Returns only on failure.
pub(crate) fn launch(bundle: Bundle, args: &[&str]) -> anyhow::Error {
    let exe = std::env::current_exe();
    let entry = match find_entry(
        bundle,
        std::env::var_os(bundle.env),
        exe.as_deref().ok(),
        checkout(),
    ) {
        Ok(entry) => entry,
        Err(e) => return e,
    };
    let mut node = Command::new("node");
    node.arg(&entry).args(args);
    if std::env::var_os("TODO_BIN").is_none_or(|bin| bin.is_empty())
        && let Ok(exe) = exe
    {
        node.env("TODO_BIN", exe);
    }
    let e = node.exec();
    if e.kind() == io::ErrorKind::NotFound {
        anyhow!(
            "cannot run {}: node is not on PATH; install Node 24 or newer",
            bundle.name
        )
    } else {
        anyhow!("cannot run node {}: {e}", entry.display())
    }
}

/// The bundle to run, in order (design 4.6):
///
/// 1. `$<bundle.env>`, when set and not empty. It must name a file: a
///    missing one is an error rather than a fall-through to another build.
/// 2. `<directory of exe>/../share/todo-tui/<file>`, the packaged layout
///    (`..` taken lexically).
/// 3. `<package>/dist/<file>` in `checkout`, the source checkout `exe` was
///    built from (`target/debug/todo`, `cargo install --path crates/todo-cli`).
fn find_entry(
    bundle: Bundle,
    env_entry: Option<OsString>,
    exe: Option<&Path>,
    checkout: &Path,
) -> anyhow::Result<PathBuf> {
    let Bundle {
        name,
        file,
        package,
        env,
    } = bundle;
    if let Some(entry) = env_entry.filter(|e| !e.is_empty()).map(PathBuf::from) {
        if entry.is_file() {
            return Ok(entry);
        }
        bail!("{env} is {}, which is not a file", entry.display());
    }
    let packaged = exe
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|prefix| prefix.join("share/todo-tui").join(file));
    let built = checkout.join(package).join("dist").join(file);
    if let Some(entry) = packaged.iter().chain([&built]).find(|e| e.is_file()) {
        return Ok(entry.clone());
    }
    if checkout.join(package).join("package.json").is_file() {
        bail!(
            "{name} is not built; run `pnpm -C {} build` (or set {env})",
            checkout.join(package).display()
        );
    }
    let looked = packaged.as_deref().unwrap_or(&built);
    bail!(
        "{name} is not installed: no {}; set {env} to a built {file}",
        looked.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_is_found_in_order() {
        for bundle in [TUI, MCP] {
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
                find_entry(bundle, env.map(OsString::from), Some(&exe), &checkout)
                    .map_err(|e| format!("{e:#}"))
            };
            let (name, js, package, var) = (bundle.name, bundle.file, bundle.package, bundle.env);

            let custom = file(&format!("custom/{js}"));
            assert_eq!(
                find(None),
                Err(format!(
                    "{name} is not installed: no {}; set {var} to a built {js}",
                    root.join("prefix/share/todo-tui").join(js).display()
                ))
            );
            file(&format!("src/{package}/package.json"));
            assert_eq!(
                find(None),
                Err(format!(
                    "{name} is not built; run `pnpm -C {} build` (or set {var})",
                    checkout.join(package).display()
                ))
            );
            let built = file(&format!("src/{package}/dist/{js}"));
            assert_eq!(find(None), Ok(built.clone()));
            // An empty override counts as unset.
            assert_eq!(find(Some(Path::new(""))), Ok(built));
            let packaged = file(&format!("prefix/share/todo-tui/{js}"));
            assert_eq!(find(None), Ok(packaged));
            assert_eq!(find(Some(&custom)), Ok(custom));
            // An override that is not a file does not fall through.
            let missing = root.join("missing.js");
            assert_eq!(
                find(Some(&missing)),
                Err(format!(
                    "{var} is {}, which is not a file",
                    missing.display()
                ))
            );
            assert_eq!(
                find(Some(root)),
                Err(format!("{var} is {}, which is not a file", root.display()))
            );
        }
    }
}
