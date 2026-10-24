//! Column encodings and the `items` row mapping.

use std::collections::BTreeMap;

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use rusqlite::{Connection, OptionalExtension, Row, ToSql, named_params};
use todo_core::{
    Actor, ArchiveReason, Archived, CoreFields, CreatedVia, DerivedFields, FieldValue, Item,
    ItemField, ItemId, ItemRecurrence, LinkState, OverlayFields, PersonRef, Priority, Relation,
    RelationKind, RemoteLink, ResolutionKind, StalenessSignals, Status, StatusCategory,
    TodoRefinement, Topic, Waiting,
};

use crate::{OpKind, Result, StoreError};

fn corrupt(what: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(what.to_string())
}

/// A snake_case TEXT encoding for a fieldless enum.
pub(crate) trait SqlEnum: Sized + Copy + 'static {
    const NAME: &str;
    const VALUES: &[(Self, &str)];

    fn as_sql(self) -> &'static str
    where
        Self: PartialEq,
    {
        Self::VALUES
            .iter()
            .find(|(v, _)| *v == self)
            .map(|(_, s)| *s)
            .expect("every variant is listed")
    }

    fn from_sql(s: &str) -> Result<Self> {
        Self::VALUES
            .iter()
            .find(|(_, name)| *name == s)
            .map(|(v, _)| *v)
            .ok_or_else(|| corrupt(format_args!("unknown {} {s:?}", Self::NAME)))
    }
}

macro_rules! sql_enum {
    ($ty:ty, $name:literal, { $($variant:expr => $s:literal),* $(,)? }) => {
        impl SqlEnum for $ty {
            const NAME: &str = $name;
            const VALUES: &[(Self, &str)] = &[$(($variant, $s)),*];
        }
    };
}

sql_enum!(CreatedVia, "created_via", {
    CreatedVia::Tui => "tui",
    CreatedVia::Cli => "cli",
    CreatedVia::Mcp => "mcp",
    CreatedVia::RemoteImport => "remote_import",
    CreatedVia::Restore => "restore",
    CreatedVia::Split => "split",
    CreatedVia::Recurrence => "recurrence",
});
sql_enum!(StatusCategory, "status_category", {
    StatusCategory::Todo => "todo",
    StatusCategory::InProgress => "in_progress",
    StatusCategory::Done => "done",
});
sql_enum!(ResolutionKind, "resolution_kind", {
    ResolutionKind::Done => "done",
    ResolutionKind::Cancelled => "cancelled",
});
sql_enum!(TodoRefinement, "todo_refinement", {
    TodoRefinement::Inbox => "inbox",
    TodoRefinement::Next => "next",
    TodoRefinement::Someday => "someday",
});
sql_enum!(Status, "status", {
    Status::Inbox => "inbox",
    Status::Next => "next",
    Status::Someday => "someday",
    Status::InProgress => "in_progress",
    Status::Done => "done",
    Status::Cancelled => "cancelled",
});
sql_enum!(Priority, "priority", {
    Priority::None => "none",
    Priority::Lowest => "lowest",
    Priority::Low => "low",
    Priority::Medium => "medium",
    Priority::High => "high",
    Priority::Highest => "highest",
});
sql_enum!(ArchiveReason, "archived_reason", {
    ArchiveReason::User => "user",
    ArchiveReason::LeftScope => "left_scope",
    ArchiveReason::RemoteUnavailable => "remote_unavailable",
    ArchiveReason::AutoDone => "auto_done",
});
sql_enum!(LinkState, "link_state", {
    LinkState::Pending => "pending",
    LinkState::Ok => "ok",
    LinkState::Broken => "broken",
});
sql_enum!(Topic, "topic", {
    Topic::Item => "item",
    Topic::Suggestion => "suggestion",
    Topic::View => "view",
    Topic::Person => "person",
    Topic::Series => "series",
    Topic::Review => "review",
    Topic::Config => "config",
});
sql_enum!(OpKind, "op_kind", {
    OpKind::Capture => "capture",
    OpKind::Update => "update",
    OpKind::SetStatus => "set_status",
    OpKind::Archive => "archive",
    OpKind::Unarchive => "unarchive",
    OpKind::Delete => "delete",
    OpKind::Undelete => "undelete",
    OpKind::Batch => "batch",
    OpKind::Undo => "undo",
    OpKind::Redo => "redo",
    OpKind::AddRelation => "add_relation",
    OpKind::RemoveRelation => "remove_relation",
});
sql_enum!(RelationKind, "relation_kind", {
    RelationKind::Blocks => "blocks",
    RelationKind::Duplicates => "duplicates",
    RelationKind::RelatesTo => "relates_to",
});

pub(crate) fn rev_to_sql(r: u64) -> Result<i64> {
    i64::try_from(r).map_err(|_| corrupt(format_args!("revision {r} out of range")))
}

pub(crate) fn rev_from_sql(r: i64) -> Result<u64> {
    u64::try_from(r).map_err(|_| corrupt(format_args!("negative revision {r}")))
}

pub(crate) fn ts_to_sql(ts: Timestamp) -> Result<i64> {
    i64::try_from(ts.as_nanosecond())
        .map_err(|_| corrupt(format_args!("timestamp {ts} out of range")))
}

pub(crate) fn ts_from_sql(ns: i64) -> Result<Timestamp> {
    Timestamp::from_nanosecond(ns.into()).map_err(corrupt)
}

fn opt_ts_to_sql(ts: Option<Timestamp>) -> Result<Option<i64>> {
    ts.map(ts_to_sql).transpose()
}

fn opt_ts_from_sql(ns: Option<i64>) -> Result<Option<Timestamp>> {
    ns.map(ts_from_sql).transpose()
}

pub(crate) fn dur_to_sql(d: Option<SignedDuration>) -> Result<Option<i64>> {
    d.map(|d| {
        i64::try_from(d.as_nanos()).map_err(|_| corrupt(format_args!("duration {d} out of range")))
    })
    .transpose()
}

pub(crate) fn date_to_sql(d: Date) -> String {
    d.to_string()
}

fn opt_date_from_sql(s: Option<String>) -> Result<Option<Date>> {
    s.map(|s| s.parse().map_err(corrupt)).transpose()
}

fn ulid_from_sql(s: &str) -> Result<ulid::Ulid> {
    s.parse()
        .map_err(|_| corrupt(format_args!("bad ulid {s:?}")))
}

fn opt_ulid_from_sql(s: Option<String>) -> Result<Option<ulid::Ulid>> {
    s.as_deref().map(ulid_from_sql).transpose()
}

fn person_cols(p: &Option<PersonRef>) -> (Option<String>, Option<&str>) {
    match p {
        Some(PersonRef::Person(id)) => (Some(id.to_string()), None),
        Some(PersonRef::FreeText(t)) => (None, Some(t)),
        None => (None, None),
    }
}

fn person_from_sql(id: Option<String>, text: Option<String>) -> Result<Option<PersonRef>> {
    Ok(match (id, text) {
        (Some(id), _) => Some(PersonRef::Person(ulid_from_sql(&id)?)),
        (None, Some(t)) => Some(PersonRef::FreeText(t)),
        (None, None) => None,
    })
}

fn strings_to_sql(v: &[String]) -> String {
    serde_json::Value::from(v).to_string()
}

fn strings_from_sql(s: &str) -> Result<Vec<String>> {
    serde_json::from_str(s).map_err(corrupt)
}

/// The `items` columns read by [`item_from_row`], in order.
pub(crate) const ITEM_COLUMNS: &str = "\
    id, created_via, created_at, updated_at, revision, \
    remote_tracker_kind, remote_instance_id, remote_id, remote_key, remote_url, \
    remote_last_pulled_version, remote_link_state, remote_pinned, \
    title, description, item_type, status_category, resolution_kind, remote_status_name, \
    priority, assignee_person_id, assignee_text, reporter_person_id, reporter_text, \
    labels, components, fix_versions, due_date, parent_id, sprint, story_points, \
    original_estimate, remaining_estimate, resolution, remote_created_at, remote_updated_at, \
    todo_refinement, waiting_on_person_id, waiting_on_text, waiting_reason, waiting_since, \
    waiting_follow_up_on, defer_until, private_notes, focus_week, local_priority_override, \
    archived_at, archived_reason, recurrence_series_id, recurrence_occurrence_date, \
    remote_extras, is_mine, pending_fields, has_conflict, has_push_failure, \
    description_lossy, description_dirty, last_user_touch_at, last_remote_change_at, \
    status_entered_at, staleness_waiting_since, scopes, deleted_at, merged_into";

/// Decodes a row selected with [`ITEM_COLUMNS`]. `today` sets `is_overdue`.
pub(crate) fn item_from_row(row: &Row<'_>, today: Option<Date>) -> Result<Item> {
    let mut i = 0usize;
    let mut next = || {
        i += 1;
        i - 1
    };
    macro_rules! col {
        () => {
            row.get(next())?
        };
    }
    macro_rules! en {
        ($ty:ty) => {{
            let s: String = col!();
            <$ty>::from_sql(&s)?
        }};
    }
    macro_rules! opt_en {
        ($ty:ty) => {{
            let s: Option<String> = col!();
            s.as_deref().map(<$ty>::from_sql).transpose()?
        }};
    }

    let id: String = col!();
    let id = ulid_from_sql(&id)?;
    let created_via = en!(CreatedVia);
    let created_at = ts_from_sql(col!())?;
    let updated_at = ts_from_sql(col!())?;
    let revision = rev_from_sql(col!())?;

    let tracker_kind: Option<String> = col!();
    let instance_id: Option<String> = col!();
    let remote_id: Option<String> = col!();
    let key: Option<String> = col!();
    let url: Option<String> = col!();
    let last_pulled_version = opt_ts_from_sql(col!())?;
    let link_state = opt_en!(LinkState);
    let pinned: bool = col!();
    let remote_link = match (instance_id, remote_id) {
        (Some(instance_id), Some(remote_id)) => Some(RemoteLink {
            tracker_kind: tracker_kind.unwrap_or_default(),
            instance_id,
            remote_id,
            key: key.unwrap_or_default(),
            url: url.unwrap_or_default(),
            last_pulled_version,
            link_state: link_state.unwrap_or(LinkState::Ok),
            pinned,
        }),
        _ => None,
    };

    let title: String = col!();
    let description: String = col!();
    let item_type: String = col!();
    let status_category = en!(StatusCategory);
    let resolution_kind = opt_en!(ResolutionKind);
    let remote_status_name: Option<String> = col!();
    let priority = en!(Priority);
    let assignee = person_from_sql(col!(), col!())?;
    let reporter = person_from_sql(col!(), col!())?;
    let labels = strings_from_sql(&row.get::<_, String>(next())?)?;
    let components = strings_from_sql(&row.get::<_, String>(next())?)?;
    let fix_versions = strings_from_sql(&row.get::<_, String>(next())?)?;
    let due_date = opt_date_from_sql(col!())?;
    let parent_id = opt_ulid_from_sql(col!())?;
    let sprint: Option<String> = col!();
    let story_points: Option<f64> = col!();
    let original_estimate = row
        .get::<_, Option<i64>>(next())?
        .map(SignedDuration::from_nanos);
    let remaining_estimate = row
        .get::<_, Option<i64>>(next())?
        .map(SignedDuration::from_nanos);
    let resolution: Option<String> = col!();
    let remote_created_at = opt_ts_from_sql(col!())?;
    let remote_updated_at = opt_ts_from_sql(col!())?;

    let todo_refinement = en!(TodoRefinement);
    let waiting_on = person_from_sql(col!(), col!())?;
    let waiting_reason: Option<String> = col!();
    let waiting_since = opt_date_from_sql(col!())?;
    let waiting_follow_up_on = opt_date_from_sql(col!())?;
    let waiting = match (waiting_on, waiting_since) {
        (Some(on), Some(since)) => Some(Waiting {
            on,
            reason: waiting_reason.unwrap_or_default(),
            since,
            follow_up_on: waiting_follow_up_on,
        }),
        _ => None,
    };
    let defer_until = opt_date_from_sql(col!())?;
    let private_notes: String = col!();
    let focus_week: Option<String> = col!();
    let local_priority_override = opt_en!(Priority);
    let archived_at = opt_ts_from_sql(col!())?;
    let archived_reason = opt_en!(ArchiveReason);
    let archived = match (archived_at, archived_reason) {
        (Some(at), Some(reason)) => Some(Archived { at, reason }),
        _ => None,
    };
    let series_id = opt_ulid_from_sql(col!())?;
    let occurrence_date = opt_date_from_sql(col!())?;
    let recurrence = series_id.map(|series_id| ItemRecurrence {
        series_id,
        occurrence_date,
    });

    let extras: String = col!();
    let remote_extras: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(&extras).map_err(corrupt)?;

    let is_mine: bool = col!();
    let pending: String = col!();
    let pending_fields = serde_json::from_str::<Vec<u32>>(&pending)
        .map_err(corrupt)?
        .into_iter()
        .map(|n| {
            ItemField::from_number(n).ok_or_else(|| corrupt(format_args!("unknown field {n}")))
        })
        .collect::<Result<Vec<_>>>()?;
    let has_conflict: bool = col!();
    let has_push_failure: bool = col!();
    let description_lossy: bool = col!();
    let description_dirty: bool = col!();
    let staleness = StalenessSignals {
        last_user_touch_at: opt_ts_from_sql(col!())?,
        last_remote_change_at: opt_ts_from_sql(col!())?,
        status_entered_at: opt_ts_from_sql(col!())?,
        waiting_since: opt_ts_from_sql(col!())?,
    };
    let scopes = strings_from_sql(&row.get::<_, String>(next())?)?;
    let deleted_at = opt_ts_from_sql(col!())?;
    let merged_into = opt_ulid_from_sql(col!())?;

    let mut item = Item {
        id,
        created_via,
        created_at,
        updated_at,
        revision,
        remote_link,
        core: CoreFields {
            title,
            description,
            item_type,
            status_category,
            resolution_kind,
            remote_status_name,
            priority,
            assignee,
            reporter,
            labels,
            components,
            fix_versions,
            due_date,
            parent_id,
            sprint,
            story_points,
            original_estimate,
            remaining_estimate,
            resolution,
            remote_created_at,
            remote_updated_at,
        },
        overlay: OverlayFields {
            todo_refinement,
            waiting,
            defer_until,
            private_notes,
            focus_week,
            local_priority_override,
            archived,
            recurrence,
        },
        remote_extras,
        derived: DerivedFields {
            is_mine,
            pending_fields,
            has_conflict,
            has_push_failure,
            description_lossy,
            description_dirty,
            staleness,
            scopes,
            deleted_at,
            merged_into,
            is_overdue: false,
        },
        relations: Vec::new(),
    };
    item.derived.is_overdue = is_overdue(&item, today);
    Ok(item)
}

pub(crate) fn is_overdue(item: &Item, today: Option<Date>) -> bool {
    match (item.core.due_date, today) {
        (Some(due), Some(today)) => due < today && !item.status().is_closed(),
        _ => false,
    }
}

pub(crate) fn load_item(conn: &Connection, id: &str, today: Option<Date>) -> Result<Option<Item>> {
    let sql = format!("SELECT {ITEM_COLUMNS} FROM items WHERE id = ?1");
    conn.prepare_cached(&sql)?
        .query_row([id], |row| Ok(item_from_row(row, today)))
        .optional()?
        .transpose()
}

/// Calls `f` with every written `items` column as a named parameter
/// (`:<column>`). [`insert_item`] and [`update_item`] build their SQL from
/// these names, so the two cannot drift.
fn with_item_params<T>(
    item: &Item,
    f: impl FnOnce(&[(&str, &dyn ToSql)]) -> Result<T>,
) -> Result<T> {
    let link = item.remote_link.as_ref();
    let core = &item.core;
    let ov = &item.overlay;
    let d = &item.derived;
    let (assignee_id, assignee_text) = person_cols(&core.assignee);
    let (reporter_id, reporter_text) = person_cols(&core.reporter);
    let waiting_on = ov.waiting.as_ref().map(|w| &w.on).cloned();
    let (waiting_on_id, waiting_on_text) = person_cols(&waiting_on);
    let pending: Vec<u32> = d.pending_fields.iter().map(|f| f.number()).collect();

    f(named_params! {
        ":id": item.id.to_string(),
        ":created_via": item.created_via.as_sql(),
        ":created_at": ts_to_sql(item.created_at)?,
        ":updated_at": ts_to_sql(item.updated_at)?,
        ":revision": rev_to_sql(item.revision)?,
        ":remote_tracker_kind": link.map(|l| &l.tracker_kind),
        ":remote_instance_id": link.map(|l| &l.instance_id),
        ":remote_id": link.map(|l| &l.remote_id),
        ":remote_key": link.map(|l| &l.key),
        ":remote_url": link.map(|l| &l.url),
        ":remote_last_pulled_version": opt_ts_to_sql(link.and_then(|l| l.last_pulled_version))?,
        ":remote_link_state": link.map(|l| l.link_state.as_sql()),
        ":remote_pinned": link.is_some_and(|l| l.pinned),
        ":title": core.title,
        ":description": core.description,
        ":item_type": core.item_type,
        ":status_category": core.status_category.as_sql(),
        ":resolution_kind": core.resolution_kind.map(SqlEnum::as_sql),
        ":remote_status_name": core.remote_status_name,
        ":priority": core.priority.as_sql(),
        ":assignee_person_id": assignee_id,
        ":assignee_text": assignee_text,
        ":reporter_person_id": reporter_id,
        ":reporter_text": reporter_text,
        ":labels": strings_to_sql(&core.labels),
        ":components": strings_to_sql(&core.components),
        ":fix_versions": strings_to_sql(&core.fix_versions),
        ":due_date": core.due_date.map(date_to_sql),
        ":parent_id": core.parent_id.map(|p| p.to_string()),
        ":sprint": core.sprint,
        ":story_points": core.story_points,
        ":original_estimate": dur_to_sql(core.original_estimate)?,
        ":remaining_estimate": dur_to_sql(core.remaining_estimate)?,
        ":resolution": core.resolution,
        ":remote_created_at": opt_ts_to_sql(core.remote_created_at)?,
        ":remote_updated_at": opt_ts_to_sql(core.remote_updated_at)?,
        ":todo_refinement": ov.todo_refinement.as_sql(),
        ":waiting_on_person_id": waiting_on_id,
        ":waiting_on_text": waiting_on_text,
        ":waiting_reason": ov.waiting.as_ref().map(|w| &w.reason),
        ":waiting_since": ov.waiting.as_ref().map(|w| date_to_sql(w.since)),
        ":waiting_follow_up_on": ov.waiting.as_ref().and_then(|w| w.follow_up_on).map(date_to_sql),
        ":defer_until": ov.defer_until.map(date_to_sql),
        ":private_notes": ov.private_notes,
        ":focus_week": ov.focus_week,
        ":local_priority_override": ov.local_priority_override.map(SqlEnum::as_sql),
        ":archived_at": opt_ts_to_sql(ov.archived.map(|a| a.at))?,
        ":archived_reason": ov.archived.map(|a| a.reason.as_sql()),
        ":recurrence_series_id": ov.recurrence.as_ref().map(|r| r.series_id.to_string()),
        ":recurrence_occurrence_date": ov.recurrence.as_ref().and_then(|r| r.occurrence_date).map(date_to_sql),
        ":remote_extras": serde_json::to_string(&item.remote_extras).map_err(corrupt)?,
        ":is_mine": d.is_mine,
        ":pending_fields": serde_json::Value::from(pending).to_string(),
        ":has_conflict": d.has_conflict,
        ":has_push_failure": d.has_push_failure,
        ":description_lossy": d.description_lossy,
        ":description_dirty": d.description_dirty,
        ":last_user_touch_at": opt_ts_to_sql(d.staleness.last_user_touch_at)?,
        ":last_remote_change_at": opt_ts_to_sql(d.staleness.last_remote_change_at)?,
        ":status_entered_at": opt_ts_to_sql(d.staleness.status_entered_at)?,
        ":staleness_waiting_since": opt_ts_to_sql(d.staleness.waiting_since)?,
        ":scopes": strings_to_sql(&d.scopes),
        ":deleted_at": opt_ts_to_sql(d.deleted_at)?,
        ":merged_into": d.merged_into.map(|m| m.to_string()),
    })
}

pub(crate) fn insert_item(conn: &Connection, item: &Item) -> Result<()> {
    with_item_params(item, |params| {
        let names: Vec<&str> = params.iter().map(|(name, _)| *name).collect();
        let columns: Vec<&str> = names.iter().map(|name| &name[1..]).collect();
        let sql = format!(
            "INSERT INTO items ({}) VALUES ({})",
            columns.join(", "),
            names.join(", ")
        );
        conn.prepare_cached(&sql)?.execute(params)?;
        Ok(())
    })
}

/// Writes every column of the existing row `item.id`.
pub(crate) fn update_item(conn: &Connection, item: &Item) -> Result<()> {
    let updated = with_item_params(item, |params| {
        let set: Vec<String> = params
            .iter()
            .filter(|(name, _)| *name != ":id")
            .map(|(name, _)| format!("{} = {name}", &name[1..]))
            .collect();
        let sql = format!("UPDATE items SET {} WHERE id = :id", set.join(", "));
        Ok(conn.prepare_cached(&sql)?.execute(params)?)
    })?;
    if updated != 1 {
        return Err(StoreError::NotFound(item.id));
    }
    Ok(())
}

/// A [`FieldValue`] as stored in `events.old` and `events.new`.
pub(crate) fn value_to_sql(value: &FieldValue) -> Result<String> {
    serde_json::to_string(value).map_err(corrupt)
}

pub(crate) fn value_from_sql(json: &str) -> Result<FieldValue> {
    serde_json::from_str(json).map_err(corrupt)
}

/// A `derived` event's `old` or `new`: the signals as a
/// [`FieldValue::Staleness`], even when empty.
pub(crate) fn staleness_to_sql(signals: StalenessSignals) -> Result<String> {
    value_to_sql(&FieldValue::Staleness(signals))
}

pub(crate) fn staleness_from_sql(json: &str) -> Result<StalenessSignals> {
    match value_from_sql(json)? {
        FieldValue::Staleness(signals) => Ok(signals),
        FieldValue::Cleared => Ok(StalenessSignals::default()),
        other => Err(corrupt(format_args!("staleness event holds {other:?}"))),
    }
}

/// The payload of a `relation_add` or `relation_remove` event.
pub(crate) fn relation_to_sql(relation: &Relation) -> Result<String> {
    serde_json::to_string(relation).map_err(corrupt)
}

pub(crate) fn relation_from_sql(json: &str) -> Result<Relation> {
    serde_json::from_str(json).map_err(corrupt)
}

/// A `relations` row; the table keeps relations canonical.
pub(crate) fn relation_from_row(from: &str, to: &str, kind: &str) -> Result<Relation> {
    let (from, to) = (ulid_from_sql(from)?, ulid_from_sql(to)?);
    let relation = Relation::new(from, to, RelationKind::from_sql(kind)?).map_err(corrupt)?;
    if (relation.from(), relation.to()) != (from, to) {
        return Err(corrupt(format_args!(
            "relation {from} {to} is not canonical"
        )));
    }
    Ok(relation)
}

/// `ops.actor` and `events.actor`: `user`, `sync`, `system` or
/// `suggestion:<ulid>`.
pub(crate) fn actor_to_sql(actor: Actor) -> String {
    match actor {
        Actor::User => "user".to_owned(),
        Actor::Sync => "sync".to_owned(),
        Actor::System => "system".to_owned(),
        Actor::Suggestion(id) => format!("suggestion:{id}"),
    }
}

pub(crate) fn item_id_from_str(s: &str) -> Result<ItemId> {
    ulid_from_sql(s)
}
