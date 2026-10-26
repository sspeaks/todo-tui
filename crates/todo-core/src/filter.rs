//! The filter language of views and `ListItems` (design 4.8, filter.proto):
//! the typed AST, the target catalogue, validation with indexed error paths,
//! relative dates, and the reference semantics.
//!
//! A [`Filter`] is checked with [`Filter::validate`] and resolved for one
//! civil today with [`Filter::resolve`]. The [`Resolved`] filter is what
//! [`matches`] evaluates and the store compiles to SQL: [`matches`] and
//! [`compare`] are the reference semantics, which the store's SQL and the
//! client's FakeDaemon must agree with.

use std::cmp::Ordering;
use std::fmt;

use jiff::Span;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::field::ItemField;
use crate::item::{ArchiveReason, CreatedVia, Item, LinkState, PersonRef, Priority};
use crate::mutation::is_iso_week;
use crate::status::{ResolutionKind, Status, StatusCategory, TodoRefinement};
use crate::value::{FieldValue, duration_in_range, timestamp_in_range};

/// How deep groups may nest; a group in the filter itself is 1 deep.
pub const MAX_GROUP_DEPTH: usize = 4;
/// The most predicates a filter holds, in all its groups.
pub const MAX_PREDICATES: usize = 128;
/// The most operands one predicate takes.
pub const MAX_PREDICATE_OPERANDS: usize = 1000;
/// The most operands a filter holds, in all its predicates.
pub const MAX_OPERANDS: usize = 4000;
/// The longest MATCHES text, in bytes of UTF-8.
pub const MAX_MATCHES_BYTES: usize = 1024;
/// The most keys a sort holds.
pub const MAX_SORT_KEYS: usize = 8;

// ---- the AST --------------------------------------------------------------

/// A filter (filter.proto `Filter`). An Item matches when every `all_of`
/// clause matches and, unless `any_of` is empty, one of its clauses does,
/// and it passes the implicit rules the `include_*` flags lift. `O` is the
/// operand: [`Operand`] as given, [`ResolvedOperand`] once resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filter<O = Operand> {
    pub all_of: Vec<Clause<O>>,
    pub any_of: Vec<Clause<O>>,
    pub include_archived: bool,
    pub include_done: bool,
    pub include_deferred: bool,
    pub include_deleted: bool,
}

impl<O> Default for Filter<O> {
    fn default() -> Self {
        Self {
            all_of: Vec::new(),
            any_of: Vec::new(),
            include_archived: false,
            include_done: false,
            include_deferred: false,
            include_deleted: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Clause<O = Operand> {
    Predicate(Predicate<O>),
    Group(Group<O>),
}

/// A nested group, with the same `all_of` / `any_of` rule as [`Filter`]. An
/// empty group matches every Item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group<O = Operand> {
    pub all_of: Vec<Clause<O>>,
    pub any_of: Vec<Clause<O>>,
}

impl<O> Default for Group<O> {
    fn default() -> Self {
        Self {
            all_of: Vec::new(),
            any_of: Vec::new(),
        }
    }
}

/// A test of one target: a stored, nested or derived [`ItemField`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Predicate<O = Operand> {
    pub target: ItemField,
    pub op: Operator,
    pub operands: Vec<O>,
}

impl<O> Predicate<O> {
    pub fn new(target: ItemField, op: Operator, operands: Vec<O>) -> Self {
        Self {
            target,
            op,
            operands,
        }
    }
}

impl<O> From<Predicate<O>> for Clause<O> {
    fn from(p: Predicate<O>) -> Self {
        Clause::Predicate(p)
    }
}

impl<O> From<Group<O>> for Clause<O> {
    fn from(g: Group<O>) -> Self {
        Clause::Group(g)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    In,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    IsSet,
    IsUnset,
    ContainsAny,
    ContainsAll,
    Matches,
}

impl Operator {
    /// The proto name without its `OPERATOR_` prefix.
    pub fn name(self) -> &'static str {
        match self {
            Operator::In => "IN",
            Operator::Eq => "EQ",
            Operator::Ne => "NE",
            Operator::Lt => "LT",
            Operator::Le => "LE",
            Operator::Gt => "GT",
            Operator::Ge => "GE",
            Operator::IsSet => "IS_SET",
            Operator::IsUnset => "IS_UNSET",
            Operator::ContainsAny => "CONTAINS_ANY",
            Operator::ContainsAll => "CONTAINS_ALL",
            Operator::Matches => "MATCHES",
        }
    }

    /// LT, LE, GT and GE.
    pub fn is_ordering(self) -> bool {
        matches!(
            self,
            Operator::Lt | Operator::Le | Operator::Gt | Operator::Ge
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operand {
    /// In the target's own [`FieldValue`] variant (one string per member of
    /// a set).
    Value(FieldValue),
    /// On date targets and `focus_week`; resolved for the request's today.
    RelativeDate(RelativeDate),
    /// The words of a MATCHES.
    Text(String),
}

impl From<FieldValue> for Operand {
    fn from(v: FieldValue) -> Self {
        Operand::Value(v)
    }
}

impl From<RelativeDate> for Operand {
    fn from(d: RelativeDate) -> Self {
        Operand::RelativeDate(d)
    }
}

/// A date relative to today: the anchor plus `offset_days`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RelativeDate {
    pub anchor: Anchor,
    /// Must be 0 for [`Anchor::CurrentWeek`].
    pub offset_days: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    Today,
    /// The Monday of today's ISO week.
    StartOfWeek,
    /// The Sunday of today's ISO week (capture's `eow` is Friday instead).
    EndOfWeek,
    /// Today's ISO week, `YYYY-Www` in the ISO week-year, for `focus_week`.
    CurrentWeek,
}

impl RelativeDate {
    pub const fn new(anchor: Anchor, offset_days: i32) -> Self {
        Self {
            anchor,
            offset_days,
        }
    }

    /// The value this names on `today`: a date, or the ISO week for
    /// `CurrentWeek`. `None` when that falls outside 0001-01-01 to
    /// 9999-12-31, or for `CurrentWeek` with an offset.
    pub fn resolve(self, today: Date) -> Option<FieldValue> {
        let in_range = |d: Date| (1..=9999).contains(&d.year());
        let monday = || {
            let back = i64::from(today.weekday().to_monday_zero_offset());
            today.checked_sub(Span::new().days(back)).ok()
        };
        let base = match self.anchor {
            Anchor::CurrentWeek => {
                if self.offset_days != 0 || !in_range(today) {
                    return None;
                }
                let week = today.iso_week_date();
                let week = format!("{:04}-W{:02}", week.year(), week.week());
                return Some(FieldValue::IsoWeek(week));
            }
            Anchor::Today => today,
            Anchor::StartOfWeek => monday()?,
            Anchor::EndOfWeek => monday()?.checked_add(Span::new().days(6)).ok()?,
        };
        let offset = Span::new().try_days(self.offset_days).ok()?;
        let date = base.checked_add(offset).ok()?;
        in_range(date).then_some(FieldValue::Date(date))
    }
}

/// A sort key: keys apply in order, each in its own direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SortKey {
    pub key: ItemField,
    pub descending: bool,
}

impl SortKey {
    pub const fn asc(key: ItemField) -> Self {
        Self {
            key,
            descending: false,
        }
    }

    pub const fn desc(key: ItemField) -> Self {
        Self {
            key,
            descending: true,
        }
    }
}

/// An operand of a [`Resolved`] filter: relative dates are values.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedOperand {
    Value(FieldValue),
    Text(String),
}

/// A filter checked and resolved for one today ([`Filter::resolve`]): every
/// combination in it is valid and built, and its relative dates are dates
/// and ISO weeks. It keeps that today, the request's only one, for the
/// defer rule and `is_overdue`.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    filter: Filter<ResolvedOperand>,
    today: Date,
}

impl Resolved {
    pub fn filter(&self) -> &Filter<ResolvedOperand> {
        &self.filter
    }

    /// The day the filter was resolved for.
    pub fn today(&self) -> Date {
        self.today
    }
}

// ---- the target catalogue -------------------------------------------------

/// What a target holds, which decides the operators and operands it takes
/// (filter.proto, design 4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueClass {
    /// An enum other than priority: EQ, IN, NE; sorted by proto number.
    Enum,
    /// Priority-typed: ordered by rank, PRIORITY_NONE outside the order.
    Priority,
    /// Compared by exact code point; no ordering operators.
    String,
    /// An Item id (`item_id` operands).
    Id,
    Markdown,
    /// A civil date; takes TODAY, START_OF_WEEK and END_OF_WEEK.
    Date,
    /// `YYYY-Www`, ordered by week; takes CURRENT_WEEK.
    FocusWeek,
    /// A point in time, at nanosecond precision.
    Instant,
    Number,
    Duration,
    Uint,
    Bool,
    /// A set of strings, never unset: IS_SET means non-empty.
    Set,
    Person,
    /// A message such as `waiting`: only IS_SET and IS_UNSET.
    Presence,
    /// A set of fields: IS_SET and IS_UNSET; CONTAINS_* are not built yet.
    PendingFields,
    /// Not built yet as a target.
    RemoteExtras,
}

impl ValueClass {
    /// For messages: "LT does not apply to {name} targets".
    pub fn name(self) -> &'static str {
        match self {
            ValueClass::Enum => "enum",
            ValueClass::Priority => "priority",
            ValueClass::String => "string",
            ValueClass::Id => "id",
            ValueClass::Markdown => "Markdown",
            ValueClass::Date => "date",
            ValueClass::FocusWeek => "week",
            ValueClass::Instant => "instant",
            ValueClass::Number => "number",
            ValueClass::Duration => "duration",
            ValueClass::Uint => "uint",
            ValueClass::Bool => "bool",
            ValueClass::Set => "set",
            ValueClass::Person => "person",
            ValueClass::Presence => "message",
            ValueClass::PendingFields => "field set",
            ValueClass::RemoteExtras => "remote_extras",
        }
    }

    /// Whether LT, LE, GT and GE apply.
    fn is_ordered(self) -> bool {
        matches!(
            self,
            ValueClass::Priority
                | ValueClass::Date
                | ValueClass::FocusWeek
                | ValueClass::Instant
                | ValueClass::Number
                | ValueClass::Duration
                | ValueClass::Uint
        )
    }
}

/// What a filter or sort can do with one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Target {
    pub class: ValueClass,
    /// Whether the target can be unset, so that IS_SET and IS_UNSET apply.
    /// Sets and `pending_fields` are never unset, but take both as
    /// non-empty and empty.
    pub can_unset: bool,
    /// Whether it can be a sort key.
    pub sortable: bool,
    /// Whether MATCHES searches it: it is a full-text column.
    pub matches: bool,
}

/// The catalogue entry of `field` (filter.proto; design 4.8).
pub fn target(field: ItemField) -> Target {
    use ItemField as F;
    use ValueClass as C;
    let (class, can_unset, sortable) = match field {
        F::Id => (C::Id, false, true),
        F::ParentId | F::MergedInto => (C::Id, true, false),
        F::CreatedVia | F::StatusCategory | F::TodoRefinement | F::Status => (C::Enum, false, true),
        F::ResolutionKind | F::ArchivedReason | F::RemoteLinkLinkState => (C::Enum, true, true),
        F::CreatedAt | F::UpdatedAt => (C::Instant, false, true),
        F::RemoteCreatedAt
        | F::RemoteUpdatedAt
        | F::DeletedAt
        | F::ArchivedAt
        | F::RemoteLinkLastPulledVersion
        | F::StalenessLastUserTouchAt
        | F::StalenessLastRemoteChangeAt
        | F::StalenessStatusEnteredAt
        | F::StalenessWaitingSince => (C::Instant, true, true),
        F::Revision => (C::Uint, false, true),
        F::Title | F::ItemType => (C::String, false, true),
        F::RemoteStatusName
        | F::Sprint
        | F::Resolution
        | F::WaitingReason
        | F::RemoteLinkTrackerKind
        | F::RemoteLinkInstanceId
        | F::RemoteLinkRemoteId
        | F::RemoteLinkKey
        | F::RemoteLinkUrl => (C::String, true, true),
        // A ULID as text, which has no useful order.
        F::RecurrenceSeriesId => (C::String, true, false),
        F::Description | F::PrivateNotes => (C::Markdown, false, false),
        F::Priority | F::EffectivePriority => (C::Priority, false, true),
        F::LocalPriorityOverride => (C::Priority, true, true),
        F::Assignee | F::Reporter | F::WaitingOn => (C::Person, true, false),
        F::Labels | F::Components | F::FixVersions | F::Scopes => (C::Set, false, false),
        F::DueDate
        | F::DeferUntil
        | F::WaitingSince
        | F::WaitingFollowUpOn
        | F::RecurrenceOccurrenceDate => (C::Date, true, true),
        F::FocusWeek => (C::FocusWeek, true, true),
        F::StoryPoints => (C::Number, true, true),
        F::OriginalEstimate | F::RemainingEstimate => (C::Duration, true, true),
        F::IsMine
        | F::HasConflict
        | F::HasPushFailure
        | F::DescriptionLossy
        | F::DescriptionDirty
        | F::IsOverdue => (C::Bool, false, true),
        F::RemoteLinkPinned => (C::Bool, true, true),
        F::RemoteLink | F::Waiting | F::Archived | F::Recurrence | F::Staleness => {
            (C::Presence, true, false)
        }
        F::PendingFields => (C::PendingFields, false, false),
        F::RemoteExtras => (C::RemoteExtras, false, false),
    };
    let matches = matches!(
        field,
        F::Title | F::Description | F::Labels | F::RemoteLinkKey
    );
    Target {
        class,
        can_unset,
        sortable,
        matches,
    }
}

/// The [`FieldValue`] variant (as [`FieldValue::kind`] names it) a value
/// operand on `field` has: the field's own, and a string per member for a
/// set. `None` for the targets that take no values.
fn value_kind(field: ItemField) -> Option<&'static str> {
    use ItemField as F;
    Some(match field {
        F::Id | F::ParentId | F::MergedInto => "item_id",
        F::CreatedVia => "created_via",
        F::StatusCategory => "status_category",
        F::ResolutionKind => "resolution_kind",
        F::TodoRefinement => "todo_refinement",
        F::Status => "status",
        F::ArchivedReason => "archive_reason",
        F::RemoteLinkLinkState => "link_state",
        F::CreatedAt
        | F::UpdatedAt
        | F::RemoteCreatedAt
        | F::RemoteUpdatedAt
        | F::DeletedAt
        | F::ArchivedAt
        | F::RemoteLinkLastPulledVersion
        | F::StalenessLastUserTouchAt
        | F::StalenessLastRemoteChangeAt
        | F::StalenessStatusEnteredAt
        | F::StalenessWaitingSince => "timestamp",
        F::Revision => "uint",
        F::Title
        | F::ItemType
        | F::RemoteStatusName
        | F::Sprint
        | F::Resolution
        | F::WaitingReason
        | F::RemoteLinkTrackerKind
        | F::RemoteLinkInstanceId
        | F::RemoteLinkRemoteId
        | F::RemoteLinkKey
        | F::RemoteLinkUrl
        | F::RecurrenceSeriesId
        | F::Labels
        | F::Components
        | F::FixVersions
        | F::Scopes => "string",
        F::Description | F::PrivateNotes => "markdown",
        F::Priority | F::EffectivePriority | F::LocalPriorityOverride => "priority",
        F::Assignee | F::Reporter | F::WaitingOn => "person",
        F::DueDate
        | F::DeferUntil
        | F::WaitingSince
        | F::WaitingFollowUpOn
        | F::RecurrenceOccurrenceDate => "date",
        F::FocusWeek => "iso_week",
        F::StoryPoints => "number",
        F::OriginalEstimate | F::RemainingEstimate => "duration",
        F::IsMine
        | F::HasConflict
        | F::HasPushFailure
        | F::DescriptionLossy
        | F::DescriptionDirty
        | F::IsOverdue
        | F::RemoteLinkPinned => "bool",
        F::RemoteLink
        | F::Waiting
        | F::Archived
        | F::Recurrence
        | F::Staleness
        | F::PendingFields
        | F::RemoteExtras => return None,
    })
}

// ---- errors ---------------------------------------------------------------

/// Why a filter or sort was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterError {
    pub kind: FilterErrorKind,
    /// Relative to the filter (`all_of[1].group.any_of[0].predicate.op`), and
    /// empty for the filter as a whole. A sort's paths start with `sort`
    /// (`sort[2].key`).
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterErrorKind {
    /// Not a valid filter or sort (INVALID_ARGUMENT).
    Invalid,
    /// Valid, but not built yet (UNIMPLEMENTED).
    Unimplemented,
}

impl FilterError {
    pub fn invalid(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            kind: FilterErrorKind::Invalid,
            path: path.into(),
            reason: reason.into(),
        }
    }

    pub fn unimplemented(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            kind: FilterErrorKind::Unimplemented,
            path: path.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.reason)
        } else {
            write!(f, "{}: {}", self.path, self.reason)
        }
    }
}

impl std::error::Error for FilterError {}

// ---- validation -----------------------------------------------------------

/// The checks behind [`Filter::validate`], one node at a time. A walk visits
/// the clauses in document order (`all_of` before `any_of`, each clause
/// depth-first): [`Checker::group`] for a group before its clauses, and
/// [`Checker::predicate`] for a predicate (its target and op, then its
/// operand count) before [`Checker::operand`] for each operand. It stops at
/// the first error, which is INVALID: that is the answer.
/// [`Checker::finish`] then checks the totals, and only then reports the
/// first UNIMPLEMENTED, so an invalid request is never told "not yet".
///
/// A converter that builds the filter as it walks reports what it cannot
/// read where it finds it (an unknown target at `.target`, an unreadable
/// operand at `.operands[k]` before its fit to the target), which keeps
/// the order for its errors too. Paths are relative to the filter.
#[derive(Debug, Clone)]
pub struct Checker {
    /// With a today, relative dates are resolved too, so a date out of
    /// range is reported in its place.
    today: Option<Date>,
    /// Whether the size limits apply.
    limits: bool,
    predicates: usize,
    operands: usize,
    unimplemented: Option<FilterError>,
}

impl Checker {
    /// The checks of a filter as a request or view gives it, limits
    /// included.
    pub fn new(today: Option<Date>) -> Self {
        Self {
            today,
            limits: true,
            predicates: 0,
            operands: 0,
            unimplemented: None,
        }
    }

    /// A group at `path` (ending in `.group`), `depth` groups deep.
    pub fn group(&mut self, path: &str, depth: usize) -> Result<(), FilterError> {
        if self.limits && depth > MAX_GROUP_DEPTH {
            return Err(FilterError::invalid(
                path,
                format!("groups nest at most {MAX_GROUP_DEPTH} deep"),
            ));
        }
        Ok(())
    }

    /// A predicate at `path` (ending in `.predicate`) on `field` with `op`
    /// and `count` operands.
    pub fn predicate(
        &mut self,
        path: &str,
        field: ItemField,
        op: Operator,
        count: usize,
    ) -> Result<(), FilterError> {
        use Operator as O;
        use ValueClass as C;
        self.predicates += 1;
        self.operands += count;
        let class = target(field).class;
        let op_error = |reason: String| Err(FilterError::invalid(format!("{path}.op"), reason));
        let name = op.name();
        match op {
            O::Matches => {
                if !target(field).matches {
                    return Err(FilterError::invalid(
                        format!("{path}.target"),
                        "MATCHES searches only title, description, labels and remote_link.key",
                    ));
                }
            }
            _ if class == C::RemoteExtras => self.not_built(
                format!("{path}.target"),
                "filters on remote_extras are not supported yet",
            ),
            O::IsSet | O::IsUnset => {
                if !target(field).can_unset && !matches!(class, C::Set | C::PendingFields) {
                    return op_error(format!(
                        "{name} applies only to a target that can be unset; use EQ"
                    ));
                }
            }
            O::ContainsAny | O::ContainsAll => match class {
                C::Set => {}
                C::PendingFields => self.not_built(
                    format!("{path}.op"),
                    format!("{name} on pending_fields is not supported yet"),
                ),
                _ => return op_error(format!("{name} applies only to set targets")),
            },
            O::In | O::Eq | O::Ne => match class {
                C::Set => {
                    return op_error(format!(
                        "{name} does not apply to set targets; use CONTAINS_ANY or CONTAINS_ALL"
                    ));
                }
                C::Presence | C::PendingFields => {
                    return op_error(format!(
                        "{name} does not apply to {} targets; use IS_SET or IS_UNSET",
                        class.name()
                    ));
                }
                _ => {}
            },
            O::Lt | O::Le | O::Gt | O::Ge => {
                if !class.is_ordered() {
                    return op_error(format!(
                        "{name} does not apply to {} targets, which have no order",
                        class.name()
                    ));
                }
            }
        }
        let fits = match op {
            O::In | O::ContainsAny | O::ContainsAll => {
                count >= 1 && (!self.limits || count <= MAX_PREDICATE_OPERANDS)
            }
            O::IsSet | O::IsUnset => count == 0,
            O::Eq | O::Ne | O::Lt | O::Le | O::Gt | O::Ge | O::Matches => count == 1,
        };
        if !fits {
            let reason = match op {
                O::In | O::ContainsAny | O::ContainsAll => {
                    format!("{name} takes 1 to {MAX_PREDICATE_OPERANDS} operands")
                }
                O::IsSet | O::IsUnset => format!("{name} takes no operands"),
                _ => format!("{name} takes exactly one operand"),
            };
            return Err(FilterError::invalid(format!("{path}.operands"), reason));
        }
        Ok(())
    }

    /// An operand at `path` (ending in `.operands[k]`) of a predicate on
    /// `field` with `op`, which [`Checker::predicate`] accepted.
    pub fn operand(
        &mut self,
        path: &str,
        field: ItemField,
        op: Operator,
        operand: &Operand,
    ) -> Result<(), FilterError> {
        use ValueClass as C;
        let class = target(field).class;
        let invalid = |reason: &str| Err(FilterError::invalid(path, reason));
        // No target takes these, built or not.
        match operand {
            Operand::Text(_) if op != Operator::Matches => {
                return invalid("text is only for MATCHES");
            }
            Operand::Value(FieldValue::Cleared) => {
                return invalid("cleared is never an operand; use IS_UNSET");
            }
            _ => {}
        }
        // Nothing more to fit an operand to until these are built.
        if class == C::RemoteExtras
            || class == C::PendingFields
                && matches!(op, Operator::ContainsAny | Operator::ContainsAll)
        {
            return Ok(());
        }
        match operand {
            Operand::Text(text) => {
                if text.chars().all(|c| c <= ' ') {
                    invalid("must not be blank")
                } else if self.limits && text.len() > MAX_MATCHES_BYTES {
                    invalid(&format!("is longer than {MAX_MATCHES_BYTES} bytes"))
                } else {
                    Ok(())
                }
            }
            _ if op == Operator::Matches => invalid("MATCHES takes a text operand"),
            Operand::RelativeDate(date) => {
                use Anchor as A;
                match (class, date.anchor) {
                    (C::Date, A::Today | A::StartOfWeek | A::EndOfWeek)
                    | (C::FocusWeek, A::CurrentWeek) => {}
                    (C::Instant, A::Today | A::StartOfWeek | A::EndOfWeek) => {
                        self.not_built(path, "relative dates on instants are not supported yet");
                        return Ok(());
                    }
                    (C::Date | C::Instant, A::CurrentWeek) => {
                        return invalid("CURRENT_WEEK is a week, for focus_week");
                    }
                    (C::FocusWeek, _) => return invalid("focus_week takes CURRENT_WEEK"),
                    _ => return invalid("relative dates are only for dates and focus_week"),
                }
                let offset = || format!("{path}.relative_date.offset_days");
                if date.anchor == A::CurrentWeek && date.offset_days != 0 {
                    return Err(FilterError::invalid(offset(), "must be 0 for CURRENT_WEEK"));
                }
                if let Some(today) = self.today
                    && date.resolve(today).is_none()
                {
                    return Err(FilterError::invalid(
                        offset(),
                        "names a date outside 0001-01-01 to 9999-12-31",
                    ));
                }
                Ok(())
            }
            Operand::Value(value) => match value_problem(field, op, value) {
                Some(reason) => invalid(&reason),
                None => Ok(()),
            },
        }
    }

    /// After the last clause: the totals, then the first UNIMPLEMENTED.
    pub fn finish(self) -> Result<(), FilterError> {
        if self.limits && self.predicates > MAX_PREDICATES {
            return Err(FilterError::invalid(
                "",
                format!(
                    "holds {} predicates; at most {MAX_PREDICATES}",
                    self.predicates
                ),
            ));
        }
        if self.limits && self.operands > MAX_OPERANDS {
            return Err(FilterError::invalid(
                "",
                format!("holds {} operands; at most {MAX_OPERANDS}", self.operands),
            ));
        }
        self.unimplemented.map_or(Ok(()), Err)
    }

    fn not_built(&mut self, path: impl Into<String>, reason: impl Into<String>) {
        self.unimplemented
            .get_or_insert_with(|| FilterError::unimplemented(path, reason));
    }
}

/// What is wrong with `value` (not `Cleared`) as an operand of `op` on
/// `field`, if anything.
fn value_problem(field: ItemField, op: Operator, value: &FieldValue) -> Option<String> {
    use FieldValue as V;
    let Some(kind) = value_kind(field) else {
        return Some("takes no values".to_owned());
    };
    if value.kind() != kind {
        return Some(format!("takes {kind} values, not {}", value.kind()));
    }
    let reason = match value {
        V::Priority(Priority::None) if op.is_ordering() => {
            "PRIORITY_NONE is outside the priority order"
        }
        V::Timestamp(t) if !timestamp_in_range(*t) => {
            "must be between 1677-09-21T00:12:44Z and 2262-04-11T23:47:16Z"
        }
        V::Duration(d) if !duration_in_range(*d) => "must be shorter than about 292 years",
        V::Number(n) if !n.is_finite() => "must be a finite number",
        V::Uint(n) if i64::try_from(*n).is_err() => "must be at most 2^63 - 1",
        V::Date(d) if !(1..=9999).contains(&d.year()) => {
            "must be between 0001-01-01 and 9999-12-31"
        }
        V::IsoWeek(w) if !is_iso_week(w) => "must be an ISO week, YYYY-Www",
        V::Person(PersonRef::FreeText(_)) if field != ItemField::WaitingOn => {
            "free text is only valid for waiting.on"
        }
        _ => return None,
    };
    Some(reason.to_owned())
}

impl Filter {
    /// Checks the filter as a request or view gives it (filter.proto). The
    /// error is the first INVALID in document order, else a size limit the
    /// filter exceeds as a whole (at the empty path), else the first
    /// UNIMPLEMENTED. Relative dates are checked when resolved.
    pub fn validate(&self) -> Result<(), FilterError> {
        self.check(Checker::new(None))
    }

    /// [`Filter::validate`] with the relative dates resolved for `today`
    /// too, so that one out of range is reported in its place.
    pub fn validate_on(&self, today: Date) -> Result<(), FilterError> {
        self.check(Checker::new(Some(today)))
    }

    /// The filter with its relative dates resolved for `today`, which it
    /// keeps, after the checks of [`Filter::validate`] with a date out of
    /// range reported in its place. The size limits do not apply: they
    /// bound requests, and a request that narrows a view is one group deeper
    /// than either.
    pub fn resolve(&self, today: Date) -> Result<Resolved, FilterError> {
        self.check(Checker {
            limits: false,
            ..Checker::new(Some(today))
        })?;
        let resolve = |o: &Operand| match o {
            Operand::Value(v) => ResolvedOperand::Value(v.clone()),
            Operand::RelativeDate(d) => {
                ResolvedOperand::Value(d.resolve(today).expect("checked above"))
            }
            Operand::Text(t) => ResolvedOperand::Text(t.clone()),
        };
        Ok(Resolved {
            filter: Filter {
                all_of: map_clauses(&self.all_of, &resolve),
                any_of: map_clauses(&self.any_of, &resolve),
                include_archived: self.include_archived,
                include_done: self.include_done,
                include_deferred: self.include_deferred,
                include_deleted: self.include_deleted,
            },
            today,
        })
    }

    fn check(&self, mut checker: Checker) -> Result<(), FilterError> {
        check_clauses(&mut checker, "", &self.all_of, &self.any_of, 0)?;
        checker.finish()
    }
}

/// Walks the clauses of the filter or group at `path`, `depth` groups deep.
fn check_clauses(
    checker: &mut Checker,
    path: &str,
    all_of: &[Clause],
    any_of: &[Clause],
    depth: usize,
) -> Result<(), FilterError> {
    for (list, clauses) in [("all_of", all_of), ("any_of", any_of)] {
        for (i, clause) in clauses.iter().enumerate() {
            let at = child_path(path, format_args!("{list}[{i}]"));
            match clause {
                Clause::Predicate(p) => {
                    let at = format!("{at}.predicate");
                    checker.predicate(&at, p.target, p.op, p.operands.len())?;
                    for (k, operand) in p.operands.iter().enumerate() {
                        checker.operand(&format!("{at}.operands[{k}]"), p.target, p.op, operand)?;
                    }
                }
                Clause::Group(g) => {
                    let at = format!("{at}.group");
                    checker.group(&at, depth + 1)?;
                    check_clauses(checker, &at, &g.all_of, &g.any_of, depth + 1)?;
                }
            }
        }
    }
    Ok(())
}

/// `name` inside the message at `parent`, which is empty for the filter.
pub fn child_path(parent: &str, name: impl fmt::Display) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}.{name}")
    }
}

fn map_clauses<O, R>(clauses: &[Clause<O>], f: &impl Fn(&O) -> R) -> Vec<Clause<R>> {
    clauses
        .iter()
        .map(|clause| match clause {
            Clause::Predicate(p) => Clause::Predicate(Predicate {
                target: p.target,
                op: p.op,
                operands: p.operands.iter().map(f).collect(),
            }),
            Clause::Group(g) => Clause::Group(Group {
                all_of: map_clauses(&g.all_of, f),
                any_of: map_clauses(&g.any_of, f),
            }),
        })
        .collect()
}

/// Checks a sort (filter.proto `SortKey`): each key sortable and given once,
/// then at most [`MAX_SORT_KEYS`] keys. Paths are `sort[i].key` and `sort`.
pub fn validate_sort(keys: &[SortKey]) -> Result<(), FilterError> {
    check_sort_keys(keys)?;
    if keys.len() > MAX_SORT_KEYS {
        return Err(FilterError::invalid(
            "sort",
            format!("holds {} keys; at most {MAX_SORT_KEYS}", keys.len()),
        ));
    }
    Ok(())
}

/// The checks [`validate_sort`] makes of each key, without the count: for a
/// converter that stops at a key it cannot read, after the keys before it.
pub fn check_sort_keys(keys: &[SortKey]) -> Result<(), FilterError> {
    for (i, key) in keys.iter().enumerate() {
        let path = || format!("sort[{i}].key");
        let class = target(key.key).class;
        if !target(key.key).sortable {
            let reason = match class {
                ValueClass::Id | ValueClass::String => {
                    "is not a sort key: a reference to another record has no useful order"
                        .to_owned()
                }
                _ => format!("is not a sort key: {} targets have no order", class.name()),
            };
            return Err(FilterError::invalid(path(), reason));
        }
        if let Some(j) = keys[..i].iter().position(|k| k.key == key.key) {
            return Err(FilterError::invalid(
                path(),
                format!("repeats sort[{j}].key"),
            ));
        }
    }
    Ok(())
}

// ---- the reference semantics ---------------------------------------------

/// `field` on `item` on `today`, `None` when unset (as [`FieldValue::of`]
/// gives it, with `is_overdue` computed for `today` rather than read).
pub fn value_of(item: &Item, field: ItemField, today: Date) -> Option<FieldValue> {
    if field == ItemField::IsOverdue {
        return Some(FieldValue::Bool(item.is_overdue(today)));
    }
    FieldValue::of(item, field).filter(|v| !matches!(v, FieldValue::Cleared))
}

/// Whether `item` passes `filter` on the today it was resolved for: the
/// implicit rules the `include_*` flags do not lift (deleted, archived, done
/// or cancelled, deferred past today), then the clauses. An unset target
/// never matches IN, EQ or an ordering operator, and always matches NE.
/// MATCHES needs the full-text index: here it never matches.
pub fn matches(item: &Item, filter: &Resolved) -> bool {
    let (f, today) = (filter.filter(), filter.today());
    let hidden = (!f.include_deleted && item.is_deleted())
        || (!f.include_archived && item.overlay.archived.is_some())
        || (!f.include_done && item.status().is_closed())
        || (!f.include_deferred && item.overlay.defer_until.is_some_and(|d| d > today));
    !hidden && clauses_match(item, &f.all_of, &f.any_of, today)
}

fn clauses_match(
    item: &Item,
    all_of: &[Clause<ResolvedOperand>],
    any_of: &[Clause<ResolvedOperand>],
    today: Date,
) -> bool {
    let clause = |c: &Clause<ResolvedOperand>| match c {
        Clause::Predicate(p) => predicate_matches(item, p, today),
        Clause::Group(g) => clauses_match(item, &g.all_of, &g.any_of, today),
    };
    all_of.iter().all(clause) && (any_of.is_empty() || any_of.iter().any(clause))
}

fn predicate_matches(item: &Item, p: &Predicate<ResolvedOperand>, today: Date) -> bool {
    use Operator as O;
    let value = value_of(item, p.target, today);
    let values = || {
        p.operands.iter().filter_map(|o| match o {
            ResolvedOperand::Value(v) => Some(v),
            ResolvedOperand::Text(_) => None,
        })
    };
    let equals = |v: &FieldValue| values().any(|x| x == v);
    let members: &[String] = match &value {
        Some(FieldValue::StringSet(set)) => set,
        _ => &[],
    };
    let member = |x: &FieldValue| matches!(x, FieldValue::String(s) if members.contains(s));
    match p.op {
        O::IsSet => is_set(value.as_ref()),
        O::IsUnset => !is_set(value.as_ref()),
        O::In | O::Eq => value.as_ref().is_some_and(equals),
        O::Ne => !value.as_ref().is_some_and(equals),
        O::Lt | O::Le | O::Gt | O::Ge => {
            let (Some(v), Some(x)) = (&value, values().next()) else {
                return false;
            };
            order(v, x).is_some_and(|o| match p.op {
                O::Lt => o.is_lt(),
                O::Le => o.is_le(),
                O::Gt => o.is_gt(),
                _ => o.is_ge(),
            })
        }
        O::ContainsAny => values().any(member),
        O::ContainsAll => values().next().is_some() && values().all(member),
        O::Matches => false,
    }
}

/// A set is set when it is non-empty; anything else when it has a value.
fn is_set(value: Option<&FieldValue>) -> bool {
    match value {
        None => false,
        Some(FieldValue::StringSet(set)) => !set.is_empty(),
        Some(FieldValue::ItemFieldSet(set)) => !set.is_empty(),
        Some(_) => true,
    }
}

/// The order of `a` and `b` under `sort` on `today` (filter.proto
/// `SortKey`): each key in its own direction with unset values, and
/// PRIORITY_NONE, last either way; then the tie-break `created_at`, `id`
/// ascending, without a key `sort` already has. A key that cannot sort
/// ([`validate_sort`]) compares only set against unset.
pub fn compare(a: &Item, b: &Item, sort: &[SortKey], today: Date) -> Ordering {
    let tie_break = [
        SortKey::asc(ItemField::CreatedAt),
        SortKey::asc(ItemField::Id),
    ];
    let given = |k: &SortKey| sort.iter().any(|s| s.key == k.key);
    sort.iter()
        .chain(tie_break.iter().filter(|k| !given(k)))
        .map(|k| compare_key(a, b, *k, today))
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
}

fn compare_key(a: &Item, b: &Item, key: SortKey, today: Date) -> Ordering {
    let value = |item| {
        value_of(item, key.key, today)
            .filter(|v| !matches!(v, FieldValue::Priority(Priority::None)))
    };
    match (value(a), value(b)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let o = order(&x, &y).unwrap_or(Ordering::Equal);
            if key.descending { o.reverse() } else { o }
        }
    }
}

/// How two values of one target order: priorities by rank, enums by proto
/// number, false before true, strings by code point. `None` when they have
/// no order (and for PRIORITY_NONE).
fn order(a: &FieldValue, b: &FieldValue) -> Option<Ordering> {
    use FieldValue as V;
    match (a, b) {
        (V::Priority(a), V::Priority(b)) => Some(priority_rank(*a)?.cmp(&priority_rank(*b)?)),
        (V::String(a), V::String(b)) | (V::IsoWeek(a), V::IsoWeek(b)) => Some(a.cmp(b)),
        (V::Bool(a), V::Bool(b)) => Some(a.cmp(b)),
        (V::Number(a), V::Number(b)) => a.partial_cmp(b),
        (V::Uint(a), V::Uint(b)) => Some(a.cmp(b)),
        (V::Date(a), V::Date(b)) => Some(a.cmp(b)),
        (V::Timestamp(a), V::Timestamp(b)) => Some(a.cmp(b)),
        (V::Duration(a), V::Duration(b)) => Some(a.cmp(b)),
        (V::ItemId(a), V::ItemId(b)) => Some(a.cmp(b)),
        _ if a.kind() == b.kind() => Some(enum_number(a)?.cmp(&enum_number(b)?)),
        _ => None,
    }
}

/// LOWEST 1 to HIGHEST 5; `None` for PRIORITY_NONE, which is outside the
/// order.
pub fn priority_rank(priority: Priority) -> Option<u8> {
    match priority {
        Priority::None => None,
        Priority::Lowest => Some(1),
        Priority::Low => Some(2),
        Priority::Medium => Some(3),
        Priority::High => Some(4),
        Priority::Highest => Some(5),
    }
}

/// The proto enum number of an enum value, which sorts enums; `None` for
/// any other value, priority included (it sorts by [`priority_rank`]).
pub fn enum_number(value: &FieldValue) -> Option<u32> {
    use FieldValue as V;
    Some(match value {
        V::Status(s) => match s {
            Status::Inbox => 1,
            Status::Next => 2,
            Status::Someday => 3,
            Status::InProgress => 4,
            Status::Done => 5,
            Status::Cancelled => 6,
        },
        V::StatusCategory(c) => match c {
            StatusCategory::Todo => 1,
            StatusCategory::InProgress => 2,
            StatusCategory::Done => 3,
        },
        V::ResolutionKind(r) => match r {
            ResolutionKind::Done => 1,
            ResolutionKind::Cancelled => 2,
        },
        V::TodoRefinement(r) => match r {
            TodoRefinement::Inbox => 1,
            TodoRefinement::Next => 2,
            TodoRefinement::Someday => 3,
        },
        V::CreatedVia(c) => match c {
            CreatedVia::Tui => 1,
            CreatedVia::Cli => 2,
            CreatedVia::Mcp => 3,
            CreatedVia::RemoteImport => 4,
            CreatedVia::Restore => 5,
            CreatedVia::Split => 6,
            CreatedVia::Recurrence => 7,
        },
        V::ArchiveReason(r) => match r {
            ArchiveReason::User => 1,
            ArchiveReason::LeftScope => 2,
            ArchiveReason::RemoteUnavailable => 3,
            ArchiveReason::AutoDone => 4,
        },
        V::LinkState(s) => match s {
            LinkState::Pending => 1,
            LinkState::Ok => 2,
            LinkState::Broken => 3,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use jiff::civil::date;
    use jiff::{SignedDuration, Timestamp};
    use serde_json::json;
    use ulid::Ulid;

    use super::*;
    use crate::item::{Archived, ItemRecurrence, RemoteLink, StalenessSignals, Waiting};
    use crate::status::StatusFields;

    use FieldValue as V;
    use FilterErrorKind::{Invalid as I, Unimplemented as U};
    use ItemField as F;
    use Operator as O;

    /// A Sunday: ISO week 2026-W43 runs from Monday 10-19 to here.
    const SUN: Date = date(2026, 10, 25);
    const MON: Date = date(2026, 10, 26);

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    fn id(n: u128) -> Ulid {
        Ulid::from(n)
    }

    /// Item `n`: id `n`, created at second `n`, in the inbox.
    fn item(n: u128) -> Item {
        Item::new_capture(id(n), format!("item {n}"), CreatedVia::Tui, ts(n as i64))
    }

    fn with(n: u128, change: impl FnOnce(&mut Item)) -> Item {
        let mut item = item(n);
        change(&mut item);
        item
    }

    fn set_status(item: &mut Item, status: Status) {
        item.set_status_fields(StatusFields::from_status(status));
    }

    /// Every field set, as in value.rs.
    fn full() -> Item {
        with(1, |item| {
            item.revision = 42;
            item.remote_link = Some(RemoteLink {
                tracker_kind: "jira".into(),
                instance_id: "work-jira".into(),
                remote_id: "10001".into(),
                key: "PLAT-530".into(),
                url: "https://jira.example.com/browse/PLAT-530".into(),
                last_pulled_version: Some(ts(3_000)),
                link_state: LinkState::Ok,
                pinned: true,
            });
            let core = &mut item.core;
            core.description = "**bold**".into();
            core.status_category = StatusCategory::Done;
            core.resolution_kind = Some(ResolutionKind::Cancelled);
            core.remote_status_name = Some("Won't Do".into());
            core.priority = Priority::High;
            core.assignee = Some(PersonRef::Person(id(2)));
            core.reporter = Some(PersonRef::Person(id(3)));
            core.labels = vec!["auth".into()];
            core.components = vec!["api".into()];
            core.fix_versions = vec!["1.2".into()];
            core.due_date = Some(date(2026, 10, 24));
            core.parent_id = Some(id(4));
            core.sprint = Some("Sprint 7".into());
            core.story_points = Some(3.5);
            core.original_estimate = Some(SignedDuration::from_hours(4));
            core.remaining_estimate = Some(SignedDuration::from_mins(90));
            core.resolution = Some("Won't Do".into());
            core.remote_created_at = Some(ts(4_000));
            core.remote_updated_at = Some(ts(5_000));
            let overlay = &mut item.overlay;
            overlay.waiting = Some(Waiting {
                on: PersonRef::FreeText("the vendor".into()),
                reason: "quote".into(),
                since: date(2026, 10, 20),
                follow_up_on: Some(date(2026, 10, 27)),
            });
            overlay.defer_until = Some(date(2026, 11, 1));
            overlay.focus_week = Some("2026-W43".into());
            overlay.local_priority_override = Some(Priority::Lowest);
            overlay.archived = Some(Archived {
                at: ts(6_000),
                reason: ArchiveReason::AutoDone,
            });
            overlay.recurrence = Some(ItemRecurrence {
                series_id: id(5),
                occurrence_date: Some(date(2026, 10, 21)),
            });
            let derived = &mut item.derived;
            derived.pending_fields = vec![F::Title];
            derived.staleness = StalenessSignals {
                last_user_touch_at: Some(ts(7_000)),
                last_remote_change_at: Some(ts(8_000)),
                status_entered_at: Some(ts(9_000)),
                waiting_since: Some(ts(10_000)),
            };
            derived.scopes = vec!["mine".into()];
            derived.deleted_at = Some(ts(11_000));
            derived.merged_into = Some(id(6));
        })
    }

    fn p(target: ItemField, op: Operator, operands: Vec<Operand>) -> Clause {
        Predicate::new(target, op, operands).into()
    }

    fn val(v: FieldValue) -> Operand {
        Operand::Value(v)
    }

    fn s(text: &str) -> Operand {
        val(V::String(text.into()))
    }

    fn rel(anchor: Anchor, offset_days: i32) -> Operand {
        RelativeDate::new(anchor, offset_days).into()
    }

    fn text(t: &str) -> Operand {
        Operand::Text(t.into())
    }

    fn status(s: Status) -> Operand {
        val(V::Status(s))
    }

    fn prio(p: Priority) -> Operand {
        val(V::Priority(p))
    }

    fn group(all_of: Vec<Clause>, any_of: Vec<Clause>) -> Clause {
        Group { all_of, any_of }.into()
    }

    fn all_of(clauses: Vec<Clause>) -> Filter {
        Filter {
            all_of: clauses,
            ..Filter::default()
        }
    }

    fn any_of(clauses: Vec<Clause>) -> Filter {
        Filter {
            any_of: clauses,
            ..Filter::default()
        }
    }

    /// The kind and path of `filter`'s error.
    fn outcome(filter: &Filter) -> Result<(), (FilterErrorKind, String)> {
        filter.validate().map_err(|e| (e.kind, e.path))
    }

    fn err(kind: FilterErrorKind, path: &str) -> Result<(), (FilterErrorKind, String)> {
        Err((kind, path.to_owned()))
    }

    // ---- catalogue ----

    #[test]
    fn the_catalogue_agrees_with_the_item() {
        let (bare, full) = (item(1), full());
        let mut searched = BTreeSet::new();
        for &field in ItemField::ALL {
            let t = target(field);
            if t.matches {
                searched.insert(field);
            }
            if field == F::RemoteExtras {
                assert_eq!(t.class, ValueClass::RemoteExtras);
                continue;
            }
            // A target can be unset exactly when a new Item has it unset.
            let unset = FieldValue::of(&bare, field) == Some(V::Cleared);
            assert_eq!(t.can_unset, unset, "{field:?}");
            let value = FieldValue::of(&full, field).unwrap();
            assert_ne!(value, V::Cleared, "{field:?}");
            // Value operands have the field's own variant, a string per set
            // member.
            let kind = match &value {
                V::StringSet(_) => "string",
                V::Waiting(_)
                | V::Archived(_)
                | V::Recurrence(_)
                | V::RemoteLink(_)
                | V::Staleness(_)
                | V::ItemFieldSet(_) => {
                    assert_eq!(value_kind(field), None, "{field:?}");
                    continue;
                }
                value => value.kind(),
            };
            assert_eq!(value_kind(field), Some(kind), "{field:?}");
            // Every sort key has an order.
            if t.sortable {
                assert_eq!(order(&value, &value), Some(Ordering::Equal), "{field:?}");
            }
            assert_eq!(
                t.class.is_ordered() && t.class != ValueClass::Priority,
                {
                    matches!(
                        value,
                        V::Date(_)
                            | V::IsoWeek(_)
                            | V::Timestamp(_)
                            | V::Number(_)
                            | V::Duration(_)
                            | V::Uint(_)
                    )
                },
                "{field:?}"
            );
        }
        let fts = [F::Title, F::Description, F::Labels, F::RemoteLinkKey];
        assert_eq!(searched, fts.into_iter().collect());
        for field in [
            F::ParentId,
            F::MergedInto,
            F::RecurrenceSeriesId,
            F::Labels,
            F::Waiting,
        ] {
            assert!(!target(field).sortable, "{field:?}");
        }
    }

    // ---- validation ----

    #[test]
    fn validation_by_value_class() {
        let ts_value = |t| val(V::Timestamp(t));
        let rows: Vec<(Clause, Option<(FilterErrorKind, &str)>)> = vec![
            // enum
            (p(F::Status, O::Eq, vec![status(Status::Inbox)]), None),
            (
                p(
                    F::Status,
                    O::In,
                    vec![status(Status::Inbox), status(Status::Next)],
                ),
                None,
            ),
            (p(F::Status, O::Ne, vec![status(Status::Done)]), None),
            (
                p(F::Status, O::Lt, vec![status(Status::Done)]),
                Some((I, ".op")),
            ),
            (p(F::Status, O::IsSet, vec![]), Some((I, ".op"))),
            (p(F::ResolutionKind, O::IsSet, vec![]), None),
            (
                p(F::Status, O::Eq, vec![prio(Priority::High)]),
                Some((I, ".operands[0]")),
            ),
            (p(F::Status, O::In, vec![]), Some((I, ".operands"))),
            (
                p(
                    F::Status,
                    O::Eq,
                    vec![status(Status::Inbox), status(Status::Next)],
                ),
                Some((I, ".operands")),
            ),
            (
                p(
                    F::ArchivedReason,
                    O::Eq,
                    vec![val(V::ArchiveReason(ArchiveReason::User))],
                ),
                None,
            ),
            (
                p(
                    F::CreatedVia,
                    O::ContainsAny,
                    vec![val(V::CreatedVia(CreatedVia::Cli))],
                ),
                Some((I, ".op")),
            ),
            // priority
            (p(F::Priority, O::Ge, vec![prio(Priority::High)]), None),
            (
                p(F::Priority, O::Lt, vec![prio(Priority::None)]),
                Some((I, ".operands[0]")),
            ),
            (p(F::Priority, O::Eq, vec![prio(Priority::None)]), None),
            (
                p(F::EffectivePriority, O::Ne, vec![prio(Priority::None)]),
                None,
            ),
            (p(F::Priority, O::IsSet, vec![]), Some((I, ".op"))),
            (p(F::LocalPriorityOverride, O::IsUnset, vec![]), None),
            // string
            (p(F::Title, O::Eq, vec![s("x")]), None),
            (p(F::Title, O::Lt, vec![s("x")]), Some((I, ".op"))),
            (p(F::Title, O::IsSet, vec![]), Some((I, ".op"))),
            (p(F::Sprint, O::IsSet, vec![]), None),
            (p(F::Title, O::Matches, vec![text("fix")]), None),
            (
                p(F::RemoteLinkKey, O::Matches, vec![text("plat-530")]),
                None,
            ),
            (
                p(F::ItemType, O::Matches, vec![text("fix")]),
                Some((I, ".target")),
            ),
            (
                p(F::Title, O::Eq, vec![val(V::Markdown("x".into()))]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::RecurrenceSeriesId, O::Eq, vec![s(&id(5).to_string())]),
                None,
            ),
            // id
            (p(F::Id, O::In, vec![val(V::ItemId(id(1)))]), None),
            (
                p(F::Id, O::Eq, vec![s(&id(1).to_string())]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::Id, O::Gt, vec![val(V::ItemId(id(1)))]),
                Some((I, ".op")),
            ),
            (p(F::Id, O::IsSet, vec![]), Some((I, ".op"))),
            (p(F::ParentId, O::IsUnset, vec![]), None),
            // markdown
            (
                p(F::Description, O::Eq, vec![val(V::Markdown("x".into()))]),
                None,
            ),
            (p(F::Description, O::Matches, vec![text("notes")]), None),
            (
                p(F::PrivateNotes, O::Matches, vec![text("notes")]),
                Some((I, ".target")),
            ),
            (p(F::Description, O::IsSet, vec![]), Some((I, ".op"))),
            (
                p(F::PrivateNotes, O::Eq, vec![s("x")]),
                Some((I, ".operands[0]")),
            ),
            // date
            (p(F::DueDate, O::Le, vec![val(V::Date(SUN))]), None),
            (p(F::DueDate, O::Le, vec![rel(Anchor::Today, 0)]), None),
            (
                p(
                    F::DeferUntil,
                    O::In,
                    vec![rel(Anchor::StartOfWeek, 0), rel(Anchor::EndOfWeek, -7)],
                ),
                None,
            ),
            (
                p(F::DueDate, O::Eq, vec![rel(Anchor::CurrentWeek, 0)]),
                Some((I, ".operands[0]")),
            ),
            (p(F::WaitingFollowUpOn, O::IsUnset, vec![]), None),
            (
                p(F::DueDate, O::Eq, vec![val(V::Date(date(0, 12, 31)))]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::DueDate, O::Eq, vec![ts_value(ts(0))]),
                Some((I, ".operands[0]")),
            ),
            // focus_week
            (
                p(F::FocusWeek, O::Eq, vec![rel(Anchor::CurrentWeek, 0)]),
                None,
            ),
            (
                p(
                    F::FocusWeek,
                    O::Ge,
                    vec![val(V::IsoWeek("2026-W43".into()))],
                ),
                None,
            ),
            (
                p(F::FocusWeek, O::Eq, vec![rel(Anchor::Today, 0)]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::FocusWeek, O::Eq, vec![rel(Anchor::CurrentWeek, 1)]),
                Some((I, ".operands[0].relative_date.offset_days")),
            ),
            (
                p(
                    F::FocusWeek,
                    O::Eq,
                    vec![val(V::IsoWeek("2025-W53".into()))],
                ),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::FocusWeek, O::Eq, vec![s("2026-W43")]),
                Some((I, ".operands[0]")),
            ),
            // instant
            (p(F::CreatedAt, O::Ge, vec![ts_value(ts(1_000))]), None),
            (
                p(F::CreatedAt, O::Ge, vec![rel(Anchor::Today, -14)]),
                Some((U, ".operands[0]")),
            ),
            (
                p(F::ArchivedAt, O::Ge, vec![rel(Anchor::CurrentWeek, 0)]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::CreatedAt, O::Lt, vec![ts_value(Timestamp::MAX)]),
                Some((I, ".operands[0]")),
            ),
            (p(F::CreatedAt, O::IsSet, vec![]), Some((I, ".op"))),
            (p(F::DeletedAt, O::IsSet, vec![]), None),
            // number, duration, uint
            (p(F::StoryPoints, O::Ge, vec![val(V::Number(1.0))]), None),
            (
                p(F::StoryPoints, O::Lt, vec![val(V::Number(f64::NAN))]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::StoryPoints, O::Eq, vec![val(V::Uint(3))]),
                Some((I, ".operands[0]")),
            ),
            (p(F::StoryPoints, O::IsSet, vec![]), None),
            (
                p(
                    F::OriginalEstimate,
                    O::Lt,
                    vec![val(V::Duration(SignedDuration::from_hours(4)))],
                ),
                None,
            ),
            (
                p(
                    F::OriginalEstimate,
                    O::Lt,
                    vec![val(V::Duration(SignedDuration::from_hours(300 * 8766)))],
                ),
                Some((I, ".operands[0]")),
            ),
            (p(F::Revision, O::Gt, vec![val(V::Uint(5))]), None),
            (
                p(F::Revision, O::Gt, vec![val(V::Uint(u64::MAX))]),
                Some((I, ".operands[0]")),
            ),
            (p(F::Revision, O::IsSet, vec![]), Some((I, ".op"))),
            // bool
            (p(F::IsOverdue, O::Eq, vec![val(V::Bool(true))]), None),
            (
                p(F::IsOverdue, O::Lt, vec![val(V::Bool(true))]),
                Some((I, ".op")),
            ),
            (p(F::IsOverdue, O::IsSet, vec![]), Some((I, ".op"))),
            (p(F::RemoteLinkPinned, O::IsSet, vec![]), None),
            // set
            (p(F::Labels, O::ContainsAny, vec![s("auth")]), None),
            (p(F::Scopes, O::ContainsAll, vec![s("a"), s("a")]), None),
            (p(F::Labels, O::Eq, vec![s("auth")]), Some((I, ".op"))),
            (p(F::Labels, O::Ge, vec![s("a")]), Some((I, ".op"))),
            (p(F::Labels, O::IsSet, vec![]), None),
            (p(F::Components, O::IsUnset, vec![]), None),
            (
                p(
                    F::Labels,
                    O::ContainsAll,
                    vec![val(V::StringSet(vec!["a".into()]))],
                ),
                Some((I, ".operands[0]")),
            ),
            (p(F::Labels, O::ContainsAny, vec![]), Some((I, ".operands"))),
            (p(F::Labels, O::Matches, vec![text("auth")]), None),
            (
                p(F::Components, O::Matches, vec![text("api")]),
                Some((I, ".target")),
            ),
            // person
            (
                p(
                    F::WaitingOn,
                    O::Eq,
                    vec![val(V::Person(PersonRef::FreeText("the vendor".into())))],
                ),
                None,
            ),
            (
                p(
                    F::Assignee,
                    O::Eq,
                    vec![val(V::Person(PersonRef::Person(id(2))))],
                ),
                None,
            ),
            (
                p(
                    F::Assignee,
                    O::Eq,
                    vec![val(V::Person(PersonRef::FreeText("dana".into())))],
                ),
                Some((I, ".operands[0]")),
            ),
            (
                p(
                    F::Reporter,
                    O::Lt,
                    vec![val(V::Person(PersonRef::Person(id(2))))],
                ),
                Some((I, ".op")),
            ),
            (p(F::Assignee, O::IsUnset, vec![]), None),
            // message presence
            (p(F::Waiting, O::IsSet, vec![]), None),
            (p(F::Staleness, O::IsUnset, vec![]), None),
            (
                p(F::Archived, O::Eq, vec![val(V::Bool(true))]),
                Some((I, ".op")),
            ),
            (
                p(F::Waiting, O::IsSet, vec![val(V::Bool(true))]),
                Some((I, ".operands")),
            ),
            // pending_fields and remote_extras
            (p(F::PendingFields, O::IsSet, vec![]), None),
            (
                p(
                    F::PendingFields,
                    O::ContainsAny,
                    vec![val(V::ItemFieldSet(vec![F::Title]))],
                ),
                Some((U, ".op")),
            ),
            (
                p(
                    F::PendingFields,
                    O::Eq,
                    vec![val(V::ItemFieldSet(vec![F::Title]))],
                ),
                Some((I, ".op")),
            ),
            (p(F::RemoteExtras, O::IsSet, vec![]), Some((U, ".target"))),
            // Not built, but the operand count is still wrong.
            (p(F::RemoteExtras, O::Eq, vec![]), Some((I, ".operands"))),
            (
                p(F::RemoteExtras, O::Matches, vec![text("x")]),
                Some((I, ".target")),
            ),
            // An operand no target takes is invalid, built or not.
            (
                p(F::RemoteExtras, O::Eq, vec![val(V::Cleared)]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::RemoteExtras, O::In, vec![text("x")]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::PendingFields, O::ContainsAll, vec![val(V::Cleared)]),
                Some((I, ".operands[0]")),
            ),
            // operand forms
            (
                p(F::DueDate, O::Eq, vec![val(V::Cleared)]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::Title, O::Eq, vec![text("x")]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::Title, O::Eq, vec![rel(Anchor::Today, 0)]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::Title, O::Matches, vec![s("x")]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::Title, O::Matches, vec![text("")]),
                Some((I, ".operands[0]")),
            ),
            (
                p(F::Title, O::Matches, vec![text(" \t\u{1}\n ")]),
                Some((I, ".operands[0]")),
            ),
            // No-break space is above U+0020: the text has a word.
            (p(F::Title, O::Matches, vec![text("\u{a0}")]), None),
            (
                p(F::Title, O::Matches, vec![text("a"), text("b")]),
                Some((I, ".operands")),
            ),
        ];
        for (clause, want) in rows {
            let filter = all_of(vec![clause.clone()]);
            let want = match want {
                None => Ok(()),
                Some((kind, suffix)) => err(kind, &format!("all_of[0].predicate{suffix}")),
            };
            assert_eq!(outcome(&filter), want, "{clause:?}");
        }
    }

    #[test]
    fn limits() {
        // A chain of `depth` groups, the innermost holding one predicate.
        let nested = |depth: usize| {
            let mut clause = p(F::Status, O::Eq, vec![status(Status::Inbox)]);
            for _ in 0..depth {
                clause = group(vec![clause], vec![]);
            }
            all_of(vec![clause])
        };
        assert_eq!(outcome(&nested(MAX_GROUP_DEPTH)), Ok(()));
        let deepest = ["all_of[0].group"; MAX_GROUP_DEPTH + 1].join(".");
        assert_eq!(outcome(&nested(MAX_GROUP_DEPTH + 1)), err(I, &deepest));

        let eq = || p(F::Title, O::Eq, vec![s("x")]);
        let many = |n| any_of((0..n).map(|_| eq()).collect());
        assert_eq!(outcome(&many(MAX_PREDICATES)), Ok(()));
        assert_eq!(outcome(&many(MAX_PREDICATES + 1)), err(I, ""));

        let titles = |n: usize| p(F::Title, O::In, (0..n).map(|i| s(&i.to_string())).collect());
        assert_eq!(
            outcome(&all_of(vec![titles(MAX_PREDICATE_OPERANDS)])),
            Ok(())
        );
        assert_eq!(
            outcome(&all_of(vec![titles(MAX_PREDICATE_OPERANDS + 1)])),
            err(I, "all_of[0].predicate.operands")
        );
        let per = MAX_OPERANDS / MAX_PREDICATE_OPERANDS;
        let full: Vec<_> = (0..per).map(|_| titles(MAX_PREDICATE_OPERANDS)).collect();
        assert_eq!(outcome(&all_of(full.clone())), Ok(()));
        let over = [full, vec![eq()]].concat();
        assert_eq!(outcome(&all_of(over)), err(I, ""));

        let search = |n| all_of(vec![p(F::Title, O::Matches, vec![text(&"é".repeat(n))])]);
        assert_eq!(outcome(&search(MAX_MATCHES_BYTES / 2)), Ok(()));
        assert_eq!(
            outcome(&search(MAX_MATCHES_BYTES / 2 + 1)),
            err(I, "all_of[0].predicate.operands[0]")
        );

        // resolve leaves the limits to validate: a request that narrows a
        // view is one group deeper than either.
        for filter in [
            nested(MAX_GROUP_DEPTH + 1),
            many(MAX_PREDICATES + 1),
            all_of(vec![titles(MAX_PREDICATE_OPERANDS + 1)]),
            search(MAX_MATCHES_BYTES),
        ] {
            assert!(filter.resolve(SUN).is_ok());
        }
    }

    #[test]
    fn sorts() {
        let keys = |keys: &[(ItemField, bool)]| -> Vec<SortKey> {
            keys.iter()
                .map(|&(key, descending)| SortKey { key, descending })
                .collect()
        };
        let check = |k: &[(ItemField, bool)]| validate_sort(&keys(k)).map_err(|e| (e.kind, e.path));
        assert_eq!(check(&[]), Ok(()));
        assert_eq!(
            check(&[
                (F::IsOverdue, true),
                (F::EffectivePriority, true),
                (F::DueDate, false)
            ]),
            Ok(())
        );
        for unsortable in [
            F::Labels,
            F::Description,
            F::Assignee,
            F::Waiting,
            F::ParentId,
            F::PendingFields,
            F::RemoteExtras,
        ] {
            assert_eq!(
                check(&[(F::Title, false), (unsortable, false)]),
                err(I, "sort[1].key"),
                "{unsortable:?}"
            );
        }
        assert_eq!(
            check(&[(F::DueDate, false), (F::DueDate, true)]),
            err(I, "sort[1].key")
        );
        let eight = [
            F::Status,
            F::Priority,
            F::DueDate,
            F::DeferUntil,
            F::CreatedAt,
            F::UpdatedAt,
            F::Title,
            F::Id,
        ];
        let eight: Vec<_> = eight.into_iter().map(|f| (f, false)).collect();
        assert_eq!(check(&eight), Ok(()));
        let nine = [eight.clone(), vec![(F::Revision, false)]].concat();
        assert_eq!(check(&nine), err(I, "sort"));
        // Each key is checked before the count.
        let nine = [eight, vec![(F::Labels, false)]].concat();
        assert_eq!(check(&nine), err(I, "sort[8].key"));
    }

    #[test]
    fn errors_come_in_document_order() {
        let ok = || p(F::Status, O::In, vec![status(Status::Inbox)]);
        let bad_op = || p(F::Title, O::Lt, vec![s("x")]);
        let unbuilt = || p(F::RemoteExtras, O::IsSet, vec![]);
        // C51: all_of before any_of.
        let filter = Filter {
            all_of: vec![ok(), bad_op()],
            any_of: vec![p(F::Labels, O::Eq, vec![s("x")])],
            ..Filter::default()
        };
        assert_eq!(outcome(&filter), err(I, "all_of[1].predicate.op"));
        // INVALID wins over an earlier UNIMPLEMENTED, which is reported
        // only when nothing is invalid: the first one.
        let filter = all_of(vec![unbuilt(), bad_op()]);
        assert_eq!(outcome(&filter), err(I, "all_of[1].predicate.op"));
        let filter = all_of(vec![
            ok(),
            unbuilt(),
            p(F::CreatedAt, O::Gt, vec![rel(Anchor::Today, 0)]),
        ]);
        assert_eq!(outcome(&filter), err(U, "all_of[1].predicate.target"));
        // Depth-first, with the paths of nested groups.
        let filter = Filter {
            all_of: vec![ok(), group(vec![ok()], vec![bad_op(), ok()])],
            any_of: vec![bad_op()],
            ..Filter::default()
        };
        assert_eq!(
            outcome(&filter),
            err(I, "all_of[1].group.any_of[0].predicate.op")
        );
        // In a predicate: target, op, operand count, then each operand.
        let rows = [
            (p(F::Status, O::Matches, vec![]), ".target"),
            (p(F::Status, O::Lt, vec![]), ".op"),
            (p(F::Status, O::Eq, vec![text("x"), text("y")]), ".operands"),
            (
                p(F::Status, O::In, vec![text("x"), val(V::Cleared)]),
                ".operands[0]",
            ),
            (
                p(
                    F::Status,
                    O::In,
                    vec![status(Status::Next), val(V::Cleared)],
                ),
                ".operands[1]",
            ),
        ];
        for (clause, suffix) in rows {
            let path = format!("all_of[0].predicate{suffix}");
            assert_eq!(outcome(&all_of(vec![clause])), err(I, &path), "{suffix}");
        }
        // The totals come after every clause, and before UNIMPLEMENTED.
        let mut clauses: Vec<_> = (0..MAX_PREDICATES).map(|_| ok()).collect();
        clauses.push(bad_op());
        assert_eq!(
            outcome(&any_of(clauses.clone())),
            err(I, "any_of[128].predicate.op")
        );
        clauses[MAX_PREDICATES] = unbuilt();
        assert_eq!(outcome(&any_of(clauses)), err(I, ""));
    }

    #[test]
    fn relative_dates_at_week_and_year_boundaries() {
        use Anchor as A;
        let day = |y, m, d| Some(V::Date(date(y, m, d)));
        let week = |w: &str| Some(V::IsoWeek(w.into()));
        let rows = [
            (A::Today, 0, SUN, day(2026, 10, 25)),
            (A::Today, -1, SUN, day(2026, 10, 24)),
            (A::Today, 7, SUN, day(2026, 11, 1)),
            // Weeks run Monday to Sunday.
            (A::StartOfWeek, 0, SUN, day(2026, 10, 19)),
            (A::EndOfWeek, 0, SUN, day(2026, 10, 25)),
            (A::StartOfWeek, 0, MON, day(2026, 10, 26)),
            (A::EndOfWeek, 0, MON, day(2026, 11, 1)),
            (A::EndOfWeek, 1, MON, day(2026, 11, 2)),
            (A::StartOfWeek, -7, MON, day(2026, 10, 19)),
            (A::StartOfWeek, 0, date(2027, 1, 1), day(2026, 12, 28)),
            (A::EndOfWeek, 0, date(2027, 1, 1), day(2027, 1, 3)),
            (A::StartOfWeek, 0, date(2024, 12, 30), day(2024, 12, 30)),
            (A::EndOfWeek, 0, date(2024, 12, 30), day(2025, 1, 5)),
            // The ISO week-year, not the calendar year.
            (A::CurrentWeek, 0, SUN, week("2026-W43")),
            (A::CurrentWeek, 0, MON, week("2026-W44")),
            (A::CurrentWeek, 0, date(2027, 1, 1), week("2026-W53")),
            (A::CurrentWeek, 0, date(2027, 1, 4), week("2027-W01")),
            (A::CurrentWeek, 0, date(2024, 12, 30), week("2025-W01")),
            (A::CurrentWeek, 0, date(2021, 1, 3), week("2020-W53")),
            (A::CurrentWeek, 1, SUN, None),
            // Out of range.
            (A::Today, 1, date(9999, 12, 31), None),
            (A::EndOfWeek, 0, date(9999, 12, 31), None),
            (A::Today, -1, date(1, 1, 1), None),
            (A::Today, i32::MAX, SUN, None),
            (A::Today, i32::MIN, SUN, None),
            (A::Today, 2_912_145, SUN, day(9999, 12, 31)),
            (A::Today, 2_912_146, SUN, None),
        ];
        for (anchor, offset, today, want) in rows {
            let got = RelativeDate::new(anchor, offset).resolve(today);
            assert_eq!(got, want, "{anchor:?} {offset} on {today}");
        }
    }

    #[test]
    fn resolve_fixes_relative_dates_and_checks_offsets() {
        let filter = Filter {
            all_of: vec![p(F::DueDate, O::Le, vec![rel(Anchor::EndOfWeek, 0)])],
            any_of: vec![
                p(F::FocusWeek, O::Eq, vec![rel(Anchor::CurrentWeek, 0)]),
                group(
                    vec![p(
                        F::DeferUntil,
                        O::In,
                        vec![rel(Anchor::Today, -1), val(V::Date(SUN))],
                    )],
                    vec![],
                ),
                p(F::Title, O::Matches, vec![text("fix")]),
            ],
            include_done: true,
            ..Filter::default()
        };
        let resolved = filter.resolve(MON).unwrap();
        let value = |v| ResolvedOperand::Value(v);
        let want = Filter {
            all_of: vec![
                Predicate::new(F::DueDate, O::Le, vec![value(V::Date(date(2026, 11, 1)))]).into(),
            ],
            any_of: vec![
                Predicate::new(
                    F::FocusWeek,
                    O::Eq,
                    vec![value(V::IsoWeek("2026-W44".into()))],
                )
                .into(),
                Group {
                    all_of: vec![
                        Predicate::new(
                            F::DeferUntil,
                            O::In,
                            vec![value(V::Date(SUN)), value(V::Date(SUN))],
                        )
                        .into(),
                    ],
                    any_of: vec![],
                }
                .into(),
                Predicate::new(
                    F::Title,
                    O::Matches,
                    vec![ResolvedOperand::Text("fix".into())],
                )
                .into(),
            ],
            include_done: true,
            ..Filter::default()
        };
        assert_eq!(resolved.filter(), &want);

        // An offset past the calendar is caught on resolving, in its place.
        let filter = all_of(vec![
            p(F::DueDate, O::Eq, vec![rel(Anchor::Today, i32::MAX)]),
            p(F::Title, O::Lt, vec![s("x")]),
        ]);
        assert_eq!(outcome(&filter), err(I, "all_of[1].predicate.op"));
        let e = filter.resolve(SUN).unwrap_err();
        assert_eq!(
            (e.kind, e.path.as_str()),
            (
                I,
                "all_of[0].predicate.operands[0].relative_date.offset_days"
            )
        );
        // So does validating on a today, which keeps the limits too.
        let e = filter.validate_on(SUN).unwrap_err();
        assert_eq!(
            e.path,
            "all_of[0].predicate.operands[0].relative_date.offset_days"
        );
        let deep = (0..=MAX_GROUP_DEPTH).fold(p(F::Title, O::Eq, vec![s("x")]), |c, _| {
            group(vec![c], vec![])
        });
        let filter = all_of(vec![deep]);
        assert!(filter.resolve(SUN).is_ok());
        let deepest = ["all_of[0].group"; MAX_GROUP_DEPTH + 1].join(".");
        assert_eq!(filter.validate_on(SUN).unwrap_err().path, deepest);
        let filter = all_of(vec![p(F::DueDate, O::Eq, vec![rel(Anchor::Today, 1)])]);
        assert!(filter.resolve(SUN).is_ok());
        assert!(filter.validate_on(SUN).is_ok());
        assert!(filter.resolve(date(9999, 12, 31)).is_err());
        assert!(filter.validate_on(date(9999, 12, 31)).is_err());
        // Nothing unbuilt resolves.
        let filter = all_of(vec![p(F::RemoteExtras, O::IsSet, vec![])]);
        assert_eq!(filter.resolve(SUN).unwrap_err().kind, U);
    }

    /// One today per request: the relative dates, the defer rule and
    /// is_overdue all use the day the filter was resolved for.
    #[test]
    fn a_resolved_filter_keeps_its_today() {
        let filter = all_of(vec![
            p(F::DueDate, O::Le, vec![rel(Anchor::Today, 0)]),
            p(F::IsOverdue, O::Eq, vec![val(V::Bool(false))]),
        ]);
        // Due and deferred to Monday: on Sunday neither due nor shown.
        let monday = with(1, |i| {
            i.core.due_date = Some(MON);
            i.overlay.defer_until = Some(MON);
        });
        let sunday = filter.resolve(SUN).unwrap();
        assert_eq!(sunday.today(), SUN);
        assert!(!matches(&monday, &sunday));
        let on_monday = filter.resolve(MON).unwrap();
        assert_eq!(on_monday.today(), MON);
        assert!(matches(&monday, &on_monday));
    }

    // ---- the evaluator ----

    /// The ids of `items` that `filter` matches on `today`.
    fn ids(items: &[Item], filter: &Filter, today: Date) -> Vec<u128> {
        let resolved = filter.resolve(today).unwrap();
        items
            .iter()
            .filter(|i| matches(i, &resolved))
            .map(|i| u128::from(i.id))
            .collect()
    }

    #[test]
    fn implicit_rules_and_flags() {
        let items = [
            item(1),
            with(2, |i| i.derived.deleted_at = Some(ts(5))),
            with(3, |i| {
                i.overlay.archived = Some(Archived {
                    at: ts(5),
                    reason: ArchiveReason::User,
                });
            }),
            with(4, |i| set_status(i, Status::Done)),
            with(5, |i| set_status(i, Status::Cancelled)),
            with(6, |i| i.overlay.defer_until = Some(SUN)),
            with(7, |i| i.overlay.defer_until = Some(MON)),
        ];
        let flags = |archived, done, deferred, deleted| Filter {
            include_archived: archived,
            include_done: done,
            include_deferred: deferred,
            include_deleted: deleted,
            ..Filter::default()
        };
        assert_eq!(ids(&items, &Filter::default(), SUN), [1, 6]);
        assert_eq!(ids(&items, &Filter::default(), MON), [1, 6, 7]);
        assert_eq!(
            ids(&items, &flags(true, false, false, false), SUN),
            [1, 3, 6]
        );
        assert_eq!(
            ids(&items, &flags(false, true, false, false), SUN),
            [1, 4, 5, 6]
        );
        assert_eq!(
            ids(&items, &flags(false, false, true, false), SUN),
            [1, 6, 7]
        );
        assert_eq!(
            ids(&items, &flags(false, false, false, true), SUN),
            [1, 2, 6]
        );
        assert_eq!(
            ids(&items, &flags(true, true, true, true), SUN),
            [1, 2, 3, 4, 5, 6, 7]
        );
        // Clauses never lift a rule: only the flags do.
        let done = all_of(vec![p(F::Status, O::In, vec![status(Status::Done)])]);
        assert_eq!(ids(&items, &done, SUN), [] as [u128; 0]);
        let done = Filter {
            include_done: true,
            ..done
        };
        assert_eq!(ids(&items, &done, SUN), [4]);
        let deleted = all_of(vec![p(F::DeletedAt, O::IsSet, vec![])]);
        assert_eq!(ids(&items, &deleted, SUN), [] as [u128; 0]);
    }

    #[test]
    fn unset_values_match_only_ne() {
        let waiting = |reason: &str| Waiting {
            on: PersonRef::FreeText("the vendor".into()),
            reason: reason.into(),
            since: date(2026, 10, 20),
            follow_up_on: None,
        };
        let items = [
            item(1),
            with(2, |i| {
                i.core.due_date = Some(SUN);
                i.overlay.waiting = Some(waiting(""));
            }),
            with(3, |i| {
                i.core.due_date = Some(MON);
                i.overlay.waiting = Some(waiting("spec"));
            }),
        ];
        let one = |field, op, operand| all_of(vec![p(field, op, vec![operand])]);
        let due = || val(V::Date(SUN));
        assert_eq!(ids(&items, &one(F::DueDate, O::Eq, due()), SUN), [2]);
        assert_eq!(ids(&items, &one(F::DueDate, O::Ne, due()), SUN), [1, 3]);
        assert_eq!(ids(&items, &one(F::DueDate, O::Ge, due()), SUN), [2, 3]);
        assert_eq!(
            ids(&items, &one(F::DueDate, O::Lt, val(V::Date(MON))), SUN),
            [2]
        );
        // A nested value under a present parent is set, "" included.
        assert_eq!(ids(&items, &one(F::WaitingReason, O::Eq, s("")), SUN), [2]);
        assert_eq!(
            ids(&items, &one(F::WaitingReason, O::Ne, s("spec")), SUN),
            [1, 2]
        );
        let is = |field, op| all_of(vec![p(field, op, vec![])]);
        assert_eq!(ids(&items, &is(F::WaitingReason, O::IsSet), SUN), [2, 3]);
        assert_eq!(
            ids(&items, &is(F::WaitingFollowUpOn, O::IsUnset), SUN),
            [1, 2, 3]
        );
        assert_eq!(ids(&items, &is(F::Waiting, O::IsUnset), SUN), [1]);
        // Free text is exact.
        let on = |text: &str| {
            one(
                F::WaitingOn,
                O::Eq,
                val(V::Person(PersonRef::FreeText(text.into()))),
            )
        };
        assert_eq!(ids(&items, &on("the vendor"), SUN), [2, 3]);
        assert_eq!(ids(&items, &on("The Vendor"), SUN), [] as [u128; 0]);
        // An unlinked Item's pinned is unset, not false.
        let linked = with(4, |i| {
            i.remote_link = Some(RemoteLink {
                tracker_kind: "jira".into(),
                instance_id: "work-jira".into(),
                remote_id: "10099".into(),
                key: "PLAT-99".into(),
                url: String::new(),
                last_pulled_version: None,
                link_state: LinkState::Ok,
                pinned: false,
            });
        });
        let items = [item(1), linked];
        let pinned = |b| one(F::RemoteLinkPinned, O::Eq, val(V::Bool(b)));
        assert_eq!(ids(&items, &pinned(false), SUN), [4]);
        let not_pinned = one(F::RemoteLinkPinned, O::Ne, val(V::Bool(true)));
        assert_eq!(ids(&items, &not_pinned, SUN), [1, 4]);
    }

    #[test]
    fn sets_compare_members_exactly() {
        let labels = |n, labels: &[&str]| {
            with(n, |i| {
                i.core.labels = labels.iter().map(|&l| l.to_owned()).collect()
            })
        };
        let items = [
            labels(1, &["auth", "oncall"]),
            labels(2, &["Auth"]),
            labels(3, &[]),
        ];
        let has = |op, members: &[&str]| {
            all_of(vec![p(
                F::Labels,
                op,
                members.iter().map(|m| s(m)).collect(),
            )])
        };
        assert_eq!(ids(&items, &has(O::ContainsAny, &["auth"]), SUN), [1]);
        assert_eq!(
            ids(&items, &has(O::ContainsAny, &["Auth", "nope"]), SUN),
            [2]
        );
        assert_eq!(
            ids(
                &items,
                &has(O::ContainsAll, &["auth", "auth", "oncall"]),
                SUN
            ),
            [1]
        );
        assert_eq!(
            ids(&items, &has(O::ContainsAll, &["auth", "nope"]), SUN),
            [] as [u128; 0]
        );
        assert_eq!(ids(&items, &has(O::IsSet, &[]), SUN), [1, 2]);
        assert_eq!(ids(&items, &has(O::IsUnset, &[]), SUN), [3]);
    }

    #[test]
    fn priorities_order_by_rank_without_none() {
        let prioritized = |n, priority, override_: Option<Priority>| {
            with(n, |i| {
                i.core.priority = priority;
                i.overlay.local_priority_override = override_;
            })
        };
        let items = [
            prioritized(1, Priority::High, None),
            prioritized(2, Priority::Highest, Some(Priority::Low)),
            prioritized(3, Priority::None, None),
            prioritized(4, Priority::Lowest, None),
            prioritized(5, Priority::None, Some(Priority::Medium)),
        ];
        let one = |field, op, level| all_of(vec![p(field, op, vec![prio(level)])]);
        assert_eq!(
            ids(
                &items,
                &one(F::EffectivePriority, O::Ge, Priority::High),
                SUN
            ),
            [1]
        );
        assert_eq!(
            ids(&items, &one(F::Priority, O::Ge, Priority::High), SUN),
            [1, 2]
        );
        // NONE is outside the order: never LT, never GE.
        assert_eq!(
            ids(
                &items,
                &one(F::EffectivePriority, O::Lt, Priority::Medium),
                SUN
            ),
            [2, 4]
        );
        assert_eq!(
            ids(&items, &one(F::Priority, O::Le, Priority::Highest), SUN),
            [1, 2, 4]
        );
        // EQ, IN and NE treat NONE as a value.
        assert_eq!(
            ids(&items, &one(F::Priority, O::Eq, Priority::None), SUN),
            [3, 5]
        );
        assert_eq!(
            ids(
                &items,
                &one(F::EffectivePriority, O::Ne, Priority::None),
                SUN
            ),
            [1, 2, 4, 5]
        );
        assert_eq!(
            ids(
                &items,
                &one(F::LocalPriorityOverride, O::Gt, Priority::Low),
                SUN
            ),
            [5]
        );
    }

    #[test]
    fn derived_targets_use_today() {
        let due = |n, d, status| {
            with(n, |i| {
                i.core.due_date = Some(d);
                set_status(i, status);
            })
        };
        let items = [
            due(1, date(2026, 10, 24), Status::Inbox),
            due(2, SUN, Status::Next),
            due(3, date(2026, 10, 1), Status::Done),
            due(4, date(2026, 10, 1), Status::Cancelled),
            // A stale stored flag is not read.
            with(5, |i| i.derived.is_overdue = true),
            with(6, |i| {
                i.core.due_date = Some(date(2026, 10, 20));
                i.overlay.archived = Some(Archived {
                    at: ts(5),
                    reason: ArchiveReason::User,
                });
                i.overlay.defer_until = Some(date(2026, 12, 1));
            }),
            due(7, date(2026, 10, 24), Status::InProgress),
        ];
        let overdue = Filter {
            all_of: vec![p(F::IsOverdue, O::Eq, vec![val(V::Bool(true))])],
            include_done: true,
            include_archived: true,
            include_deferred: true,
            ..Filter::default()
        };
        assert_eq!(ids(&items, &overdue, SUN), [1, 6, 7]);
        assert_eq!(ids(&items, &overdue, MON), [1, 2, 6, 7]);
        let in_progress = all_of(vec![p(F::Status, O::Eq, vec![status(Status::InProgress)])]);
        assert_eq!(ids(&items, &in_progress, SUN), [7]);
        let this_week = all_of(vec![p(
            F::FocusWeek,
            O::Eq,
            vec![rel(Anchor::CurrentWeek, 0)],
        )]);
        let focused = [
            with(1, |i| i.overlay.focus_week = Some("2026-W43".into())),
            with(2, |i| i.overlay.focus_week = Some("2026-W44".into())),
            item(3),
        ];
        assert_eq!(ids(&focused, &this_week, SUN), [1]);
        assert_eq!(ids(&focused, &this_week, MON), [2]);
        let other_weeks = all_of(vec![p(
            F::FocusWeek,
            O::Ne,
            vec![rel(Anchor::CurrentWeek, 0)],
        )]);
        assert_eq!(ids(&focused, &other_weeks, SUN), [2, 3]);
        let by_week = all_of(vec![p(
            F::FocusWeek,
            O::Ge,
            vec![val(V::IsoWeek("2026-W44".into()))],
        )]);
        assert_eq!(ids(&focused, &by_week, SUN), [2]);
    }

    #[test]
    fn groups_and_lists() {
        let items: Vec<Item> = [Status::Inbox, Status::Next, Status::Someday]
            .into_iter()
            .zip(1..)
            .map(|(s, n)| {
                with(n, |i| {
                    set_status(i, s);
                    i.core.priority = Priority::Medium;
                })
            })
            .chain([with(4, |i| set_status(i, Status::Next))])
            .collect();
        let is = |s| p(F::Status, O::Eq, vec![status(s)]);
        let medium = || p(F::Priority, O::Ge, vec![prio(Priority::Medium)]);
        // C47: nested groups.
        let filter = all_of(vec![group(
            vec![],
            vec![
                is(Status::Inbox),
                group(vec![is(Status::Next), medium()], vec![]),
            ],
        )]);
        assert_eq!(ids(&items, &filter, SUN), [1, 2]);
        // Every all_of and one any_of.
        let filter = Filter {
            all_of: vec![medium()],
            any_of: vec![is(Status::Next), is(Status::Someday)],
            ..Filter::default()
        };
        assert_eq!(ids(&items, &filter, SUN), [2, 3]);
        // Empty groups and lists are vacuous.
        assert_eq!(
            ids(&items, &all_of(vec![group(vec![], vec![])]), SUN),
            [1, 2, 3, 4]
        );
        assert_eq!(
            ids(
                &items,
                &any_of(vec![group(vec![], vec![]), is(Status::Inbox)]),
                SUN
            ),
            [1, 2, 3, 4]
        );
        // MATCHES needs the full-text index; the reference never matches it.
        let search = all_of(vec![p(F::Title, O::Matches, vec![text("item")])]);
        assert_eq!(ids(&items, &search, SUN), [] as [u128; 0]);
    }

    #[test]
    fn numbers_and_instants() {
        let items = [
            with(1, |i| i.core.story_points = Some(3.0)),
            with(2, |i| i.core.story_points = Some(0.5)),
            item(3),
        ];
        let points = |op, n| all_of(vec![p(F::StoryPoints, op, vec![val(V::Number(n))])]);
        assert_eq!(ids(&items, &points(O::Ge, 1.0), SUN), [1]);
        assert_eq!(ids(&items, &points(O::Lt, 1.0), SUN), [2]);
        assert_eq!(ids(&items, &points(O::Ne, 3.0), SUN), [2, 3]);
        // One nanosecond apart.
        let at = Timestamp::new(1_000, 1).unwrap();
        let items = [
            with(1, |i| i.created_at = Timestamp::new(1_000, 0).unwrap()),
            with(2, |i| i.created_at = at),
        ];
        let created = |op| all_of(vec![p(F::CreatedAt, op, vec![val(V::Timestamp(at))])]);
        assert_eq!(ids(&items, &created(O::Lt), SUN), [1]);
        assert_eq!(ids(&items, &created(O::Ge), SUN), [2]);
    }

    // ---- the sort order ----

    /// The ids of `items` sorted by `keys` on `today`.
    fn sorted(items: &[Item], keys: &[SortKey], today: Date) -> Vec<u128> {
        let mut items = items.to_vec();
        items.sort_by(|a, b| compare(a, b, keys, today));
        items.iter().map(|i| u128::from(i.id)).collect()
    }

    #[test]
    fn unset_sorts_last_both_ways() {
        let items = [
            item(1),
            with(2, |i| i.core.due_date = Some(MON)),
            with(3, |i| i.core.due_date = Some(SUN)),
            item(4),
        ];
        assert_eq!(
            sorted(&items, &[SortKey::asc(F::DueDate)], SUN),
            [3, 2, 1, 4]
        );
        assert_eq!(
            sorted(&items, &[SortKey::desc(F::DueDate)], SUN),
            [2, 3, 1, 4]
        );
    }

    #[test]
    fn priority_sorts_by_rank_with_none_last() {
        let prioritized = |n, p| with(n, |i| i.core.priority = p);
        let items = [
            prioritized(1, Priority::None),
            prioritized(2, Priority::Low),
            prioritized(3, Priority::Highest),
            prioritized(4, Priority::Lowest),
            prioritized(5, Priority::None),
            with(6, |i| {
                i.core.priority = Priority::Highest;
                i.overlay.local_priority_override = Some(Priority::Medium);
            }),
        ];
        assert_eq!(
            sorted(&items, &[SortKey::asc(F::Priority)], SUN),
            [4, 2, 3, 6, 1, 5]
        );
        assert_eq!(
            sorted(&items, &[SortKey::desc(F::Priority)], SUN),
            [3, 6, 2, 4, 1, 5]
        );
        assert_eq!(
            sorted(&items, &[SortKey::desc(F::EffectivePriority)], SUN),
            [3, 6, 2, 4, 1, 5]
        );
        assert_eq!(
            sorted(&items, &[SortKey::asc(F::EffectivePriority)], SUN),
            [4, 2, 6, 3, 1, 5]
        );
        assert_eq!(
            sorted(&items, &[SortKey::desc(F::LocalPriorityOverride)], SUN),
            [6, 1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn enums_bools_and_strings() {
        let statuses = [
            Status::Cancelled,
            Status::InProgress,
            Status::Inbox,
            Status::Done,
            Status::Someday,
            Status::Next,
        ];
        let items: Vec<Item> = statuses
            .into_iter()
            .zip(1..)
            .map(|(s, n)| with(n, |i| set_status(i, s)))
            .collect();
        // By proto number: inbox, next, someday, in_progress, done, cancelled.
        assert_eq!(
            sorted(&items, &[SortKey::asc(F::Status)], SUN),
            [3, 6, 5, 2, 4, 1]
        );
        assert_eq!(
            sorted(&items, &[SortKey::desc(F::Status)], SUN),
            [1, 4, 2, 5, 6, 3]
        );

        let items = [
            with(1, |i| i.core.due_date = Some(MON)),
            with(2, |i| i.core.due_date = Some(date(2026, 10, 1))),
            item(3),
        ];
        assert_eq!(
            sorted(&items, &[SortKey::asc(F::IsOverdue)], SUN),
            [1, 3, 2]
        );
        assert_eq!(
            sorted(&items, &[SortKey::desc(F::IsOverdue)], SUN),
            [2, 1, 3]
        );

        // Code points: "B" before "a", and U+FF21 before U+1F600 (UTF-16
        // order would put the emoji first).
        let titled = |n, t: &str| with(n, |i| i.core.title = t.into());
        let items = [
            titled(1, "a"),
            titled(2, "\u{1F600} grin"),
            titled(3, "B"),
            titled(4, "\u{FF21} fullwidth"),
            titled(5, "Café"),
        ];
        assert_eq!(
            sorted(&items, &[SortKey::asc(F::Title)], SUN),
            [3, 5, 1, 4, 2]
        );
    }

    #[test]
    fn the_tie_break_is_created_at_then_id() {
        let at = |n, seconds| with(n, |i| i.created_at = ts(seconds));
        let items = [at(4, 10), at(2, 10), at(3, 5), at(1, 20)];
        assert_eq!(sorted(&items, &[], SUN), [3, 2, 4, 1]);
        // A key equal for all falls through to the tie-break.
        assert_eq!(
            sorted(&items, &[SortKey::asc(F::DueDate)], SUN),
            [3, 2, 4, 1]
        );
        // created_at given descending: only id follows.
        assert_eq!(
            sorted(&items, &[SortKey::desc(F::CreatedAt)], SUN),
            [1, 2, 4, 3]
        );
        // id given descending: it decides alone.
        assert_eq!(sorted(&items, &[SortKey::desc(F::Id)], SUN), [4, 3, 2, 1]);
        // One nanosecond apart.
        let items = [
            with(1, |i| i.created_at = Timestamp::new(7, 1).unwrap()),
            with(2, |i| i.created_at = Timestamp::new(7, 0).unwrap()),
        ];
        assert_eq!(sorted(&items, &[], SUN), [2, 1]);
    }

    #[test]
    fn mixed_directions() {
        // The Today sort: is_overdue desc, effective_priority desc, due asc.
        let item_with = |n, due: Option<Date>, p| {
            with(n, |i| {
                i.core.due_date = due;
                i.core.priority = p;
            })
        };
        let items = [
            item_with(1, Some(date(2026, 10, 24)), Priority::High),
            item_with(2, Some(SUN), Priority::Medium),
            item_with(3, None, Priority::Low),
            item_with(4, Some(date(2026, 10, 20)), Priority::None),
            item_with(5, None, Priority::Medium),
            item_with(6, Some(date(2026, 10, 22)), Priority::High),
        ];
        let today_sort = [
            SortKey::desc(F::IsOverdue),
            SortKey::desc(F::EffectivePriority),
            SortKey::asc(F::DueDate),
        ];
        assert_eq!(sorted(&items, &today_sort, SUN), [6, 1, 4, 2, 5, 3]);
    }

    #[test]
    fn enum_numbers_and_ranks() {
        assert_eq!(enum_number(&V::Status(Status::Cancelled)), Some(6));
        assert_eq!(enum_number(&V::LinkState(LinkState::Pending)), Some(1));
        assert_eq!(enum_number(&V::Priority(Priority::High)), None);
        assert_eq!(enum_number(&V::String("x".into())), None);
        assert_eq!(priority_rank(Priority::None), None);
        assert_eq!(priority_rank(Priority::Lowest), Some(1));
        assert_eq!(priority_rank(Priority::Highest), Some(5));
    }

    /// The serde form, stored with views, stays stable.
    #[test]
    fn golden_json() {
        let filter = Filter {
            all_of: vec![
                p(F::Status, O::In, vec![status(Status::Next)]),
                group(
                    vec![],
                    vec![p(F::DueDate, O::Le, vec![rel(Anchor::Today, -1)])],
                ),
            ],
            any_of: vec![p(F::Title, O::Matches, vec![text("fix")])],
            include_done: true,
            ..Filter::default()
        };
        let json = json!({
            "all_of": [
                {"predicate": {"target": 70, "op": "in", "operands": [{"value": {"status": "next"}}]}},
                {"group": {"all_of": [], "any_of": [
                    {"predicate": {"target": 22, "op": "le", "operands": [
                        {"relative_date": {"anchor": "today", "offset_days": -1}}
                    ]}}
                ]}},
            ],
            "any_of": [
                {"predicate": {"target": 10, "op": "matches", "operands": [{"text": "fix"}]}},
            ],
            "include_archived": false,
            "include_done": true,
            "include_deferred": false,
            "include_deleted": false,
        });
        assert_eq!(serde_json::to_value(&filter).unwrap(), json);
        assert_eq!(serde_json::from_value::<Filter>(json).unwrap(), filter);
        let sort = [SortKey::desc(F::IsOverdue)];
        assert_eq!(
            serde_json::to_value(sort).unwrap(),
            json!([{"key": 82, "descending": true}])
        );
    }
}
