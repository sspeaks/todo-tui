//! Conversions between `todo_core` types and generated `todo.v1` messages.
//!
//! core -> proto is infallible (`From`). proto -> core is `TryFrom`: ids must
//! be valid ULIDs, enums must be known and not `*_UNSPECIFIED`, and dates and
//! instants must be in range. Derived values that core computes (`status`,
//! `effective_priority`) are emitted on the way out and ignored on the way in;
//! unknown values in the derived `pending_fields` are dropped. Filters and
//! sorts are also checked as they are read, so that the first error in
//! document order is reported (filter.proto).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use jiff::civil::{Date, ISOWeekDate, Weekday};
use jiff::{SignedDuration, Timestamp};
use todo_core as core;
use todo_core::filter::{Checker, check_sort_keys, child_path, validate_sort};
use ulid::Ulid;

use crate::google::r#type as gtype;
use crate::v1;

/// Each variant carries the offending field as a path relative to the
/// converted message (`item.waiting.since`); [`ConvertError::within`] makes
/// it relative to an enclosing message.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConvertError {
    #[error("{0}: required field is missing")]
    Missing(Cow<'static, str>),
    #[error("{0}: invalid ULID {1:?}")]
    InvalidId(Cow<'static, str>, String),
    #[error("{0}: value must not be UNSPECIFIED")]
    Unspecified(Cow<'static, str>),
    #[error("{0}: unknown enum value {1}")]
    UnknownEnum(Cow<'static, str>, i32),
    #[error("{0}: {1}")]
    Invalid(Cow<'static, str>, String),
    /// Valid, but not built yet: only a filter has these (filter.proto).
    #[error("{0}: {1}")]
    Unimplemented(Cow<'static, str>, String),
}

impl ConvertError {
    /// The offending field's path.
    pub fn path(&self) -> &str {
        match self {
            ConvertError::Missing(path)
            | ConvertError::InvalidId(path, _)
            | ConvertError::Unspecified(path)
            | ConvertError::UnknownEnum(path, _)
            | ConvertError::Invalid(path, _)
            | ConvertError::Unimplemented(path, _) => path,
        }
    }

    /// Prefixes the path with the field holding the converted message:
    /// `title` within `item` is `item.title`, and that within `mutations[3]`
    /// is `mutations[3].item.title`. An empty path, the converted message
    /// itself, becomes the prefix.
    pub fn within(mut self, prefix: impl fmt::Display) -> Self {
        let path = match &mut self {
            ConvertError::Missing(path)
            | ConvertError::InvalidId(path, _)
            | ConvertError::Unspecified(path)
            | ConvertError::UnknownEnum(path, _)
            | ConvertError::Invalid(path, _)
            | ConvertError::Unimplemented(path, _) => path,
        };
        *path = if path.is_empty() {
            prefix.to_string().into()
        } else {
            format!("{prefix}.{path}").into()
        };
        self
    }
}

/// An INVALID filter error is `Invalid`, an UNIMPLEMENTED one
/// `Unimplemented`, at the same path.
impl From<core::FilterError> for ConvertError {
    fn from(e: core::FilterError) -> Self {
        match e.kind {
            core::FilterErrorKind::Invalid => ConvertError::Invalid(e.path.into(), e.reason),
            core::FilterErrorKind::Unimplemented => {
                ConvertError::Unimplemented(e.path.into(), e.reason)
            }
        }
    }
}

pub(crate) type Result<T, E = ConvertError> = std::result::Result<T, E>;

pub(crate) fn parse_id(field: &'static str, s: &str) -> Result<Ulid> {
    s.parse()
        .map_err(|_| ConvertError::InvalidId(field.into(), s.to_owned()))
}

pub(crate) fn require<T>(field: &'static str, v: Option<T>) -> Result<T> {
    v.ok_or(ConvertError::Missing(field.into()))
}

// ---- scalars ------------------------------------------------------------

const NANOS_PER_SEC: i32 = 1_000_000_000;

/// Proto3 limit on `google.protobuf.Duration.seconds` (about 10,000 years).
const MAX_DURATION_SECS: u64 = 315_576_000_000;

/// 0001-01-01T00:00:00Z, the earliest `google.protobuf.Timestamp`.
const MIN_TIMESTAMP_SECS: i64 = -62_135_596_800;

/// `nanos` is always in `0..1e9`, as proto3 requires, also before the epoch.
pub fn timestamp_to_proto(t: Timestamp) -> prost_types::Timestamp {
    let (mut seconds, mut nanos) = (t.as_second(), t.subsec_nanosecond());
    if nanos < 0 {
        seconds -= 1;
        nanos += NANOS_PER_SEC;
    }
    prost_types::Timestamp { seconds, nanos }
}

pub fn timestamp_from_proto(field: &'static str, t: &prost_types::Timestamp) -> Result<Timestamp> {
    if !(0..NANOS_PER_SEC).contains(&t.nanos) {
        return Err(ConvertError::Invalid(
            field.into(),
            format!("nanos {} out of range", t.nanos),
        ));
    }
    if t.seconds < MIN_TIMESTAMP_SECS {
        return Err(ConvertError::Invalid(
            field.into(),
            "before 0001-01-01T00:00:00Z".into(),
        ));
    }
    Timestamp::new(t.seconds, t.nanos)
        .map_err(|e| ConvertError::Invalid(field.into(), e.to_string()))
}

pub fn date_to_proto(d: Date) -> gtype::Date {
    gtype::Date {
        year: d.year().into(),
        month: d.month().into(),
        day: d.day().into(),
    }
}

/// Only full dates are accepted (no year-less or day-less dates), in years
/// 1 to 9999 as `google.type.Date` allows.
pub fn date_from_proto(field: &'static str, d: &gtype::Date) -> Result<Date> {
    let invalid = |e: String| ConvertError::Invalid(field.into(), e);
    if !(1..=9999).contains(&d.year) {
        return Err(invalid(format!("year {} is not in 1..=9999", d.year)));
    }
    let year = i16::try_from(d.year).map_err(|e| invalid(e.to_string()))?;
    let month = i8::try_from(d.month).map_err(|e| invalid(e.to_string()))?;
    let day = i8::try_from(d.day).map_err(|e| invalid(e.to_string()))?;
    Date::new(year, month, day).map_err(|e| invalid(e.to_string()))
}

pub fn duration_to_proto(d: SignedDuration) -> prost_types::Duration {
    prost_types::Duration {
        seconds: d.as_secs(),
        nanos: d.subsec_nanos(),
    }
}

pub fn duration_from_proto(
    field: &'static str,
    d: &prost_types::Duration,
) -> Result<SignedDuration> {
    let invalid = |msg: &str| Err(ConvertError::Invalid(field.into(), msg.into()));
    // unsigned_abs: abs() overflows on i32::MIN / i64::MIN.
    if d.nanos.unsigned_abs() >= NANOS_PER_SEC as u32 {
        return invalid("nanos out of range");
    }
    if d.seconds.unsigned_abs() > MAX_DURATION_SECS {
        return invalid("seconds out of range");
    }
    if d.seconds != 0 && d.nanos != 0 && (d.seconds < 0) != (d.nanos < 0) {
        return invalid("seconds and nanos differ in sign");
    }
    Ok(SignedDuration::new(d.seconds, d.nanos))
}

pub fn json_to_proto(v: &serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind;
    use serde_json::Value as J;
    let kind = match v {
        J::Null => Kind::NullValue(prost_types::NullValue::NullValue as i32),
        J::Bool(b) => Kind::BoolValue(*b),
        J::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(f64::NAN)),
        J::String(s) => Kind::StringValue(s.clone()),
        J::Array(a) => Kind::ListValue(prost_types::ListValue {
            values: a.iter().map(json_to_proto).collect(),
        }),
        J::Object(o) => Kind::StructValue(prost_types::Struct {
            fields: o
                .iter()
                .map(|(k, v)| (k.clone(), json_to_proto(v)))
                .collect(),
        }),
    };
    prost_types::Value { kind: Some(kind) }
}

/// Integral numbers within ±2^53 become JSON integers; non-finite numbers
/// become `null`, as in proto3 JSON.
pub fn json_from_proto(v: &prost_types::Value) -> serde_json::Value {
    use prost_types::value::Kind;
    use serde_json::Value as J;
    match &v.kind {
        None | Some(Kind::NullValue(_)) => J::Null,
        Some(Kind::BoolValue(b)) => J::Bool(*b),
        Some(Kind::NumberValue(n)) => {
            const MAX_SAFE: f64 = 9_007_199_254_740_992.0; // 2^53
            if n.is_finite() && n.fract() == 0.0 && n.abs() <= MAX_SAFE {
                J::Number((*n as i64).into())
            } else {
                serde_json::Number::from_f64(*n).map_or(J::Null, J::Number)
            }
        }
        Some(Kind::StringValue(s)) => J::String(s.clone()),
        Some(Kind::ListValue(l)) => J::Array(l.values.iter().map(json_from_proto).collect()),
        Some(Kind::StructValue(s)) => J::Object(
            s.fields
                .iter()
                .map(|(k, v)| (k.clone(), json_from_proto(v)))
                .collect(),
        ),
    }
}

// ---- enums --------------------------------------------------------------

/// Maps a core enum to its proto enum and back. `*_UNSPECIFIED` and unknown
/// values are errors on the way in.
macro_rules! enum_map {
    ($core:ty, $proto:ty, $name:literal, { $($c:ident => $p:ident),+ $(,)? }) => {
        impl From<$core> for $proto {
            fn from(v: $core) -> Self {
                match v { $(<$core>::$c => <$proto>::$p),+ }
            }
        }

        impl TryFrom<$proto> for $core {
            type Error = ConvertError;
            fn try_from(v: $proto) -> Result<Self> {
                match v {
                    $(<$proto>::$p => Ok(<$core>::$c),)+
                    #[allow(unreachable_patterns)]
                    _ => Err(ConvertError::Unspecified($name.into())),
                }
            }
        }

        impl EnumField for $core {
            type Proto = $proto;
            fn from_i32(field: &'static str, v: i32) -> Result<Self> {
                let p = <$proto>::try_from(v)
                    .map_err(|_| ConvertError::UnknownEnum(field.into(), v))?;
                <$core>::try_from(p).map_err(|_| ConvertError::Unspecified(field.into()))
            }
            fn to_i32(self) -> i32 {
                <$proto>::from(self) as i32
            }
        }
    };
}

/// Decoding an enum stored as `i32` in a prost message.
pub trait EnumField: Sized {
    type Proto;
    fn from_i32(field: &'static str, v: i32) -> Result<Self>;
    fn to_i32(self) -> i32;
}

enum_map!(core::Topic, v1::Topic, "topic", {
    Item => Item, Suggestion => Suggestion, View => View, Person => Person,
    Series => Series, Review => Review, Config => Config,
});
enum_map!(core::Priority, v1::Priority, "priority", {
    None => None, Lowest => Lowest, Low => Low, Medium => Medium, High => High, Highest => Highest,
});
enum_map!(core::Status, v1::Status, "status", {
    Inbox => Inbox, Next => Next, Someday => Someday, InProgress => InProgress,
    Done => Done, Cancelled => Cancelled,
});
enum_map!(core::StatusCategory, v1::StatusCategory, "status_category", {
    Todo => Todo, InProgress => InProgress, Done => Done,
});
enum_map!(core::ResolutionKind, v1::ResolutionKind, "resolution_kind", {
    Done => Done, Cancelled => Cancelled,
});
enum_map!(core::TodoRefinement, v1::TodoRefinement, "todo_refinement", {
    Inbox => Inbox, Next => Next, Someday => Someday,
});
enum_map!(core::CreatedVia, v1::CreatedVia, "created_via", {
    Tui => Tui, Cli => Cli, Mcp => Mcp, RemoteImport => RemoteImport, Restore => Restore,
    Split => Split, Recurrence => Recurrence,
});
enum_map!(core::ArchiveReason, v1::ArchiveReason, "archive_reason", {
    User => User, LeftScope => LeftScope, RemoteUnavailable => RemoteUnavailable, AutoDone => AutoDone,
});
enum_map!(core::LinkState, v1::LinkState, "link_state", {
    Pending => Pending, Ok => Ok, Broken => Broken,
});
enum_map!(core::CaptureTokenKind, v1::CaptureTokenKind, "capture_token_kind", {
    Priority => Priority, DueDate => DueDate, Label => Label, Waiting => Waiting, Status => Status,
});
enum_map!(core::RelationKind, v1::RelationKind, "kind", {
    Blocks => Blocks, Duplicates => Duplicates, RelatesTo => RelatesTo,
});

impl From<core::ItemField> for v1::ItemField {
    fn from(f: core::ItemField) -> Self {
        // Both enums share the todo.v1.ItemField numbering.
        v1::ItemField::try_from(f.number() as i32).expect("core::ItemField out of sync with proto")
    }
}

impl TryFrom<v1::ItemField> for core::ItemField {
    type Error = ConvertError;
    fn try_from(f: v1::ItemField) -> Result<Self> {
        core::ItemField::from_number(f as u32).ok_or(ConvertError::Unspecified("item_field".into()))
    }
}

impl EnumField for core::ItemField {
    type Proto = v1::ItemField;
    fn from_i32(field: &'static str, v: i32) -> Result<Self> {
        u32::try_from(v)
            .ok()
            .and_then(core::ItemField::from_number)
            .ok_or(if v == 0 {
                ConvertError::Unspecified(field.into())
            } else {
                ConvertError::UnknownEnum(field.into(), v)
            })
    }
    fn to_i32(self) -> i32 {
        self.number() as i32
    }
}

// ---- messages -----------------------------------------------------------

impl From<core::MutationResult> for v1::MutationResult {
    fn from(r: core::MutationResult) -> Self {
        Self {
            op_id: r.op_id.to_string(),
            revision: r.revision,
        }
    }
}

impl TryFrom<v1::MutationResult> for core::MutationResult {
    type Error = ConvertError;
    fn try_from(r: v1::MutationResult) -> Result<Self> {
        Ok(Self {
            op_id: parse_id("op_id", &r.op_id)?,
            revision: r.revision,
        })
    }
}

impl From<&core::PersonRef> for v1::PersonRef {
    fn from(p: &core::PersonRef) -> Self {
        use v1::person_ref::Ref;
        let r = match p {
            core::PersonRef::Person(id) => Ref::PersonId(id.to_string()),
            core::PersonRef::FreeText(s) => Ref::FreeText(s.clone()),
        };
        Self { r#ref: Some(r) }
    }
}

/// An ISO week string, `YYYY-Www` with a week that exists in that year.
pub fn iso_week_from_proto(field: &'static str, s: &str) -> Result<String> {
    let invalid =
        || ConvertError::Invalid(field.into(), format!("{s:?} is not an ISO week (YYYY-Www)"));
    let (year, week) = s.split_once("-W").ok_or_else(invalid)?;
    if year.len() != 4
        || week.len() != 2
        || !(year.bytes().chain(week.bytes())).all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    let year: i16 = year.parse().map_err(|_| invalid())?;
    let week: i8 = week.parse().map_err(|_| invalid())?;
    ISOWeekDate::new(year, week, Weekday::Monday).map_err(|_| invalid())?;
    Ok(s.to_owned())
}

/// A person id only: free text is valid only for `waiting.on` (design 4.8).
fn person_id_from_proto(field: &'static str, p: &v1::PersonRef) -> Result<core::PersonRef> {
    use v1::person_ref::Ref;
    match require(field, p.r#ref.as_ref())? {
        Ref::PersonId(id) => Ok(core::PersonRef::Person(parse_id(field, id)?)),
        Ref::FreeText(_) => Err(ConvertError::Invalid(
            field.into(),
            "free text only valid for waiting.on".into(),
        )),
    }
}

pub(crate) fn person_from_proto(field: &'static str, p: &v1::PersonRef) -> Result<core::PersonRef> {
    use v1::person_ref::Ref;
    match require(field, p.r#ref.as_ref())? {
        Ref::PersonId(id) => Ok(core::PersonRef::Person(parse_id(field, id)?)),
        Ref::FreeText(s) => Ok(core::PersonRef::FreeText(s.clone())),
    }
}

impl TryFrom<&v1::PersonRef> for core::PersonRef {
    type Error = ConvertError;
    fn try_from(p: &v1::PersonRef) -> Result<Self> {
        person_from_proto("person", p)
    }
}

impl From<&core::Waiting> for v1::Waiting {
    fn from(w: &core::Waiting) -> Self {
        Self {
            on: Some((&w.on).into()),
            reason: w.reason.clone(),
            since: Some(date_to_proto(w.since)),
            follow_up_on: w.follow_up_on.map(date_to_proto),
        }
    }
}

impl TryFrom<&v1::Waiting> for core::Waiting {
    type Error = ConvertError;
    fn try_from(w: &v1::Waiting) -> Result<Self> {
        Ok(Self {
            on: person_from_proto("waiting.on", require("waiting.on", w.on.as_ref())?)?,
            reason: w.reason.clone(),
            since: date_from_proto("waiting.since", require("waiting.since", w.since.as_ref())?)?,
            follow_up_on: w
                .follow_up_on
                .as_ref()
                .map(|d| date_from_proto("waiting.follow_up_on", d))
                .transpose()?,
        })
    }
}

impl From<&core::Archived> for v1::Archived {
    fn from(a: &core::Archived) -> Self {
        Self {
            at: Some(timestamp_to_proto(a.at)),
            reason: a.reason.to_i32(),
        }
    }
}

impl TryFrom<&v1::Archived> for core::Archived {
    type Error = ConvertError;
    fn try_from(a: &v1::Archived) -> Result<Self> {
        Ok(Self {
            at: timestamp_from_proto("archived.at", require("archived.at", a.at.as_ref())?)?,
            reason: EnumField::from_i32("archived.reason", a.reason)?,
        })
    }
}

impl From<&core::RemoteLink> for v1::RemoteLink {
    fn from(l: &core::RemoteLink) -> Self {
        Self {
            tracker_kind: l.tracker_kind.clone(),
            instance_id: l.instance_id.clone(),
            remote_id: l.remote_id.clone(),
            key: l.key.clone(),
            url: l.url.clone(),
            last_pulled_version: l.last_pulled_version.map(timestamp_to_proto),
            link_state: l.link_state.to_i32(),
            pinned: l.pinned,
        }
    }
}

impl TryFrom<&v1::RemoteLink> for core::RemoteLink {
    type Error = ConvertError;
    fn try_from(l: &v1::RemoteLink) -> Result<Self> {
        Ok(Self {
            tracker_kind: l.tracker_kind.clone(),
            instance_id: l.instance_id.clone(),
            remote_id: l.remote_id.clone(),
            key: l.key.clone(),
            url: l.url.clone(),
            last_pulled_version: l
                .last_pulled_version
                .as_ref()
                .map(|t| timestamp_from_proto("remote_link.last_pulled_version", t))
                .transpose()?,
            link_state: EnumField::from_i32("remote_link.link_state", l.link_state)?,
            pinned: l.pinned,
        })
    }
}

impl From<&core::ItemRecurrence> for v1::ItemRecurrence {
    fn from(r: &core::ItemRecurrence) -> Self {
        Self {
            series_id: r.series_id.to_string(),
            occurrence_date: r.occurrence_date.map(date_to_proto),
        }
    }
}

impl TryFrom<&v1::ItemRecurrence> for core::ItemRecurrence {
    type Error = ConvertError;
    fn try_from(r: &v1::ItemRecurrence) -> Result<Self> {
        Ok(Self {
            series_id: parse_id("recurrence.series_id", &r.series_id)?,
            occurrence_date: r
                .occurrence_date
                .as_ref()
                .map(|d| date_from_proto("recurrence.occurrence_date", d))
                .transpose()?,
        })
    }
}

impl From<&core::StalenessSignals> for v1::StalenessSignals {
    fn from(s: &core::StalenessSignals) -> Self {
        Self {
            last_user_touch_at: s.last_user_touch_at.map(timestamp_to_proto),
            last_remote_change_at: s.last_remote_change_at.map(timestamp_to_proto),
            status_entered_at: s.status_entered_at.map(timestamp_to_proto),
            waiting_since: s.waiting_since.map(timestamp_to_proto),
        }
    }
}

impl TryFrom<&v1::StalenessSignals> for core::StalenessSignals {
    type Error = ConvertError;
    fn try_from(s: &v1::StalenessSignals) -> Result<Self> {
        let ts = |field, t: &Option<prost_types::Timestamp>| {
            t.as_ref()
                .map(|t| timestamp_from_proto(field, t))
                .transpose()
        };
        Ok(Self {
            last_user_touch_at: ts("staleness.last_user_touch_at", &s.last_user_touch_at)?,
            last_remote_change_at: ts("staleness.last_remote_change_at", &s.last_remote_change_at)?,
            status_entered_at: ts("staleness.status_entered_at", &s.status_entered_at)?,
            waiting_since: ts("staleness.waiting_since", &s.waiting_since)?,
        })
    }
}

impl From<&core::Relation> for v1::Relation {
    fn from(r: &core::Relation) -> Self {
        Self {
            from_id: r.from().to_string(),
            to_id: r.to().to_string(),
            kind: r.kind().to_i32(),
        }
    }
}

impl From<core::Relation> for v1::Relation {
    fn from(r: core::Relation) -> Self {
        (&r).into()
    }
}

/// The canonical relation: a `RELATES_TO` named either way round has
/// `from_id < to_id`. An Item related to itself is invalid at `to_id`.
impl TryFrom<&v1::Relation> for core::Relation {
    type Error = ConvertError;
    fn try_from(r: &v1::Relation) -> Result<Self> {
        let from = parse_id("from_id", &r.from_id)?;
        let to = parse_id("to_id", &r.to_id)?;
        let kind = EnumField::from_i32("kind", r.kind)?;
        core::Relation::new(from, to, kind).map_err(|e| match e {
            core::RelationError::SameItem => {
                ConvertError::Invalid("to_id".into(), "must differ from from_id".into())
            }
        })
    }
}

impl From<&core::Item> for v1::Item {
    fn from(item: &core::Item) -> Self {
        let c = &item.core;
        let o = &item.overlay;
        let d = &item.derived;
        Self {
            id: item.id.to_string(),
            created_via: item.created_via.to_i32(),
            created_at: Some(timestamp_to_proto(item.created_at)),
            updated_at: Some(timestamp_to_proto(item.updated_at)),
            revision: item.revision,
            remote_link: item.remote_link.as_ref().map(Into::into),

            title: c.title.clone(),
            description: c.description.clone(),
            item_type: c.item_type.clone(),
            status_category: c.status_category.to_i32(),
            resolution_kind: c.resolution_kind.map(EnumField::to_i32),
            remote_status_name: c.remote_status_name.clone(),
            priority: c.priority.to_i32(),
            assignee: c.assignee.as_ref().map(Into::into),
            reporter: c.reporter.as_ref().map(Into::into),
            labels: c.labels.clone(),
            components: c.components.clone(),
            fix_versions: c.fix_versions.clone(),
            due_date: c.due_date.map(date_to_proto),
            parent_id: c.parent_id.map(|id| id.to_string()),
            sprint: c.sprint.clone(),
            story_points: c.story_points,
            original_estimate: c.original_estimate.map(duration_to_proto),
            remaining_estimate: c.remaining_estimate.map(duration_to_proto),
            resolution: c.resolution.clone(),
            remote_created_at: c.remote_created_at.map(timestamp_to_proto),
            remote_updated_at: c.remote_updated_at.map(timestamp_to_proto),

            todo_refinement: o.todo_refinement.to_i32(),
            waiting: o.waiting.as_ref().map(Into::into),
            defer_until: o.defer_until.map(date_to_proto),
            private_notes: o.private_notes.clone(),
            focus_week: o.focus_week.clone(),
            local_priority_override: o.local_priority_override.map(EnumField::to_i32),
            archived: o.archived.as_ref().map(Into::into),
            recurrence: o.recurrence.as_ref().map(Into::into),

            remote_extras: item
                .remote_extras
                .iter()
                .map(|(k, v)| (k.clone(), json_to_proto(v)))
                .collect(),

            status: item.status().to_i32(),
            effective_priority: item.effective_priority().to_i32(),
            is_mine: d.is_mine,
            pending_fields: d.pending_fields.iter().map(|f| f.to_i32()).collect(),
            has_conflict: d.has_conflict,
            has_push_failure: d.has_push_failure,
            description_lossy: d.description_lossy,
            description_dirty: d.description_dirty,
            staleness: (!d.staleness.is_empty()).then(|| (&d.staleness).into()),
            scopes: d.scopes.clone(),
            deleted_at: d.deleted_at.map(timestamp_to_proto),
            merged_into: d.merged_into.map(|id| id.to_string()),
            is_overdue: d.is_overdue,

            relations: item.relations.iter().map(Into::into).collect(),
        }
    }
}

impl From<core::Item> for v1::Item {
    fn from(item: core::Item) -> Self {
        (&item).into()
    }
}

impl TryFrom<&v1::Item> for core::Item {
    type Error = ConvertError;
    fn try_from(p: &v1::Item) -> Result<Self> {
        let ts = |field, t: &Option<prost_types::Timestamp>| {
            t.as_ref()
                .map(|t| timestamp_from_proto(field, t))
                .transpose()
        };
        let date = |field, d: &Option<gtype::Date>| {
            d.as_ref().map(|d| date_from_proto(field, d)).transpose()
        };
        let dur = |field, d: &Option<prost_types::Duration>| {
            d.as_ref()
                .map(|d| duration_from_proto(field, d))
                .transpose()
        };
        let person = |field, r: &Option<v1::PersonRef>| {
            r.as_ref()
                .map(|r| person_id_from_proto(field, r))
                .transpose()
        };
        let id = |field, s: &Option<String>| s.as_deref().map(|s| parse_id(field, s)).transpose();

        Ok(core::Item {
            id: parse_id("id", &p.id)?,
            created_via: EnumField::from_i32("created_via", p.created_via)?,
            created_at: require("created_at", ts("created_at", &p.created_at)?)?,
            updated_at: require("updated_at", ts("updated_at", &p.updated_at)?)?,
            revision: p.revision,
            remote_link: p.remote_link.as_ref().map(TryInto::try_into).transpose()?,
            core: core::CoreFields {
                title: p.title.clone(),
                description: p.description.clone(),
                item_type: p.item_type.clone(),
                status_category: EnumField::from_i32("status_category", p.status_category)?,
                resolution_kind: p
                    .resolution_kind
                    .map(|v| EnumField::from_i32("resolution_kind", v))
                    .transpose()?,
                remote_status_name: p.remote_status_name.clone(),
                priority: EnumField::from_i32("priority", p.priority)?,
                assignee: person("assignee", &p.assignee)?,
                reporter: person("reporter", &p.reporter)?,
                labels: p.labels.clone(),
                components: p.components.clone(),
                fix_versions: p.fix_versions.clone(),
                due_date: date("due_date", &p.due_date)?,
                parent_id: id("parent_id", &p.parent_id)?,
                sprint: p.sprint.clone(),
                story_points: p.story_points,
                original_estimate: dur("original_estimate", &p.original_estimate)?,
                remaining_estimate: dur("remaining_estimate", &p.remaining_estimate)?,
                resolution: p.resolution.clone(),
                remote_created_at: ts("remote_created_at", &p.remote_created_at)?,
                remote_updated_at: ts("remote_updated_at", &p.remote_updated_at)?,
            },
            overlay: core::OverlayFields {
                todo_refinement: EnumField::from_i32("todo_refinement", p.todo_refinement)?,
                waiting: p.waiting.as_ref().map(TryInto::try_into).transpose()?,
                defer_until: date("defer_until", &p.defer_until)?,
                private_notes: p.private_notes.clone(),
                focus_week: p
                    .focus_week
                    .as_deref()
                    .map(|w| iso_week_from_proto("focus_week", w))
                    .transpose()?,
                local_priority_override: p
                    .local_priority_override
                    .map(|v| EnumField::from_i32("local_priority_override", v))
                    .transpose()?,
                archived: p.archived.as_ref().map(TryInto::try_into).transpose()?,
                recurrence: p.recurrence.as_ref().map(TryInto::try_into).transpose()?,
            },
            remote_extras: p
                .remote_extras
                .iter()
                .map(|(k, v)| (k.clone(), json_from_proto(v)))
                .collect::<BTreeMap<_, _>>(),
            derived: core::DerivedFields {
                is_mine: p.is_mine,
                pending_fields: p
                    .pending_fields
                    .iter()
                    .filter_map(|&f| u32::try_from(f).ok().and_then(core::ItemField::from_number))
                    .collect(),
                has_conflict: p.has_conflict,
                has_push_failure: p.has_push_failure,
                description_lossy: p.description_lossy,
                description_dirty: p.description_dirty,
                staleness: p
                    .staleness
                    .as_ref()
                    .map(TryInto::try_into)
                    .transpose()?
                    .unwrap_or_default(),
                scopes: p.scopes.clone(),
                deleted_at: ts("deleted_at", &p.deleted_at)?,
                merged_into: id("merged_into", &p.merged_into)?,
                is_overdue: p.is_overdue,
            },
            relations: p
                .relations
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    r.try_into()
                        .map_err(|e: ConvertError| e.within(format!("relations[{i}]")))
                })
                .collect::<Result<_>>()?,
        })
    }
}

impl TryFrom<v1::Item> for core::Item {
    type Error = ConvertError;
    fn try_from(p: v1::Item) -> Result<Self> {
        (&p).try_into()
    }
}

// ---- field values -------------------------------------------------------

impl From<&core::FieldValue> for v1::FieldValue {
    fn from(v: &core::FieldValue) -> Self {
        use core::FieldValue as V;
        use v1::field_value::Kind as K;
        let kind = match v {
            V::Cleared => K::Cleared(()),
            V::String(s) => K::StringValue(s.clone()),
            V::Markdown(s) => K::MarkdownValue(s.clone()),
            V::Bool(b) => K::BoolValue(*b),
            V::Number(n) => K::NumberValue(*n),
            V::Uint(n) => K::UintValue(*n),
            V::Date(d) => K::DateValue(date_to_proto(*d)),
            V::Timestamp(t) => K::TimestampValue(timestamp_to_proto(*t)),
            V::Duration(d) => K::DurationValue(duration_to_proto(*d)),
            V::StringSet(values) => K::StringSet(v1::StringSet {
                values: values.clone(),
            }),
            V::Person(p) => K::Person(p.into()),
            V::ItemId(id) => K::ItemId(id.to_string()),
            V::IsoWeek(w) => K::IsoWeek(w.clone()),
            V::Priority(p) => K::Priority(p.to_i32()),
            V::Status(s) => K::Status(s.to_i32()),
            V::StatusCategory(c) => K::StatusCategory(c.to_i32()),
            V::ResolutionKind(r) => K::ResolutionKind(r.to_i32()),
            V::TodoRefinement(r) => K::TodoRefinement(r.to_i32()),
            V::CreatedVia(c) => K::CreatedVia(c.to_i32()),
            V::ArchiveReason(r) => K::ArchiveReason(r.to_i32()),
            V::LinkState(s) => K::LinkState(s.to_i32()),
            V::ItemFieldSet(fields) => K::ItemFieldSet(v1::ItemFieldSet {
                fields: fields.iter().map(|f| f.to_i32()).collect(),
            }),
            V::Waiting(w) => K::Waiting(w.into()),
            V::Archived(a) => K::Archived(a.into()),
            V::Recurrence(r) => K::Recurrence(r.into()),
            V::RemoteLink(l) => K::RemoteLink(l.into()),
            V::Staleness(s) => K::Staleness(s.into()),
        };
        Self { kind: Some(kind) }
    }
}

/// The `FieldValue` at `field`. Errors name the oneof case below `field`
/// (`value.date_value`, `value.waiting.since`, and `value.kind` when no case
/// is set). A value is checked on its own, not against any `ItemField`: free
/// text is a valid `person`, and core decides where it may go.
pub fn field_value_from_proto(field: &'static str, v: &v1::FieldValue) -> Result<core::FieldValue> {
    field_value(v).map_err(|e| e.within(field))
}

fn field_value(v: &v1::FieldValue) -> Result<core::FieldValue> {
    use core::FieldValue as V;
    use v1::field_value::Kind as K;
    Ok(match require("kind", v.kind.as_ref())? {
        K::Cleared(()) => V::Cleared,
        K::StringValue(s) => V::String(s.clone()),
        K::MarkdownValue(s) => V::Markdown(s.clone()),
        K::BoolValue(b) => V::Bool(*b),
        K::NumberValue(n) => V::Number(*n),
        K::UintValue(n) => V::Uint(*n),
        K::DateValue(d) => V::Date(date_from_proto("date_value", d)?),
        K::TimestampValue(t) => V::Timestamp(timestamp_from_proto("timestamp_value", t)?),
        K::DurationValue(d) => V::Duration(duration_from_proto("duration_value", d)?),
        K::StringSet(s) => V::StringSet(s.values.clone()),
        K::Person(p) => V::Person(person_from_proto("person", p)?),
        K::ItemId(id) => V::ItemId(parse_id("item_id", id)?),
        K::IsoWeek(w) => V::IsoWeek(iso_week_from_proto("iso_week", w)?),
        K::Priority(p) => V::Priority(EnumField::from_i32("priority", *p)?),
        K::Status(s) => V::Status(EnumField::from_i32("status", *s)?),
        K::StatusCategory(c) => V::StatusCategory(EnumField::from_i32("status_category", *c)?),
        K::ResolutionKind(r) => V::ResolutionKind(EnumField::from_i32("resolution_kind", *r)?),
        K::TodoRefinement(r) => V::TodoRefinement(EnumField::from_i32("todo_refinement", *r)?),
        K::CreatedVia(c) => V::CreatedVia(EnumField::from_i32("created_via", *c)?),
        K::ArchiveReason(r) => V::ArchiveReason(EnumField::from_i32("archive_reason", *r)?),
        K::LinkState(s) => V::LinkState(EnumField::from_i32("link_state", *s)?),
        K::ItemFieldSet(s) => V::ItemFieldSet(
            s.fields
                .iter()
                .map(|&f| EnumField::from_i32("item_field_set.fields", f))
                .collect::<Result<_>>()?,
        ),
        K::Waiting(w) => V::Waiting(w.try_into()?),
        K::Archived(a) => V::Archived(a.try_into()?),
        K::Recurrence(r) => V::Recurrence(r.try_into()?),
        K::RemoteLink(l) => V::RemoteLink(l.try_into()?),
        K::Staleness(s) => V::Staleness(s.try_into()?),
    })
}

// ---- filters, sorts and views -------------------------------------------

enum_map!(core::filter::Operator, v1::Operator, "op", {
    In => In, Eq => Eq, Ne => Ne, Lt => Lt, Le => Le, Gt => Gt, Ge => Ge, IsSet => IsSet,
    IsUnset => IsUnset, ContainsAny => ContainsAny, ContainsAll => ContainsAll, Matches => Matches,
});
enum_map!(core::filter::Anchor, v1::relative_date::Anchor, "anchor", {
    Today => Today, StartOfWeek => StartOfWeek, EndOfWeek => EndOfWeek, CurrentWeek => CurrentWeek,
});

impl From<&core::Filter> for v1::Filter {
    fn from(f: &core::Filter) -> Self {
        Self {
            all_of: f.all_of.iter().map(Into::into).collect(),
            any_of: f.any_of.iter().map(Into::into).collect(),
            include_archived: f.include_archived,
            include_done: f.include_done,
            include_deferred: f.include_deferred,
            include_deleted: f.include_deleted,
        }
    }
}

impl From<&core::filter::Clause> for v1::Clause {
    fn from(c: &core::filter::Clause) -> Self {
        use core::filter::Clause;
        use v1::clause::Kind;
        let kind = match c {
            Clause::Predicate(p) => Kind::Predicate(v1::Predicate {
                target: p.target.to_i32(),
                op: p.op.to_i32(),
                operands: p.operands.iter().map(Into::into).collect(),
            }),
            Clause::Group(g) => Kind::Group(v1::ClauseGroup {
                all_of: g.all_of.iter().map(Into::into).collect(),
                any_of: g.any_of.iter().map(Into::into).collect(),
            }),
        };
        Self { kind: Some(kind) }
    }
}

impl From<&core::filter::Operand> for v1::Operand {
    fn from(o: &core::filter::Operand) -> Self {
        use core::filter::Operand;
        use v1::operand::Kind;
        let kind = match o {
            Operand::Value(v) => Kind::Value(v.into()),
            Operand::RelativeDate(d) => Kind::RelativeDate(v1::RelativeDate {
                anchor: d.anchor.to_i32(),
                offset_days: d.offset_days,
            }),
            Operand::Text(t) => Kind::Text(t.clone()),
        };
        Self { kind: Some(kind) }
    }
}

impl From<core::SortKey> for v1::SortKey {
    fn from(k: core::SortKey) -> Self {
        Self {
            key: k.key.to_i32(),
            descending: k.descending,
        }
    }
}

/// The filter at `filter` of a request or view, checked as it is read: the
/// error is the first in document order (filter.proto), as
/// [`core::Filter::validate`] reports them, with what cannot be read (an
/// unset clause, an unknown target or op, an operand that is no valid
/// value) in its place. Paths start with `filter` (`filter.all_of[1].
/// predicate.op`, and `filter` for a limit on the whole filter). With
/// `today`, relative dates are resolved too, so a date out of range is also
/// reported in its place.
pub fn filter_from_proto(f: &v1::Filter, today: Option<Date>) -> Result<core::Filter> {
    let read = || {
        let mut check = Checker::new(today);
        let (all_of, any_of) = clauses_from_proto(&mut check, "", &f.all_of, &f.any_of, 0)?;
        check.finish()?;
        Ok(core::Filter {
            all_of,
            any_of,
            include_archived: f.include_archived,
            include_done: f.include_done,
            include_deferred: f.include_deferred,
            include_deleted: f.include_deleted,
        })
    };
    read().map_err(|e: ConvertError| e.within("filter"))
}

type Clauses = (Vec<core::filter::Clause>, Vec<core::filter::Clause>);

/// The clauses of the filter or group at `path`, `depth` groups deep.
fn clauses_from_proto(
    check: &mut Checker,
    path: &str,
    all_of: &[v1::Clause],
    any_of: &[v1::Clause],
    depth: usize,
) -> Result<Clauses> {
    let mut read = |list: &str, clauses: &[v1::Clause]| {
        clauses
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let path = child_path(path, format_args!("{list}[{i}]"));
                clause_from_proto(check, &path, c, depth)
            })
            .collect::<Result<Vec<_>>>()
    };
    let all_of = read("all_of", all_of)?;
    let any_of = read("any_of", any_of)?;
    Ok((all_of, any_of))
}

fn clause_from_proto(
    check: &mut Checker,
    path: &str,
    c: &v1::Clause,
    depth: usize,
) -> Result<core::filter::Clause> {
    use core::filter::{Clause, Group, Predicate};
    use v1::clause::Kind;
    match &c.kind {
        None => Err(ConvertError::Invalid(
            path.to_owned().into(),
            "must be a predicate or a group".into(),
        )),
        Some(Kind::Predicate(p)) => {
            let path = format!("{path}.predicate");
            let at = |e: ConvertError| e.within(&path);
            let target = core::ItemField::from_i32("target", p.target).map_err(at)?;
            let op = core::filter::Operator::from_i32("op", p.op).map_err(at)?;
            check.predicate(&path, target, op, p.operands.len())?;
            let operands = p
                .operands
                .iter()
                .enumerate()
                .map(|(k, o)| {
                    let path = format!("{path}.operands[{k}]");
                    let operand = operand_from_proto(&path, o)?;
                    check.operand(&path, target, op, &operand)?;
                    Ok(operand)
                })
                .collect::<Result<_>>()?;
            Ok(Clause::Predicate(Predicate::new(target, op, operands)))
        }
        Some(Kind::Group(g)) => {
            let path = format!("{path}.group");
            check.group(&path, depth + 1)?;
            let (all_of, any_of) =
                clauses_from_proto(check, &path, &g.all_of, &g.any_of, depth + 1)?;
            Ok(Clause::Group(Group { all_of, any_of }))
        }
    }
}

/// The operand at `path` as it reads, before its fit to the target: a value
/// that does not read is named as the whole operand, its case and reason in
/// the description.
fn operand_from_proto(path: &str, o: &v1::Operand) -> Result<core::filter::Operand> {
    use core::filter::{Operand, RelativeDate};
    use v1::operand::Kind;
    let invalid = |reason: String| ConvertError::Invalid(path.to_owned().into(), reason);
    match &o.kind {
        None => Err(invalid("must be a value, a relative date or text".into())),
        Some(Kind::Value(v)) => field_value_from_proto("value", v)
            .map(Operand::Value)
            .map_err(|e| invalid(e.to_string())),
        Some(Kind::RelativeDate(d)) => Ok(Operand::RelativeDate(RelativeDate::new(
            EnumField::from_i32("relative_date.anchor", d.anchor).map_err(|e| e.within(path))?,
            d.offset_days,
        ))),
        Some(Kind::Text(t)) => Ok(Operand::Text(t.clone())),
    }
}

/// The sort at `sort` of a request or view, checked as it is read (see
/// [`core::filter::validate_sort`]): paths are `sort[i].key`, and `sort`
/// for too many keys.
pub fn sort_from_proto(keys: &[v1::SortKey]) -> Result<Vec<core::SortKey>> {
    let mut sort = Vec::with_capacity(keys.len());
    for (i, k) in keys.iter().enumerate() {
        match core::ItemField::from_i32("key", k.key) {
            Ok(key) => sort.push(core::SortKey {
                key,
                descending: k.descending,
            }),
            Err(e) => {
                // The keys before it are checked first.
                check_sort_keys(&sort)?;
                return Err(e.within(format_args!("sort[{i}]")));
            }
        }
    }
    validate_sort(&sort)?;
    Ok(sort)
}

impl From<&core::View> for v1::View {
    fn from(v: &core::View) -> Self {
        Self {
            id: v.id.map(|id| id.to_string()).unwrap_or_default(),
            name: v.name.clone(),
            filter: Some((&v.filter).into()),
            sort: v.sort.iter().map(|&k| k.into()).collect(),
            show_count: v.show_count,
            position: v.position,
            seed: v.seed.clone(),
            revision: v.revision,
            created_at: v.created_at.map(timestamp_to_proto),
            updated_at: v.updated_at.map(timestamp_to_proto),
        }
    }
}

/// An empty id is none; the filter and sort are checked as a request's.
impl TryFrom<&v1::View> for core::View {
    type Error = ConvertError;
    fn try_from(v: &v1::View) -> Result<Self> {
        let ts = |field, t: &Option<prost_types::Timestamp>| {
            t.as_ref()
                .map(|t| timestamp_from_proto(field, t))
                .transpose()
        };
        let no_filter = v1::Filter::default();
        Ok(core::View {
            id: Some(v.id.as_str())
                .filter(|id| !id.is_empty())
                .map(|id| parse_id("id", id))
                .transpose()?,
            name: v.name.clone(),
            filter: filter_from_proto(v.filter.as_ref().unwrap_or(&no_filter), None)?,
            sort: sort_from_proto(&v.sort)?,
            show_count: v.show_count,
            position: v.position,
            seed: v.seed.clone(),
            revision: v.revision,
            created_at: ts("created_at", &v.created_at)?,
            updated_at: ts("updated_at", &v.updated_at)?,
        })
    }
}

// ---- field paths --------------------------------------------------------

/// The `FieldMask` path of `field`: its `todo.v1.Item` field name, dotted
/// for a field inside an Item sub-message (`waiting.follow_up_on`).
pub fn item_field_path(field: core::ItemField) -> &'static str {
    use core::ItemField as F;
    match field {
        F::Id => "id",
        F::CreatedVia => "created_via",
        F::CreatedAt => "created_at",
        F::UpdatedAt => "updated_at",
        F::Revision => "revision",
        F::RemoteLink => "remote_link",
        F::Title => "title",
        F::Description => "description",
        F::ItemType => "item_type",
        F::StatusCategory => "status_category",
        F::ResolutionKind => "resolution_kind",
        F::RemoteStatusName => "remote_status_name",
        F::Priority => "priority",
        F::Assignee => "assignee",
        F::Reporter => "reporter",
        F::Labels => "labels",
        F::Components => "components",
        F::FixVersions => "fix_versions",
        F::DueDate => "due_date",
        F::ParentId => "parent_id",
        F::Sprint => "sprint",
        F::StoryPoints => "story_points",
        F::OriginalEstimate => "original_estimate",
        F::RemainingEstimate => "remaining_estimate",
        F::Resolution => "resolution",
        F::RemoteCreatedAt => "remote_created_at",
        F::RemoteUpdatedAt => "remote_updated_at",
        F::TodoRefinement => "todo_refinement",
        F::Waiting => "waiting",
        F::DeferUntil => "defer_until",
        F::PrivateNotes => "private_notes",
        F::FocusWeek => "focus_week",
        F::LocalPriorityOverride => "local_priority_override",
        F::Archived => "archived",
        F::Recurrence => "recurrence",
        F::RemoteExtras => "remote_extras",
        F::Status => "status",
        F::EffectivePriority => "effective_priority",
        F::IsMine => "is_mine",
        F::PendingFields => "pending_fields",
        F::HasConflict => "has_conflict",
        F::HasPushFailure => "has_push_failure",
        F::DescriptionLossy => "description_lossy",
        F::DescriptionDirty => "description_dirty",
        F::Staleness => "staleness",
        F::Scopes => "scopes",
        F::DeletedAt => "deleted_at",
        F::MergedInto => "merged_into",
        F::IsOverdue => "is_overdue",
        F::WaitingOn => "waiting.on",
        F::WaitingReason => "waiting.reason",
        F::WaitingSince => "waiting.since",
        F::WaitingFollowUpOn => "waiting.follow_up_on",
        F::ArchivedAt => "archived.at",
        F::ArchivedReason => "archived.reason",
        F::RemoteLinkTrackerKind => "remote_link.tracker_kind",
        F::RemoteLinkInstanceId => "remote_link.instance_id",
        F::RemoteLinkRemoteId => "remote_link.remote_id",
        F::RemoteLinkKey => "remote_link.key",
        F::RemoteLinkUrl => "remote_link.url",
        F::RemoteLinkLastPulledVersion => "remote_link.last_pulled_version",
        F::RemoteLinkLinkState => "remote_link.link_state",
        F::RemoteLinkPinned => "remote_link.pinned",
        F::RecurrenceSeriesId => "recurrence.series_id",
        F::RecurrenceOccurrenceDate => "recurrence.occurrence_date",
        F::StalenessLastUserTouchAt => "staleness.last_user_touch_at",
        F::StalenessLastRemoteChangeAt => "staleness.last_remote_change_at",
        F::StalenessStatusEnteredAt => "staleness.status_entered_at",
        F::StalenessWaitingSince => "staleness.waiting_since",
    }
}

/// The field a `FieldMask` path names; `None` for an unknown path.
pub fn item_field_from_path(path: &str) -> Option<core::ItemField> {
    core::ItemField::ALL
        .iter()
        .copied()
        .find(|&f| item_field_path(f) == path)
}

// ---- undo ---------------------------------------------------------------

impl From<&core::SkipReason> for v1::UndoSkipReason {
    fn from(r: &core::SkipReason) -> Self {
        match r {
            core::SkipReason::ChangedSince => v1::UndoSkipReason::ChangedSince,
            core::SkipReason::ItemDeleted => v1::UndoSkipReason::ItemDeleted,
            core::SkipReason::Invalid(_) => v1::UndoSkipReason::Invalid,
        }
    }
}

/// The `UndoSkip.field` of a unit of `fields` that was restored together:
/// `STATUS` for one on the status triple, which is restored as one unit, the
/// field itself for any other, and `UNSPECIFIED` for a relation step, which
/// has no fields.
pub fn undo_skip_field(fields: &[core::ItemField]) -> v1::ItemField {
    use core::ItemField as F;
    let status = |f: &F| matches!(f, F::StatusCategory | F::ResolutionKind | F::TodoRefinement);
    match fields.first() {
        None => v1::ItemField::Unspecified,
        Some(_) if fields.iter().any(status) => v1::ItemField::Status,
        Some(&field) => field.into(),
    }
}

/// Why a change was left alone, for a user: the reason a value is no longer
/// valid, or a short sentence for the other reasons.
pub fn undo_skip_message(reason: &core::SkipReason, relation_step: bool) -> String {
    match reason {
        core::SkipReason::ChangedSince if relation_step => "the relation has changed since",
        core::SkipReason::ChangedSince => "the field has changed since",
        core::SkipReason::ItemDeleted => "the Item is deleted",
        core::SkipReason::Invalid(reason) => return reason.clone(),
    }
    .to_owned()
}

/// A change undo or redo left alone: the unit of `fields` on `item_id`, or
/// the relation step on `relation` (whose `from` end `item_id` is).
pub fn undo_skip_to_proto(
    item_id: core::ItemId,
    fields: &[core::ItemField],
    reason: &core::SkipReason,
    relation: Option<&core::Relation>,
) -> v1::UndoSkip {
    v1::UndoSkip {
        item_id: item_id.to_string(),
        field: undo_skip_field(fields) as i32,
        reason: v1::UndoSkipReason::from(reason) as i32,
        message: undo_skip_message(reason, relation.is_some()),
        relation: relation.map(Into::into),
    }
}

// ---- capture ------------------------------------------------------------

impl From<&core::CaptureToken> for v1::CaptureToken {
    fn from(t: &core::CaptureToken) -> Self {
        // No capture text is 4 GiB long; saturate rather than panic.
        let offset = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        Self {
            start: offset(t.start),
            end: offset(t.end),
            kind: t.kind.to_i32(),
            applied: t.applied,
            warning: t.warning.clone().unwrap_or_default(),
        }
    }
}

impl From<&core::CaptureDraft> for v1::CaptureDraft {
    fn from(d: &core::CaptureDraft) -> Self {
        Self {
            title: d.title.clone(),
            priority: d.priority.to_i32(),
            due_date: d.due_date.map(date_to_proto),
            labels: d.labels.clone(),
            waiting_on: d.waiting.as_ref().map(|w| (&w.on).into()),
            status: d.status.to_i32(),
            tokens: d.tokens.iter().map(Into::into).collect(),
            warnings: d.warnings.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use jiff::civil::date;
    use prost::Message;
    use prost_reflect::{DescriptorPool, DynamicMessage};
    use todo_core::{ItemField, Status, new_ulid};

    fn ts(s: i64) -> Timestamp {
        Timestamp::new(s, 123_000_000).unwrap()
    }

    fn full_item() -> core::Item {
        let mut item = core::Item::new_capture(
            new_ulid(),
            "Fix flaky login test".into(),
            core::CreatedVia::Tui,
            ts(1_700_000_000),
        );
        item.updated_at = ts(1_700_000_100);
        item.revision = 42;
        item.remote_link = Some(core::RemoteLink {
            tracker_kind: "jira".into(),
            instance_id: "work-jira".into(),
            remote_id: "10042".into(),
            key: "PLAT-530".into(),
            url: "https://example.atlassian.net/browse/PLAT-530".into(),
            last_pulled_version: Some(ts(1_700_000_050)),
            link_state: core::LinkState::Ok,
            pinned: true,
        });
        let c = &mut item.core;
        c.description = "**markdown**".into();
        c.item_type = "Bug".into();
        c.remote_status_name = Some("In Review".into());
        c.priority = core::Priority::Medium;
        c.assignee = Some(core::PersonRef::Person(new_ulid()));
        c.reporter = Some(core::PersonRef::Person(new_ulid()));
        c.labels = vec!["phone".into(), "auth".into()];
        c.components = vec!["api".into()];
        c.fix_versions = vec!["1.2".into()];
        c.due_date = Some(date(2026, 11, 2));
        c.parent_id = Some(new_ulid());
        c.sprint = Some("Sprint 7".into());
        c.story_points = Some(2.5);
        c.original_estimate = Some(SignedDuration::from_hours(4));
        c.remaining_estimate = Some(SignedDuration::from_mins(90));
        c.resolution = Some("Fixed".into());
        c.remote_created_at = Some(Timestamp::new(-5, -123_456_789).unwrap());
        c.remote_updated_at = Some(ts(1_600_000_500));
        item.set_status_fields(
            item.status_fields()
                .with_status(Status::Someday)
                .with_status(Status::InProgress),
        );
        let o = &mut item.overlay;
        o.waiting = Some(core::Waiting {
            on: core::PersonRef::FreeText("Dana".into()),
            reason: "review".into(),
            since: date(2026, 10, 1),
            follow_up_on: Some(date(2026, 10, 20)),
        });
        o.defer_until = Some(date(2026, 10, 25));
        o.private_notes = "notes".into();
        o.focus_week = Some("2026-W43".into());
        o.local_priority_override = Some(core::Priority::Highest);
        o.archived = Some(core::Archived {
            at: ts(1_700_000_200),
            reason: core::ArchiveReason::LeftScope,
        });
        o.recurrence = Some(core::ItemRecurrence {
            series_id: new_ulid(),
            occurrence_date: Some(date(2026, 10, 18)),
        });
        item.remote_extras.insert(
            "customfield_10010".into(),
            serde_json::json!({"a": [1, -7, "x", true, null], "b": {"c": 2.5}}),
        );
        let d = &mut item.derived;
        d.is_mine = true;
        d.pending_fields = vec![ItemField::Title, ItemField::Priority];
        d.has_conflict = true;
        d.description_lossy = true;
        d.staleness.last_user_touch_at = Some(ts(1_700_000_300));
        d.staleness.waiting_since = Some(ts(1_700_000_301));
        d.scopes = vec!["mine".into()];
        d.deleted_at = Some(ts(1_700_000_400));
        d.merged_into = Some(new_ulid());
        d.is_overdue = true;
        let other = new_ulid();
        item.relations = vec![
            core::Relation::new(item.id, other, core::RelationKind::Blocks).unwrap(),
            core::Relation::new(other, item.id, core::RelationKind::RelatesTo).unwrap(),
        ];
        item
    }

    #[test]
    fn full_item_round_trips_through_proto_and_wire() {
        let item = full_item();
        let proto = v1::Item::from(&item);
        assert_eq!(proto.status, v1::Status::InProgress as i32);
        assert_eq!(proto.effective_priority, v1::Priority::Highest as i32);
        assert_eq!(proto.todo_refinement, v1::TodoRefinement::Someday as i32);

        let decoded = v1::Item::decode(proto.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, proto);
        assert_eq!(core::Item::try_from(&decoded).unwrap(), item);
    }

    #[test]
    fn minimal_capture_round_trips() {
        let item = core::Item::new_capture(
            new_ulid(),
            "call dentist".into(),
            core::CreatedVia::Cli,
            ts(1),
        );
        let proto = v1::Item::from(&item);
        assert_eq!(proto.status, v1::Status::Inbox as i32);
        assert!(proto.staleness.is_none());
        assert_eq!(proto.resolution_kind, None);
        assert_eq!(core::Item::try_from(proto).unwrap(), item);
    }

    #[test]
    fn rejects_bad_input() {
        let good = v1::Item::from(core::Item::new_capture(
            new_ulid(),
            "x".into(),
            core::CreatedVia::Cli,
            ts(1),
        ));

        let mut p = good.clone();
        p.id = "nope".into();
        assert!(matches!(
            core::Item::try_from(&p),
            Err(ConvertError::InvalidId(f, _)) if f == "id"
        ));

        let mut p = good.clone();
        p.priority = 0;
        assert_eq!(
            core::Item::try_from(&p),
            Err(ConvertError::Unspecified("priority".into()))
        );

        let mut p = good.clone();
        p.priority = 99;
        assert_eq!(
            core::Item::try_from(&p),
            Err(ConvertError::UnknownEnum("priority".into(), 99))
        );

        let mut p = good.clone();
        p.created_at = None;
        assert_eq!(
            core::Item::try_from(&p),
            Err(ConvertError::Missing("created_at".into()))
        );

        let mut p = good;
        p.due_date = Some(gtype::Date {
            year: 2026,
            month: 2,
            day: 30,
        });
        assert!(matches!(
            core::Item::try_from(&p),
            Err(ConvertError::Invalid(f, _)) if f == "due_date"
        ));
    }

    #[test]
    fn validates_people_weeks_and_derived_fields() {
        let good = v1::Item::from(core::Item::new_capture(
            new_ulid(),
            "x".into(),
            core::CreatedVia::Cli,
            ts(1),
        ));
        let free_text = v1::PersonRef::from(&core::PersonRef::FreeText("Dana".into()));

        let mut p = good.clone();
        p.assignee = Some(free_text.clone());
        assert!(matches!(
            core::Item::try_from(&p),
            Err(ConvertError::Invalid(f, _)) if f == "assignee"
        ));
        let mut p = good.clone();
        p.reporter = Some(free_text);
        assert!(matches!(
            core::Item::try_from(&p),
            Err(ConvertError::Invalid(f, _)) if f == "reporter"
        ));

        for bad in [
            "next week",
            "2026-43",
            "2026-W00",
            "2026-W54",
            "2025-W53",
            "2026-W4",
            "+026-W01",
        ] {
            let mut p = good.clone();
            p.focus_week = Some(bad.into());
            assert!(
                matches!(
                    core::Item::try_from(&p),
                    Err(ConvertError::Invalid(f, _)) if f == "focus_week"
                ),
                "{bad}"
            );
        }
        let mut p = good.clone();
        p.focus_week = Some("2026-W53".into());
        assert_eq!(
            core::Item::try_from(&p)
                .unwrap()
                .overlay
                .focus_week
                .as_deref(),
            Some("2026-W53")
        );

        let mut p = good;
        p.pending_fields = vec![ItemField::Title.to_i32(), 0, 999, -3];
        assert_eq!(
            core::Item::try_from(&p).unwrap().derived.pending_fields,
            vec![ItemField::Title]
        );
    }

    #[test]
    fn scalars_follow_proto3_ranges() {
        let pre_epoch = Timestamp::new(-5, -123_456_789).unwrap();
        let p = timestamp_to_proto(pre_epoch);
        assert_eq!((p.seconds, p.nanos), (-6, 876_543_211));
        assert_eq!(timestamp_from_proto("t", &p).unwrap(), pre_epoch);
        let neg = prost_types::Timestamp {
            seconds: 1,
            nanos: -1,
        };
        assert!(timestamp_from_proto("t", &neg).is_err());
        // 0001-01-01T00:00:00Z is the earliest proto3 Timestamp.
        let first = prost_types::Timestamp {
            seconds: -62_135_596_800,
            nanos: 0,
        };
        assert!(timestamp_from_proto("t", &first).is_ok());
        let before = prost_types::Timestamp {
            seconds: first.seconds - 1,
            nanos: 999_999_999,
        };
        assert!(timestamp_from_proto("t", &before).is_err());

        // Year 0 is google.type.Date's "no year"; years are 1..=9999.
        for year in [0, -1, -9999, 10_000] {
            let d = gtype::Date {
                year,
                month: 10,
                day: 24,
            };
            assert!(date_from_proto("d", &d).is_err(), "{year}");
        }
        for year in [1, 9999] {
            let d = date(year as i16, 10, 24);
            assert_eq!(date_from_proto("d", &date_to_proto(d)), Ok(d));
        }

        for (seconds, nanos) in [
            (i64::MAX, 1_500_000_000),
            (0, 1_000_000_000),
            (315_576_000_001, 0),
            (1, -1),
            (i64::MIN, 0),
            (i64::MIN, -1),
            (0, i32::MIN),
            (-1, i32::MIN),
        ] {
            let d = prost_types::Duration { seconds, nanos };
            assert!(duration_from_proto("d", &d).is_err(), "{seconds} {nanos}");
        }
        let d = SignedDuration::new(-90, -5);
        assert_eq!(duration_from_proto("d", &duration_to_proto(d)).unwrap(), d);

        let v = serde_json::json!({"a": [1, 2.5, -3, 1e17]});
        let back = json_from_proto(&json_to_proto(&v));
        assert_eq!(back["a"][0], serde_json::json!(1));
        assert!(back["a"][0].is_i64());
        assert_eq!(back["a"][1], serde_json::json!(2.5));
        assert_eq!(back["a"][2], serde_json::json!(-3));
        assert!(back["a"][3].is_f64());
    }

    #[test]
    fn enums_and_fields_map_both_ways() {
        for &f in ItemField::ALL {
            let p = v1::ItemField::from(f);
            assert_eq!(core::ItemField::try_from(p).unwrap(), f);
            assert_eq!(p as i32, f.number() as i32);
        }
        assert!(v1::ItemField::try_from(300).is_err());
        let r = core::MutationResult {
            op_id: new_ulid(),
            revision: 7,
        };
        assert_eq!(
            core::MutationResult::try_from(v1::MutationResult::from(r)).unwrap(),
            r
        );
    }

    /// D4: prost-reflect renders an Item as canonical proto3 JSON.
    #[test]
    fn relations_convert_canonically() {
        use todo_core::RelationKind as K;
        let (a, b) = (Ulid::from(1u128), Ulid::from(2u128));
        let rows = [
            (K::Blocks, v1::RelationKind::Blocks),
            (K::Duplicates, v1::RelationKind::Duplicates),
            (K::RelatesTo, v1::RelationKind::RelatesTo),
        ];
        for (kind, proto_kind) in rows {
            let r = core::Relation::new(b, a, kind).unwrap();
            let p = v1::Relation::from(r);
            assert_eq!(
                (p.from_id.as_str(), p.to_id.as_str(), p.kind),
                (
                    r.from().to_string().as_str(),
                    r.to().to_string().as_str(),
                    proto_kind as i32
                )
            );
            assert_eq!(core::Relation::try_from(&p), Ok(r));
        }
        // A RELATES_TO named the other way round is the stored one.
        let swapped = v1::Relation {
            from_id: b.to_string(),
            to_id: a.to_string(),
            kind: v1::RelationKind::RelatesTo as i32,
        };
        let r = core::Relation::try_from(&swapped).unwrap();
        assert_eq!((r.from(), r.to()), (a, b));
        assert_eq!(v1::Relation::from(r).from_id, a.to_string());

        let bad = |kind: i32, to_id: String| v1::Relation {
            from_id: a.to_string(),
            to_id,
            kind,
        };
        let blocks = v1::RelationKind::Blocks as i32;
        assert_eq!(
            core::Relation::try_from(&bad(0, b.to_string())),
            Err(ConvertError::Unspecified("kind".into()))
        );
        assert_eq!(
            core::Relation::try_from(&bad(4, b.to_string())),
            Err(ConvertError::UnknownEnum("kind".into(), 4))
        );
        assert_eq!(
            core::Relation::try_from(&bad(blocks, a.to_string())),
            Err(ConvertError::Invalid(
                "to_id".into(),
                "must differ from from_id".into()
            ))
        );
        assert_eq!(
            core::Relation::try_from(&bad(blocks, "x".into())),
            Err(ConvertError::InvalidId("to_id".into(), "x".into()))
        );

        // An Item's relations are read back too, errors naming the entry.
        let mut item = v1::Item::from(&full_item());
        item.relations[1].kind = 0;
        assert_eq!(
            core::Item::try_from(&item),
            Err(ConvertError::Unspecified("relations[1].kind".into()))
        );
    }

    #[test]
    fn undo_skips_convert() {
        use todo_core::ItemField as F;
        use todo_core::SkipReason as R;
        let fields = [
            (vec![], v1::ItemField::Unspecified),
            (vec![F::Title], v1::ItemField::Title),
            (vec![F::WaitingFollowUpOn], v1::ItemField::WaitingFollowUpOn),
            (vec![F::DeletedAt], v1::ItemField::DeletedAt),
            (vec![F::TodoRefinement], v1::ItemField::Status),
            (
                vec![F::StatusCategory, F::ResolutionKind],
                v1::ItemField::Status,
            ),
            (
                vec![F::StatusCategory, F::ResolutionKind, F::TodoRefinement],
                v1::ItemField::Status,
            ),
        ];
        for (unit, want) in fields {
            assert_eq!(undo_skip_field(&unit), want, "{unit:?}");
        }

        let item_id = new_ulid();
        let relation =
            core::Relation::new(item_id, new_ulid(), core::RelationKind::Duplicates).unwrap();
        let rows = [
            (
                R::ChangedSince,
                None,
                v1::UndoSkipReason::ChangedSince,
                "the field has changed since",
            ),
            (
                R::ChangedSince,
                Some(&relation),
                v1::UndoSkipReason::ChangedSince,
                "the relation has changed since",
            ),
            (
                R::ItemDeleted,
                None,
                v1::UndoSkipReason::ItemDeleted,
                "the Item is deleted",
            ),
            (
                R::Invalid("the parent is deleted".into()),
                None,
                v1::UndoSkipReason::Invalid,
                "the parent is deleted",
            ),
        ];
        for (reason, relation, want, message) in rows {
            let unit: &[F] = if relation.is_some() { &[] } else { &[F::Title] };
            let skip = undo_skip_to_proto(item_id, unit, &reason, relation);
            assert_eq!(
                skip,
                v1::UndoSkip {
                    item_id: item_id.to_string(),
                    field: if relation.is_some() {
                        v1::ItemField::Unspecified
                    } else {
                        v1::ItemField::Title
                    } as i32,
                    reason: want as i32,
                    message: message.into(),
                    relation: relation.map(Into::into),
                }
            );
        }
    }

    #[test]
    fn prost_reflect_renders_proto3_json() {
        let pool = DescriptorPool::decode(crate::FILE_DESCRIPTOR_SET).unwrap();
        let desc = pool.get_message_by_name("todo.v1.Item").unwrap();
        let proto = v1::Item::from(&full_item());
        let dynamic =
            DynamicMessage::decode(desc.clone(), proto.encode_to_vec().as_slice()).unwrap();
        let json = serde_json::to_value(&dynamic).unwrap();

        assert_eq!(json["title"], "Fix flaky login test");
        assert_eq!(json["status"], "STATUS_IN_PROGRESS");
        assert_eq!(json["createdVia"], "CREATED_VIA_TUI");
        assert_eq!(json["revision"], "42");
        assert_eq!(json["createdAt"], "2023-11-14T22:13:20.123Z");
        assert_eq!(
            json["dueDate"],
            serde_json::json!({"year": 2026, "month": 11, "day": 2})
        );
        assert_eq!(json["originalEstimate"], "14400s");
        assert_eq!(
            json["pendingFields"],
            serde_json::json!(["ITEM_FIELD_TITLE", "ITEM_FIELD_PRIORITY"])
        );
        assert_eq!(json["remoteExtras"]["customfield_10010"]["b"]["c"], 2.5);
        assert_eq!(json["waiting"]["on"]["freeText"], "Dana");
        assert_eq!(json["relations"][0]["kind"], "RELATION_KIND_BLOCKS");
        assert_eq!(json["relations"][0]["fromId"], proto.id);

        let back: DynamicMessage = DynamicMessage::deserialize(desc, json).unwrap();
        assert_eq!(
            v1::Item::decode(back.encode_to_vec().as_slice()).unwrap(),
            proto
        );
    }

    #[test]
    fn error_paths_compose() {
        let e = ConvertError::Missing("title".into()).within("item");
        assert_eq!(e, ConvertError::Missing("item.title".into()));
        let e = e
            .within("update_item")
            .within(format_args!("mutations[{}]", 3));
        assert_eq!(e.path(), "mutations[3].update_item.item.title");
        assert_eq!(
            e.to_string(),
            "mutations[3].update_item.item.title: required field is missing"
        );

        let rows = [
            (
                ConvertError::InvalidId("id".into(), "nope".into()),
                ConvertError::InvalidId("set_status.id".into(), "nope".into()),
            ),
            (
                ConvertError::Unspecified("status".into()),
                ConvertError::Unspecified("set_status.status".into()),
            ),
            (
                ConvertError::UnknownEnum("status".into(), 9),
                ConvertError::UnknownEnum("set_status.status".into(), 9),
            ),
            (
                ConvertError::Invalid("request_id".into(), "x".into()),
                ConvertError::Invalid("set_status.request_id".into(), "x".into()),
            ),
        ];
        for (e, want) in rows {
            assert_eq!(e.within("set_status"), want);
        }
    }

    /// One value of every `FieldValue` variant.
    fn field_values() -> Vec<core::FieldValue> {
        use todo_core::FieldValue as V;
        let item = full_item();
        vec![
            V::Cleared,
            V::String("Fix \"flaky\" test".into()),
            V::Markdown("# Notes\n\n- a\n".into()),
            V::Bool(true),
            V::Number(2.5),
            V::Uint(u64::MAX),
            V::Date(date(2026, 10, 24)),
            V::Timestamp(Timestamp::new(-5, -123_456_789).unwrap()),
            V::Duration(SignedDuration::new(-90, -5)),
            V::StringSet(vec!["b".into(), "a".into()]),
            V::Person(core::PersonRef::Person(new_ulid())),
            V::Person(core::PersonRef::FreeText("Dana".into())),
            V::ItemId(new_ulid()),
            V::IsoWeek("2026-W53".into()),
            V::Priority(core::Priority::None),
            V::Status(Status::InProgress),
            V::StatusCategory(core::StatusCategory::Done),
            V::ResolutionKind(core::ResolutionKind::Cancelled),
            V::TodoRefinement(core::TodoRefinement::Someday),
            V::CreatedVia(core::CreatedVia::RemoteImport),
            V::ArchiveReason(core::ArchiveReason::AutoDone),
            V::LinkState(core::LinkState::Broken),
            V::ItemFieldSet(vec![ItemField::WaitingOn, ItemField::Title]),
            V::Waiting(item.overlay.waiting.clone().unwrap()),
            V::Archived(item.overlay.archived.unwrap()),
            V::Recurrence(item.overlay.recurrence.clone().unwrap()),
            V::RemoteLink(item.remote_link.clone().unwrap()),
            V::Staleness(item.derived.staleness),
        ]
    }

    #[test]
    fn field_values_round_trip_through_proto_and_wire() {
        let mut cases = HashSet::new();
        for value in field_values() {
            let proto = v1::FieldValue::from(&value);
            let decoded = v1::FieldValue::decode(proto.encode_to_vec().as_slice()).unwrap();
            assert_eq!(decoded, proto);
            assert_eq!(field_value_from_proto("value", &decoded), Ok(value));
            cases.insert(std::mem::discriminant(proto.kind.as_ref().unwrap()));
        }
        let pool = DescriptorPool::decode(crate::FILE_DESCRIPTOR_SET).unwrap();
        let desc = pool.get_message_by_name("todo.v1.FieldValue").unwrap();
        let oneof = desc.oneofs().next().unwrap();
        assert_eq!(cases.len(), oneof.fields().count());
    }

    #[test]
    fn field_value_errors_name_the_case() {
        use v1::field_value::Kind as K;
        let fails = |kind: Option<K>| {
            field_value_from_proto("value", &v1::FieldValue { kind }).unwrap_err()
        };
        let bad_date = gtype::Date {
            year: 2026,
            month: 2,
            day: 30,
        };
        assert_eq!(fails(None), ConvertError::Missing("value.kind".into()));
        assert_eq!(
            fails(Some(K::ItemId("nope".into()))),
            ConvertError::InvalidId("value.item_id".into(), "nope".into())
        );
        assert_eq!(
            fails(Some(K::Person(v1::PersonRef::default()))),
            ConvertError::Missing("value.person".into())
        );
        assert_eq!(
            fails(Some(K::Priority(99))),
            ConvertError::UnknownEnum("value.priority".into(), 99)
        );
        assert_eq!(
            fails(Some(K::Status(0))),
            ConvertError::Unspecified("value.status".into())
        );
        assert_eq!(
            fails(Some(K::LinkState(0))),
            ConvertError::Unspecified("value.link_state".into())
        );
        assert_eq!(
            fails(Some(K::ItemFieldSet(v1::ItemFieldSet {
                fields: vec![ItemField::Title.to_i32(), 9],
            }))),
            ConvertError::UnknownEnum("value.item_field_set.fields".into(), 9)
        );
        assert_eq!(
            fails(Some(K::Waiting(v1::Waiting::default()))),
            ConvertError::Missing("value.waiting.on".into())
        );
        assert_eq!(
            fails(Some(K::Archived(v1::Archived {
                at: Some(timestamp_to_proto(ts(1))),
                reason: 0,
            }))),
            ConvertError::Unspecified("value.archived.reason".into())
        );
        let invalid = [
            (K::IsoWeek("2025-W53".into()), "value.iso_week"),
            (K::DateValue(bad_date), "value.date_value"),
            (
                K::TimestampValue(prost_types::Timestamp {
                    seconds: 0,
                    nanos: -1,
                }),
                "value.timestamp_value",
            ),
            (
                K::DurationValue(prost_types::Duration {
                    seconds: 1,
                    nanos: -1,
                }),
                "value.duration_value",
            ),
        ];
        for (kind, path) in invalid {
            let err = fails(Some(kind));
            assert!(matches!(err, ConvertError::Invalid(..)), "{err}");
            assert_eq!(err.path(), path);
        }
    }

    #[test]
    fn item_field_paths_name_item_fields() {
        let pool = DescriptorPool::decode(crate::FILE_DESCRIPTOR_SET).unwrap();
        let item = pool.get_message_by_name("todo.v1.Item").unwrap();
        let mut seen = HashSet::new();
        for &f in ItemField::ALL {
            let path = item_field_path(f);
            assert!(seen.insert(path), "{path} twice");
            assert_eq!(item_field_from_path(path), Some(f), "{path}");
            // The ItemField number is the field numbers along the path.
            let mut message = Some(item.clone());
            let mut number = 0;
            for name in path.split('.') {
                let field = message
                    .and_then(|m| m.get_field_by_name(name))
                    .unwrap_or_else(|| panic!("{path}: no field {name}"));
                number = number * 1000 + field.number();
                message = field.kind().as_message().cloned();
            }
            assert_eq!(number, f.number(), "{path}");
        }
        for bad in [
            "",
            "*",
            "Title",
            "dueDate",
            "item.title",
            "waiting.",
            ".title",
            "waiting.bogus",
            "title.x",
        ] {
            assert_eq!(item_field_from_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn capture_drafts_convert() {
        use v1::CaptureTokenKind as K;
        let text = "call dentist +2 ^2026-11-02 #home ~dana >next +9";
        let draft = core::parse_capture(text, date(2026, 10, 22));
        let p = v1::CaptureDraft::from(&draft);
        assert_eq!(p.title, "call dentist +9");
        assert_eq!(p.priority, v1::Priority::High as i32);
        assert_eq!(p.due_date, Some(date_to_proto(date(2026, 11, 2))));
        assert_eq!(p.labels, ["home"]);
        assert_eq!(
            p.waiting_on,
            Some(v1::PersonRef::from(&core::PersonRef::FreeText(
                "dana".into()
            )))
        );
        assert_eq!(p.status, v1::Status::Next as i32);
        let warning = "+9: priority must be +1 to +5";
        assert_eq!(p.warnings, [warning]);
        let token = |start, end, kind: K, warning: &str| v1::CaptureToken {
            start,
            end,
            kind: kind as i32,
            applied: warning.is_empty(),
            warning: warning.into(),
        };
        assert_eq!(
            p.tokens,
            [
                token(13, 15, K::Priority, ""),
                token(16, 27, K::DueDate, ""),
                token(28, 33, K::Label, ""),
                token(34, 39, K::Waiting, ""),
                token(40, 45, K::Status, ""),
                token(46, 48, K::Priority, warning),
            ]
        );

        let p = v1::CaptureDraft::from(&core::parse_capture("call dentist", date(2026, 10, 22)));
        assert_eq!(
            p,
            v1::CaptureDraft {
                title: "call dentist".into(),
                priority: v1::Priority::None as i32,
                status: v1::Status::Inbox as i32,
                ..Default::default()
            }
        );

        let huge = core::CaptureToken {
            start: usize::MAX,
            end: usize::MAX,
            kind: core::CaptureTokenKind::Label,
            applied: true,
            warning: None,
        };
        let p = v1::CaptureToken::from(&huge);
        assert_eq!((p.start, p.end), (u32::MAX, u32::MAX));
    }

    // ---- filters, sorts and views ----

    use todo_core::filter::{Anchor, Operand, Operator, RelativeDate};

    fn raw(target: i32, op: i32, operands: Vec<v1::Operand>) -> v1::Clause {
        v1::Clause {
            kind: Some(v1::clause::Kind::Predicate(v1::Predicate {
                target,
                op,
                operands,
            })),
        }
    }

    fn clause(target: i32, op: v1::Operator, operands: Vec<v1::Operand>) -> v1::Clause {
        raw(target, op as i32, operands)
    }

    fn pred(target: ItemField, op: v1::Operator, operands: Vec<v1::Operand>) -> v1::Clause {
        clause(target.to_i32(), op, operands)
    }

    fn value(v: core::FieldValue) -> v1::Operand {
        v1::Operand::from(&Operand::Value(v))
    }

    fn relative(anchor: i32, offset_days: i32) -> v1::Operand {
        v1::Operand {
            kind: Some(v1::operand::Kind::RelativeDate(v1::RelativeDate {
                anchor,
                offset_days,
            })),
        }
    }

    fn group(all_of: Vec<v1::Clause>, any_of: Vec<v1::Clause>) -> v1::Clause {
        v1::Clause {
            kind: Some(v1::clause::Kind::Group(v1::ClauseGroup { all_of, any_of })),
        }
    }

    fn all_of(clauses: Vec<v1::Clause>) -> v1::Filter {
        v1::Filter {
            all_of: clauses,
            ..Default::default()
        }
    }

    fn status(s: Status) -> v1::Operand {
        value(core::FieldValue::Status(s))
    }

    /// Every operator, operand kind and anchor, in nested groups, with
    /// every flag.
    fn every_filter() -> core::Filter {
        use ItemField as F;
        use Operator as O;
        use todo_core::FieldValue as V;
        use todo_core::filter::{Clause, Group, Predicate};
        let p =
            |target, op, operands: Vec<Operand>| Clause::from(Predicate::new(target, op, operands));
        let v = Operand::Value;
        let rel = |anchor, offset| Operand::from(RelativeDate::new(anchor, offset));
        core::Filter {
            all_of: vec![
                p(
                    F::Status,
                    O::In,
                    vec![v(V::Status(Status::Next)), v(V::Status(Status::InProgress))],
                ),
                p(
                    F::Priority,
                    O::Ge,
                    vec![v(V::Priority(core::Priority::Medium))],
                ),
                p(F::Title, O::Ne, vec![v(V::String("x".into()))]),
                p(F::StoryPoints, O::Lt, vec![v(V::Number(2.5))]),
                p(F::DueDate, O::Le, vec![rel(Anchor::EndOfWeek, 3)]),
                p(F::DeferUntil, O::Gt, vec![rel(Anchor::StartOfWeek, -7)]),
                Group {
                    all_of: vec![p(F::Waiting, O::IsSet, vec![])],
                    any_of: vec![
                        p(F::Archived, O::IsUnset, vec![]),
                        p(F::Labels, O::ContainsAny, vec![v(V::String("auth".into()))]),
                        Group {
                            all_of: vec![p(
                                F::Scopes,
                                O::ContainsAll,
                                vec![v(V::String("a".into()))],
                            )],
                            any_of: vec![],
                        }
                        .into(),
                    ],
                }
                .into(),
            ],
            any_of: vec![
                p(F::FocusWeek, O::Eq, vec![rel(Anchor::CurrentWeek, 0)]),
                p(F::WaitingFollowUpOn, O::Eq, vec![rel(Anchor::Today, -1)]),
                p(
                    F::Description,
                    O::Matches,
                    vec![Operand::Text("plat 530".into())],
                ),
                p(
                    F::CreatedAt,
                    O::Ge,
                    vec![v(V::Timestamp(ts(1_700_000_000)))],
                ),
                p(
                    F::WaitingOn,
                    O::Eq,
                    vec![v(V::Person(core::PersonRef::FreeText("Dana".into())))],
                ),
            ],
            include_archived: true,
            include_done: true,
            include_deferred: true,
            include_deleted: true,
        }
    }

    #[test]
    fn filters_and_sorts_round_trip_through_proto_and_wire() {
        use prost::Message as _;
        let filter = every_filter();
        assert_eq!(filter.validate(), Ok(()));
        let proto = v1::Filter::from(&filter);
        let decoded = v1::Filter::decode(proto.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, proto);
        assert_eq!(filter_from_proto(&decoded, None), Ok(filter.clone()));
        assert_eq!(
            filter_from_proto(&decoded, Some(date(2026, 10, 25))),
            Ok(filter)
        );
        assert_eq!(
            filter_from_proto(&v1::Filter::default(), None),
            Ok(core::Filter::default())
        );

        let sort = vec![
            core::SortKey::desc(ItemField::IsOverdue),
            core::SortKey::asc(ItemField::DueDate),
        ];
        let proto: Vec<v1::SortKey> = sort.iter().map(|&k| k.into()).collect();
        assert_eq!(proto[0].key, v1::ItemField::IsOverdue as i32);
        assert!(proto[0].descending);
        assert_eq!(sort_from_proto(&proto), Ok(sort));
    }

    #[test]
    fn operators_and_anchors_map_both_ways() {
        use Operator as O;
        let ops = [
            O::In,
            O::Eq,
            O::Ne,
            O::Lt,
            O::Le,
            O::Gt,
            O::Ge,
            O::IsSet,
            O::IsUnset,
            O::ContainsAny,
            O::ContainsAll,
            O::Matches,
        ];
        for op in ops {
            assert_eq!(Operator::from_i32("op", op.to_i32()), Ok(op));
            assert_eq!(
                v1::Operator::from(op).as_str_name(),
                format!("OPERATOR_{}", op.name())
            );
        }
        let numbers: HashSet<i32> = ops.iter().map(|op| op.to_i32()).collect();
        assert_eq!(numbers, (1..=12).collect());
        assert_eq!(
            Operator::from_i32("op", 0),
            Err(ConvertError::Unspecified("op".into()))
        );
        assert_eq!(
            Operator::from_i32("op", 13),
            Err(ConvertError::UnknownEnum("op".into(), 13))
        );
        let anchors = [
            Anchor::Today,
            Anchor::StartOfWeek,
            Anchor::EndOfWeek,
            Anchor::CurrentWeek,
        ];
        for (anchor, n) in anchors.into_iter().zip(1..) {
            assert_eq!(anchor.to_i32(), n);
            assert_eq!(Anchor::from_i32("anchor", n), Ok(anchor));
        }
        assert!(Anchor::from_i32("anchor", 5).is_err());
    }

    /// Enums sort by proto number and priorities by rank, which core
    /// knows without the proto.
    #[test]
    fn enum_numbers_are_the_proto_numbers() {
        use todo_core::FieldValue as V;
        use todo_core::filter::{enum_number, priority_rank};
        let check = |v: V, proto: i32| assert_eq!(enum_number(&v), Some(proto as u32), "{v:?}");
        for s in [
            Status::Inbox,
            Status::Next,
            Status::Someday,
            Status::InProgress,
            Status::Done,
            Status::Cancelled,
        ] {
            check(V::Status(s), s.to_i32());
        }
        for c in [
            core::StatusCategory::Todo,
            core::StatusCategory::InProgress,
            core::StatusCategory::Done,
        ] {
            check(V::StatusCategory(c), c.to_i32());
        }
        for r in [core::ResolutionKind::Done, core::ResolutionKind::Cancelled] {
            check(V::ResolutionKind(r), r.to_i32());
        }
        for r in [
            core::TodoRefinement::Inbox,
            core::TodoRefinement::Next,
            core::TodoRefinement::Someday,
        ] {
            check(V::TodoRefinement(r), r.to_i32());
        }
        use todo_core::CreatedVia as Via;
        for c in [
            Via::Tui,
            Via::Cli,
            Via::Mcp,
            Via::RemoteImport,
            Via::Restore,
            Via::Split,
            Via::Recurrence,
        ] {
            check(V::CreatedVia(c), c.to_i32());
        }
        use todo_core::ArchiveReason as Why;
        for r in [
            Why::User,
            Why::LeftScope,
            Why::RemoteUnavailable,
            Why::AutoDone,
        ] {
            check(V::ArchiveReason(r), r.to_i32());
        }
        for l in [
            core::LinkState::Pending,
            core::LinkState::Ok,
            core::LinkState::Broken,
        ] {
            check(V::LinkState(l), l.to_i32());
        }
        use todo_core::Priority as P;
        assert_eq!(priority_rank(P::None), None);
        for p in [P::Lowest, P::Low, P::Medium, P::High, P::Highest] {
            // PRIORITY_NONE = 1 is outside the order.
            assert_eq!(
                priority_rank(p).map(i32::from),
                Some(p.to_i32() - 1),
                "{p:?}"
            );
        }
    }

    #[test]
    fn filter_errors_name_the_first_path_in_document_order() {
        use v1::Operator as O;
        let ok = || pred(ItemField::Status, O::In, vec![status(Status::Inbox)]);
        let bad_op = || {
            pred(
                ItemField::Title,
                O::Lt,
                vec![value(core::FieldValue::String("x".into()))],
            )
        };
        let unset = v1::Operand::default;
        let bad_date = v1::Operand {
            kind: Some(v1::operand::Kind::Value(v1::FieldValue {
                kind: Some(v1::field_value::Kind::DateValue(gtype::Date {
                    year: 2026,
                    month: 2,
                    day: 30,
                })),
            })),
        };
        let unknown_target = || clause(999, O::Eq, vec![status(Status::Inbox)]);
        let invalid = |path: &str| ConvertError::Invalid(path.to_owned().into(), String::new());
        let unspecified = |path: &str| ConvertError::Unspecified(path.to_owned().into());
        let unknown = |path: &str, n| ConvertError::UnknownEnum(path.to_owned().into(), n);
        let p = "filter.all_of[0].predicate";
        let deep = {
            let mut c = v1::Clause::default();
            for _ in 0..5 {
                c = group(vec![c], vec![]);
            }
            c
        };
        let rows: Vec<(v1::Filter, ConvertError)> = vec![
            // C51: all_of first, the semantic error before the unreadable target.
            (
                v1::Filter {
                    all_of: vec![ok(), bad_op()],
                    any_of: vec![unknown_target()],
                    ..Default::default()
                },
                invalid("filter.all_of[1].predicate.op"),
            ),
            (
                v1::Filter {
                    any_of: vec![ok(), unknown_target()],
                    ..Default::default()
                },
                unknown("filter.any_of[1].predicate.target", 999),
            ),
            (
                all_of(vec![clause(0, O::Eq, vec![])]),
                unspecified(&format!("{p}.target")),
            ),
            (
                all_of(vec![pred(ItemField::Title, O::Unspecified, vec![])]),
                unspecified(&format!("{p}.op")),
            ),
            (
                all_of(vec![raw(ItemField::Title.to_i32(), 99, vec![])]),
                unknown(&format!("{p}.op"), 99),
            ),
            (
                all_of(vec![ok(), v1::Clause::default()]),
                invalid("filter.all_of[1]"),
            ),
            (
                v1::Filter {
                    any_of: vec![ok(), group(vec![], vec![ok(), v1::Clause::default()])],
                    ..Default::default()
                },
                invalid("filter.any_of[1].group.any_of[1]"),
            ),
            (
                all_of(vec![pred(ItemField::Status, O::Eq, vec![unset()])]),
                invalid(&format!("{p}.operands[0]")),
            ),
            (
                all_of(vec![pred(
                    ItemField::DueDate,
                    O::Eq,
                    vec![bad_date.clone()],
                )]),
                invalid(&format!("{p}.operands[0]")),
            ),
            (
                all_of(vec![pred(ItemField::DueDate, O::Eq, vec![relative(0, 0)])]),
                unspecified(&format!("{p}.operands[0].relative_date.anchor")),
            ),
            (
                all_of(vec![pred(ItemField::DueDate, O::Eq, vec![relative(9, 0)])]),
                unknown(&format!("{p}.operands[0].relative_date.anchor"), 9),
            ),
            // An operand's form comes before its fit to the target.
            (
                all_of(vec![pred(ItemField::Title, O::Eq, vec![relative(0, 0)])]),
                unspecified(&format!("{p}.operands[0].relative_date.anchor")),
            ),
            // The count before any operand.
            (
                all_of(vec![pred(ItemField::Status, O::Eq, vec![unset(), unset()])]),
                invalid(&format!("{p}.operands")),
            ),
            // Operands in order, each read then fitted.
            (
                all_of(vec![pred(
                    ItemField::Status,
                    O::In,
                    vec![value(core::FieldValue::Bool(true)), unset()],
                )]),
                invalid(&format!("{p}.operands[0]")),
            ),
            (
                all_of(vec![pred(
                    ItemField::Status,
                    O::In,
                    vec![
                        status(Status::Next),
                        unset(),
                        value(core::FieldValue::Bool(true)),
                    ],
                )]),
                invalid(&format!("{p}.operands[1]")),
            ),
            // A group too deep is named before what it holds.
            (
                all_of(vec![deep]),
                invalid(&format!("filter{}", ".all_of[0].group".repeat(5))),
            ),
            // The totals come after the clauses.
            (all_of((0..129).map(|_| ok()).collect()), invalid("filter")),
            (
                all_of((0..129).map(|_| ok()).chain([unknown_target()]).collect()),
                unknown("filter.all_of[129].predicate.target", 999),
            ),
        ];
        for (filter, want) in rows {
            let got = filter_from_proto(&filter, None).unwrap_err();
            assert_eq!(
                std::mem::discriminant(&got),
                std::mem::discriminant(&want),
                "{got}"
            );
            assert_eq!(got.path(), want.path(), "{got}");
        }
        let got = filter_from_proto(
            &all_of(vec![pred(ItemField::DueDate, O::Eq, vec![bad_date])]),
            None,
        );
        assert!(
            got.unwrap_err()
                .to_string()
                .contains(": value.date_value: ")
        );
    }

    #[test]
    fn unbuilt_filters_and_offsets() {
        use v1::Operator as O;
        let unbuilt = || pred(ItemField::RemoteExtras, O::IsSet, vec![]);
        let filter = all_of(vec![unbuilt()]);
        assert_eq!(
            filter_from_proto(&filter, None).unwrap_err(),
            ConvertError::Unimplemented(
                "filter.all_of[0].predicate.target".into(),
                "filters on remote_extras are not supported yet".into()
            )
        );
        // Anything invalid wins, even later.
        let filter = all_of(vec![unbuilt(), clause(999, O::IsSet, vec![])]);
        assert!(matches!(
            filter_from_proto(&filter, None),
            Err(ConvertError::UnknownEnum(path, 999)) if path == "filter.all_of[1].predicate.target"
        ));
        // With today, a date out of range is named in its place.
        let far = || pred(ItemField::DueDate, O::Eq, vec![relative(1, i32::MAX)]);
        let filter = all_of(vec![far(), clause(999, O::IsSet, vec![])]);
        let got = filter_from_proto(&filter, Some(date(2026, 10, 25))).unwrap_err();
        assert_eq!(
            got.path(),
            "filter.all_of[0].predicate.operands[0].relative_date.offset_days"
        );
        assert!(matches!(got, ConvertError::Invalid(..)));
        let got = filter_from_proto(&filter, None).unwrap_err();
        assert_eq!(got.path(), "filter.all_of[1].predicate.target");
        assert!(filter_from_proto(&all_of(vec![far()]), None).is_ok());
    }

    #[test]
    fn sort_errors() {
        let keys = |keys: &[i32]| -> Vec<v1::SortKey> {
            keys.iter()
                .map(|&key| v1::SortKey {
                    key,
                    descending: false,
                })
                .collect()
        };
        let f = |f: ItemField| f.to_i32();
        let path = |k: &[i32]| sort_from_proto(&keys(k)).map_err(|e| e.path().to_owned());
        assert_eq!(
            path(&[f(ItemField::Labels), 999]),
            Err("sort[0].key".into())
        );
        assert_eq!(path(&[f(ItemField::Title), 999]), Err("sort[1].key".into()));
        assert!(matches!(
            sort_from_proto(&keys(&[f(ItemField::Title), 999])),
            Err(ConvertError::UnknownEnum(_, 999))
        ));
        assert!(matches!(
            sort_from_proto(&keys(&[0])),
            Err(ConvertError::Unspecified(p)) if p == "sort[0].key"
        ));
        assert_eq!(
            path(&[f(ItemField::DueDate), f(ItemField::DueDate)]),
            Err("sort[1].key".into())
        );
        let nine: Vec<i32> = [
            ItemField::Status,
            ItemField::Priority,
            ItemField::DueDate,
            ItemField::DeferUntil,
            ItemField::CreatedAt,
            ItemField::UpdatedAt,
            ItemField::Title,
            ItemField::Id,
            ItemField::Revision,
        ]
        .into_iter()
        .map(f)
        .collect();
        assert_eq!(path(&nine[..8]).map(|_| ()), Ok(()));
        assert_eq!(path(&nine), Err("sort".into()));
    }

    #[test]
    fn views_round_trip() {
        use prost::Message as _;
        let mut views = core::view::defaults();
        let mut stored = views[1].clone();
        stored.id = Some(new_ulid());
        stored.revision = 7;
        stored.created_at = Some(ts(1_700_000_000));
        stored.updated_at = Some(ts(1_700_000_100));
        views.push(stored);
        for view in views {
            let proto = v1::View::from(&view);
            let decoded = v1::View::decode(proto.encode_to_vec().as_slice()).unwrap();
            assert_eq!(decoded, proto);
            assert_eq!(core::View::try_from(&decoded), Ok(view));
        }
        let today = v1::View::from(&core::view::defaults()[1]);
        assert_eq!(today.id, "");
        assert_eq!(today.seed.as_deref(), Some("today"));
        assert_eq!(today.sort.len(), 3);

        let mut bad = today.clone();
        bad.id = "nope".into();
        assert!(
            matches!(core::View::try_from(&bad), Err(ConvertError::InvalidId(p, _)) if p == "id")
        );
        let mut bad = today.clone();
        bad.filter.as_mut().unwrap().any_of[0] = v1::Clause::default();
        assert_eq!(
            core::View::try_from(&bad).unwrap_err().path(),
            "filter.any_of[0]"
        );
        let mut bad = today;
        bad.sort[2].key = ItemField::Labels.to_i32();
        assert_eq!(
            core::View::try_from(&bad).unwrap_err().path(),
            "sort[2].key"
        );
        // A view without a filter matches everything the implicit rules let by.
        let bare = v1::View::default();
        assert_eq!(
            core::View::try_from(&bare).unwrap().filter,
            core::Filter::default()
        );
    }

    #[test]
    fn filter_errors_convert() {
        let invalid = core::FilterError::invalid("all_of[0].predicate.op", "x");
        assert_eq!(
            ConvertError::from(invalid).within("filter"),
            ConvertError::Invalid("filter.all_of[0].predicate.op".into(), "x".into())
        );
        let totals = core::FilterError::invalid("", "holds 129 predicates; at most 128");
        let e = ConvertError::from(totals).within("filter");
        assert_eq!(e.path(), "filter");
        assert_eq!(e.to_string(), "filter: holds 129 predicates; at most 128");
        let unbuilt = core::FilterError::unimplemented("sort", "y");
        assert_eq!(
            ConvertError::from(unbuilt).within("view"),
            ConvertError::Unimplemented("view.sort".into(), "y".into())
        );
    }
}
