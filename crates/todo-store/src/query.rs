//! The filter compiler (design 4.8): a resolved filter and a sort as SQL over
//! `items`, the keyset that continues after a page's last row, the `p3` page
//! tokens, and the list and count queries built on them.
//!
//! SQL text comes only from the fixed vocabulary in this module: column
//! names, the per-target expressions of [`col`] and the operator templates.
//! Every value, today included, is a bound parameter. Each predicate
//! compiles to an expression that is 0 or 1, never NULL, so that NE can be
//! exactly NOT EQ.

use std::collections::BTreeSet;

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp};
use prost::Message as _;
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};
use todo_core::filter::{
    Anchor, Clause, Filter, FilterErrorKind, Group, Operand, Operator, Predicate, RelativeDate,
    ResolvedOperand, SortKey, ValueClass, enum_number, priority_rank, target, validate_sort,
    value_of,
};
use todo_core::view::{View, ViewId};
use todo_core::{
    ArchiveReason, CreatedVia, FieldValue, Item, ItemField, LinkState, PersonRef, Priority,
    ResolutionKind, Resolved, Status, StatusCategory, TodoRefinement,
};

use crate::codec::{ITEM_COLUMNS, SqlEnum, date_to_sql, item_from_row};
use crate::{
    DEFAULT_PAGE_SIZE, ItemPage, ListInput, MAX_PAGE_SIZE, Result, StatusCounts, StoreError,
    current_revision_in, fnv1a, relation, today_local, view,
};

// ---- SQL text --------------------------------------------------------------

/// SQL text and the values bound to its `?`s, in order.
#[derive(Debug, Default)]
struct Sql {
    text: String,
    args: Vec<Value>,
}

impl Sql {
    fn push(&mut self, s: &str) -> &mut Self {
        self.text.push_str(s);
        self
    }

    fn bind(&mut self, value: Value) -> &mut Self {
        self.text.push('?');
        self.args.push(value);
        self
    }

    /// Binds each text, separated by commas, as in an IN list.
    fn bind_list(&mut self, texts: &[String]) -> &mut Self {
        for (i, t) in texts.iter().enumerate() {
            if i > 0 {
                self.text.push_str(", ");
            }
            self.bind(Value::Text(t.clone()));
        }
        self
    }
}

/// Whether the Item is linked: the schema ties the instance id to the
/// remote id.
macro_rules! linked {
    () => {
        "remote_id IS NOT NULL"
    };
}

/// Whether the Item is waiting: the codec reads `waiting` only when both its
/// `on` and its `since` are stored.
macro_rules! waiting {
    () => {
        "(waiting_since IS NOT NULL \
         AND (waiting_on_person_id IS NOT NULL OR waiting_on_text IS NOT NULL))"
    };
}

/// How a target reads in SQL: the value [`item_from_row`] decodes, so that
/// the SQL agrees with [`todo_core::filter::matches`] on every row.
#[derive(Debug, Clone, Copy)]
enum Col {
    /// An expression for the value, NULL when unset.
    Value(&'static str),
    /// `is_overdue`, which compares `due_date` with today.
    Overdue,
    /// A message (`waiting`, `archived`, ...): an expression for whether it
    /// is present.
    Presence(&'static str),
    /// A JSON array column, never NULL.
    Set(&'static str),
    /// A PersonRef, stored as a person id or else free text, while `guard`
    /// holds.
    Person {
        id: &'static str,
        text: &'static str,
        guard: &'static str,
    },
    /// `remote_extras`, which validation keeps out of filters and sorts.
    Unbuilt,
}

/// The catalogue: each target's SQL (design 4.8).
fn col(field: ItemField) -> Col {
    use Col::{Overdue, Person, Presence, Set, Unbuilt, Value as V};
    use ItemField as F;
    match field {
        F::Id => V("id"),
        F::CreatedVia => V("created_via"),
        F::CreatedAt => V("created_at"),
        F::UpdatedAt => V("updated_at"),
        F::Revision => V("revision"),
        F::RemoteLink => Presence(linked!()),
        F::Title => V("title"),
        F::Description => V("description"),
        F::ItemType => V("item_type"),
        F::StatusCategory => V("status_category"),
        F::ResolutionKind => V("resolution_kind"),
        F::RemoteStatusName => V("remote_status_name"),
        F::Priority => V("priority"),
        F::Assignee => Person {
            id: "assignee_person_id",
            text: "assignee_text",
            guard: "1",
        },
        F::Reporter => Person {
            id: "reporter_person_id",
            text: "reporter_text",
            guard: "1",
        },
        F::Labels => Set("labels"),
        F::Components => Set("components"),
        F::FixVersions => Set("fix_versions"),
        F::DueDate => V("due_date"),
        F::ParentId => V("parent_id"),
        F::Sprint => V("sprint"),
        F::StoryPoints => V("story_points"),
        F::OriginalEstimate => V("original_estimate"),
        F::RemainingEstimate => V("remaining_estimate"),
        F::Resolution => V("resolution"),
        F::RemoteCreatedAt => V("remote_created_at"),
        F::RemoteUpdatedAt => V("remote_updated_at"),
        F::TodoRefinement => V("todo_refinement"),
        F::Waiting => Presence(waiting!()),
        F::DeferUntil => V("defer_until"),
        F::PrivateNotes => V("private_notes"),
        F::FocusWeek => V("focus_week"),
        F::LocalPriorityOverride => V("local_priority_override"),
        F::Archived => Presence("archived_at IS NOT NULL"),
        F::Recurrence => Presence("recurrence_series_id IS NOT NULL"),
        F::RemoteExtras => Unbuilt,
        F::Status => V("status"),
        F::EffectivePriority => V("COALESCE(local_priority_override, priority)"),
        F::IsMine => V("is_mine"),
        F::PendingFields => Set("pending_fields"),
        F::HasConflict => V("has_conflict"),
        F::HasPushFailure => V("has_push_failure"),
        F::DescriptionLossy => V("description_lossy"),
        F::DescriptionDirty => V("description_dirty"),
        F::Staleness => Presence(
            "(last_user_touch_at IS NOT NULL OR last_remote_change_at IS NOT NULL \
             OR status_entered_at IS NOT NULL OR staleness_waiting_since IS NOT NULL)",
        ),
        F::Scopes => Set("scopes"),
        F::DeletedAt => V("deleted_at"),
        F::MergedInto => V("merged_into"),
        F::IsOverdue => Overdue,
        F::WaitingOn => Person {
            id: "waiting_on_person_id",
            text: "waiting_on_text",
            guard: waiting!(),
        },
        F::WaitingReason => V(concat!(
            "CASE WHEN ",
            waiting!(),
            " THEN COALESCE(waiting_reason, '') END"
        )),
        F::WaitingSince => V(concat!("CASE WHEN ", waiting!(), " THEN waiting_since END")),
        F::WaitingFollowUpOn => V(concat!(
            "CASE WHEN ",
            waiting!(),
            " THEN waiting_follow_up_on END"
        )),
        F::ArchivedAt => V("archived_at"),
        F::ArchivedReason => V("archived_reason"),
        F::RemoteLinkTrackerKind => V(concat!(
            "CASE WHEN ",
            linked!(),
            " THEN COALESCE(remote_tracker_kind, '') END"
        )),
        F::RemoteLinkInstanceId => V("remote_instance_id"),
        F::RemoteLinkRemoteId => V("remote_id"),
        F::RemoteLinkKey => V(concat!(
            "CASE WHEN ",
            linked!(),
            " THEN COALESCE(remote_key, '') END"
        )),
        F::RemoteLinkUrl => V(concat!(
            "CASE WHEN ",
            linked!(),
            " THEN COALESCE(remote_url, '') END"
        )),
        F::RemoteLinkLastPulledVersion => V(concat!(
            "CASE WHEN ",
            linked!(),
            " THEN remote_last_pulled_version END"
        )),
        F::RemoteLinkLinkState => V(concat!(
            "CASE WHEN ",
            linked!(),
            " THEN COALESCE(remote_link_state, 'ok') END"
        )),
        F::RemoteLinkPinned => V(concat!("CASE WHEN ", linked!(), " THEN remote_pinned END")),
        F::RecurrenceSeriesId => V("recurrence_series_id"),
        F::RecurrenceOccurrenceDate => {
            V("CASE WHEN recurrence_series_id IS NOT NULL THEN recurrence_occurrence_date END")
        }
        F::StalenessLastUserTouchAt => V("last_user_touch_at"),
        F::StalenessLastRemoteChangeAt => V("last_remote_change_at"),
        F::StalenessStatusEnteredAt => V("status_entered_at"),
        F::StalenessWaitingSince => V("staleness_waiting_since"),
    }
}

/// The FTS5 column MATCHES searches for `field`.
fn fts_column(field: ItemField) -> Option<&'static str> {
    match field {
        ItemField::Title => Some("title"),
        ItemField::Description => Some("description"),
        ItemField::Labels => Some("labels"),
        ItemField::RemoteLinkKey => Some("remote_key"),
        _ => None,
    }
}

/// Writes `field`'s value; `NULL` for a target that has none.
fn value(sql: &mut Sql, field: ItemField, today: Date) {
    match col(field) {
        Col::Value(expr) => sql.push(expr),
        Col::Overdue => sql
            .push("(due_date IS NOT NULL AND due_date < ")
            .bind(Value::Text(date_to_sql(today)))
            .push(" AND status NOT IN ('done', 'cancelled'))"),
        _ => sql.push("NULL"),
    };
}

/// Writes `field`'s sort expression: priorities as their rank (NULL for
/// PRIORITY_NONE), other enums as their proto number, anything else as its
/// value. Strings compare BINARY, which is code-point order in UTF-8.
fn sort_expr(sql: &mut Sql, field: ItemField, today: Date) {
    let ranks = match target(field).class {
        ValueClass::Priority => priority_ranks(),
        ValueClass::Enum => enum_numbers(field),
        _ => return value(sql, field, today),
    };
    sql.push("CASE (");
    value(sql, field, today);
    sql.push(")");
    for (name, rank) in ranks {
        sql.push(" WHEN '")
            .push(name)
            .push("' THEN ")
            .push(&rank.to_string());
    }
    sql.push(" END");
}

/// Each priority's stored name and rank, without PRIORITY_NONE.
fn priority_ranks() -> Vec<(&'static str, u32)> {
    Priority::VALUES
        .iter()
        .filter_map(|&(p, name)| Some((name, u32::from(priority_rank(p)?))))
        .collect()
}

/// Each value's stored name and proto number for the enum target `field`.
fn enum_numbers(field: ItemField) -> Vec<(&'static str, u32)> {
    fn numbers<T: SqlEnum + PartialEq>(wrap: fn(T) -> FieldValue) -> Vec<(&'static str, u32)> {
        T::VALUES
            .iter()
            .filter_map(|&(v, name)| Some((name, enum_number(&wrap(v))?)))
            .collect()
    }
    match field {
        ItemField::Status => numbers(FieldValue::Status),
        ItemField::StatusCategory => numbers(FieldValue::StatusCategory),
        ItemField::ResolutionKind => numbers(FieldValue::ResolutionKind),
        ItemField::TodoRefinement => numbers(FieldValue::TodoRefinement),
        ItemField::CreatedVia => numbers(FieldValue::CreatedVia),
        ItemField::ArchivedReason => numbers(FieldValue::ArchiveReason),
        ItemField::RemoteLinkLinkState => numbers(FieldValue::LinkState),
        _ => Vec::new(),
    }
}

/// Saturates out-of-range nanoseconds; validation keeps operands in range.
fn ns(n: i128) -> i64 {
    i64::try_from(n).unwrap_or(if n < 0 { i64::MIN } else { i64::MAX })
}

/// An operand as its column stores it.
fn bind_value(v: &FieldValue) -> Value {
    use FieldValue as V;
    match v {
        V::String(s) | V::Markdown(s) | V::IsoWeek(s) => Value::Text(s.clone()),
        V::Bool(b) => Value::Integer((*b).into()),
        V::Number(n) => Value::Real(*n),
        V::Uint(n) => Value::Integer(i64::try_from(*n).unwrap_or(i64::MAX)),
        V::Date(d) => Value::Text(date_to_sql(*d)),
        V::Timestamp(t) => Value::Integer(ns(t.as_nanosecond())),
        V::Duration(d) => Value::Integer(ns(d.as_nanos())),
        V::ItemId(id) => Value::Text(id.to_string()),
        V::Priority(p) => Value::Text(p.as_sql().into()),
        V::Status(s) => Value::Text(s.as_sql().into()),
        V::StatusCategory(c) => Value::Text(c.as_sql().into()),
        V::ResolutionKind(r) => Value::Text(r.as_sql().into()),
        V::TodoRefinement(r) => Value::Text(r.as_sql().into()),
        V::CreatedVia(c) => Value::Text(c.as_sql().into()),
        V::ArchiveReason(r) => Value::Text(r.as_sql().into()),
        V::LinkState(s) => Value::Text(s.as_sql().into()),
        _ => Value::Null,
    }
}

/// A sort key's value as its [`sort_expr`] reads, in the same storage
/// class (SQLite orders INTEGER before TEXT, so a mistyped value would
/// misplace the cursor): `Cleared` and PRIORITY_NONE are NULL. `None` when
/// `v` is not a value of `field`.
fn bind_key(field: ItemField, v: &FieldValue) -> Option<Value> {
    use FieldValue as V;
    use ItemField as F;
    use ValueClass as C;
    let class = target(field).class;
    Some(match (class, v) {
        (_, V::Cleared) => Value::Null,
        (C::Priority, V::Priority(p)) => {
            priority_rank(*p).map_or(Value::Null, |r| Value::Integer(r.into()))
        }
        (C::Enum, v) => {
            let fits = matches!(
                (field, v),
                (F::Status, V::Status(_))
                    | (F::StatusCategory, V::StatusCategory(_))
                    | (F::ResolutionKind, V::ResolutionKind(_))
                    | (F::TodoRefinement, V::TodoRefinement(_))
                    | (F::CreatedVia, V::CreatedVia(_))
                    | (F::ArchivedReason, V::ArchiveReason(_))
                    | (F::RemoteLinkLinkState, V::LinkState(_))
            );
            if !fits {
                return None;
            }
            Value::Integer(enum_number(v)?.into())
        }
        // SQLite binds a NaN as NULL.
        (C::Number, V::Number(n)) if !n.is_finite() => return None,
        (C::String, V::String(_))
        | (C::Id, V::ItemId(_))
        | (C::Date, V::Date(_))
        | (C::FocusWeek, V::IsoWeek(_))
        | (C::Instant, V::Timestamp(_))
        | (C::Number, V::Number(_))
        | (C::Duration, V::Duration(_))
        | (C::Uint, V::Uint(_))
        | (C::Bool, V::Bool(_)) => bind_value(v),
        _ => return None,
    })
}

// ---- filters ---------------------------------------------------------------

/// Writes the implicit rules the filter's flags leave, then its clauses.
fn where_sql(sql: &mut Sql, filter: &Resolved) {
    let (f, today) = (filter.filter(), filter.today());
    sql.push("(");
    if !f.include_deleted {
        sql.push("deleted_at IS NULL AND ");
    }
    if !f.include_archived {
        sql.push("archived_at IS NULL AND ");
    }
    if !f.include_done {
        sql.push("status NOT IN ('done', 'cancelled') AND ");
    }
    if !f.include_deferred {
        sql.push("(defer_until IS NULL OR defer_until <= ")
            .bind(Value::Text(date_to_sql(today)))
            .push(") AND ");
    }
    clauses(sql, &f.all_of, &f.any_of, today);
    sql.push(")");
}

/// Whether `c` holds for every Item: a group whose `all_of` clauses all do
/// and whose `any_of` is empty or has one that does, as an empty group.
fn always(c: &Clause<ResolvedOperand>) -> bool {
    match c {
        Clause::Predicate(_) => false,
        Clause::Group(g) => {
            g.all_of.iter().all(always) && (g.any_of.is_empty() || g.any_of.iter().any(always))
        }
    }
}

/// Every `all_of` clause and, unless `any_of` is empty, one of its clauses;
/// `1` when both are empty. A clause that [`always`] holds is left out, and
/// so is an `any_of` that has one, so that each clause written holds a
/// predicate: the predicate limit then bounds how deep the SQL nests,
/// which SQLite caps at 1000, however many groups the filter has.
fn clauses(
    sql: &mut Sql,
    all_of: &[Clause<ResolvedOperand>],
    any_of: &[Clause<ResolvedOperand>],
    today: Date,
) {
    let all_of: Vec<_> = all_of.iter().filter(|c| !always(c)).collect();
    let any_of: Vec<_> = if any_of.iter().any(always) {
        Vec::new()
    } else {
        any_of.iter().collect()
    };
    sql.push("(");
    for (i, c) in all_of.iter().enumerate() {
        if i > 0 {
            sql.push(" AND ");
        }
        clause(sql, c, today);
    }
    if !any_of.is_empty() {
        if !all_of.is_empty() {
            sql.push(" AND ");
        }
        sql.push("(");
        for (i, c) in any_of.iter().enumerate() {
            if i > 0 {
                sql.push(" OR ");
            }
            clause(sql, c, today);
        }
        sql.push(")");
    }
    if all_of.is_empty() && any_of.is_empty() {
        sql.push("1");
    }
    sql.push(")");
}

fn clause(sql: &mut Sql, c: &Clause<ResolvedOperand>, today: Date) {
    match c {
        Clause::Predicate(p) => predicate(sql, p, today),
        Clause::Group(g) => clauses(sql, &g.all_of, &g.any_of, today),
    }
}

/// One predicate, which validation has fitted to its target (filter.proto):
/// unset never matches IN, EQ or an ordering operator, and NE is NOT EQ.
fn predicate(sql: &mut Sql, p: &Predicate<ResolvedOperand>, today: Date) {
    use Operator as O;
    let values: Vec<&FieldValue> = p
        .operands
        .iter()
        .filter_map(|o| match o {
            ResolvedOperand::Value(v) => Some(v),
            ResolvedOperand::Text(_) => None,
        })
        .collect();
    match p.op {
        O::IsSet => presence(sql, p.target, today),
        O::IsUnset => {
            sql.push("NOT ");
            presence(sql, p.target, today);
        }
        O::In | O::Eq => equals(sql, p.target, &values, today),
        O::Ne => {
            sql.push("NOT ");
            equals(sql, p.target, &values, today);
        }
        O::Lt | O::Le | O::Gt | O::Ge => compare(sql, p.target, p.op, values.first(), today),
        O::ContainsAny | O::ContainsAll => {
            contains(sql, p.target, p.op == O::ContainsAll, &values);
        }
        O::Matches => {
            let text = p.operands.iter().find_map(|o| match o {
                ResolvedOperand::Text(t) => Some(t.as_str()),
                ResolvedOperand::Value(_) => None,
            });
            matches(sql, p.target, text.unwrap_or_default());
        }
    }
}

/// Whether `field` is set; for a set, whether it is non-empty.
fn presence(sql: &mut Sql, field: ItemField, today: Date) {
    match col(field) {
        Col::Value(_) | Col::Overdue => {
            sql.push("(");
            value(sql, field, today);
            sql.push(" IS NOT NULL)");
        }
        Col::Presence(expr) => {
            sql.push("(").push(expr).push(")");
        }
        Col::Set(column) => {
            sql.push("(json_array_length(").push(column).push(") > 0)");
        }
        Col::Person { id, text, guard } => {
            sql.push("(")
                .push(guard)
                .push(" AND (")
                .push(id)
                .push(" IS NOT NULL OR ")
                .push(text)
                .push(" IS NOT NULL))");
        }
        Col::Unbuilt => {
            sql.push("0");
        }
    }
}

/// Whether `field` equals one of `values`: IN, and EQ with one.
fn equals(sql: &mut Sql, field: ItemField, values: &[&FieldValue], today: Date) {
    match col(field) {
        Col::Person { id, text, guard } => {
            // IN lists, not an OR per operand, which would nest as deep as
            // the operands are many.
            let (mut ids, mut texts) = (Vec::new(), Vec::new());
            for v in values {
                match v {
                    FieldValue::Person(PersonRef::Person(p)) => ids.push(p.to_string()),
                    FieldValue::Person(PersonRef::FreeText(t)) => texts.push(t.clone()),
                    _ => {}
                }
            }
            sql.push("COALESCE(").push(guard).push(" AND (");
            if !ids.is_empty() {
                sql.push(id).push(" IN (").bind_list(&ids).push(")");
            }
            // A person id wins over free text, as the codec reads them.
            if !texts.is_empty() {
                if !ids.is_empty() {
                    sql.push(" OR ");
                }
                sql.push("(")
                    .push(id)
                    .push(" IS NULL AND ")
                    .push(text)
                    .push(" IN (")
                    .bind_list(&texts)
                    .push("))");
            }
            if ids.is_empty() && texts.is_empty() {
                sql.push("0");
            }
            sql.push("), 0)");
        }
        Col::Value(_) | Col::Overdue => {
            // Only a target that can be unset reads NULL.
            let nullable = target(field).can_unset;
            sql.push(if nullable { "COALESCE(" } else { "(" });
            value(sql, field, today);
            sql.push(" IN (");
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    sql.push(", ");
                }
                sql.bind(bind_value(v));
            }
            sql.push(if nullable { "), 0)" } else { "))" });
        }
        Col::Presence(_) | Col::Set(_) | Col::Unbuilt => {
            sql.push("0");
        }
    }
}

/// LT, LE, GT or GE; priorities compare by rank, where PRIORITY_NONE has
/// none and so never matches.
fn compare(
    sql: &mut Sql,
    field: ItemField,
    op: Operator,
    operand: Option<&&FieldValue>,
    today: Date,
) {
    let Some(v) = operand else {
        sql.push("0");
        return;
    };
    let sign = match op {
        Operator::Lt => " < ",
        Operator::Le => " <= ",
        Operator::Gt => " > ",
        _ => " >= ",
    };
    sql.push("COALESCE(");
    if target(field).class == ValueClass::Priority {
        sort_expr(sql, field, today);
        let rank = match v {
            FieldValue::Priority(p) => priority_rank(*p).map_or(Value::Null, |r| r.into()),
            _ => Value::Null,
        };
        sql.push(sign).bind(rank);
    } else {
        value(sql, field, today);
        sql.push(sign).bind(bind_value(v));
    }
    sql.push(", 0)");
}

/// CONTAINS_ANY (`all` false) or CONTAINS_ALL of a set: exact membership,
/// false on an empty set; repeated operands count once.
fn contains(sql: &mut Sql, field: ItemField, all: bool, values: &[&FieldValue]) {
    let members: BTreeSet<&str> = values
        .iter()
        .filter_map(|v| match v {
            FieldValue::String(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    let Col::Set(column) = col(field) else {
        sql.push("0");
        return;
    };
    if members.is_empty() {
        sql.push("0");
        return;
    }
    sql.push(if all {
        "((SELECT count(DISTINCT j.value)"
    } else {
        "EXISTS (SELECT 1"
    });
    sql.push(" FROM json_each(items.")
        .push(column)
        .push(") AS j WHERE j.value IN (");
    for (i, m) in members.iter().enumerate() {
        if i > 0 {
            sql.push(", ");
        }
        sql.bind(Value::Text((*m).to_owned()));
    }
    sql.push(")");
    if all {
        let n = i64::try_from(members.len()).unwrap_or(i64::MAX);
        sql.push(") = ").bind(Value::Integer(n));
    }
    sql.push(")");
}

/// MATCHES `text` in `field`'s full-text column.
fn matches(sql: &mut Sql, field: ItemField, text: &str) {
    match fts_column(field).and_then(|column| fts_query(column, text)) {
        Some(query) => {
            sql.push("(seq IN (SELECT rowid FROM items_fts WHERE items_fts MATCH ")
                .bind(Value::Text(query))
                .push("))");
        }
        None => {
            sql.push("0");
        }
    }
}

/// The FTS5 query for MATCHES `text` in `column` (filter.proto): the text
/// is split into terms on code points up to U+0020 (NUL included), each
/// term is a quoted phrase whose last word is a prefix, and every term must
/// match. Inside quotes FTS5 syntax is plain text. `None` for blank text.
pub(crate) fn fts_query(column: &str, text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| c <= ' ')
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| format!("{{{column}}} : ({})", terms.join(" ")))
}

// ---- sorting and the keyset -----------------------------------------------

/// `sort`, then the tie-break `created_at`, `id` ascending without the keys
/// `sort` already has (filter.proto `SortKey`, as
/// [`todo_core::filter::compare`]).
fn sort_keys(sort: &[SortKey]) -> Vec<SortKey> {
    let mut keys = sort.to_vec();
    for key in [
        SortKey::asc(ItemField::CreatedAt),
        SortKey::asc(ItemField::Id),
    ] {
        if !sort.iter().any(|k| k.key == key.key) {
            keys.push(key);
        }
    }
    keys
}

fn order_by(sql: &mut Sql, keys: &[SortKey], today: Date) {
    sql.push(" ORDER BY ");
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            sql.push(", ");
        }
        sort_expr(sql, key.key, today);
        sql.push(if key.descending {
            " DESC NULLS LAST"
        } else {
            " ASC NULLS LAST"
        });
    }
}

/// The rows after `cursor` (the last row's value of each key; NULL for
/// unset) in `keys` order, where unset sorts last either way: for some
/// key, the row ties every key before it and is strictly after the cursor
/// on it. Nothing is strictly after an unset value, so a NULL cursor value
/// drops its term, and no NULL is ever bound.
fn after(sql: &mut Sql, keys: &[SortKey], cursor: &[Value], today: Date) {
    sql.push("(");
    let mut terms = 0;
    for (i, key) in keys.iter().enumerate() {
        if cursor[i] == Value::Null {
            continue;
        }
        if terms > 0 {
            sql.push(" OR ");
        }
        terms += 1;
        sql.push("(");
        for (tie, c) in keys[..i].iter().zip(cursor) {
            sort_expr(sql, tie.key, today);
            match c {
                Value::Null => sql.push(" IS NULL AND "),
                c => sql.push(" = ").bind(c.clone()).push(" AND "),
            };
        }
        sql.push("(");
        sort_expr(sql, key.key, today);
        sql.push(" IS NULL OR ");
        sort_expr(sql, key.key, today);
        sql.push(if key.descending { " < " } else { " > " })
            .bind(cursor[i].clone())
            .push("))");
    }
    if terms == 0 {
        sql.push("0");
    }
    sql.push(")");
}

/// The page query: the Items `filter` passes, in `keys` order, after
/// `cursor` if any.
fn select(filter: &Resolved, keys: &[SortKey], cursor: Option<&[Value]>, limit: usize) -> Sql {
    let today = filter.today();
    let mut sql = Sql::default();
    sql.push("SELECT ")
        .push(ITEM_COLUMNS)
        .push(" FROM items WHERE ");
    where_sql(&mut sql, filter);
    if let Some(cursor) = cursor {
        sql.push(" AND ");
        after(&mut sql, keys, cursor, today);
    }
    order_by(&mut sql, keys, today);
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    sql.push(" LIMIT ").bind(Value::Integer(limit));
    sql
}

/// How many Items `filter` passes.
fn count(conn: &Connection, filter: &Resolved) -> Result<u32> {
    let mut sql = Sql::default();
    sql.push("SELECT count(*) FROM items WHERE ");
    where_sql(&mut sql, filter);
    let n: i64 = conn
        .prepare(&sql.text)?
        .query_row(params_from_iter(sql.args), |r| r.get(0))?;
    u32::try_from(n).map_err(|_| StoreError::Corrupt(format!("count {n} out of range")))
}

// ---- page tokens -------------------------------------------------------------

const TOKEN_PREFIX: &str = "p3.";

/// A page token: `p3.` and the base64url of this message. It binds the
/// token to the query that issued it, pins that query's today, and holds
/// the keys of the page's last row.
#[derive(Clone, PartialEq, prost::Message)]
struct PageToken {
    /// [`fingerprint`] of the effective filter and sort.
    #[prost(fixed64, tag = "1")]
    fingerprint: u64,
    /// The first page's today.
    #[prost(message, optional, tag = "2")]
    today: Option<TokenDate>,
    /// The last row's value of each sort key but `id`, in order; `cleared`
    /// for unset.
    #[prost(message, repeated, tag = "3")]
    keys: Vec<TokenValue>,
    /// The last row's id.
    #[prost(string, tag = "4")]
    id: String,
}

/// `google.type.Date`.
#[derive(Clone, PartialEq, prost::Message)]
struct TokenDate {
    #[prost(int32, tag = "1")]
    year: i32,
    #[prost(int32, tag = "2")]
    month: i32,
    #[prost(int32, tag = "3")]
    day: i32,
}

/// `google.protobuf.Timestamp` and `Duration`, which share a layout.
#[derive(Clone, PartialEq, prost::Message)]
struct TokenSeconds {
    #[prost(int64, tag = "1")]
    seconds: i64,
    #[prost(int32, tag = "2")]
    nanos: i32,
}

/// `google.protobuf.Empty`.
#[derive(Clone, PartialEq, prost::Message)]
struct TokenEmpty {}

/// `todo.v1.FieldValue`, with its field numbers, for the cases a sort key
/// takes.
#[derive(Clone, PartialEq, prost::Message)]
struct TokenValue {
    #[prost(
        oneof = "TokenKind",
        tags = "1, 2, 4, 5, 6, 7, 8, 9, 12, 13, 20, 21, 22, 23, 24, 25, 26, 27"
    )]
    kind: Option<TokenKind>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
enum TokenKind {
    #[prost(message, tag = "1")]
    Cleared(TokenEmpty),
    #[prost(string, tag = "2")]
    String(String),
    #[prost(bool, tag = "4")]
    Bool(bool),
    #[prost(double, tag = "5")]
    Number(f64),
    #[prost(uint64, tag = "6")]
    Uint(u64),
    #[prost(message, tag = "7")]
    Date(TokenDate),
    #[prost(message, tag = "8")]
    Timestamp(TokenSeconds),
    #[prost(message, tag = "9")]
    Duration(TokenSeconds),
    #[prost(string, tag = "12")]
    ItemId(String),
    #[prost(string, tag = "13")]
    IsoWeek(String),
    // Enums by proto number.
    #[prost(int32, tag = "20")]
    Priority(i32),
    #[prost(int32, tag = "21")]
    Status(i32),
    #[prost(int32, tag = "22")]
    StatusCategory(i32),
    #[prost(int32, tag = "23")]
    ResolutionKind(i32),
    #[prost(int32, tag = "24")]
    TodoRefinement(i32),
    #[prost(int32, tag = "25")]
    CreatedVia(i32),
    #[prost(int32, tag = "26")]
    ArchiveReason(i32),
    #[prost(int32, tag = "27")]
    LinkState(i32),
}

impl TokenDate {
    fn new(d: Date) -> Self {
        Self {
            year: d.year().into(),
            month: d.month().into(),
            day: d.day().into(),
        }
    }

    /// The date, if it is one between 0001-01-01 and 9999-12-31, as every
    /// today is.
    fn date(&self) -> Option<Date> {
        Date::new(
            self.year.try_into().ok()?,
            self.month.try_into().ok()?,
            self.day.try_into().ok()?,
        )
        .ok()
        .filter(|d| (1..=9999).contains(&d.year()))
    }
}

/// PRIORITY_NONE 1, LOWEST 2, ..., HIGHEST 6.
fn priority_number(p: Priority) -> u32 {
    priority_rank(p).map_or(1, |r| u32::from(r) + 1)
}

/// The value of the enum `T` whose proto number is `n`.
fn from_number<T: SqlEnum + PartialEq>(n: i32, wrap: fn(T) -> FieldValue) -> Option<FieldValue> {
    T::VALUES
        .iter()
        .map(|&(v, _)| wrap(v))
        .find(|v| enum_number(v).and_then(|m| i32::try_from(m).ok()) == Some(n))
}

impl TokenKind {
    /// A sort key's value; `None` for a kind no sort key has.
    fn new(v: &FieldValue) -> Option<Self> {
        use FieldValue as V;
        let number = |v| enum_number(v).and_then(|n| i32::try_from(n).ok());
        Some(match v {
            V::Cleared => TokenKind::Cleared(TokenEmpty {}),
            V::String(s) => TokenKind::String(s.clone()),
            V::Bool(b) => TokenKind::Bool(*b),
            V::Number(n) => TokenKind::Number(*n),
            V::Uint(n) => TokenKind::Uint(*n),
            V::Date(d) => TokenKind::Date(TokenDate::new(*d)),
            V::Timestamp(t) => {
                let (mut seconds, mut nanos) = (t.as_second(), t.subsec_nanosecond());
                if nanos < 0 {
                    seconds -= 1;
                    nanos += 1_000_000_000;
                }
                TokenKind::Timestamp(TokenSeconds { seconds, nanos })
            }
            V::Duration(d) => TokenKind::Duration(TokenSeconds {
                seconds: d.as_secs(),
                nanos: d.subsec_nanos(),
            }),
            V::ItemId(id) => TokenKind::ItemId(id.to_string()),
            V::IsoWeek(w) => TokenKind::IsoWeek(w.clone()),
            V::Priority(p) => TokenKind::Priority(priority_number(*p).try_into().ok()?),
            V::Status(_) => TokenKind::Status(number(v)?),
            V::StatusCategory(_) => TokenKind::StatusCategory(number(v)?),
            V::ResolutionKind(_) => TokenKind::ResolutionKind(number(v)?),
            V::TodoRefinement(_) => TokenKind::TodoRefinement(number(v)?),
            V::CreatedVia(_) => TokenKind::CreatedVia(number(v)?),
            V::ArchiveReason(_) => TokenKind::ArchiveReason(number(v)?),
            V::LinkState(_) => TokenKind::LinkState(number(v)?),
            _ => return None,
        })
    }

    /// The value; `None` when it does not read.
    fn value(&self) -> Option<FieldValue> {
        use FieldValue as V;
        Some(match self {
            TokenKind::Cleared(_) => V::Cleared,
            TokenKind::String(s) => V::String(s.clone()),
            TokenKind::Bool(b) => V::Bool(*b),
            TokenKind::Number(n) => V::Number(*n),
            TokenKind::Uint(n) => V::Uint(*n),
            TokenKind::Date(d) => V::Date(d.date()?),
            TokenKind::Timestamp(t) => V::Timestamp(Timestamp::new(t.seconds, t.nanos).ok()?),
            // Nanoseconds within a second, as `google.protobuf.Duration`
            // has them: more could overflow the seconds.
            TokenKind::Duration(d) if d.nanos.unsigned_abs() >= 1_000_000_000 => return None,
            TokenKind::Duration(d) => V::Duration(SignedDuration::new(d.seconds, d.nanos)),
            TokenKind::ItemId(id) => V::ItemId(id.parse().ok()?),
            TokenKind::IsoWeek(w) => V::IsoWeek(w.clone()),
            TokenKind::Priority(n) => Priority::VALUES
                .iter()
                .find(|&&(p, _)| i32::try_from(priority_number(p)).ok() == Some(*n))
                .map(|&(p, _)| V::Priority(p))?,
            TokenKind::Status(n) => from_number::<Status>(*n, V::Status)?,
            TokenKind::StatusCategory(n) => from_number::<StatusCategory>(*n, V::StatusCategory)?,
            TokenKind::ResolutionKind(n) => from_number::<ResolutionKind>(*n, V::ResolutionKind)?,
            TokenKind::TodoRefinement(n) => from_number::<TodoRefinement>(*n, V::TodoRefinement)?,
            TokenKind::CreatedVia(n) => from_number::<CreatedVia>(*n, V::CreatedVia)?,
            TokenKind::ArchiveReason(n) => from_number::<ArchiveReason>(*n, V::ArchiveReason)?,
            TokenKind::LinkState(n) => from_number::<LinkState>(*n, V::LinkState)?,
        })
    }
}

impl PageToken {
    /// The token after `last`, the last row of a page listed by `keys` on
    /// `today`.
    fn after(last: &Item, fingerprint: u64, today: Date, keys: &[SortKey]) -> Result<Self> {
        let keys = keys
            .iter()
            .filter(|k| k.key != ItemField::Id)
            .map(|k| {
                // As the comparator reads it: PRIORITY_NONE is unset.
                let value = value_of(last, k.key, today)
                    .filter(|v| !matches!(v, FieldValue::Priority(Priority::None)))
                    .unwrap_or(FieldValue::Cleared);
                let kind = TokenKind::new(&value).ok_or_else(|| {
                    StoreError::Corrupt(format!("sort key {:?} holds {value:?}", k.key))
                })?;
                Ok(TokenValue { kind: Some(kind) })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            fingerprint,
            today: Some(TokenDate::new(today)),
            keys,
            id: last.id.to_string(),
        })
    }

    fn encode(&self) -> String {
        format!("{TOKEN_PREFIX}{}", base64url(&self.encode_to_vec()))
    }

    /// Reads a token and the today it pins; a `p2` token, or anything else
    /// that is not a `p3` token, is [`StoreError::InvalidPageToken`].
    fn parse(s: &str) -> Result<(Self, Date)> {
        let token = s
            .strip_prefix(TOKEN_PREFIX)
            .and_then(from_base64url)
            .and_then(|bytes| PageToken::decode(bytes.as_slice()).ok())
            .ok_or(StoreError::InvalidPageToken)?;
        let today = token
            .today
            .as_ref()
            .and_then(TokenDate::date)
            .ok_or(StoreError::InvalidPageToken)?;
        Ok((token, today))
    }

    /// The cursor for `keys`, as [`after`] binds it, if the token was issued
    /// for the query with this `fingerprint`.
    fn cursor(&self, fingerprint: u64, keys: &[SortKey]) -> Result<Vec<Value>> {
        let invalid = || StoreError::InvalidPageToken;
        if self.fingerprint != fingerprint || self.keys.len() + 1 != keys.len() {
            return Err(invalid());
        }
        let id: todo_core::ItemId = self.id.parse().map_err(|_| invalid())?;
        let mut values = self.keys.iter();
        keys.iter()
            .map(|k| {
                if k.key == ItemField::Id {
                    return Ok(Value::Text(id.to_string()));
                }
                let v = values.next().and_then(|v| v.kind.as_ref()?.value());
                v.and_then(|v| bind_key(k.key, &v)).ok_or_else(invalid)
            })
            .collect()
    }
}

/// Binds a token to its query: FNV-1a over the effective filter and sort,
/// keys included. Not over the page size, which a client may change between
/// pages, nor today, which the token pins.
fn fingerprint(filter: &Filter, keys: &[SortKey]) -> Result<u64> {
    let json = |e: serde_json::Error| StoreError::Corrupt(e.to_string());
    let mut bytes = TOKEN_PREFIX.as_bytes().to_vec();
    bytes.extend(serde_json::to_vec(filter).map_err(json)?);
    bytes.push(b'|');
    bytes.extend(serde_json::to_vec(keys).map_err(json)?);
    Ok(fnv1a(&bytes))
}

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url (RFC 4648 5) without padding.
fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(char::from(BASE64URL[(n >> (18 - 6 * i) & 63) as usize]));
        }
    }
    out
}

/// The bytes of unpadded base64url; `None` unless `s` is exactly what
/// [`base64url`] writes for them.
fn from_base64url(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = BASE64URL.iter().position(|&a| a == c)?;
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (bits < 6 && acc == 0).then_some(out)
}

// ---- the queries -------------------------------------------------------------

/// The filter and sort a listing of `view` uses, narrowed by a request's
/// `filter` and `sort` (item_service.proto `ListItemsRequest.view_id`): the
/// view's filter AND the request's clauses as one more group, each include
/// flag set in either, and the request's sort unless it is empty.
fn narrow(view: View, filter: &Filter, sort: &[SortKey]) -> (Filter, Vec<SortKey>) {
    let mut merged = view.filter;
    // An empty group matches everything, so it is left out.
    if !filter.all_of.is_empty() || !filter.any_of.is_empty() {
        merged.all_of.push(Clause::Group(Group {
            all_of: filter.all_of.clone(),
            any_of: filter.any_of.clone(),
        }));
    }
    merged.include_archived |= filter.include_archived;
    merged.include_done |= filter.include_done;
    merged.include_deferred |= filter.include_deferred;
    merged.include_deleted |= filter.include_deleted;
    let sort = if sort.is_empty() {
        view.sort
    } else {
        sort.to_vec()
    };
    (merged, sort)
}

/// The view `id`, or [`StoreError::ViewNotFound`].
fn load_view(conn: &Connection, id: ViewId) -> Result<View> {
    view::load(conn, id)?.ok_or(StoreError::ViewNotFound(id))
}

pub(crate) fn list_items_tx(conn: &Connection, input: ListInput) -> Result<ItemPage> {
    let page_size = match input.page_size {
        0 => DEFAULT_PAGE_SIZE,
        n => n.min(MAX_PAGE_SIZE),
    } as usize;
    let token = input.page_token.as_deref().map(PageToken::parse);
    // A later page keeps the first page's today.
    let today = match &token {
        Some(Ok((_, today))) => *today,
        _ => input.today.unwrap_or_else(today_local),
    };
    // Errors in the order filter.proto gives them: the filter's first
    // INVALID (its relative dates checked for today), the sort's, and only
    // then the filter's UNIMPLEMENTED; then the page token, then the view.
    // The request's paths are its own, not those of a narrowed view.
    let checked = input.filter.validate_on(today);
    if let Err(e) = &checked
        && e.kind == FilterErrorKind::Invalid
    {
        return Err(StoreError::Filter(e.clone()));
    }
    validate_sort(&input.sort).map_err(StoreError::Filter)?;
    checked.map_err(StoreError::Filter)?;
    let token = token.transpose()?.map(|(token, _)| token);

    let tx = conn.unchecked_transaction()?;
    let revision = current_revision_in(&tx)?;
    let (filter, sort, resolved) = match input.view_id {
        None => {
            let resolved = input.filter.resolve(today).map_err(StoreError::Filter)?;
            (input.filter, input.sort, resolved)
        }
        Some(id) => {
            let (filter, sort) = narrow(load_view(&tx, id)?, &input.filter, &input.sort);
            // The request resolves (checked above), so only the stored
            // view can fail here: on a token's today, which its first page
            // resolved it for, only when the token was not issued for it.
            let resolved = filter.resolve(today).map_err(|e| match token {
                Some(_) => StoreError::InvalidPageToken,
                None => StoreError::Corrupt(format!("view {id}: {e}")),
            })?;
            (filter, sort, resolved)
        }
    };
    let keys = sort_keys(&sort);
    let fingerprint = fingerprint(&filter, &keys)?;
    let cursor = token.map(|t| t.cursor(fingerprint, &keys)).transpose()?;

    let sql = select(&resolved, &keys, cursor.as_deref(), page_size + 1);
    let mut stmt = tx.prepare(&sql.text)?;
    let mut rows = stmt.query(params_from_iter(sql.args))?;
    let mut items = Vec::new();
    let mut has_more = false;
    while let Some(row) = rows.next()? {
        if items.len() == page_size {
            has_more = true;
            break;
        }
        // The same today as the filter, so is_overdue agrees with it.
        items.push(item_from_row(row, Some(today))?);
    }
    relation::attach(&tx, &mut items)?;
    let next_page_token = match items.last() {
        Some(last) if has_more => Some(PageToken::after(last, fingerprint, today, &keys)?.encode()),
        _ => None,
    };
    Ok(ItemPage {
        items,
        next_page_token,
        revision,
    })
}

pub(crate) fn status_counts_tx(
    conn: &Connection,
    today: Date,
    include_views: bool,
) -> Result<StatusCounts> {
    use ItemField as F;
    let tx = conn.unchecked_transaction()?;
    let revision = current_revision_in(&tx)?;
    // These filters are the store's own and the stored views', so one that
    // does not resolve is corrupt.
    let count_of = |filter: &Filter, what: &dyn std::fmt::Display| {
        let resolved = filter
            .resolve(today)
            .map_err(|e| StoreError::Corrupt(format!("{what}: {e}")))?;
        count(&tx, &resolved)
    };
    // Each passes the implicit rules, as any listing does.
    let one = |field, op, operand: Operand| Filter {
        all_of: vec![Predicate::new(field, op, vec![operand]).into()],
        ..Filter::default()
    };
    let today_operand = || Operand::from(RelativeDate::new(Anchor::Today, 0));
    let inbox = one(
        F::Status,
        Operator::In,
        FieldValue::Status(Status::Inbox).into(),
    );
    let inbox = count_of(&inbox, &"inbox")?;
    let due = count_of(&one(F::DueDate, Operator::Le, today_operand()), &"due")?;
    let overdue = count_of(&one(F::DueDate, Operator::Lt, today_operand()), &"overdue")?;
    let mut views = Vec::new();
    if include_views {
        for v in view::load_all(&tx)? {
            let Some(id) = v.id.filter(|_| v.show_count) else {
                continue;
            };
            // As ListItems lists the view with nothing to narrow it.
            let (filter, _) = narrow(v, &Filter::default(), &[]);
            views.push((id, count_of(&filter, &format_args!("view {id}"))?));
        }
    }
    Ok(StatusCounts {
        inbox,
        due,
        overdue,
        views,
        revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips_and_is_canonical() {
        for n in 0..=7 {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 97 + 251) as u8).collect();
            let s = base64url(&bytes);
            assert!(!s.contains('='), "{s}");
            assert_eq!(from_base64url(&s), Some(bytes));
        }
        assert_eq!(base64url(b"\xfb\xff"), "-_8");
        assert_eq!(from_base64url("-_8"), Some(b"\xfb\xff".to_vec()));
        // Not the alphabet, a lone character, and stray low bits.
        for bad in ["a+b", "a/b", "a=", "Q", "-_9"] {
            assert_eq!(from_base64url(bad), None, "{bad}");
        }
    }

    #[test]
    fn fts_queries_quote_every_term_as_a_prefix_phrase() {
        assert_eq!(
            fts_query("title", "plat-530  fix").as_deref(),
            Some("{title} : (\"plat-530\"* \"fix\"*)")
        );
        assert_eq!(
            fts_query("labels", "say \"hi\"\0NOT a*b").as_deref(),
            Some("{labels} : (\"say\"* \"\"\"hi\"\"\"* \"NOT\"* \"a*b\"*)")
        );
        assert_eq!(fts_query("title", " \t\0\n"), None);
    }

    #[test]
    fn token_values_round_trip() {
        use jiff::civil::date;
        let values = [
            FieldValue::Cleared,
            FieldValue::String("x".into()),
            FieldValue::Bool(true),
            FieldValue::Number(-0.5),
            FieldValue::Uint(7),
            FieldValue::Date(date(2026, 10, 25)),
            FieldValue::Timestamp(Timestamp::new(-2, 5).unwrap()),
            FieldValue::Duration(SignedDuration::new(-3, -9)),
            FieldValue::ItemId(todo_core::new_ulid()),
            FieldValue::IsoWeek("2026-W43".into()),
            FieldValue::Priority(Priority::None),
            FieldValue::Priority(Priority::Highest),
            FieldValue::Status(Status::Cancelled),
            FieldValue::StatusCategory(StatusCategory::InProgress),
            FieldValue::ResolutionKind(ResolutionKind::Done),
            FieldValue::TodoRefinement(TodoRefinement::Someday),
            FieldValue::CreatedVia(CreatedVia::Recurrence),
            FieldValue::ArchiveReason(ArchiveReason::AutoDone),
            FieldValue::LinkState(LinkState::Pending),
        ];
        for v in values {
            let kind = TokenKind::new(&v).unwrap();
            let bytes = TokenValue { kind: Some(kind) }.encode_to_vec();
            let back = TokenValue::decode(bytes.as_slice()).unwrap();
            assert_eq!(back.kind.unwrap().value(), Some(v));
        }
        assert_eq!(priority_number(Priority::Highest), 6);
        assert_eq!(TokenKind::Status(0).value(), None);
    }

    /// A token a client made up, with the right fingerprint (the client
    /// can read it from any token) but values no page ends at, is
    /// [`StoreError::InvalidPageToken`]: never a panic, a corrupt-database
    /// error or a page from a cursor that binds as NULL.
    #[tokio::test]
    async fn forged_tokens_are_invalid() {
        use jiff::civil::date;
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(&dir.path().join("todo.db")).unwrap();
        let sun = date(2026, 10, 25);
        let forge = |input: &ListInput, today: Date, keys: Vec<TokenKind>| {
            let (filter, sort) = (&input.filter, &input.sort);
            PageToken {
                fingerprint: fingerprint(filter, &sort_keys(sort)).unwrap(),
                today: Some(TokenDate::new(today)),
                keys: keys
                    .into_iter()
                    .map(|kind| TokenValue { kind: Some(kind) })
                    .collect(),
                id: todo_core::new_ulid().to_string(),
            }
            .encode()
        };
        let list = |input: &ListInput, token: String| {
            store.list_items(ListInput {
                page_token: Some(token),
                ..input.clone()
            })
        };
        let epoch = || TokenKind::Timestamp(TokenSeconds::default());
        let by = |key| ListInput {
            sort: vec![SortKey::asc(key)],
            today: Some(sun),
            ..ListInput::default()
        };

        // Nanoseconds past a second that overflow the seconds.
        let estimate = by(ItemField::OriginalEstimate);
        let duration = |seconds, nanos| TokenKind::Duration(TokenSeconds { seconds, nanos });
        let token = forge(&estimate, sun, vec![duration(5, 1), epoch()]);
        assert!(list(&estimate, token).await.is_ok());
        for (seconds, nanos) in [(i64::MAX, 1_500_000_000), (0, -1_000_000_000)] {
            let token = forge(&estimate, sun, vec![duration(seconds, nanos), epoch()]);
            let listed = list(&estimate, token).await;
            assert!(
                matches!(listed, Err(StoreError::InvalidPageToken)),
                "{listed:?}"
            );
        }

        // A number that binds as NULL.
        let points = by(ItemField::StoryPoints);
        let token = forge(&points, sun, vec![TokenKind::Number(0.5), epoch()]);
        assert!(list(&points, token).await.is_ok());
        for n in [f64::NAN, f64::INFINITY] {
            let token = forge(&points, sun, vec![TokenKind::Number(n), epoch()]);
            let listed = list(&points, token).await;
            assert!(
                matches!(listed, Err(StoreError::InvalidPageToken)),
                "{listed:?}"
            );
        }

        // A today no first page has, for a view whose relative dates do not
        // resolve on it.
        let (views, _) = store.list_views().await.unwrap();
        let today_view = views[1].clone();
        assert_eq!(today_view.seed.as_deref(), Some("today"));
        let (filter, sort) = narrow(today_view.clone(), &Filter::default(), &[]);
        let effective = ListInput {
            filter,
            sort,
            ..ListInput::default()
        };
        let in_view = ListInput {
            view_id: today_view.id,
            today: Some(sun),
            ..ListInput::default()
        };
        let unset = || TokenKind::Cleared(TokenEmpty {});
        let keys = || vec![TokenKind::Bool(false), unset(), unset(), epoch()];
        let token = forge(&effective, sun, keys());
        assert!(list(&in_view, token).await.is_ok());
        for today in [Date::new(-5, 1, 1).unwrap(), Date::new(0, 12, 31).unwrap()] {
            let token = forge(&effective, today, keys());
            let listed = list(&in_view, token).await;
            assert!(
                matches!(listed, Err(StoreError::InvalidPageToken)),
                "{listed:?}"
            );
        }
    }
}
