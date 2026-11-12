//! The `todo` binary.

mod add;
mod assistant;
mod backup;
mod config;
mod conn;
mod export;
mod frames;
mod launch;
mod remote;
mod report;
mod rpc;
mod spawn;
mod status;
mod sync;
mod undo;

use std::fmt::Display;
use std::io::Write as _;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use todo_daemon::paths::{Env, Paths};
use todo_proto::v1::ShutdownRequest;
use todo_proto::v1::system_service_client::SystemServiceClient;

#[derive(Parser)]
#[command(name = "todo", version, about = "A keyboard-driven todo manager")]
#[command(after_help = "Without a command, `todo` opens the TUI.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Capture an Item into the inbox.
    #[command(visible_alias = "a")]
    Add {
        /// The capture text; several words are joined with spaces. Without
        /// it, each non-empty line of stdin is one capture.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        text: Vec<String>,
    },
    /// Undo the most recent change, made from the CLI or the TUI.
    Undo,
    /// Redo the most recently undone change.
    Redo,
    /// Show the inbox and due counts, and the count of each view.
    Status {
        /// Print one line for a shell prompt, e.g. `📥3 ⏰1`, leaving out
        /// zero counts. Prints nothing, and still succeeds, when the daemon
        /// is not running or does not answer within 40 ms; never starts it.
        #[arg(long)]
        short: bool,
    },
    /// Print the L48 / N48 report for a scrum: what moved in the last two
    /// working days, and what is planned for the next two.
    L48 {
        /// Count from this day instead, back from today: thu, -3d, oct29,
        /// yesterday or 2026-10-29.
        #[arg(long, allow_hyphen_values = true)]
        since: Option<String>,
        /// Also copy it to the clipboard (pbcopy, wl-copy or xclip).
        #[arg(long)]
        copy: bool,
    },
    /// Make a todo a Jira issue (design 5.7): created now, in promote_default's
    /// project unless given, and linked. Your team sees it.
    Promote {
        /// The todo: its short id or full id.
        id: String,
        /// The Jira project key; else promote_default's.
        #[arg(long)]
        project: Option<String>,
        /// The issue type's name; else promote_default's.
        #[arg(long = "type")]
        kind: Option<String>,
    },
    /// Link a todo to an existing Jira issue by key (design 3.3): Jira's
    /// fields win, and the todo's that differed wait in the TUI (S).
    Link {
        /// The todo: its short id or full id.
        id: String,
        /// The issue's key, e.g. PLAT-12.
        key: String,
    },
    /// Ask Claude to refine an Item (ADR-0010): it proposes a clearer title,
    /// due date, priority, labels and waiting-on, for review in the TUI (S).
    Refine {
        /// The Item: the 8-character short id `todo add` prints, or the full id.
        id: String,
    },
    /// The assistant (Claude, through Claude Code's CLI).
    Assistant {
        #[command(subcommand)]
        action: AssistantAction,
    },
    /// Serve the MCP tools on stdio, for Claude Code: `claude mcp add todo -- todo mcp`.
    Mcp {
        /// Offer only the tools that read: list, get, views and counts.
        #[arg(long)]
        read_only: bool,
    },
    /// Jira sync (design 5): check a token, see the state, pull now.
    Sync {
        #[command(subcommand)]
        action: SyncAction,
    },
    /// The config file: where it is, what is wrong with it, and reloading it.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Write every record of the database as JSON Lines (design 7.4).
    Export {
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Leave out the ops and events, the history behind undo.
        #[arg(long)]
        no_history: bool,
    },
    /// Restore an export into an empty database.
    Import {
        /// The export file, or - for stdin.
        path: std::path::PathBuf,
    },
    /// Back up the database, or list the backups (design 7.5).
    Backup {
        #[command(subcommand)]
        action: BackupAction,
    },
    /// Put a backup in place of the database: the current one is backed
    /// up first, and the daemon restarted.
    Restore {
        /// The backup file.
        path: std::path::PathBuf,
        /// Do not ask first.
        #[arg(long)]
        yes: bool,
    },
    /// Run the daemon (in the foreground unless --detach).
    Daemon {
        /// Start the daemon in the background and return once it is listening.
        #[arg(long)]
        detach: bool,
        #[command(subcommand)]
        action: Option<DaemonAction>,
    },
    /// Pair the phone app with the remote listener, and manage its devices.
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },
}

#[derive(Subcommand)]
enum AssistantAction {
    /// The latest requests to Claude and what they cost.
    Usage,
}

#[derive(Subcommand)]
enum RemoteAction {
    /// Show whether the remote listener is on, and how to turn it on.
    Status,
    /// Pair a device: prints its token and a QR code for the app.
    Pair {
        /// A name for the device, e.g. `phone`.
        #[arg(long)]
        name: String,
        /// The address the app connects to. Defaults to this machine's
        /// Tailscale name, https://<machine>.<tailnet>.ts.net.
        #[arg(long)]
        url: Option<String>,
        /// Leave out the QR code.
        #[arg(long)]
        no_qr: bool,
    },
    /// List the paired devices.
    List,
    /// Unpair a device, by name or by the id `list` shows. Takes effect at
    /// once.
    Revoke { device: String },
}

#[derive(Subcommand)]
enum BackupAction {
    /// Take a backup now, also when automatic backups are off.
    Now,
    /// List the backups, oldest first.
    List,
}

#[derive(Subcommand)]
enum SyncAction {
    /// Check each Jira instance's config and token, reading only: no daemon needed.
    Check,
    /// Each instance's sync state: ok, offline, a refused token; the last pull.
    Status,
    /// Pull now, and wait for it.
    Now {
        /// Only this instance (its id in the config).
        instance: Option<String>,
    },
    /// What waits to reach Jira, and what Jira refused.
    Outbox,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the path of the config file.
    Path,
    /// Check the config file without applying it; fails if anything is wrong.
    Check,
    /// Have the running daemon read the config file again (as SIGHUP does).
    Reload,
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Ask a running daemon to shut down and wait until it has exited.
    Stop,
}

fn main() -> ExitCode {
    let Some(command) = Cli::parse().command else {
        eprintln!("todo: {:#}", launch::launch(launch::TUI, &[]));
        return ExitCode::FAILURE;
    };
    if let Command::Mcp { read_only } = command {
        let args: &[&str] = if read_only { &["--read-only"] } else { &[] };
        eprintln!("todo: {:#}", launch::launch(launch::MCP, args));
        return ExitCode::FAILURE;
    }
    // Ahead of anything that could fail: a prompt hook must not.
    if let Command::Status { short: true } = command {
        return status::short();
    }
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("todo: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(dispatch(command)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("todo: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(command: Command) -> anyhow::Result<ExitCode> {
    let paths = Paths::resolve(&Env::from_process())?;
    // With a daemon elsewhere (ADR-0008), what works on this machine's own
    // daemon or its files is refused rather than done to the wrong one.
    // Stopping one left running here is still allowed.
    let local_only = match &command {
        Command::Daemon {
            action: Some(DaemonAction::Stop),
            ..
        } => None,
        Command::Daemon { .. } => Some("todo daemon"),
        Command::Restore { .. } => Some("todo restore"),
        Command::Backup {
            action: BackupAction::List,
        } => Some("todo backup list"),
        Command::Remote { .. } => Some("todo remote"),
        _ => None,
    };
    if let Some(what) = local_only
        && let Some(there) = conn::elsewhere(&paths)
    {
        anyhow::bail!(
            "{what} acts on this machine's daemon, but this machine uses {there} \
             ([client] remote, or TODO_REMOTE): run it there"
        );
    }
    match command {
        Command::Add { text } => return add::add(&paths, text).await,
        Command::Undo => return undo::undo(&paths, undo::Direction::Undo).await,
        Command::Redo => return undo::undo(&paths, undo::Direction::Redo).await,
        // `--short` is run by main.
        Command::Status { .. } => return status::status(&paths).await,
        Command::L48 { since, copy } => return report::l48(&paths, since, copy).await,
        Command::Refine { id } => return assistant::refine(&paths, id).await,
        Command::Promote { id, project, kind } => {
            return sync::promote(&paths, id, project, kind).await;
        }
        Command::Link { id, key } => return sync::link(&paths, id, key).await,
        Command::Assistant { action } => {
            return match action {
                AssistantAction::Usage => assistant::usage(&paths).await,
            };
        }
        Command::Export { out, no_history } => {
            return export::export(&paths, out, no_history).await;
        }
        Command::Import { path } => return export::import(&paths, &path).await,
        Command::Backup { action } => {
            return match action {
                BackupAction::Now => backup::now(&paths).await,
                BackupAction::List => backup::list(&paths),
            };
        }
        Command::Restore { path, yes } => return backup::restore(&paths, &path, yes).await,
        Command::Sync { action } => {
            return match action {
                SyncAction::Check => sync::check(&paths).await,
                SyncAction::Status => sync::status(&paths).await,
                SyncAction::Now { instance } => sync::now(&paths, instance).await,
                SyncAction::Outbox => sync::outbox(&paths).await,
            };
        }
        Command::Config { action } => {
            return match action {
                ConfigAction::Path => Ok(config::path(&paths)),
                ConfigAction::Check => Ok(config::check(&paths)),
                ConfigAction::Reload => config::reload(&paths).await,
            };
        }
        // Run by main, which execs it.
        Command::Mcp { .. } => unreachable!("todo mcp is run by main"),
        Command::Daemon {
            action: Some(DaemonAction::Stop),
            ..
        } => stop(&paths).await?,
        Command::Daemon { detach: true, .. } => {
            if spawn::is_listening(&paths.socket).await {
                println!("todo daemon: already running");
            } else {
                spawn::spawn_and_wait(&paths).await?;
                println!("todo daemon: started");
            }
        }
        Command::Remote { action } => {
            return match action {
                RemoteAction::Status => remote::status(&paths).await,
                RemoteAction::Pair { name, url, no_qr } => remote::pair(&paths, &name, url, !no_qr),
                RemoteAction::List => remote::list(&paths),
                RemoteAction::Revoke { device } => remote::revoke(&paths, &device),
            };
        }
        Command::Daemon { detach: false, .. } => {
            if todo_daemon::run(paths).await? == todo_daemon::Exit::AlreadyRunning {
                // A service (TODO_NO_SPAWN) that lost the lock to a daemon
                // started some other way has failed: it is not the one
                // running.
                if spawn::no_spawn() {
                    anyhow::bail!(
                        "another daemon holds the lock: stop it (`todo daemon stop`), then start this one"
                    );
                }
                println!("todo daemon: already running");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn stop(paths: &Paths) -> anyhow::Result<()> {
    if !spawn::is_listening(&paths.socket).await {
        println!("todo daemon: not running");
        return Ok(());
    }
    let pid = spawn::daemon_pid(paths)
        .with_context(|| format!("no daemon pid in {}", paths.lock.display()))?;
    let channel = todo_daemon::client::connect(&paths.socket).await?;
    let mut system = SystemServiceClient::new(channel);
    rpc::call(system.shutdown(ShutdownRequest { restart: false }))
        .await
        .map_err(|s| anyhow::anyhow!("shutdown failed: {}", s.message()))?;
    spawn::wait_for_exit(pid).await?;
    println!("todo daemon: stopped");
    Ok(())
}

/// Writes `line` to stdout. Unlike `println!` it does not panic when stdout
/// is closed (`todo add < notes | head -1`), which would drop the captures
/// still to come.
fn print_out(line: impl Display) {
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// [`print_out`] for stderr.
fn print_err(line: impl Display) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// The ULID's random tail, which tells captures from the same second apart.
fn short_id(id: &str) -> &str {
    id.get(id.len().saturating_sub(8)..).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_id_is_the_tail() {
        assert_eq!(short_id("01JB0000000000000000ABCDEF"), "00ABCDEF");
        assert_eq!(short_id("xy"), "xy");
    }

    #[test]
    fn cli_parses() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        let cli = Cli::try_parse_from(["todo", "a", "call", "-x", "dentist"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Add { text }) if text == ["call", "-x", "dentist"])
        );
        let cli = Cli::try_parse_from(["todo", "daemon", "stop"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Daemon {
                action: Some(DaemonAction::Stop),
                ..
            })
        ));
        // Without text, `add` reads stdin.
        let cli = Cli::try_parse_from(["todo", "add"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Add { text }) if text.is_empty()));
        let cli = Cli::try_parse_from(["todo", "undo"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Undo)));
        let cli = Cli::try_parse_from(["todo", "redo"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Redo)));
        assert!(Cli::try_parse_from(["todo", "undo", "extra"]).is_err());
        let cli = Cli::try_parse_from(["todo", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Status { short: false })
        ));
        let cli = Cli::try_parse_from(["todo", "status", "--short"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Status { short: true })));
        let cli = Cli::try_parse_from(["todo", "config", "check"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::Check
            })
        ));
        assert!(Cli::try_parse_from(["todo", "config"]).is_err());
        let cli =
            Cli::try_parse_from(["todo", "remote", "pair", "--name", "phone", "--no-qr"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Remote { action: RemoteAction::Pair { name, url: None, no_qr: true } }) if name == "phone"
        ));
        let cli = Cli::try_parse_from(["todo", "remote", "revoke", "phone"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Remote { action: RemoteAction::Revoke { device } }) if device == "phone"
        ));
        assert!(Cli::try_parse_from(["todo", "remote", "pair"]).is_err());
        assert!(Cli::try_parse_from(["todo", "remote"]).is_err());
        // Without a command, `todo` opens the TUI; --help still helps.
        assert!(Cli::try_parse_from(["todo"]).unwrap().command.is_none());
        let help = Cli::try_parse_from(["todo", "--help"]).err().unwrap();
        assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
    }
}
