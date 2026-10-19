//! Conversions between `todo_core` types and generated `todo.v1` messages.
//!
//! core -> proto is infallible (`From`). proto -> core is `TryFrom`: ids must
//! be valid ULIDs, enums must be known and not `*_UNSPECIFIED`, and dates and
//! instants must be in range. Derived values that core computes (`status`,
//! `effective_priority`) are emitted on the way out and ignored on the way in;
//! unknown values in the derived `pending_fields` are dropped.

use std::collections::BTreeMap;

use jiff::civil::{Date, ISOWeekDate, Weekday};
use jiff::{SignedDuration, Timestamp};
use todo_core as core;
use ulid::Ulid;

use crate::google::r#type as gtype;
use crate::v1;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConvertError {
    #[error("{0}: required field is missing")]
    Missing(&'static str),
    #[error("{0}: invalid ULID {1:?}")]
    InvalidId(&'static str, String),
    #[error("{0}: value must not be UNSPECIFIED")]
    Unspecified(&'static str),
    #[error("{0}: unknown enum value {1}")]
    UnknownEnum(&'static str, i32),
    #[error("{0}: {1}")]
    Invalid(&'static str, String),
}

type Result<T, E = ConvertError> = std::result::Result<T, E>;

fn parse_id(field: &'static str, s: &str) -> Result<Ulid> {
    s.parse()
        .map_err(|_| ConvertError::InvalidId(field, s.to_owned()))
}

fn require<T>(field: &'static str, v: Option<T>) -> Result<T> {
    v.ok_or(ConvertError::Missing(field))
}

// ---- scalars ------------------------------------------------------------

const NANOS_PER_SEC: i32 = 1_000_000_000;

/// Proto3 limit on `google.protobuf.Duration.seconds` (about 10,000 years).
const MAX_DURATION_SECS: i64 = 315_576_000_000;

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
            field,
            format!("nanos {} out of range", t.nanos),
        ));
    }
    Timestamp::new(t.seconds, t.nanos).map_err(|e| ConvertError::Invalid(field, e.to_string()))
}

pub fn date_to_proto(d: Date) -> gtype::Date {
    gtype::Date {
        year: d.year().into(),
        month: d.month().into(),
        day: d.day().into(),
    }
}

/// Only full dates are accepted (no year-less or day-less dates).
pub fn date_from_proto(field: &'static str, d: &gtype::Date) -> Result<Date> {
    let invalid = |e: String| ConvertError::Invalid(field, e);
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
    let invalid = |msg: &str| Err(ConvertError::Invalid(field, msg.into()));
    if d.nanos.abs() >= NANOS_PER_SEC {
        return invalid("nanos out of range");
    }
    if d.seconds.abs() > MAX_DURATION_SECS {
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
                    _ => Err(ConvertError::Unspecified($name)),
                }
            }
        }

        impl EnumField for $core {
            type Proto = $proto;
            fn from_i32(field: &'static str, v: i32) -> Result<Self> {
                let p = <$proto>::try_from(v).map_err(|_| ConvertError::UnknownEnum(field, v))?;
                <$core>::try_from(p).map_err(|_| ConvertError::Unspecified(field))
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

impl From<core::ItemField> for v1::ItemField {
    fn from(f: core::ItemField) -> Self {
        // Both enums share the todo.v1.ItemField numbering.
        v1::ItemField::try_from(f.number() as i32).expect("core::ItemField out of sync with proto")
    }
}

impl TryFrom<v1::ItemField> for core::ItemField {
    type Error = ConvertError;
    fn try_from(f: v1::ItemField) -> Result<Self> {
        core::ItemField::from_number(f as u32).ok_or(ConvertError::Unspecified("item_field"))
    }
}

impl EnumField for core::ItemField {
    type Proto = v1::ItemField;
    fn from_i32(field: &'static str, v: i32) -> Result<Self> {
        u32::try_from(v)
            .ok()
            .and_then(core::ItemField::from_number)
            .ok_or(if v == 0 {
                ConvertError::Unspecified(field)
            } else {
                ConvertError::UnknownEnum(field, v)
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
    let invalid = || ConvertError::Invalid(field, format!("{s:?} is not an ISO week (YYYY-Www)"));
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
            field,
            "free text only valid for waiting.on".into(),
        )),
    }
}

fn person_from_proto(field: &'static str, p: &v1::PersonRef) -> Result<core::PersonRef> {
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
        })
    }
}

impl TryFrom<v1::Item> for core::Item {
    type Error = ConvertError;
    fn try_from(p: v1::Item) -> Result<Self> {
        (&p).try_into()
    }
}

#[cfg(test)]
mod tests {
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
            Err(ConvertError::InvalidId("id", _))
        ));

        let mut p = good.clone();
        p.priority = 0;
        assert_eq!(
            core::Item::try_from(&p),
            Err(ConvertError::Unspecified("priority"))
        );

        let mut p = good.clone();
        p.priority = 99;
        assert_eq!(
            core::Item::try_from(&p),
            Err(ConvertError::UnknownEnum("priority", 99))
        );

        let mut p = good.clone();
        p.created_at = None;
        assert_eq!(
            core::Item::try_from(&p),
            Err(ConvertError::Missing("created_at"))
        );

        let mut p = good;
        p.due_date = Some(gtype::Date {
            year: 2026,
            month: 2,
            day: 30,
        });
        assert!(matches!(
            core::Item::try_from(&p),
            Err(ConvertError::Invalid("due_date", _))
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
            Err(ConvertError::Invalid("assignee", _))
        ));
        let mut p = good.clone();
        p.reporter = Some(free_text);
        assert!(matches!(
            core::Item::try_from(&p),
            Err(ConvertError::Invalid("reporter", _))
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
                    Err(ConvertError::Invalid("focus_week", _))
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

        for (seconds, nanos) in [
            (i64::MAX, 1_500_000_000),
            (0, 1_000_000_000),
            (315_576_000_001, 0),
            (1, -1),
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

        let back: DynamicMessage = DynamicMessage::deserialize(desc, json).unwrap();
        assert_eq!(
            v1::Item::decode(back.encode_to_vec().as_slice()).unwrap(),
            proto
        );
    }
}
