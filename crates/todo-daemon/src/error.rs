//! Mapping failures to gRPC statuses with `google.rpc` details (design 4.3 rule 5).

use std::collections::HashMap;

use todo_core::{ItemField, ItemId, MutationError};
use todo_proto::convert::{ConvertError, item_field_path};
use todo_proto::v1;
use todo_store::StoreError;
use tonic::{Code, Status};
use tonic_types::{ErrorDetails, StatusExt};

pub(crate) const DOMAIN: &str = "todo-tui";

/// Descriptions may echo client input (an unknown mask path, a bad ULID);
/// longer ones are cut so a huge value is not reflected into the trailers.
const MAX_DESCRIPTION_CHARS: usize = 200;

/// INVALID_ARGUMENT with one BadRequest violation; the message is
/// `"{field}: {description}"`.
pub(crate) fn bad_request(field: &str, description: impl Into<String>) -> Status {
    let description = cap(description.into());
    Status::with_error_details(
        Code::InvalidArgument,
        format!("{field}: {description}"),
        ErrorDetails::with_bad_request_violation(field, description),
    )
}

/// `s` cut to [`MAX_DESCRIPTION_CHARS`] characters, marked with an ellipsis.
fn cap(s: String) -> String {
    match s.char_indices().nth(MAX_DESCRIPTION_CHARS) {
        Some((end, _)) => format!("{}…", &s[..end]),
        None => s,
    }
}

pub(crate) fn error_info(
    code: Code,
    reason: &str,
    message: impl Into<String>,
    metadata: impl IntoIterator<Item = (&'static str, String)>,
) -> Status {
    let metadata: HashMap<String, String> = metadata
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
    Status::with_error_details(
        code,
        message,
        ErrorDetails::with_error_info(reason, DOMAIN, metadata),
    )
}

pub(crate) fn convert(e: ConvertError) -> Status {
    // The Display form starts with the path, which bad_request adds back.
    let message = e.to_string();
    let description = message
        .strip_prefix(e.path())
        .and_then(|rest| rest.strip_prefix(": "))
        .unwrap_or(&message);
    bad_request(e.path(), description)
}

pub(crate) fn store(e: StoreError) -> Status {
    match e {
        StoreError::NotFound(id) => error_info(
            Code::NotFound,
            "ITEM_NOT_FOUND",
            e.to_string(),
            [("item_id", id.to_string())],
        ),
        StoreError::RevisionMismatch {
            id,
            expected,
            actual,
        } => error_info(
            Code::Aborted,
            "REVISION_MISMATCH",
            e.to_string(),
            [
                ("item_id", id.to_string()),
                ("expected_revision", expected.to_string()),
                ("actual_revision", actual.to_string()),
            ],
        ),
        // Mutating RPCs map through `mutation`, which knows the request's
        // shape; without it the field is named as an Item path.
        StoreError::Mutation { index, error } => Shape::Update.status(index, &error, None),
        // INVALID_ARGUMENT rather than ALREADY_EXISTS: nothing the client
        // asked to create exists; the request names a request_id it may not
        // use, and a fresh one fixes it (AIP-155 leaves the code open).
        StoreError::RequestIdReused => {
            bad_request("request_id", "was already used for a different request")
        }
        StoreError::InvalidPageToken => bad_request("page_token", e.to_string()),
        StoreError::SchemaTooNew { .. } => error_info(
            Code::FailedPrecondition,
            "SCHEMA_TOO_NEW",
            e.to_string(),
            [],
        ),
        StoreError::Closed => error_info(Code::Unavailable, "SHUTTING_DOWN", e.to_string(), []),
        StoreError::Sqlite(_) | StoreError::Corrupt(_) => {
            tracing::error!(error = %e, "store failure");
            error_info(Code::Internal, "STORE", e.to_string(), [])
        }
    }
}

/// How a mutating request lays out its mutations, so that a store error
/// names the request field it is about.
pub(crate) enum Shape<'a> {
    /// `CaptureItem`: everything is about `text`, except the parent.
    Capture,
    /// `UpdateItem`: fields under `item`, repeats in `update_mask`.
    Update,
    SetStatus,
    /// Archive, unarchive, delete and undelete: only `id`.
    ItemOp,
    /// `BatchMutate`: mutation `i` under `mutations[i].<oneof case>`.
    Batch(&'a [v1::Mutation]),
}

impl Shape<'_> {
    /// The request path of `error` in mutation `index`.
    fn path(&self, index: usize, error: &MutationError) -> String {
        match self {
            Shape::Capture if error.field() == Some(ItemField::ParentId) => "parent_id".to_owned(),
            Shape::Capture => "text".to_owned(),
            Shape::Update => match (error, error.field()) {
                (MutationError::Duplicate(_), _) => "update_mask".to_owned(),
                (_, Some(field)) => format!("item.{}", item_field_path(field)),
                (_, None) => "item".to_owned(),
            },
            Shape::SetStatus => "status".to_owned(),
            Shape::ItemOp => "id".to_owned(),
            Shape::Batch(mutations) => {
                use v1::mutation::Kind;
                let prefix = format!("mutations[{index}]");
                let (case, inner) = match mutations.get(index).and_then(|m| m.kind.as_ref()) {
                    Some(Kind::UpdateItem(_)) => ("update_item", Shape::Update),
                    Some(Kind::SetStatus(_)) => ("set_status", Shape::SetStatus),
                    Some(Kind::ArchiveItem(_)) => ("archive_item", Shape::ItemOp),
                    Some(Kind::UnarchiveItem(_)) => ("unarchive_item", Shape::ItemOp),
                    Some(Kind::DeleteItem(_)) => ("delete_item", Shape::ItemOp),
                    Some(Kind::UndeleteItem(_)) => ("undelete_item", Shape::ItemOp),
                    None => return prefix,
                };
                format!("{prefix}.{case}.{}", inner.path(0, error))
            }
        }
    }

    /// The status for `error` in mutation `index`, which targets `id`.
    fn status(&self, index: usize, error: &MutationError, id: Option<ItemId>) -> Status {
        match error {
            MutationError::Deleted => error_info(
                Code::FailedPrecondition,
                "ITEM_DELETED",
                match id {
                    Some(id) => format!("item {id} is deleted"),
                    None => "the item is deleted".to_owned(),
                },
                id.map(|id| ("item_id", id.to_string())),
            ),
            _ => bad_request(&self.path(index, error), describe(error)),
        }
    }
}

/// Maps a [`StoreError`] from a mutating request of `shape`, whose mutation
/// `i` targets `ids[i]` (empty for a capture).
pub(crate) fn mutation(e: StoreError, shape: &Shape<'_>, ids: &[ItemId]) -> Status {
    match e {
        StoreError::Mutation { index, error } => {
            shape.status(index, &error, ids.get(index).copied())
        }
        other => store(other),
    }
}

/// What is wrong, for a user; the violation's path names the field.
fn describe(error: &MutationError) -> String {
    match error {
        MutationError::ReadOnly(_) => "is read-only".to_owned(),
        MutationError::UseSetStatus(_) => "changes only through SetStatus".to_owned(),
        MutationError::UseArchive(_) => {
            "changes only through ArchiveItem and UnarchiveItem".to_owned()
        }
        MutationError::WrongType { got: "cleared", .. } => "cannot be cleared".to_owned(),
        MutationError::WrongType { got, .. } => format!("does not take a {got} value"),
        MutationError::Invalid { reason, .. } | MutationError::Unsupported { reason, .. } => {
            reason.clone()
        }
        MutationError::ParentUnset(field) => match field.parent() {
            Some(parent) => format!("cannot be set while {} is unset", item_field_path(parent)),
            None => "cannot be set while its parent is unset".to_owned(),
        },
        MutationError::Duplicate(field) => format!(
            "{:?} repeats or overlaps an earlier path",
            item_field_path(*field)
        ),
        MutationError::Deleted => "the item is deleted".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_descriptions_are_capped_on_a_char_boundary() {
        assert_eq!(cap("short".to_owned()), "short");
        let exact = "é".repeat(MAX_DESCRIPTION_CHARS);
        assert_eq!(cap(exact.clone()), exact);
        let long = cap("é".repeat(10_000));
        assert_eq!(long.chars().count(), MAX_DESCRIPTION_CHARS + 1);
        assert!(long.ends_with("é…"));
    }

    #[test]
    fn convert_errors_name_the_path_once() {
        let status = convert(ConvertError::Missing("title".into()).within("item"));
        assert_eq!(status.message(), "item.title: required field is missing");
        let bad = status.get_details_bad_request().unwrap();
        assert_eq!(bad.field_violations[0].field, "item.title");
        assert_eq!(
            bad.field_violations[0].description,
            "required field is missing"
        );
    }

    #[test]
    fn batch_paths_name_the_oneof_case() {
        use v1::mutation::Kind;
        let mutations = [
            v1::Mutation {
                kind: Some(Kind::SetStatus(Default::default())),
            },
            v1::Mutation {
                kind: Some(Kind::UpdateItem(Default::default())),
            },
            v1::Mutation {
                kind: Some(Kind::DeleteItem(Default::default())),
            },
        ];
        let batch = Shape::Batch(&mutations);
        let invalid = MutationError::Invalid {
            field: ItemField::WaitingFollowUpOn,
            reason: "x".into(),
        };
        let duplicate = MutationError::Duplicate(ItemField::Title);
        assert_eq!(batch.path(0, &invalid), "mutations[0].set_status.status");
        assert_eq!(
            batch.path(1, &invalid),
            "mutations[1].update_item.item.waiting.follow_up_on"
        );
        assert_eq!(
            batch.path(1, &duplicate),
            "mutations[1].update_item.update_mask"
        );
        assert_eq!(batch.path(2, &invalid), "mutations[2].delete_item.id");
        assert_eq!(Shape::Capture.path(0, &invalid), "text");
        let parent = MutationError::Invalid {
            field: ItemField::ParentId,
            reason: "x".into(),
        };
        assert_eq!(Shape::Capture.path(0, &parent), "parent_id");
    }
}
