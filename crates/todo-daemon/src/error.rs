//! Mapping failures to gRPC statuses with `google.rpc` details (design 4.3 rule 5).

use std::collections::HashMap;

use todo_proto::convert::ConvertError;
use todo_store::StoreError;
use tonic::{Code, Status};
use tonic_types::{ErrorDetails, StatusExt};

pub(crate) const DOMAIN: &str = "todo-tui";

pub(crate) fn bad_request(field: &str, description: impl Into<String>) -> Status {
    let description = description.into();
    Status::with_error_details(
        Code::InvalidArgument,
        format!("{field}: {description}"),
        ErrorDetails::with_bad_request_violation(field, description),
    )
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
    let field = match &e {
        ConvertError::Missing(f)
        | ConvertError::InvalidId(f, _)
        | ConvertError::Unspecified(f)
        | ConvertError::UnknownEnum(f, _)
        | ConvertError::Invalid(f, _) => *f,
    };
    bad_request(field, e.to_string())
}

pub(crate) fn store(e: StoreError) -> Status {
    match e {
        StoreError::NotFound(id) => error_info(
            Code::NotFound,
            "ITEM_NOT_FOUND",
            e.to_string(),
            [("item_id", id.to_string())],
        ),
        StoreError::RevisionMismatch { expected, actual } => error_info(
            Code::Aborted,
            "REVISION_MISMATCH",
            e.to_string(),
            [
                ("expected_revision", expected.to_string()),
                ("actual_revision", actual.to_string()),
            ],
        ),
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
