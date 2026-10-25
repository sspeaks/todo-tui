//! `todo add` (design 2.1): capture through the daemon, restarting an
//! outdated one (4.2), and spool whatever it cannot take.

use std::io::{BufRead as _, IsTerminal as _, Read as _};
use std::process::ExitCode;

use anyhow::Context as _;
use todo_core::{MAX_CAPTURE_BYTES, new_ulid};
use todo_daemon::paths::Paths;
use todo_daemon::spool::{self, SpoolEntry};
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::{
    CaptureDraft, CaptureItemRequest, CaptureItemResponse, CreatedVia, Priority,
    Status as ItemStatus, person_ref,
};
use tonic::transport::Channel;
use ulid::Ulid;

use crate::rpc::{Why, call, describe, is_transport, ready};
use crate::{print_err, print_out, short_id};

/// At most this many characters of a capture's text in an error message.
const EXCERPT_CHARS: usize = 60;

/// One capture. Its id is the CaptureItem request id and spool id, and the
/// spool entry's id if the capture is spooled, so the daemon skips a spooled
/// capture whose request did reach it, even after the request id's 24h
/// window.
struct Capture {
    id: Ulid,
    text: String,
}

/// Captures `words` joined with spaces (an error if that is blank) or,
/// without words, each non-empty line of stdin: as it is entered when stdin
/// is a terminal, otherwise once all of it is read. Prints
/// `<short id>  <title>[  <fields>]` per capture and `spooled  <text>` per
/// spooled one; exits non-zero if any capture was rejected, was over
/// [`MAX_CAPTURE_BYTES`], or could not be spooled.
pub(crate) async fn add(paths: &Paths, words: Vec<String>) -> anyhow::Result<ExitCode> {
    let mut sink = Sink::new(paths);
    if !words.is_empty() {
        let text = words.join(" ");
        // The daemon, or the spool import, would reject it.
        anyhow::ensure!(!text.trim().is_empty(), "nothing to capture");
        sink.capture(text).await;
    } else if std::io::stdin().is_terminal() {
        print_err("todo add: each line is captured when you press Enter; end with Ctrl-D");
        let mut line = Vec::new();
        for number in 1.. {
            line.clear();
            // Blocks this thread only: the runtime's workers keep the
            // connection to the daemon going meanwhile.
            let read = std::io::stdin().lock().read_until(b'\n', &mut line);
            if read.context("reading stdin")? == 0 {
                break;
            }
            if let Some((text, utf8)) = line_text(&line) {
                if !utf8 {
                    warn_not_utf8(number);
                }
                sink.capture(text).await;
            }
            // Only end of file ends a line without a newline: Ctrl-D after
            // text, then Ctrl-D again. A terminal reports it once, so a
            // further read would wait for a third.
            if !line.ends_with(b"\n") {
                break;
            }
        }
    } else {
        let mut input = Vec::new();
        std::io::stdin()
            .read_to_end(&mut input)
            .context("reading stdin")?;
        let (texts, not_utf8) = capture_lines(&input);
        for number in not_utf8 {
            warn_not_utf8(number);
        }
        for text in texts {
            sink.capture(text).await;
        }
    }
    Ok(if sink.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Where captures go: the daemon, connected on the first capture, until it
/// becomes unavailable; the spool from then on.
struct Sink<'a> {
    paths: &'a Paths,
    to: To,
    /// False once a capture was rejected or lost.
    ok: bool,
}

enum To {
    /// Nothing was sent yet.
    Unconnected,
    Daemon(ItemServiceClient<Channel>),
    Spool {
        why: Why,
        told: Told,
    },
}

/// What stderr has said about spooling so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Told {
    Nothing,
    Why,
    /// Why, and where captures are spooled.
    Where,
}

impl<'a> Sink<'a> {
    fn new(paths: &'a Paths) -> Self {
        Sink {
            paths,
            to: To::Unconnected,
            ok: true,
        }
    }

    /// Captures `text`, or spools it, and prints the outcome. Text over
    /// [`MAX_CAPTURE_BYTES`] is neither sent nor spooled: the daemon and the
    /// spool import would refuse it.
    async fn capture(&mut self, text: String) {
        if text.len() > MAX_CAPTURE_BYTES {
            print_err(format_args!(
                "todo: error: NOT CAPTURED: {}: longer than {} KiB",
                excerpt(&text),
                MAX_CAPTURE_BYTES / 1024
            ));
            self.ok = false;
            return;
        }
        let capture = Capture {
            id: new_ulid(),
            text,
        };
        if let To::Unconnected = self.to {
            self.to = match ready(self.paths).await {
                // The daemon bounds what it accepts. The reply to a capture
                // it stored must always decode, or the capture would look
                // lost.
                Ok(channel) => To::Daemon(
                    ItemServiceClient::new(channel).max_decoding_message_size(usize::MAX),
                ),
                Err(why) => To::Spool {
                    why,
                    told: Told::Nothing,
                },
            };
        }
        if let To::Daemon(items) = &mut self.to {
            let request = CaptureItemRequest {
                request_id: capture.id.to_string(),
                text: capture.text.clone(),
                created_via: CreatedVia::Cli.into(),
                spool_id: Some(capture.id.to_string()),
                ..Default::default()
            };
            match call(items.capture_item(request)).await {
                Ok(response) => {
                    self.ok &= print_captured(response);
                    return;
                }
                // This capture and the ones after it go to the spool.
                Err(status) if is_transport(&status) => {
                    self.to = To::Spool {
                        why: Why::Unavailable(describe(&status)),
                        told: Told::Nothing,
                    };
                }
                Err(status) => {
                    print_err(format_args!(
                        "todo: error: NOT CAPTURED: {}: {}",
                        excerpt(&capture.text),
                        status.message()
                    ));
                    self.ok = false;
                    return;
                }
            }
        }
        if let To::Spool { why, told } = &mut self.to {
            self.ok &= spool_one(self.paths, &capture, why, told);
        }
    }
}

/// The captures in stdin: each line trimmed, blank ones skipped, and the
/// numbers (from 1) of the lines that are not valid UTF-8, which are
/// decoded lossily.
fn capture_lines(input: &[u8]) -> (Vec<String>, Vec<usize>) {
    let mut texts = Vec::new();
    let mut not_utf8 = Vec::new();
    for (i, line) in input.split(|&b| b == b'\n').enumerate() {
        let Some((text, utf8)) = line_text(line) else {
            continue;
        };
        if !utf8 {
            not_utf8.push(i + 1);
        }
        texts.push(text);
    }
    (texts, not_utf8)
}

/// One line of stdin as a capture, trimmed (`None` if blank), and whether
/// it was valid UTF-8; if not, it is decoded lossily.
fn line_text(line: &[u8]) -> Option<(String, bool)> {
    let decoded = String::from_utf8_lossy(line);
    let text = decoded.trim();
    if text.is_empty() {
        return None;
    }
    Some((text.to_owned(), std::str::from_utf8(line).is_ok()))
}

fn warn_not_utf8(number: usize) {
    print_err(format_args!(
        "todo: warning: line {number} is not valid UTF-8; \
         its invalid bytes were replaced with U+FFFD"
    ));
}

/// `text` quoted for a message, cut to [`EXCERPT_CHARS`] characters and
/// marked with an ellipsis.
fn excerpt(text: &str) -> String {
    match text.char_indices().nth(EXCERPT_CHARS) {
        Some((end, _)) => format!("{:?}", format!("{}…", &text[..end])),
        None => format!("{text:?}"),
    }
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

/// Appends `capture` to the spool and prints `spooled  <text>`, or a
/// NOT CAPTURED error if it cannot. Unless `told` says it already did,
/// stderr also says why captures are spooled, and where once one is.
/// Returns false if the capture was lost.
fn spool_one(paths: &Paths, capture: &Capture, why: &Why, told: &mut Told) -> bool {
    let path = paths.spool();
    match spool::append(paths, &SpoolEntry::now(capture.id, capture.text.as_str())) {
        Ok(()) => {
            print_out(format_args!("spooled  {}", capture.text));
            if *told < Told::Where {
                print_err(format_args!(
                    "todo: {why}; spooled to {}; it will be imported once the daemon is running again",
                    path.display()
                ));
                *told = Told::Where;
            }
            true
        }
        Err(e) => {
            if *told == Told::Nothing {
                print_err(format_args!("todo: {why}"));
                *told = Told::Why;
            }
            print_err(format_args!(
                "todo: error: NOT CAPTURED: {}: cannot write {}: {e}",
                excerpt(&capture.text),
                path.display()
            ));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use todo_proto::google::r#type::Date;
    use todo_proto::v1::PersonRef;

    use super::*;

    #[test]
    fn stdin_lines_are_trimmed_and_blank_ones_skipped() {
        let input = b"  call dentist +2 \n\n \t \r\nbuy milk\r\nlast without newline";
        let (texts, not_utf8) = capture_lines(input);
        assert_eq!(
            texts,
            ["call dentist +2", "buy milk", "last without newline"]
        );
        assert!(not_utf8.is_empty());
        assert_eq!(capture_lines(b"\n  \n"), (vec![], vec![]));
        let (texts, not_utf8) = capture_lines(b"ok\n\xff\n \xe9t\xe9 \n\n");
        assert_eq!(texts, ["ok", "\u{fffd}", "\u{fffd}t\u{fffd}"]);
        assert_eq!(not_utf8, [2, 3]);
    }

    #[test]
    fn excerpts_are_cut_on_a_char_boundary() {
        assert_eq!(excerpt(r#"call "dentist""#), r#""call \"dentist\"""#);
        let exact = "é".repeat(EXCERPT_CHARS);
        assert_eq!(excerpt(&exact), format!("\"{exact}\""));
        let long = excerpt(&"é".repeat(10_000));
        assert_eq!(long, format!("\"{}…\"", "é".repeat(EXCERPT_CHARS)));
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
}
