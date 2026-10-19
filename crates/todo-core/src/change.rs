use crate::ids::Revision;
use ulid::Ulid;

/// Change-stream topic (design 4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Topic {
    /// The Item aggregate, incl. comments, relations and pending-push state.
    Item,
    Suggestion,
    View,
    Person,
    Series,
    Review,
    Config,
}

/// The outcome of a committed mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MutationResult {
    /// The op; the unit of undo.
    pub op_id: Ulid,
    /// The revision after commit.
    pub revision: Revision,
}
