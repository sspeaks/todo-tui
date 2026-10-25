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

use crate::rpc::{Why, call, describe, is_transport, ready};
use crate::{print_out, short_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Undo,
    Redo,
}

/// Undoes, or redoes, one entry through the daemon, which is started or
/// restarted as for `todo add`. Prints `Undid: <entry>` (`Redid: …`) and a
/// line per change left alone, or `Nothing to undo` (`… redo`). There is no
/// spool: without a daemon it fails.
pub(crate) async fn undo(paths: &Paths, direction: Direction) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    // As for a capture: the reply to an undo the daemon applied must always
    // decode, however many Items it carries.
    let mut items = ItemServiceClient::new(channel).max_decoding_message_size(usize::MAX);
    let request_id = new_ulid().to_string();
    let reply = match direction {
        Direction::Undo => {
            let request = UndoRequest {
                request_id,
                expected_op_id: None,
            };
            call(items.undo(request))
                .await
                .map(|r| (r.entry, r.skipped))
        }
        Direction::Redo => {
            let request = RedoRequest {
                request_id,
                expected_op_id: None,
            };
            call(items.redo(request))
                .await
                .map(|r| (r.entry, r.skipped))
        }
    };
    let (entry, skipped) = reply.map_err(|status| anyhow::anyhow!("{}", failure(&status)))?;
    for line in report(direction, entry.as_ref(), &skipped) {
        print_out(line);
    }
    Ok(ExitCode::SUCCESS)
}

/// What `todo: …` says when the call fails: that the daemon is unavailable
/// if the call did not reach it or its answer was lost, otherwise why the
/// daemon refused.
fn failure(status: &Status) -> String {
    if is_transport(status) {
        Why::Unavailable(describe(status)).to_string()
    } else {
        status.message().to_owned()
    }
}

/// The lines `todo undo` (`todo redo`) prints for the daemon's reply.
fn report(direction: Direction, entry: Option<&OpSummary>, skipped: &[UndoSkip]) -> Vec<String> {
    let (done, nothing) = match direction {
        Direction::Undo => ("Undid", "Nothing to undo"),
        Direction::Redo => ("Redid", "Nothing to redo"),
    };
    let Some(entry) = entry else {
        return vec![nothing.to_owned()];
    };
    let mut lines = vec![format!("{done}: {}", entry.description)];
    lines.extend(skipped.iter().map(skip_line));
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

    #[test]
    fn failures_say_whether_the_daemon_was_reached() {
        let lost = Status::unavailable("connection refused");
        assert_eq!(failure(&lost), "daemon unavailable (connection refused)");
        let changed = Status::aborted("the undo stack changed: expected entry A, but it is empty");
        assert_eq!(
            failure(&changed),
            "the undo stack changed: expected entry A, but it is empty"
        );
    }

    #[test]
    fn an_empty_stack_has_nothing_to_do() {
        assert_eq!(report(Direction::Undo, None, &[]), ["Nothing to undo"]);
        assert_eq!(report(Direction::Redo, None, &[]), ["Nothing to redo"]);
    }

    #[test]
    fn the_entry_is_named_by_its_description() {
        let capture = entry("capture \"call dentist\"");
        assert_eq!(
            report(Direction::Undo, Some(&capture), &[]),
            ["Undid: capture \"call dentist\""]
        );
        let done = entry("mark done (3 items)");
        assert_eq!(
            report(Direction::Redo, Some(&done), &[]),
            ["Redid: mark done (3 items)"]
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
        let lines = report(Direction::Undo, Some(&entry("edit 2 items")), &skipped);
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
