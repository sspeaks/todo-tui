//! `todo sync …` (design 5, ADR-0006). `check` needs no daemon: it reads the
//! config, resolves the credential and asks Jira who the token is, which
//! only reads.

use std::process::ExitCode;

use todo_daemon::config::Config;
use todo_daemon::paths::Paths;
use todo_sync::credential::{SystemSecrets, resolve};

use crate::{print_err, print_out};

/// `todo sync check`: for each instance, `ok as <name> (<key>)` or why not.
pub(crate) async fn check(paths: &Paths) -> anyhow::Result<ExitCode> {
    let config = Config::load_or_defaults(&paths.config);
    for w in config
        .warnings
        .iter()
        // The file as a whole (not TOML) and the instances.
        .filter(|w| w.key.is_empty() || w.key.starts_with("instances"))
    {
        print_err(format!("{}: {w}", paths.config.display()));
    }
    if config.instances.is_empty() {
        print_err(format!(
            "{}: no [[instances]] to sync; see the README's Jira section",
            paths.config.display()
        ));
        return Ok(ExitCode::FAILURE);
    }
    let mut failed = false;
    for instance in &config.instances {
        let line = match resolve(&instance.credential, &SystemSecrets) {
            Err(e) => {
                failed = true;
                format!("{}: credential {e}", instance.id)
            }
            Ok(token) => match todo_jira::Client::new(&instance.base_url, token) {
                Err(e) => {
                    failed = true;
                    format!("{}: {e}", instance.id)
                }
                Ok(client) => match client.myself().await {
                    Ok(me) => {
                        let tz = if me.time_zone.is_empty() {
                            "unknown".to_owned()
                        } else {
                            me.time_zone
                        };
                        format!(
                            "{}: ok as {} ({}), time zone {tz}",
                            instance.id,
                            if me.display_name.is_empty() {
                                &me.name
                            } else {
                                &me.display_name
                            },
                            me.key
                        )
                    }
                    Err(e) => {
                        failed = true;
                        format!("{}: {} — {e}", instance.id, instance.base_url)
                    }
                },
            },
        };
        print_out(line);
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
