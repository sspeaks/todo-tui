//! The `todo` binary.

mod add;
mod spawn;

use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use todo_daemon::paths::{Env, Paths};
use todo_proto::v1::ShutdownRequest;
use todo_proto::v1::system_service_client::SystemServiceClient;

#[derive(Parser)]
#[command(name = "todo", version, about = "A keyboard-driven todo manager")]
#[command(arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("todo: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(dispatch(cli.command)) {
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
    SystemServiceClient::new(channel)
        .shutdown(ShutdownRequest { restart: false })
        .await
        .map_err(|s| anyhow::anyhow!("shutdown failed: {}", s.message()))?;
    spawn::wait_for_exit(pid).await?;
    println!("todo daemon: stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        let cli = Cli::try_parse_from(["todo", "a", "call", "-x", "dentist"]).unwrap();
        assert!(matches!(cli.command, Command::Add { text } if text == ["call", "-x", "dentist"]));
        let cli = Cli::try_parse_from(["todo", "daemon", "stop"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Daemon {
                action: Some(DaemonAction::Stop),
                ..
            }
        ));
        // Without text, `add` reads stdin.
        let cli = Cli::try_parse_from(["todo", "add"]).unwrap();
        assert!(matches!(cli.command, Command::Add { text } if text.is_empty()));
    }
}
