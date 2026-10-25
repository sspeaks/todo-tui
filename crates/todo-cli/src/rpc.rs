//! Talking to the daemon (design 4.2): connect, spawning it if needed,
//! handshake, restart an outdated daemon, and RPCs with a timeout. Shared by
//! the commands that call the daemon.

use std::error::Error as _;
use std::time::Duration;

use anyhow::Context as _;
use todo_daemon::client::connect;
use todo_daemon::paths::Paths;
use todo_proto::API_MINOR;
use todo_proto::v1::system_service_client::SystemServiceClient;
use todo_proto::v1::{ClientKind, GetServerInfoRequest, GetServerInfoResponse, ShutdownRequest};
use tonic::transport::Channel;
use tonic::{Code, Response, Status};
use tonic_types::StatusExt as _;

use crate::spawn;

/// How long any one RPC may take, so a wedged daemon cannot hang a command.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the daemon cannot be used.
pub(crate) enum Why {
    /// The daemon could not be reached, started or spoken to.
    Unavailable(String),
    /// The daemon is older than this `todo` and was not restarted.
    Outdated(String),
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Why::Unavailable(reason) => write!(f, "daemon unavailable ({reason})"),
            Why::Outdated(detail) => f.write_str(detail),
        }
    }
}

/// A channel to a daemon that serves this build's API: connects (spawning
/// the daemon if needed), handshakes, and restarts an outdated daemon.
pub(crate) async fn ready(paths: &Paths) -> Result<Channel, Why> {
    let channel = spawn::connect_or_spawn(paths)
        .await
        .map_err(|e| Why::Unavailable(format!("{e:#}")))?;
    let info = handshake(&channel).await?;
    if info.api_minor >= API_MINOR {
        return Ok(channel);
    }
    upgrade(paths, channel, &info).await
}

async fn handshake(channel: &Channel) -> Result<GetServerInfoResponse, Why> {
    let request = GetServerInfoRequest {
        client_kind: ClientKind::Oneshot.into(),
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        required_api_minor: API_MINOR,
    };
    call(SystemServiceClient::new(channel.clone()).get_server_info(request))
        .await
        .map_err(|s| Why::Unavailable(format!("handshake failed: {}", describe(&s))))
}

/// Upgrade restart (design 4.2): asks the outdated daemon on `channel` to
/// shut down, waits for its process to exit, starts this binary's daemon in
/// its place and handshakes with it.
async fn upgrade(
    paths: &Paths,
    channel: Channel,
    old: &GetServerInfoResponse,
) -> Result<Channel, Why> {
    let outdated = format!(
        "the running daemon is outdated (api_minor {}, this todo needs {API_MINOR})",
        old.api_minor
    );
    let Some(pid) = spawn::daemon_pid(paths) else {
        return Err(Why::Outdated(format!(
            "{outdated} and cannot be restarted: no pid in {}",
            paths.lock.display()
        )));
    };
    // The client holds the last handle on this connection, so dropping it
    // closes the connection and the old daemon has nothing left to drain.
    let shutdown = {
        let mut system = SystemServiceClient::new(channel);
        call(system.shutdown(ShutdownRequest { restart: true })).await
    };
    match after_shutdown(shutdown.map(drop)) {
        AfterShutdown::Respawn => {}
        AfterShutdown::Refused => {
            return Err(Why::Outdated(format!(
                "{outdated} and must be restarted after closing the other todo sessions"
            )));
        }
        AfterShutdown::Failed(message) => {
            return Err(Why::Outdated(format!(
                "{outdated} and did not shut down: {message}"
            )));
        }
    }
    let restart = async {
        spawn::wait_for_exit(pid).await?;
        spawn::spawn_and_wait(paths).await?;
        connect(&paths.socket)
            .await
            .with_context(|| format!("connecting to {}", paths.socket.display()))
    };
    let channel = restart
        .await
        .map_err(|e| Why::Outdated(format!("{outdated} and restarting it failed: {e:#}")))?;
    let info = handshake(&channel).await?;
    if info.api_minor < API_MINOR {
        return Err(Why::Outdated(format!(
            "the daemon is still outdated after a restart (api_minor {}, this todo needs {API_MINOR})",
            info.api_minor
        )));
    }
    Ok(channel)
}

/// What the outcome of `Shutdown { restart: true }` means for an upgrade.
#[derive(Debug, PartialEq, Eq)]
enum AfterShutdown {
    /// The daemon is exiting, or is already gone: start the new one.
    Respawn,
    /// Other interactive clients are connected (ErrorInfo
    /// INTERACTIVE_CLIENTS_CONNECTED), so it keeps running.
    Refused,
    /// It refused for another reason.
    Failed(String),
}

fn after_shutdown(result: Result<(), Status>) -> AfterShutdown {
    match result {
        Ok(()) => AfterShutdown::Respawn,
        Err(s) if is_transport(&s) => AfterShutdown::Respawn,
        Err(s)
            if s.code() == Code::FailedPrecondition
                && s.get_details_error_info()
                    .is_some_and(|info| info.reason == "INTERACTIVE_CLIENTS_CONNECTED") =>
        {
            AfterShutdown::Refused
        }
        Err(s) => AfterShutdown::Failed(s.message().to_owned()),
    }
}

/// Awaits an RPC for at most [`RPC_TIMEOUT`]; running out of time is
/// DEADLINE_EXCEEDED.
pub(crate) async fn call<T>(
    rpc: impl Future<Output = Result<Response<T>, Status>>,
) -> Result<T, Status> {
    match tokio::time::timeout(RPC_TIMEOUT, rpc).await {
        Ok(result) => result.map(Response::into_inner),
        Err(_) => Err(Status::deadline_exceeded(format!(
            "no answer within {RPC_TIMEOUT:?}"
        ))),
    }
}

/// Whether `status` means the call failed on the way to or from the daemon,
/// so the daemon may not have acted on it, rather than that the daemon
/// refused it. Statuses that tonic makes from a connection error carry that
/// error as their source; statuses the daemon sends never do.
pub(crate) fn is_transport(status: &Status) -> bool {
    match status.code() {
        Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled => true,
        Code::Unknown | Code::Internal => status.source().is_some(),
        _ => false,
    }
}

/// A status's message followed by its source chain, for transport errors
/// whose message alone ("transport error") says little.
pub(crate) fn describe(status: &Status) -> String {
    let mut text = status.message().to_owned();
    let mut source = status.source();
    while let Some(e) = source {
        let cause = e.to_string();
        if !text.contains(&cause) {
            text = format!("{text}: {cause}");
        }
        source = e.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tokio::net::UnixListener;
    use tonic_types::ErrorDetails;

    use super::*;

    #[test]
    fn transport_failures_are_told_from_rejections() {
        for code in [Code::Unavailable, Code::DeadlineExceeded, Code::Cancelled] {
            assert!(is_transport(&Status::new(code, "x")), "{code:?}");
        }
        for code in [
            Code::InvalidArgument,
            Code::FailedPrecondition,
            Code::NotFound,
            Code::OutOfRange,
            Code::ResourceExhausted,
            Code::Unknown,
            Code::Internal,
        ] {
            assert!(!is_transport(&Status::new(code, "x")), "{code:?}");
        }
        let broken = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe");
        let status = Status::from_error(Box::new(broken));
        assert_eq!(status.code(), Code::Unknown);
        assert!(is_transport(&status));
    }

    #[tokio::test]
    async fn a_dropped_connection_is_a_transport_failure() {
        let dir = tempfile::tempdir().unwrap();
        let private = std::os::unix::fs::PermissionsExt::from_mode(0o700);
        std::fs::set_permissions(dir.path(), private).unwrap();
        let socket = dir.path().join("s.sock");
        // Accepts connections and closes them unanswered.
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        let channel = connect(&socket).await.unwrap();
        let mut system = SystemServiceClient::new(channel);
        let rpc = system.get_server_info(GetServerInfoRequest::default());
        let status = call(rpc).await.unwrap_err();
        // tonic reports UNKNOWN "transport error", with the cause as source.
        assert!(is_transport(&status), "{status:?}");
        assert!(
            describe(&status).len() > status.message().len(),
            "{status:?}"
        );
    }

    #[test]
    fn upgrade_restarts_unless_refused() {
        assert_eq!(after_shutdown(Ok(())), AfterShutdown::Respawn);
        // Already gone counts as shut down.
        assert_eq!(
            after_shutdown(Err(Status::unavailable("connection refused"))),
            AfterShutdown::Respawn
        );
        let refused = Status::with_error_details(
            Code::FailedPrecondition,
            "restart required; close the other 1 interactive session(s)",
            ErrorDetails::with_error_info(
                "INTERACTIVE_CLIENTS_CONNECTED",
                "todo-tui",
                HashMap::new(),
            ),
        );
        assert_eq!(after_shutdown(Err(refused)), AfterShutdown::Refused);
        assert_eq!(
            after_shutdown(Err(Status::failed_precondition("no detail"))),
            AfterShutdown::Failed("no detail".into())
        );
        assert_eq!(
            after_shutdown(Err(Status::unimplemented("no shutdown"))),
            AfterShutdown::Failed("no shutdown".into())
        );
    }
}
