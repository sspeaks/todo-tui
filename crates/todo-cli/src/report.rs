//! `todo l48` (design 6.6): the L48 / N48 report, to paste into a scrum.

use std::io::Write as _;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use todo_daemon::paths::Paths;
use todo_proto::v1::GetActivityReportRequest;
use todo_proto::v1::item_service_client::ItemServiceClient;

use crate::rpc::{call, ready};
use crate::status::failed;
use crate::{print_err, print_out};

/// `todo l48 [--since DATE] [--copy]`: prints the report; `--copy` puts it
/// on the clipboard as well. `since` counts back from today (`thu`,
/// `-3d`, `oct29`, `2026-10-29`), read by the daemon.
pub(crate) async fn l48(
    paths: &Paths,
    since: Option<String>,
    copy: bool,
) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let mut items = ItemServiceClient::new(channel);
    // since is the daemon's to read: it counts back from its today.
    let report = call(items.get_activity_report(GetActivityReportRequest {
        reference_time: None,
        since: since.unwrap_or_default(),
    }))
    .await
    .map_err(failed)?;
    print_out(report.text.trim_end());
    if copy {
        match copy_to_clipboard(&report.text) {
            Ok(tool) => print_err(format!("copied with {tool}")),
            Err(why) => {
                print_err(format!("todo: could not copy: {why}"));
                return Ok(ExitCode::FAILURE);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// How long one clipboard command may take.
const CLIPBOARD_WAIT: Duration = Duration::from_secs(2);

/// The clipboard commands tried in order: macOS, then Wayland, then X.
const CLIPBOARDS: [(&str, &[&str]); 3] = [
    ("pbcopy", &[]),
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
];

/// Writes `text` to the first clipboard command that takes it; the name of
/// that command, or why none did.
pub(crate) fn copy_to_clipboard(text: &str) -> Result<&'static str, String> {
    for (tool, args) in CLIPBOARDS {
        let Ok(mut child) = Command::new(tool)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        // xclip forks to serve the selection, but a command that hangs
        // must not hang `todo l48`.
        let deadline = Instant::now() + CLIPBOARD_WAIT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        if wrote && status.is_some_and(|s| s.success()) {
            return Ok(tool);
        }
    }
    Err("no clipboard command worked (pbcopy, wl-copy, xclip)".to_owned())
}
