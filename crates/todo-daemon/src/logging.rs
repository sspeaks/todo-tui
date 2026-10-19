use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

/// Sends `tracing` output to `log_file` (appending). `TODO_TUI_LOG` takes an
/// env-filter directive; the default is `info`.
pub(crate) fn init(log_file: &Path) -> anyhow::Result<()> {
    if let Some(dir) = log_file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .with_context(|| format!("opening {}", log_file.display()))?;
    let filter = EnvFilter::try_from_env("TODO_TUI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    // Ignore "already set": tests and embedders may install their own.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Arc::new(file))
        .try_init();
    Ok(())
}
