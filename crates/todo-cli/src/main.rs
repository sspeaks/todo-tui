//! The `todo` binary.

mod add;
mod backup;
mod config;
mod launch;
mod rpc;
mod spawn;
mod status;
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
    /// Serve the MCP tools on stdio, for Claude Code: `claude mcp add todo -- todo mcp`.
    Mcp {
        /// Offer only the tools that read: list, get, views and counts.
        #[arg(long)]
        read_only: bool,
    },
    /// The config file: where it is, what is wrong with it, and reloading it.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
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
}

#[derive(Subcommand)]
enum BackupAction {
    /// Take a backup now, also when automatic backups are off.
    Now,
    /// List the backups, oldest first.
    List,
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
    match command {
        Command::Add { text } => return add::add(&paths, text).await,
        Command::Undo => return undo::undo(&paths, undo::Direction::Undo).await,
        Command::Redo => return undo::undo(&paths, undo::Direction::Redo).await,
        // `--short` is run by main.
        Command::Status { .. } => return status::status(&paths).await,
        Command::Backup { action } => {
            return match action {
                BackupAction::Now => backup::now(&paths).await,
                BackupAction::List => backup::list(&paths),
            };
        }
        Command::Restore { path, yes } => return backup::restore(&paths, &path, yes).await,
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
        Command::Daemon { detach: false, .. } => {
            if todo_daemon::run(paths).await? == todo_daemon::Exit::AlreadyRunning {
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
        // Without a command, `todo` opens the TUI; --help still helps.
        assert!(Cli::try_parse_from(["todo"]).unwrap().command.is_none());
        let help = Cli::try_parse_from(["todo", "--help"]).err().unwrap();
        assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
    }
}
