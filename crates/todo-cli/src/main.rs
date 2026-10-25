//! The `todo` binary.

mod add;
mod rpc;
mod spawn;
mod tui;
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
enum DaemonAction {
    /// Ask a running daemon to shut down and wait until it has exited.
    Stop,
}

fn main() -> ExitCode {
    let Some(command) = Cli::parse().command else {
        eprintln!("todo: {:#}", tui::launch());
        return ExitCode::FAILURE;
    };
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
        // Without a command, `todo` opens the TUI; --help still helps.
        assert!(Cli::try_parse_from(["todo"]).unwrap().command.is_none());
        let help = Cli::try_parse_from(["todo", "--help"]).err().unwrap();
        assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
    }
}
