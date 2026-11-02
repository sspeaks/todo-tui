//! `todo config` (design 8.2): where the config file is, what is wrong
//! with it, and telling the daemon to read it again.

use std::process::ExitCode;

use todo_daemon::config::Config;
use todo_daemon::paths::Paths;
use todo_proto::v1::ReloadConfigRequest;
use todo_proto::v1::system_service_client::SystemServiceClient;

use crate::rpc::{call, ready};
use crate::{print_err, print_out};

/// `todo config path`.
pub(crate) fn path(paths: &Paths) -> ExitCode {
    print_out(paths.config.display());
    ExitCode::SUCCESS
}

/// `todo config check`: reads the file here, as the daemon would, and
/// prints each warning. Fails when there are any, or when the file is not
/// TOML; needs no daemon and changes nothing.
pub(crate) fn check(paths: &Paths) -> ExitCode {
    match Config::load(&paths.config) {
        Err(e) => {
            print_err(format!("todo config: {e}"));
            ExitCode::FAILURE
        }
        Ok(config) if !config.exists => {
            print_out(format!(
                "{}: no config file; the defaults apply",
                paths.config.display()
            ));
            ExitCode::SUCCESS
        }
        Ok(config) if config.warnings.is_empty() => {
            print_out(format!("{}: ok", paths.config.display()));
            ExitCode::SUCCESS
        }
        Ok(config) => {
            for warning in &config.warnings {
                print_out(format!("{}: {warning}", paths.config.display()));
            }
            ExitCode::FAILURE
        }
    }
}

/// `todo config reload`: has the daemon read the file again (as SIGHUP
/// does), starting it if needed, and prints its warnings.
pub(crate) async fn reload(paths: &Paths) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let res = call(SystemServiceClient::new(channel).reload_config(ReloadConfigRequest {}))
        .await
        .map_err(|s| anyhow::anyhow!("{}", s.message()))?;
    print_out(if res.changed { "reloaded" } else { "unchanged" });
    for warning in res.config.map(|c| c.warnings).unwrap_or_default() {
        let line = if warning.key.is_empty() {
            warning.message
        } else {
            format!("{}: {}", warning.key, warning.message)
        };
        print_out(format!("warning: {line}"));
    }
    Ok(ExitCode::SUCCESS)
}
