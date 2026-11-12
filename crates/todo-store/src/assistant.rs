//! What the assistant leaves in the store (design 6.3, ADR-0010): the jobs it
//! runs and what they cost, and the Suggestions it makes. A refine is never
//! an op: the person applies it, or not, in the Suggestions pane.

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use todo_core::{FieldValue, ItemField, ItemId, new_ulid};

use crate::codec::{self, ts_from_sql, ts_to_sql};
use crate::{Result, WriteTx};

/// A job's place: queued, then running, then done or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
}

impl JobState {
    fn as_sql(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
        }
    }

    fn from_sql(s: &str) -> Self {
        match s {
            "queued" => JobState::Queued,
            "running" => JobState::Running,
            "done" => JobState::Done,
            _ => JobState::Failed,
        }
    }
}

/// One request to Claude, as `todo assistant usage` lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub item_ids: Vec<ItemId>,
    pub state: JobState,
    pub error: Option<String>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost_usd: f64,
    pub duration_ms: u64,
    pub created_at: Timestamp,
}

/// How a job ended, to record.
#[derive(Debug, Clone, PartialEq)]
pub struct JobEnd {
    pub error: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost_usd: f64,
    pub duration_ms: u64,
}

/// A refine for one Item: the rationale and the fields it would change, each
/// as `(field, the value Claude saw, the value proposed)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Refined {
    pub item: ItemId,
    pub rationale: String,
    pub patches: Vec<(ItemField, FieldValue, FieldValue)>,
}

/// A new job for `items`, queued; its id.
pub(crate) fn queue_tx(
    conn: &Connection,
    items: &[ItemId],
    model: &str,
    at: Timestamp,
) -> Result<String> {
    let id = new_ulid().to_string();
    let ids: Vec<String> = items.iter().map(ToString::to_string).collect();
    let now = ts_to_sql(at)?;
    conn.prepare_cached(
        "INSERT INTO assistant_jobs (id, kind, item_ids, state, model, created_at, updated_at)
         VALUES (?1, 'refine', ?2, 'queued', ?3, ?4, ?4)",
    )?
    .execute(params![
        id,
        serde_json::to_string(&ids).map_err(codec::corrupt)?,
        model,
        now
    ])?;
    Ok(id)
}

/// The job `id` is running.
pub(crate) fn start_tx(conn: &Connection, id: &str, at: Timestamp) -> Result<()> {
    conn.prepare_cached(
        "UPDATE assistant_jobs SET state = 'running', updated_at = ?2 WHERE id = ?1",
    )?
    .execute(params![id, ts_to_sql(at)?])?;
    Ok(())
}

/// The job `id` ended, done or failed, with what it cost; and the refines it
/// made, each a pending `refine` Suggestion (design 3.11) streamed as one.
/// Returns the Suggestions' ids.
pub(crate) fn finish_tx(
    tx: &mut WriteTx<'_>,
    id: &str,
    end: JobEnd,
    refined: Vec<Refined>,
    at: Timestamp,
) -> Result<Vec<String>> {
    let now = ts_to_sql(at)?;
    let state = if end.error.is_some() {
        JobState::Failed
    } else {
        JobState::Done
    };
    tx.conn()
        .prepare_cached(
            "UPDATE assistant_jobs SET state = ?2, error = ?3, input_tokens = ?4, output_tokens = ?5,
                    cached_tokens = ?6, cost_usd = ?7, duration_ms = ?8, updated_at = ?9
             WHERE id = ?1",
        )?
        .execute(params![
            id,
            state.as_sql(),
            end.error,
            end.input_tokens as i64,
            end.output_tokens as i64,
            end.cached_tokens as i64,
            end.cost_usd,
            end.duration_ms as i64,
            now
        ])?;
    let mut made = Vec::new();
    for r in refined.into_iter().filter(|r| !r.patches.is_empty()) {
        let sid = new_ulid().to_string();
        tx.conn()
            .prepare_cached(
                "INSERT INTO suggestions (id, item_id, kind, source, rationale, state, created_at, updated_at)
                 VALUES (?1, ?2, 'refine', 'claude', ?3, 'pending', ?4, ?4)",
            )?
            .execute(params![sid, r.item.to_string(), r.rationale, now])?;
        for (ord, (field, old, new)) in r.patches.iter().enumerate() {
            tx.conn()
                .prepare_cached(
                    "INSERT INTO suggestion_patches (id, suggestion_id, ord, field, expected_old_json, new_json, state)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending')",
                )?
                .execute(params![
                    new_ulid().to_string(),
                    sid,
                    ord as i64,
                    field.number(),
                    codec::value_to_sql(old)?,
                    codec::value_to_sql(new)?
                ])?;
        }
        crate::outbox::record_suggestion(tx, &sid)?;
        made.push(sid);
    }
    tx.conn()
        .prepare_cached("UPDATE assistant_jobs SET suggestion_ids = ?2 WHERE id = ?1")?
        .execute(params![
            id,
            serde_json::to_string(&made).map_err(codec::corrupt)?
        ])?;
    Ok(made)
}

/// Whether a refine of `item` waits to run (queued or running): a second
/// request then joins it rather than asking twice.
pub(crate) fn waiting_tx(conn: &Connection, item: ItemId) -> Result<Option<String>> {
    let like = format!("%\"{item}\"%");
    Ok(conn
        .prepare_cached(
            "SELECT id FROM assistant_jobs WHERE state IN ('queued', 'running') AND item_ids LIKE ?1
             ORDER BY created_at LIMIT 1",
        )?
        .query_row([like], |r| r.get(0))
        .optional()?)
}

/// The job `id`.
pub(crate) fn job_tx(conn: &Connection, id: &str) -> Result<Option<Job>> {
    let mut stmt = conn.prepare_cached(&format!("{SELECT_JOB} WHERE id = ?1"))?;
    stmt.query_row([id], read_job).optional()?.transpose()
}

/// The latest `limit` jobs, newest first.
pub(crate) fn jobs_tx(conn: &Connection, limit: usize) -> Result<Vec<Job>> {
    let mut stmt =
        conn.prepare_cached(&format!("{SELECT_JOB} ORDER BY created_at DESC LIMIT ?1"))?;
    let rows = stmt.query_map([limit as i64], read_job)?;
    rows.map(|r| r?).collect()
}

/// Jobs left queued or running by a daemon that stopped: failed, so a new
/// request is not joined to one that will never run.
pub(crate) fn abandon_tx(conn: &Connection, at: Timestamp) -> Result<usize> {
    Ok(conn
        .prepare_cached(
            "UPDATE assistant_jobs SET state = 'failed', error = 'the daemon stopped before it ran',
                    updated_at = ?1
             WHERE state IN ('queued', 'running')",
        )?
        .execute([ts_to_sql(at)?])?)
}

/// What a refine shows Claude besides the Item (design 6.3): the labels in
/// use, and Items that may be the same thing as `(key or short id, title)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RefineContext {
    pub labels: Vec<String>,
    pub candidates: Vec<(String, String)>,
}

/// What a refine of `item` shows Claude besides the Item (design 6.3): the
/// labels live Items use, and up to `limit` live Items whose title or key
/// shares a word with it, best first, as `(key or short id, title)`.
pub(crate) fn context_tx(
    conn: &Connection,
    item: &todo_core::Item,
    limit: usize,
) -> Result<RefineContext> {
    let mut labels: Vec<String> = Vec::new();
    let mut stmt = conn
        .prepare_cached("SELECT labels FROM items WHERE deleted_at IS NULL AND labels != '[]'")?;
    for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
        let set: Vec<String> = serde_json::from_str(&row?).map_err(codec::corrupt)?;
        labels.extend(set);
    }
    labels.sort();
    labels.dedup();
    // Words of three letters or more, any of them: an OR, ranked.
    let words: Vec<String> = item
        .core
        .title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    let mut candidates = Vec::new();
    if !words.is_empty() {
        let query = format!("{{title remote_key}} : ({})", words.join(" OR "));
        let mut stmt = conn.prepare_cached(
            "SELECT i.id, i.remote_key, i.title FROM items_fts f JOIN items i ON i.seq = f.rowid
             WHERE items_fts MATCH ?1 AND i.id != ?2 AND i.deleted_at IS NULL
             ORDER BY bm25(items_fts) LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![query, item.id.to_string(), limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, key, title) = row?;
            let short = id[id.len().saturating_sub(8)..].to_owned();
            candidates.push((key.unwrap_or(short), title));
        }
    }
    Ok(RefineContext { labels, candidates })
}

/// The Suggestions the job `id` made (it finished done).
pub(crate) fn suggestions_tx(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare_cached("SELECT suggestion_ids FROM assistant_jobs WHERE id = ?1")?;
    let ids: Option<String> = stmt.query_row([id], |r| r.get(0)).optional()?.flatten();
    match ids {
        Some(json) => serde_json::from_str(&json).map_err(codec::corrupt),
        None => Ok(Vec::new()),
    }
}

const SELECT_JOB: &str = "SELECT id, item_ids, state, error, model, input_tokens, output_tokens,
        cached_tokens, cost_usd, duration_ms, created_at FROM assistant_jobs";

fn read_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Job>> {
    let ids: String = r.get(1)?;
    let state: String = r.get(2)?;
    let created: i64 = r.get(10)?;
    let n = |i: usize| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
    let (input_tokens, output_tokens, cached_tokens, duration_ms) = (n(5)?, n(6)?, n(7)?, n(9)?);
    let id: String = r.get(0)?;
    let error: Option<String> = r.get(3)?;
    let model: String = r.get(4)?;
    let cost_usd: f64 = r.get(8)?;
    Ok((|| {
        let ids: Vec<String> = serde_json::from_str(&ids).map_err(codec::corrupt)?;
        Ok(Job {
            id,
            item_ids: ids
                .iter()
                .map(|i| codec::item_id_from_str(i))
                .collect::<Result<_>>()?,
            state: JobState::from_sql(&state),
            error,
            model,
            input_tokens,
            output_tokens,
            cached_tokens,
            cost_usd,
            duration_ms,
            created_at: ts_from_sql(created)?,
        })
    })())
}
