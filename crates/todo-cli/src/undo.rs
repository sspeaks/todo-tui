//! `todo undo` and `todo redo` (design 3.10): revert the top entry of the
//! undo stack, or re-apply the entry undone last. The stack is the daemon's,
//! shared by the CLI and the TUI.

use std::process::ExitCode;

use todo_core::new_ulid;
use todo_daemon::paths::Paths;
use todo_proto::convert::item_field_path;
use todo_proto::v1::item_service_client::ItemServiceClient;
use todo_proto::v1::{OpSummary, RedoRequest, RelationKind, UndoRequest, UndoSkip};
use tonic::Status;
use tonic::transport::Channel;

use crate::rpc::{call, describe, is_transport, ready};
use crate::{print_out, short_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Undo,
    Redo,
}

/// How many times `todo undo` sends its request while no answer comes
/// back.
const ATTEMPTS: usize = 3;

/// Undoes, or redoes, one entry through the daemon, which is started or
/// restarted as for `todo add`. Prints `Undid: <entry>` (`Redid: …`) and a
/// line per change left alone, `Could not undo: …` when the entry was
/// dropped, or `Nothing to undo` (`… redo`). There is no spool: without a
/// daemon it fails.
///
/// The request is sent again, with the same request id, when its answer is
/// lost (the daemon may have acted on it): the daemon answers a repeat with
/// what the first one did, so it never undoes a second entry.
pub(crate) async fn undo(paths: &Paths, direction: Direction) -> anyhow::Result<ExitCode> {
    let request_id = new_ulid().to_string();
    let connect = async || ready(paths).await.map_err(|why| why.to_string());
    let send = async |channel: Channel| {
        // As for a capture: the reply to an undo the daemon applied must
        // always decode, however many Items it carries.
        let mut items = ItemServiceClient::new(channel).max_decoding_message_size(usize::MAX);
        let request_id = request_id.clone();
        match direction {
            Direction::Undo => {
                let request = UndoRequest {
                    request_id,
                    expected_op_id: None,
                };
                call(items.undo(request))
                    .await
                    .map(|r| Reply::new(r.entry, r.items.is_empty(), r.skipped))
            }
            Direction::Redo => {
                let request = RedoRequest {
                    request_id,
                    expected_op_id: None,
                };
                call(items.redo(request))
                    .await
                    .map(|r| Reply::new(r.entry, r.items.is_empty(), r.skipped))
            }
        }
    };
    let reply = until_answered(connect, send)
        .await
        .map_err(|failure| anyhow::anyhow!("{}", failure.message(direction)))?;
    for line in report(direction, &reply) {
        print_out(line);
    }
    Ok(ExitCode::SUCCESS)
}

/// What an Undo or Redo answered.
#[derive(Debug, Clone, PartialEq)]
struct Reply {
    /// The entry it acted on; `None` when the stack was empty.
    entry: Option<OpSummary>,
    /// Whether it restored nothing and so dropped the entry. The proto says
    /// so with an entry and no Items: `items` lists the Items it changed,
    /// and one that restores nothing drops its entry from both stacks.
    dropped: bool,
    skipped: Vec<UndoSkip>,
}

impl Reply {
    fn new(entry: Option<OpSummary>, no_items: bool, skipped: Vec<UndoSkip>) -> Reply {
        Reply {
            dropped: entry.is_some() && no_items,
            entry,
            skipped,
        }
    }
}

/// Why `todo undo` (`todo redo`) failed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Failure {
    /// The daemon could not be reached, started or spoken to, so nothing
    /// was sent: why, as [`crate::rpc::Why`] puts it.
    Unavailable(String),
    /// The daemon refused the request.
    Refused(String),
    /// It was sent, but no answer came back however often it was: the
    /// daemon may have acted on it.
    Unanswered(String),
}

impl Failure {
    /// What `todo: …` says.
    fn message(&self, direction: Direction) -> String {
        let (verb, again) = match direction {
            Direction::Undo => ("undo", "undoing"),
            Direction::Redo => ("redo", "redoing"),
        };
        match self {
            Failure::Unavailable(why) | Failure::Refused(why) => why.clone(),
            Failure::Unanswered(why) => format!(
                "no answer from the daemon ({why}); the {verb} may have been applied, \
                 so check before {again} again"
            ),
        }
    }
}

/// Connects with `connect` and sends the request with `send` until it is
/// answered or refused: after a failure on the way to or from the daemon
/// it reconnects and sends it again, [`ATTEMPTS`] times in all.
async fn until_answered<C, T>(
    mut connect: impl AsyncFnMut() -> Result<C, String>,
    mut send: impl AsyncFnMut(C) -> Result<T, Status>,
) -> Result<T, Failure> {
    let mut sent = 0;
    loop {
        let channel = match connect().await {
            Ok(channel) => channel,
            Err(why) if sent == 0 => return Err(Failure::Unavailable(why)),
            Err(why) => return Err(Failure::Unanswered(why)),
        };
        sent += 1;
        match send(channel).await {
            Ok(reply) => return Ok(reply),
            Err(status) if !is_transport(&status) => {
                return Err(Failure::Refused(status.message().to_owned()));
            }
            Err(status) if sent == ATTEMPTS => return Err(Failure::Unanswered(describe(&status))),
            Err(_) => {}
        }
    }
}

/// The lines `todo undo` (`todo redo`) prints for the daemon's reply.
fn report(direction: Direction, reply: &Reply) -> Vec<String> {
    let (done, failed, nothing) = match direction {
        Direction::Undo => ("Undid", "Could not undo", "Nothing to undo"),
        Direction::Redo => ("Redid", "Could not redo", "Nothing to redo"),
    };
    let Some(entry) = &reply.entry else {
        return vec![nothing.to_owned()];
    };
    let mut lines = vec![if reply.dropped {
        format!(
            "{failed}: {} (everything it changed has changed since)",
            entry.description
        )
    } else {
        format!("{done}: {}", entry.description)
    }];
    lines.extend(reply.skipped.iter().map(skip_line));
    lines
}

/// `  not restored: <field> of <short id> (<why>)`, where the field is
/// `status` for the status triple; for a relation step,
/// `  not restored: relation <from> <kind> <to> (<why>)`.
fn skip_line(skip: &UndoSkip) -> String {
    let what = match &skip.relation {
        Some(relation) => format!(
            "relation {} {} {}",
            short_id(&relation.from_id),
            relation_kind(relation.kind()),
            short_id(&relation.to_id)
        ),
        None => {
            let field =
                todo_core::ItemField::try_from(skip.field()).map_or("a field", item_field_path);
            format!("{field} of {}", short_id(&skip.item_id))
        }
    };
    if skip.message.is_empty() {
        format!("  not restored: {what}")
    } else {
        format!("  not restored: {what} ({})", skip.message)
    }
}

fn relation_kind(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Blocks => "blocks",
        RelationKind::Duplicates => "duplicates",
        RelationKind::RelatesTo => "relates to",
        RelationKind::Unspecified => "and",
    }
}

#[cfg(test)]
mod tests {
    use todo_proto::v1::{ItemField, Relation, UndoSkipReason};

    use super::*;

    const X: &str = "01JB0000000000000000ABCDEF";
    const Y: &str = "01JB000000000000000012345Z";

    fn entry(description: &str) -> OpSummary {
        OpSummary {
            op_id: "01JB0000000000000000000000".into(),
            description: description.into(),
            item_ids: vec![X.into()],
            ..Default::default()
        }
    }

    fn skip(field: ItemField, message: &str) -> UndoSkip {
        UndoSkip {
            item_id: X.into(),
            field: field.into(),
            reason: UndoSkipReason::ChangedSince.into(),
            message: message.into(),
            relation: None,
        }
    }

    /// A reply that changed Items, or none when `entry` is `None`.
    fn reply(entry: Option<OpSummary>, skipped: &[UndoSkip]) -> Reply {
        Reply::new(entry, false, skipped.to_vec())
    }

    /// Runs [`until_answered`] with `connects` and `answers` in turn, and
    /// returns its outcome, how many times it connected and what it sent.
    async fn attempts(
        connects: &[Result<(), &str>],
        answers: &[Result<&str, Status>],
    ) -> (Result<String, Failure>, usize, usize) {
        let (mut connected, mut sent) = (0, 0);
        let connect = async || {
            connected += 1;
            connects[connected - 1].map_err(str::to_owned)
        };
        let send = async |()| {
            sent += 1;
            answers[sent - 1].clone().map(str::to_owned)
        };
        let outcome = until_answered(connect, send).await;
        (outcome, connected, sent)
    }

    #[tokio::test]
    async fn a_lost_answer_is_asked_for_again() {
        let lost = || Err(Status::deadline_exceeded("no answer within 5s"));
        let dropped = || Err(Status::unavailable("connection reset"));
        // Answered the second or the third time, after reconnecting.
        assert_eq!(
            attempts(&[Ok(()); 2], &[lost(), Ok("undone")]).await,
            (Ok("undone".into()), 2, 2)
        );
        assert_eq!(
            attempts(&[Ok(()); 3], &[dropped(), lost(), Ok("undone")]).await,
            (Ok("undone".into()), 3, 3)
        );
        // Never answered: it may have been applied.
        assert_eq!(
            attempts(&[Ok(()); 3], &[lost(), lost(), lost()]).await,
            (Err(Failure::Unanswered("no answer within 5s".into())), 3, 3)
        );
        assert_eq!(
            attempts(&[Ok(()), Err("daemon unavailable (gone)")], &[dropped()]).await,
            (
                Err(Failure::Unanswered("daemon unavailable (gone)".into())),
                2,
                1
            )
        );
        // Never sent, or refused: no second try.
        assert_eq!(
            attempts(&[Err("daemon unavailable (no socket)")], &[]).await,
            (
                Err(Failure::Unavailable(
                    "daemon unavailable (no socket)".into()
                )),
                1,
                0
            )
        );
        let refused = Status::aborted("the undo stack changed");
        assert_eq!(
            attempts(&[Ok(())], &[Err(refused)]).await,
            (Err(Failure::Refused("the undo stack changed".into())), 1, 1)
        );
    }

    #[test]
    fn failures_say_whether_the_undo_may_have_been_applied() {
        let unavailable = Failure::Unavailable("daemon unavailable (no socket)".into());
        assert_eq!(
            unavailable.message(Direction::Undo),
            "daemon unavailable (no socket)"
        );
        let refused = Failure::Refused("the undo stack changed".into());
        assert_eq!(refused.message(Direction::Redo), "the undo stack changed");
        let lost = Failure::Unanswered("no answer within 5s".into());
        assert_eq!(
            lost.message(Direction::Undo),
            "no answer from the daemon (no answer within 5s); the undo may have been \
             applied, so check before undoing again"
        );
        assert_eq!(
            lost.message(Direction::Redo),
            "no answer from the daemon (no answer within 5s); the redo may have been \
             applied, so check before redoing again"
        );
    }

    #[test]
    fn an_empty_stack_has_nothing_to_do() {
        assert_eq!(
            report(Direction::Undo, &Reply::new(None, true, Vec::new())),
            ["Nothing to undo"]
        );
        assert_eq!(
            report(Direction::Redo, &reply(None, &[])),
            ["Nothing to redo"]
        );
    }

    #[test]
    fn the_entry_is_named_by_its_description() {
        let capture = entry("capture \"call dentist\"");
        assert_eq!(
            report(Direction::Undo, &reply(Some(capture), &[])),
            ["Undid: capture \"call dentist\""]
        );
        let done = entry("mark done (3 items)");
        assert_eq!(
            report(Direction::Redo, &reply(Some(done), &[])),
            ["Redid: mark done (3 items)"]
        );
    }

    #[test]
    fn an_entry_that_restored_nothing_could_not_be_undone() {
        let skipped = [skip(ItemField::Status, "the field has changed since")];
        let dropped = Reply::new(Some(entry("mark done \"x\"")), true, skipped.to_vec());
        assert!(dropped.dropped);
        let line = "  not restored: status of 00ABCDEF (the field has changed since)";
        assert_eq!(
            report(Direction::Undo, &dropped),
            [
                "Could not undo: mark done \"x\" (everything it changed has changed since)",
                line
            ]
        );
        assert_eq!(
            report(Direction::Redo, &dropped),
            [
                "Could not redo: mark done \"x\" (everything it changed has changed since)",
                line
            ]
        );
    }

    #[test]
    fn each_change_left_alone_gets_a_line() {
        let blocks = UndoSkip {
            field: ItemField::Unspecified.into(),
            message: "the relation has changed since".into(),
            relation: Some(Relation {
                from_id: X.into(),
                to_id: Y.into(),
                kind: RelationKind::Blocks.into(),
            }),
            ..skip(ItemField::Unspecified, "")
        };
        let relates = UndoSkip {
            relation: Some(Relation {
                kind: RelationKind::RelatesTo.into(),
                ..blocks.relation.clone().unwrap()
            }),
            ..blocks.clone()
        };
        let skipped = [
            skip(ItemField::Status, "the field has changed since"),
            skip(ItemField::Title, "the Item is deleted"),
            skip(ItemField::WaitingFollowUpOn, "the field has changed since"),
            skip(ItemField::ParentId, "the parent is deleted"),
            blocks,
            relates,
            // Neither a known field nor a relation, nor a message.
            skip(ItemField::Unspecified, ""),
        ];
        let lines = report(
            Direction::Undo,
            &reply(Some(entry("edit 2 items")), &skipped),
        );
        assert_eq!(
            lines,
            [
                "Undid: edit 2 items",
                "  not restored: status of 00ABCDEF (the field has changed since)",
                "  not restored: title of 00ABCDEF (the Item is deleted)",
                "  not restored: waiting.follow_up_on of 00ABCDEF (the field has changed since)",
                "  not restored: parent_id of 00ABCDEF (the parent is deleted)",
                "  not restored: relation 00ABCDEF blocks 0012345Z (the relation has changed since)",
                "  not restored: relation 00ABCDEF relates to 0012345Z (the relation has changed since)",
                "  not restored: a field of 00ABCDEF",
            ]
        );
    }
}
