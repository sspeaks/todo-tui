//! The reads of the L48 / N48 report (design 6.6); the window and the text
//! are todo_core::report's.

use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::Timestamp;
use jiff::civil::Date;
use rusqlite::{Connection, params};
use todo_core::filter::SortKey;
use todo_core::report::{line_text, next_filter, render};
use todo_core::{Item, ItemField, ItemId, Status};

use crate::codec::{SqlEnum, load_item, ts_to_sql};
use crate::query::list_items_tx;
use crate::{ListInput, OpKind, Result, attach};

/// Input for [`crate::Store::activity_report`].
#[derive(Debug, Clone, PartialEq)]
pub struct ReportInput {
    /// The start of L48, the first instant counted.
    pub since: Timestamp,
    /// Today, for N48's relative dates.
    pub today: Date,
    /// The last day of N48, as days after today.
    pub days_ahead: i32,
}

/// One line of the report and the Item it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportLine {
    pub item_id: ItemId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub last: Vec<ReportLine>,
    pub next: Vec<ReportLine>,
    /// Both halves as they are pasted.
    pub text: String,
    pub revision: u64,
}

/// The most lines N48 lists: the planned work, not the backlog.
pub const MAX_NEXT: usize = 15;

/// Fields whose change alone is triage, not work (design 6.6): an Item whose
/// only changes in the window are to these is left out of L48.
const TRIAGE: [ItemField; 5] = [
    ItemField::Labels,
    ItemField::Priority,
    ItemField::LocalPriorityOverride,
    ItemField::FocusWeek,
    ItemField::Energy,
];

/// How deep a subtask chain is followed up to its top-level Item.
const MAX_DEPTH: usize = 64;

pub(crate) fn report_tx(conn: &Connection, input: ReportInput) -> Result<Report> {
    // N48 reads through list_items_tx, which holds a read transaction of
    // its own; the revision is read first, so a change between the halves
    // is after it and reaches a subscriber from it.
    let revision = crate::current_revision_in(conn)?;
    let last = last_lines(conn, input.since, input.today)?;
    let next = next_lines(conn, input.today, input.days_ahead)?;
    let texts = |lines: &[ReportLine]| lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>();
    let text = render(&texts(&last), &texts(&next));
    Ok(Report {
        last,
        next,
        text,
        revision,
    })
}

/// What a top-level Item gathered from its own events and its subtasks'.
#[derive(Default)]
struct Moved {
    /// The latest counted event, nanoseconds.
    latest: i64,
    /// Its direct subtasks completed in the window and done now.
    done: HashSet<ItemId>,
}

/// The Items with a user event in the window that is work: every `set`
/// event of a user (or an applied suggestion), not undone, on a field other
/// than the triage ones. A capture is not work by itself, and neither are
/// sync, system and scheduler events. One line per top-level Item, its
/// subtasks folded under it; instances of one series fold into one line.
fn last_lines(conn: &Connection, since: Timestamp, today: Date) -> Result<Vec<ReportLine>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.item_id, MAX(e.at),
                MAX(e.field = ?2 AND e.new = '{\"status_category\":\"done\"}')
         FROM events e JOIN ops o ON o.id = e.op_id
         WHERE e.at >= ?1 AND e.kind = 'set'
               AND (e.actor = 'user' OR e.actor LIKE 'suggestion:%')
               AND e.field NOT IN (?3, ?4, ?5, ?6, ?7)
               AND o.kind NOT IN (?8, ?9)
               AND COALESCE(o.undo_state, '') NOT IN ('undone', 'discarded')
         GROUP BY e.item_id",
    )?;
    let rows = stmt.query_map(
        params![
            ts_to_sql(since)?,
            ItemField::StatusCategory.number(),
            TRIAGE[0].number(),
            TRIAGE[1].number(),
            TRIAGE[2].number(),
            TRIAGE[3].number(),
            TRIAGE[4].number(),
            OpKind::Undo.as_sql(),
            OpKind::Redo.as_sql(),
        ],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, bool>(2)?,
            ))
        },
    )?;
    // Grouped by top-level Item, then by series, so a daily chore done on
    // both days is one line.
    let mut roots: BTreeMap<String, (ItemId, Moved)> = BTreeMap::new();
    let mut cache: HashMap<ItemId, Option<Item>> = HashMap::new();
    let mut load = |conn: &Connection, id: ItemId| -> Result<Option<Item>> {
        if let Some(item) = cache.get(&id) {
            return Ok(item.clone());
        }
        let item = load_item(conn, &id.to_string(), Some(today))?;
        cache.insert(id, item.clone());
        Ok(item)
    };
    for row in rows {
        let (id, at, completed) = row?;
        let Ok(id) = id.parse::<ItemId>() else {
            continue;
        };
        let Some(item) = load(conn, id)? else {
            continue;
        };
        // A deleted Item, or one whose top-level Item is deleted, is gone.
        let mut root = item.clone();
        let mut depth = 0;
        while let Some(parent) = root.core.parent_id
            && depth < MAX_DEPTH
        {
            let Some(p) = load(conn, parent)? else { break };
            root = p;
            depth += 1;
        }
        if item.derived.deleted_at.is_some() || root.derived.deleted_at.is_some() {
            continue;
        }
        let key = root.overlay.recurrence.as_ref().map_or_else(
            || root.id.to_string(),
            |r| format!("series:{}", r.series_id),
        );
        let entry = roots
            .entry(key)
            .or_insert_with(|| (root.id, Moved::default()));
        // The latest instance of a series names the line.
        if at >= entry.1.latest {
            entry.1.latest = at;
            entry.0 = root.id;
        }
        // A direct subtask of the line's Item that is done now: the note
        // counts what subtasks_done counts (not a cancelled or reopened
        // one, nor one further down).
        if completed && item.core.parent_id == Some(root.id) && item.status() == Status::Done {
            entry.1.done.insert(item.id);
        }
    }
    let mut moved: Vec<(ItemId, Moved)> = roots.into_values().collect();
    moved.sort_by(|a, b| b.1.latest.cmp(&a.1.latest).then(a.0.cmp(&b.0)));
    let mut lines = Vec::with_capacity(moved.len());
    for (id, m) in moved {
        let mut items = vec![load(conn, id)?.expect("loaded above")];
        attach(conn, &mut items)?;
        let item = &items[0];
        // "2 of 5 subtasks done": done in the window, of its subtasks now.
        let note = (!m.done.is_empty()).then(|| {
            let total = item.derived.subtasks_total.max(m.done.len() as u32);
            format!("{} of {total} subtasks done", m.done.len())
        });
        lines.push(ReportLine {
            item_id: item.id,
            text: line_text(item, note.as_deref()),
        });
    }
    Ok(lines)
}

/// The open top-level Items N48 plans, ordered as the Today view orders.
fn next_lines(conn: &Connection, today: Date, days_ahead: i32) -> Result<Vec<ReportLine>> {
    let mut filter = next_filter(days_ahead);
    filter.all_of.push(todo_core::filter::Clause::Predicate(
        todo_core::filter::Predicate::new(
            ItemField::ParentId,
            todo_core::filter::Operator::IsUnset,
            vec![],
        ),
    ));
    let page = list_items_tx(
        conn,
        ListInput {
            filter,
            sort: vec![
                SortKey::desc(ItemField::IsOverdue),
                SortKey::desc(ItemField::EffectivePriority),
                SortKey::asc(ItemField::DueDate),
            ],
            page_size: MAX_NEXT as u32,
            today: Some(today),
            ..ListInput::default()
        },
    )?;
    Ok(page
        .items
        .iter()
        .filter(|i| !matches!(i.status(), Status::Done | Status::Cancelled))
        .map(|i| ReportLine {
            item_id: i.id,
            text: line_text(i, None),
        })
        .collect())
}
