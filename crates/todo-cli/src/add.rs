//! `todo add` (design 2.1): capture through the daemon, restarting an
//! outdated one (4.2), and spool whatever it cannot take.

use std::error::Error as _;
use std::fmt::Display;
use std::io::{IsTerminal as _, Read as _, Write as _};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use todo_core::new_ulid;
use todo_daemon::client::connect;
use todo_daemon::paths::Paths;
use todo_daemon::spool::{self, SpoolEntry};
use todo_proto::API_MINOR;
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::system_service_client::SystemServiceClient;
use todo_proto::v1::{
    CaptureDraft, CaptureItemRequest, CaptureItemResponse, ClientKind, CreatedVia,
    GetServerInfoRequest, GetServerInfoResponse, Priority, ShutdownRequest, Status as ItemStatus,
    person_ref,
};
use tonic::transport::Channel;
use tonic::{Code, Response, Status};
use tonic_types::StatusExt as _;
use ulid::Ulid;

use crate::spawn;

/// How long any one RPC may take, so a wedged daemon cannot hang a capture.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// One capture. Its id is the CaptureItem request id and, if the capture is
/// spooled, the spool id, so the daemon skips a spooled capture whose
/// request did reach it.
struct Capture {
    id: Ulid,
    text: String,
}

/// Why captures go to the spool instead of the daemon.
enum Why {
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

/// Captures `words` joined with spaces (an error if that is blank) or,
/// without words, each non-empty line of stdin. Prints
/// `<short id>  <title>[  <fields>]` per capture and `spooled  <text>` per
/// spooled one; exits non-zero if any capture was rejected or could not be
/// spooled.
pub(crate) async fn add(paths: &Paths, words: Vec<String>) -> anyhow::Result<ExitCode> {
    let texts = if words.is_empty() {
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            print_err("todo add: reading captures from stdin, one per line; end with Ctrl-D");
        }
        let mut input = Vec::new();
        stdin.read_to_end(&mut input).context("reading stdin")?;
        capture_lines(&String::from_utf8_lossy(&input))
    } else {
        let text = words.join(" ");
        // The daemon, or the spool import, would reject it.
        anyhow::ensure!(!text.trim().is_empty(), "nothing to capture");
        vec![text]
    };
    let captures: Vec<Capture> = texts
        .into_iter()
        .map(|text| Capture {
            id: new_ulid(),
            text,
        })
        .collect();
    if captures.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let mut ok = true;
    match ready(paths).await {
        Err(why) => ok = spool_all(paths, &captures, &why),
        Ok(channel) => {
            let mut items = ItemServiceClient::new(channel);
            for (i, capture) in captures.iter().enumerate() {
                let request = CaptureItemRequest {
                    request_id: capture.id.to_string(),
                    text: capture.text.clone(),
                    created_via: CreatedVia::Cli.into(),
                    ..Default::default()
                };
                match call(items.capture_item(request)).await {
                    Ok(response) => ok &= print_captured(response),
                    Err(status) if is_transport(&status) => {
                        let why = Why::Unavailable(describe(&status));
                        ok &= spool_all(paths, &captures[i..], &why);
                        break;
                    }
                    Err(status) => {
                        print_err(format_args!("todo: {}", status.message()));
                        ok = false;
                    }
                }
            }
        }
    }
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The captures in stdin text: each line trimmed, blank ones skipped.
fn capture_lines(input: &str) -> Vec<String> {
    input
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A channel to a daemon that serves this build's API: connects (spawning
/// the daemon if needed), handshakes, and restarts an outdated daemon.
async fn ready(paths: &Paths) -> Result<Channel, Why> {
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
async fn call<T>(rpc: impl Future<Output = Result<Response<T>, Status>>) -> Result<T, Status> {
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
fn is_transport(status: &Status) -> bool {
    match status.code() {
        Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled => true,
        Code::Unknown | Code::Internal => status.source().is_some(),
        _ => false,
    }
}

/// A status's message followed by its source chain, for transport errors
/// whose message alone ("transport error") says little.
fn describe(status: &Status) -> String {
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

/// Prints a captured Item and the parse warnings; false if the daemon sent
/// no Item.
fn print_captured(response: CaptureItemResponse) -> bool {
    let Some(item) = response.item else {
        print_err("todo: daemon returned no item");
        return false;
    };
    let draft = response.draft.unwrap_or_default();
    print_out(captured_line(&item.id, &item.title, &draft));
    for warning in &draft.warnings {
        print_err(format_args!("todo: warning: {warning}"));
    }
    true
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

/// `<short id>  <title>`, then two spaces and the parsed fields in capture
/// syntax when anything was parsed.
fn captured_line(id: &str, title: &str, draft: &CaptureDraft) -> String {
    let fields = parsed_fields(draft);
    if fields.is_empty() {
        format!("{}  {title}", short_id(id))
    } else {
        format!("{}  {title}  {}", short_id(id), fields.join(" "))
    }
}

/// The ULID's random tail, which tells captures from the same second apart.
fn short_id(id: &str) -> &str {
    id.get(id.len().saturating_sub(8)..).unwrap_or(id)
}

/// The fields `draft` sets, as tokens: priority, due date, labels, waiting
/// and a status other than inbox.
fn parsed_fields(draft: &CaptureDraft) -> Vec<String> {
    let mut fields = Vec::new();
    let priority = match draft.priority() {
        Priority::Highest => Some(1),
        Priority::High => Some(2),
        Priority::Medium => Some(3),
        Priority::Low => Some(4),
        Priority::Lowest => Some(5),
        Priority::None | Priority::Unspecified => None,
    };
    if let Some(n) = priority {
        fields.push(format!("+{n}"));
    }
    if let Some(d) = &draft.due_date {
        fields.push(format!("^{:04}-{:02}-{:02}", d.year, d.month, d.day));
    }
    fields.extend(draft.labels.iter().map(|label| format!("#{label}")));
    match draft.waiting_on.as_ref().and_then(|p| p.r#ref.as_ref()) {
        Some(person_ref::Ref::FreeText(name)) if name.contains(char::is_whitespace) => {
            fields.push(format!("~\"{name}\""));
        }
        Some(person_ref::Ref::FreeText(name) | person_ref::Ref::PersonId(name)) => {
            fields.push(format!("~{name}"));
        }
        None => {}
    }
    match draft.status() {
        ItemStatus::Next => fields.push(">next".to_owned()),
        ItemStatus::Someday => fields.push(">someday".to_owned()),
        _ => {}
    }
    fields
}

/// Appends `captures` to the spool, printing `spooled  <text>` for each,
/// then says on stderr why and where, and which captures were lost because
/// they could not be appended. Returns false if any was.
fn spool_all(paths: &Paths, captures: &[Capture], why: &Why) -> bool {
    let path = paths.spool();
    let mut lost = Vec::new();
    for capture in captures {
        match spool::append(paths, &SpoolEntry::now(capture.id, capture.text.as_str())) {
            Ok(()) => print_out(format_args!("spooled  {}", capture.text)),
            Err(e) => lost.push((capture, e)),
        }
    }
    if lost.len() < captures.len() {
        print_err(format_args!(
            "todo: {why}; spooled to {}; it will be imported when the daemon next starts",
            path.display()
        ));
    } else {
        print_err(format_args!("todo: {why}"));
    }
    for (capture, e) in &lost {
        print_err(format_args!(
            "todo: error: NOT CAPTURED: {:?}: cannot write {}: {e}",
            capture.text,
            path.display()
        ));
    }
    lost.is_empty()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use todo_proto::google::r#type::Date;
    use todo_proto::v1::PersonRef;
    use tokio::net::UnixListener;
    use tonic_types::ErrorDetails;

    use super::*;

    #[test]
    fn short_id_is_the_tail() {
        assert_eq!(short_id("01JB0000000000000000ABCDEF"), "00ABCDEF");
        assert_eq!(short_id("xy"), "xy");
    }

    #[test]
    fn stdin_lines_are_trimmed_and_blank_ones_skipped() {
        let input = "  call dentist +2 \n\n \t \r\nbuy milk\r\nlast without newline";
        assert_eq!(
            capture_lines(input),
            ["call dentist +2", "buy milk", "last without newline"]
        );
        assert!(capture_lines("\n  \n").is_empty());
    }

    fn free_text(name: &str) -> Option<PersonRef> {
        Some(PersonRef {
            r#ref: Some(person_ref::Ref::FreeText(name.into())),
        })
    }

    #[test]
    fn captured_lines_show_parsed_fields_in_capture_syntax() {
        let id = "01JB000000000000007GQ4ZK2M";
        let plain = CaptureDraft {
            title: "call dentist".into(),
            priority: Priority::None.into(),
            status: ItemStatus::Inbox.into(),
            ..Default::default()
        };
        assert_eq!(
            captured_line(id, "call dentist", &plain),
            "7GQ4ZK2M  call dentist"
        );
        assert_eq!(
            captured_line(id, "call dentist", &CaptureDraft::default()),
            "7GQ4ZK2M  call dentist"
        );

        let full = CaptureDraft {
            priority: Priority::High.into(),
            due_date: Some(Date {
                year: 2026,
                month: 10,
                day: 4,
            }),
            labels: vec!["home".into(), "errand".into()],
            waiting_on: free_text("dana"),
            status: ItemStatus::Someday.into(),
            ..plain.clone()
        };
        assert_eq!(
            captured_line(id, "call dentist", &full),
            "7GQ4ZK2M  call dentist  +2 ^2026-10-04 #home #errand ~dana >someday"
        );

        for (priority, token) in [
            (Priority::Highest, "+1"),
            (Priority::Medium, "+3"),
            (Priority::Low, "+4"),
            (Priority::Lowest, "+5"),
        ] {
            let draft = CaptureDraft {
                priority: priority.into(),
                ..plain.clone()
            };
            assert_eq!(parsed_fields(&draft), [token]);
        }
        let draft = CaptureDraft {
            waiting_on: free_text("the vendor"),
            status: ItemStatus::Next.into(),
            ..plain.clone()
        };
        assert_eq!(parsed_fields(&draft), ["~\"the vendor\"", ">next"]);
    }

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
