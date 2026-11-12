//! The Data Center adapter (design 5.2, ADR-0006, ADR-0007): discovery,
//! scope pulls by cursor, the scope's ids for reconciliation, fetching one
//! issue; and the pushes: a few fields, and transitions.

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};
use todo_core::remote::RemoteIssue;

use crate::client::{Client, JiraError};
use jiff::civil::Date;
use todo_core::{Priority, ResolutionKind};
use todo_sync::{Category, Push};

use crate::issue::{Discovery, priority_maps, search_fields, to_remote};

/// Issues per search page: below the server's cap (1000 by default), and
/// small enough that one page is one quick transaction.
pub const PAGE_SIZE: u32 = 100;

/// How far back of the cursor a pull asks (design 5.3): JQL is minute
/// precise, so an issue changed in the cursor's own minute is seen again
/// and deduplicated rather than missed.
pub const OVERLAP: SignedDuration = SignedDuration::from_mins(2);

/// Ids per `id in (…)` query (design 5.4 step 2b).
pub const ID_BATCH: usize = 100;

/// What `fetch` found (design 5.2).
#[derive(Debug, Clone, PartialEq)]
pub enum Fetched {
    Found(Box<RemoteIssue>),
    NotFound,
    Forbidden,
}

pub struct Adapter {
    client: Client,
    instance: String,
    discovery: Discovery,
    push: Push,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchResult {
    #[serde(default)]
    start_at: u32,
    #[serde(default)]
    max_results: u32,
    #[serde(default)]
    total: u32,
    #[serde(default)]
    issues: Vec<Value>,
}

impl Adapter {
    /// Discovers the instance (design 5.8): who the token is, its time
    /// zone, the priorities in order, the story points and sprint fields.
    pub async fn connect(
        client: Client,
        instance: &str,
        cancelled_resolutions: Vec<String>,
        push: Push,
    ) -> Result<Self, JiraError> {
        let me = client.myself().await?;
        let priorities: Vec<Value> = client.get("/rest/api/2/priority", &[]).await?;
        let names: Vec<String> = priorities
            .iter()
            .filter_map(|p| p.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect();
        let fields: Vec<Value> = client.get("/rest/api/2/field", &[]).await?;
        let id_of = |f: &Value| f.get("id").and_then(Value::as_str).map(str::to_owned);
        let schema = |f: &Value| {
            f.pointer("/schema/custom")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let named = |f: &Value, n: &str| {
            f.get("name")
                .and_then(Value::as_str)
                .is_some_and(|x| x.eq_ignore_ascii_case(n))
        };
        // Story points: a number field named so (Jira Software's own first,
        // `Story Points`, then `Story point estimate`).
        let number =
            |f: &&Value| f.pointer("/schema/type").and_then(Value::as_str) == Some("number");
        let story_points = ["Story Points", "Story point estimate"]
            .iter()
            .find_map(|n| {
                fields
                    .iter()
                    .filter(number)
                    .find(|f| named(f, n))
                    .and_then(id_of)
            });
        let sprint = fields
            .iter()
            .find(|f| schema(f).ends_with(":gh-sprint"))
            .and_then(id_of);
        let (priorities, priority_names) = priority_maps(&names, &push.priority_map);
        let discovery = Discovery {
            me: me.key,
            time_zone: me.time_zone,
            priorities,
            priority_names,
            story_points,
            sprint,
            cancelled_resolutions,
        };
        Ok(Adapter {
            client,
            instance: instance.to_owned(),
            discovery,
            push,
        })
    }

    pub fn discovery(&self) -> &Discovery {
        &self.discovery
    }

    /// The level a push of `level` reads back as: the level of the Jira
    /// priority it is sent as, which for a level Jira has no name of its
    /// own for is a neighbour's (design 5.8). `None`: it cannot be sent.
    pub fn priority_read_back(&self, level: Priority) -> Option<Priority> {
        let name = self.discovery.priority_names.get(&level)?;
        self.discovery.priorities.get(name).copied()
    }

    /// The scope query for changes since `cursor` (minus the overlap), in
    /// the Jira user's time zone (design 5.3), oldest change first.
    ///
    /// The bound is a wall time without an offset, so it is made safely
    /// early: in a zone this machine cannot read, the user's offset is
    /// unknown and the bound is taken 14 hours (the widest offset) back, in
    /// UTC; in a repeated hour (a fall-back), an hour back. A pull reads an
    /// issue again rather than miss it, and the store makes a re-read cost
    /// nothing (an unchanged issue writes nothing).
    pub fn changes_jql(&self, scope: &str, cursor: Option<Timestamp>) -> String {
        let mut jql = format!("({})", scope_clause(scope));
        if let Some(at) = cursor {
            let since = at - OVERLAP;
            let text = match TimeZone::get(&self.discovery.time_zone) {
                Ok(tz) => {
                    let wall = since.to_zoned(tz.clone()).datetime();
                    let ambiguous = tz.to_ambiguous_zoned(wall).is_ambiguous();
                    let at = if ambiguous {
                        since - SignedDuration::from_hours(1)
                    } else {
                        since
                    };
                    at.to_zoned(tz).strftime("%Y/%m/%d %H:%M").to_string()
                }
                Err(_) => (since - SignedDuration::from_hours(14))
                    .to_zoned(TimeZone::UTC)
                    .strftime("%Y/%m/%d %H:%M")
                    .to_string(),
            };
            jql.push_str(&format!(" AND updated >= \"{text}\""));
        }
        jql.push_str(" ORDER BY updated ASC, key ASC");
        jql
    }

    /// One search page. `strict` validates the JQL (a scope's typo is an
    /// error); an id list is not strict, so a deleted or forbidden id is a
    /// warning rather than failing the whole query. Data Center's POST body
    /// takes `validateQuery` as a boolean (true is strict, false warns); the
    /// "strict" / "warn" strings are only the GET form's, and a 400 here.
    async fn search(
        &self,
        jql: &str,
        start_at: u32,
        fields: Vec<String>,
        strict: bool,
    ) -> Result<SearchResult, JiraError> {
        self.client
            .post(
                "/rest/api/2/search",
                &json!({
                    "jql": jql,
                    "startAt": start_at,
                    "maxResults": PAGE_SIZE,
                    "fields": fields,
                    "validateQuery": strict,
                }),
            )
            .await
    }

    fn issues_of(&self, res: &SearchResult) -> Vec<RemoteIssue> {
        res.issues
            .iter()
            .filter_map(|v| to_remote(v, &self.discovery, &self.instance, self.client.base_url()))
            .collect()
    }

    /// Every issue of `scope` changed since `cursor`, a page at a time to
    /// `page` (which commits it and its cursor, design 5.4 step 6), oldest
    /// change first.
    ///
    /// Pages are keyed by the cursor, not an offset: an offset into a list
    /// sorted by `updated` skips an issue when an earlier one changes or
    /// leaves mid-pull. Each page asks again from the newest cursor and
    /// leaves out what this pull already handed over; only a page of issues
    /// all changed in one minute steps on by offset, since the cursor cannot
    /// move past it.
    pub async fn pull_scope<F, Fut, E>(
        &self,
        scope: &str,
        cursor: Option<Timestamp>,
        mut page: F,
    ) -> Result<usize, PullError<E>>
    where
        F: FnMut(Vec<RemoteIssue>, Option<Timestamp>) -> Fut,
        Fut: std::future::Future<Output = Result<(), E>>,
    {
        let mut cursor = cursor;
        let mut seen: std::collections::HashSet<(String, Timestamp)> = Default::default();
        let mut start = 0u32;
        let mut total = 0usize;
        // A bound on the work: no scope changes this much in one pull.
        for _ in 0..10_000 {
            let jql = self.changes_jql(scope, cursor);
            let res = self
                .search(&jql, start, search_fields(&self.discovery), true)
                .await
                .map_err(PullError::Jira)?;
            let all = self.issues_of(&res);
            let got = res.issues.len();
            let fresh: Vec<RemoteIssue> = all
                .iter()
                .filter(|i| !seen.contains(&(i.remote_id.clone(), i.updated_at)))
                .cloned()
                .collect();
            let newest = all.iter().map(|i| i.updated_at).max();
            for i in &fresh {
                seen.insert((i.remote_id.clone(), i.updated_at));
            }
            total += fresh.len();
            let advanced = newest.is_some_and(|n| cursor.is_none_or(|c| n > c));
            if !fresh.is_empty() || advanced {
                page(fresh, newest.max(cursor))
                    .await
                    .map_err(PullError::Page)?;
            }
            let full = u32::try_from(got).unwrap_or(u32::MAX) >= res.max_results.max(1)
                && (res.start_at as usize + got) < res.total as usize;
            if !full {
                return Ok(total);
            }
            // The same minute filled the page: the cursor cannot move past
            // it, so step through it by offset.
            let one_minute = match (all.first(), all.last()) {
                (Some(a), Some(b)) => {
                    a.updated_at.duration_until(b.updated_at) < SignedDuration::from_mins(1)
                }
                _ => false,
            };
            if advanced && !one_minute {
                cursor = newest;
                start = 0;
            } else {
                start = res
                    .start_at
                    .saturating_add(u32::try_from(got).unwrap_or(u32::MAX));
            }
        }
        Ok(total)
    }

    /// The tracked Items outside every scope that changed since `cursor`
    /// (design 5.4 step 2b), by id, up to [`ID_BATCH`] at a time. An id
    /// that is gone or forbidden is left out; reconciliation finds it.
    pub async fn pull_ids(
        &self,
        ids: &[String],
        cursor: Option<Timestamp>,
    ) -> Result<Vec<RemoteIssue>, JiraError> {
        let mut out = Vec::new();
        for chunk in ids.chunks(ID_BATCH) {
            let list: Vec<&String> = chunk.iter().filter(|i| is_issue_id(i)).collect();
            if list.is_empty() {
                continue;
            }
            let jql = self.changes_jql(
                &format!(
                    "id in ({})",
                    list.iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                cursor,
            );
            let mut start = 0;
            loop {
                let res = self
                    .search(&jql, start, search_fields(&self.discovery), false)
                    .await?;
                let got = res.issues.len();
                out.extend(self.issues_of(&res));
                let next = res
                    .start_at
                    .saturating_add(u32::try_from(got).unwrap_or(u32::MAX));
                if got == 0 || next >= res.total {
                    break;
                }
                start = next;
            }
        }
        Ok(out)
    }

    /// Which of `ids` still exist and can be read (design 5.5 step 1): by
    /// id, ids only, a gone or forbidden one simply not returned.
    pub async fn existing_ids(&self, ids: &[String]) -> Result<Vec<String>, JiraError> {
        let mut out = Vec::new();
        for chunk in ids.chunks(ID_BATCH) {
            let list: Vec<&str> = chunk
                .iter()
                .map(String::as_str)
                .filter(|i| is_issue_id(i))
                .collect();
            if list.is_empty() {
                continue;
            }
            let jql = format!("id in ({}) ORDER BY id ASC", list.join(","));
            let res = self.search(&jql, 0, vec!["id".to_owned()], false).await?;
            out.extend(
                res.issues
                    .iter()
                    .filter_map(|v| v.get("id").and_then(Value::as_str).map(str::to_owned)),
            );
        }
        Ok(out)
    }

    /// Every id `scope` lists now (design 5.5), ids only: keyed by id, so
    /// an issue leaving the scope mid-listing does not hide another.
    pub async fn scope_ids(&self, scope: &str) -> Result<Vec<String>, JiraError> {
        let mut ids: Vec<String> = Vec::new();
        let mut after: Option<u64> = None;
        loop {
            let jql = match after {
                None => format!("({}) ORDER BY id ASC", scope_clause(scope)),
                Some(id) => format!("({}) AND id > {id} ORDER BY id ASC", scope_clause(scope)),
            };
            let res = self.search(&jql, 0, vec!["id".to_owned()], true).await?;
            let page: Vec<String> = res
                .issues
                .iter()
                .filter_map(|v| v.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect();
            let last = page.iter().filter_map(|i| i.parse::<u64>().ok()).max();
            let more = res.issues.len() < res.total as usize;
            ids.extend(page);
            match last {
                Some(l) if more && after.is_none_or(|a| l > a) => after = Some(l),
                _ => return Ok(ids),
            }
        }
    }

    /// One issue by id (design 5.5 step 2). A 200 whose issue cannot be
    /// read is an error, not a deletion: the Item must not be marked gone.
    pub async fn fetch(&self, id: &str) -> Result<Fetched, JiraError> {
        let fields = search_fields(&self.discovery).join(",");
        let path = format!("/rest/api/2/issue/{id}");
        match self
            .client
            .get::<Value>(&path, &[("fields", fields.as_str())])
            .await
        {
            Ok(v) => to_remote(&v, &self.discovery, &self.instance, self.client.base_url())
                .map(|i| Fetched::Found(Box::new(i)))
                .ok_or_else(|| {
                    JiraError::Malformed(format!("issue {id}: not an issue Jira would send"))
                }),
            Err(JiraError::NotFound) => Ok(Fetched::NotFound),
            Err(JiraError::Forbidden(_)) => Ok(Fetched::Forbidden),
            Err(e) => Err(e),
        }
    }
}

/// A new issue (design 5.7): what promote sends. `None` leaves a field to
/// Jira's default.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewIssue {
    pub project: String,
    pub issue_type: String,
    pub summary: String,
    /// Markdown: sent as wiki markup (ADR-0009).
    pub description: String,
    pub priority: Option<Priority>,
    pub labels: Vec<String>,
    pub due_date: Option<Date>,
    /// The parent issue's key, for a sub-task.
    pub parent: Option<String>,
}

/// Why a create did not return the issue it made.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CreateError {
    /// Jira refused it: no issue was made.
    #[error(transparent)]
    Jira(#[from] JiraError),
    /// Jira needs these fields set first (design 5.7 step 1): by id, what it
    /// said of each.
    #[error("Jira needs {}", .0.keys().cloned().collect::<Vec<_>>().join(", "))]
    MissingFields(std::collections::BTreeMap<String, String>),
    /// The request may have made the issue, its answer lost or unreadable
    /// (design 5.7 step 4): [`Adapter::find_created`] looks for it.
    #[error("Jira may have made it: {0}")]
    Unknown(JiraError),
    /// Jira made `key` and it could not be read back.
    #[error("Jira made {key} but it could not be read back: {error}")]
    Unread { key: String, error: JiraError },
}

impl Adapter {
    /// Creates the issue (`POST /issue`, design 5.7) and reads it back whole.
    /// A create is not idempotent: once the request may have made the issue,
    /// it never says it failed ([`CreateError::Unknown`], `Unread`).
    pub async fn create(&self, new: &NewIssue) -> Result<RemoteIssue, CreateError> {
        let mut fields = serde_json::Map::new();
        fields.insert("project".into(), json!({ "key": new.project }));
        fields.insert("issuetype".into(), json!({ "name": new.issue_type }));
        fields.insert("summary".into(), json!(new.summary));
        if !new.description.trim().is_empty() {
            fields.insert(
                "description".into(),
                json!(crate::wiki::to_wiki(&new.description)),
            );
        }
        if let Some(name) = new
            .priority
            .and_then(|p| self.discovery.priority_names.get(&p))
        {
            fields.insert("priority".into(), json!({ "name": name }));
        }
        if !new.labels.is_empty() {
            fields.insert("labels".into(), json!(new.labels));
        }
        if let Some(d) = new.due_date {
            fields.insert("duedate".into(), json!(d.to_string()));
        }
        if let Some(parent) = &new.parent {
            fields.insert("parent".into(), json!({ "key": parent }));
        }
        let made: Value = match self
            .client
            .post("/rest/api/2/issue", &json!({ "fields": fields }))
            .await
        {
            Ok(v) => v,
            Err(JiraError::Fields(f)) => return Err(CreateError::MissingFields(f)),
            // A 2xx Jira's body does not explain made the issue all the same.
            Err(e @ JiraError::Malformed(_)) => return Err(CreateError::Unknown(e)),
            Err(e) if e.is_transient() => return Err(CreateError::Unknown(e)),
            Err(e) => return Err(e.into()),
        };
        let Some(id) = ["key", "id"]
            .into_iter()
            .find_map(|k| made.get(k).and_then(Value::as_str))
        else {
            return Err(CreateError::Unknown(JiraError::Malformed(
                "the created issue has no key".into(),
            )));
        };
        let unread = |error| CreateError::Unread {
            key: id.to_owned(),
            error,
        };
        match self.fetch(id).await {
            Ok(Fetched::Found(issue)) => Ok(*issue),
            Ok(_) => Err(unread(JiraError::Malformed("Jira does not show it".into()))),
            Err(e) => Err(unread(e)),
        }
    }

    /// An issue the person created as `new`, since `since` (within the last
    /// ten minutes), with exactly its summary, in its project, of its type and
    /// under its parent, newest first (design 5.7 step 4, open question 2):
    /// what a create whose answer was lost may have made.
    pub async fn find_created(
        &self,
        new: &NewIssue,
        since: Timestamp,
    ) -> Result<Option<RemoteIssue>, JiraError> {
        let summary = &new.summary;
        // Lower case: Jira's text search reads AND, OR and NOT as operators.
        let words: String = summary
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == ' ' {
                    c
                } else {
                    ' '
                }
            })
            .collect();
        let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
        if words.is_empty() {
            return Ok(None);
        }
        let mut jql = format!(
            "project = {} AND issuetype = {} AND reporter = currentUser() AND created >= -10m AND summary ~ \"{words}\"",
            jql_string(&new.project),
            jql_string(&new.issue_type),
        );
        if let Some(parent) = &new.parent {
            jql.push_str(&format!(" AND parent = {}", jql_string(parent)));
        }
        jql.push_str(" ORDER BY created DESC");
        let res = self
            .search(&jql, 0, search_fields(&self.discovery), false)
            .await?;
        // A minute's grace: Jira's clock is not this one.
        let since = since - SignedDuration::from_mins(1);
        Ok(self
            .issues_of(&res)
            .into_iter()
            .find(|i| &i.title == summary && i.created_at >= since))
    }
}

/// `s` as a JQL string literal.
fn jql_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A field a push sets (ADR-0007): the few that reach Jira.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldPatch {
    Title(String),
    /// Markdown: sent as wiki markup (ADR-0009).
    Description(String),
    Priority(Priority),
    Labels(Vec<String>),
    DueDate(Option<Date>),
}

/// Where a transition goes (design 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Todo,
    InProgress,
    Done(ResolutionKind),
}

impl Target {
    fn category(self) -> Category {
        match self {
            Target::Todo => Category::Todo,
            Target::InProgress => Category::InProgress,
            Target::Done(_) => Category::Done,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Target::Todo => "new",
            Target::InProgress => "indeterminate",
            Target::Done(_) => "done",
        }
    }
}

/// Why a push could not be made (ADR-0007): Jira refused, or no transition
/// fits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushError {
    #[error(transparent)]
    Jira(#[from] JiraError),
    /// No transition goes there now, or its screen needs a field a push
    /// cannot fill; `available` names what Jira offered.
    #[error("no transition to {wanted}: {why} (Jira offers {})", if available.is_empty() { "none".to_owned() } else { available.join(", ") })]
    NoTransition {
        wanted: String,
        why: String,
        available: Vec<String>,
    },
}

impl Adapter {
    /// Sets `patches` on the issue (`PUT /issue/{id}`): Jira answers 204.
    pub async fn update_fields(&self, id: &str, patches: &[FieldPatch]) -> Result<(), PushError> {
        if patches.is_empty() {
            return Ok(());
        }
        let mut fields = serde_json::Map::new();
        for p in patches {
            let (k, v) = match p {
                FieldPatch::Title(t) => ("summary", json!(t)),
                FieldPatch::Description(md) => ("description", json!(crate::wiki::to_wiki(md))),
                FieldPatch::Priority(level) => match self.discovery.priority_names.get(level) {
                    Some(name) => ("priority", json!({ "name": name })),
                    // None, or an instance without priorities: Jira's own
                    // default is not ours to pick.
                    None => {
                        return Err(PushError::Jira(JiraError::Refused(
                            400,
                            format!("no Jira priority for {level:?}"),
                        )));
                    }
                },
                FieldPatch::Labels(l) => ("labels", json!(l)),
                FieldPatch::DueDate(d) => {
                    ("duedate", d.map_or(Value::Null, |d| json!(d.to_string())))
                }
            };
            fields.insert(k.to_owned(), v);
        }
        self.client
            .put_empty(
                &format!("/rest/api/2/issue/{id}"),
                &json!({ "fields": fields }),
            )
            .await?;
        Ok(())
    }

    /// Moves the issue into `target`'s category (design 3.4, ADR-0007): by
    /// the transition config names (by its target status), else the first
    /// Jira offers there. A close sets the resolution its screen asks for.
    pub async fn transition(&self, id: &str, target: Target) -> Result<(), PushError> {
        #[derive(Deserialize)]
        struct List {
            #[serde(default)]
            transitions: Vec<Value>,
        }
        let path = format!("/rest/api/2/issue/{id}/transitions");
        let list: List = self
            .client
            .get(&path, &[("expand", "transitions.fields")])
            .await?;
        let available: Vec<String> = list
            .transitions
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect();
        let wanted = self.push.transitions.get(&target.category());
        let chosen = match wanted {
            Some(status) => list
                .transitions
                .iter()
                .find(|t| to_name(t).is_some_and(|n| n.eq_ignore_ascii_case(status))),
            None => list
                .transitions
                .iter()
                .find(|t| to_key(t) == Some(target.key())),
        };
        let wanted_text = wanted
            .cloned()
            .unwrap_or_else(|| format!("{:?}", target.category()));
        let Some(t) = chosen else {
            return Err(PushError::NoTransition {
                wanted: wanted_text,
                why: "Jira offers none there from the issue's status".into(),
                available,
            });
        };
        let tid = t.get("id").and_then(Value::as_str).unwrap_or_default();
        // The screen's required fields: a resolution is ours to set; any
        // other cannot be filled from here.
        let mut fields = serde_json::Map::new();
        if let Some(screen) = t.get("fields").and_then(Value::as_object) {
            for (name, f) in screen {
                let required = f.get("required").and_then(Value::as_bool).unwrap_or(false);
                if name == "resolution" {
                    let resolution = match target {
                        Target::Done(ResolutionKind::Cancelled) => &self.push.cancel_resolution,
                        Target::Done(ResolutionKind::Done) => &self.push.done_resolution,
                        // Not a close: a resolution would leave the issue
                        // resolved (out of an `Unresolved` scope). An
                        // optional one (a reopen screen) is left empty.
                        Target::Todo | Target::InProgress if !required => continue,
                        Target::Todo | Target::InProgress => {
                            return Err(PushError::NoTransition {
                                wanted: wanted_text,
                                why: "its screen requires a resolution, which would leave the issue resolved".into(),
                                available,
                            });
                        }
                    };
                    let allowed: Vec<&Value> = f
                        .get("allowedValues")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().collect())
                        .unwrap_or_default();
                    let value = if allowed.is_empty() {
                        json!({ "name": resolution })
                    } else {
                        // Jira's own spelling (or id), whatever the config's case.
                        let found = allowed.iter().find(|v| {
                            v.get("name")
                                .and_then(Value::as_str)
                                .is_some_and(|n| n.eq_ignore_ascii_case(resolution))
                        });
                        let Some(v) = found else {
                            return Err(PushError::NoTransition {
                                wanted: wanted_text,
                                why: format!(
                                    "its resolution {resolution:?} is not one Jira allows here"
                                ),
                                available,
                            });
                        };
                        match (v.get("id").and_then(Value::as_str), v.get("name")) {
                            (Some(id), _) => json!({ "id": id }),
                            (None, Some(n)) => json!({ "name": n }),
                            (None, None) => json!({ "name": resolution }),
                        }
                    };
                    fields.insert("resolution".into(), value);
                } else if required
                    && f.get("hasDefaultValue").and_then(Value::as_bool) != Some(true)
                {
                    let label = f.get("name").and_then(Value::as_str).unwrap_or(name);
                    return Err(PushError::NoTransition {
                        wanted: wanted_text,
                        why: format!("its screen requires {label}, which a push does not fill"),
                        available,
                    });
                }
            }
        }
        let mut body = json!({ "transition": { "id": tid } });
        if !fields.is_empty() {
            body["fields"] = Value::Object(fields);
        }
        self.client.post_empty(&path, &body).await?;
        Ok(())
    }
}

fn to_key(t: &Value) -> Option<&str> {
    t.pointer("/to/statusCategory/key").and_then(Value::as_str)
}

fn to_name(t: &Value) -> Option<&str> {
    t.pointer("/to/name").and_then(Value::as_str)
}

/// Why a scope pull stopped: Jira, or the caller's page (the store).
#[derive(Debug)]
pub enum PullError<E> {
    Jira(JiraError),
    Page(E),
}

/// A numeric issue id, the only kind an `id in (…)` list takes.
fn is_issue_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// The scope query without an `ORDER BY` of its own (a saved filter pasted
/// in has one), which the pull's own order replaces.
fn scope_clause(scope: &str) -> &str {
    let upper = scope.to_ascii_uppercase();
    match upper.rfind("ORDER BY") {
        // Only an ORDER BY outside quotes and parentheses ends the clause.
        Some(i)
            if scope[..i].matches('"').count().is_multiple_of(2)
                && scope[..i].matches('(').count() == scope[..i].matches(')').count() =>
        {
            scope[..i].trim_end()
        }
        _ => scope.trim(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use todo_core::{Priority, StatusCategory};
    use todo_sync::Credential;

    use super::*;
    use crate::fake::{FakeJira, issue_json};

    async fn adapter(fake: &FakeJira) -> Adapter {
        fake.serve_discovery();
        let client = Client::new(&fake.base_url(), Credential::new("t")).unwrap();
        Adapter::connect(
            client,
            "work-jira",
            vec!["Won't Do".into()],
            Push::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn discovery_reads_the_user_priorities_and_fields() {
        let fake = FakeJira::start("t").await;
        let a = adapter(&fake).await;
        let d = a.discovery();
        assert_eq!(
            (d.me.as_str(), d.time_zone.as_str()),
            ("JIRAUSER10100", "America/Los_Angeles")
        );
        assert_eq!(d.priorities["Major"], Priority::Medium);
        assert_eq!(
            (d.story_points.as_deref(), d.sprint.as_deref()),
            (Some("customfield_10002"), Some("customfield_10004"))
        );
    }

    #[tokio::test]
    async fn the_changes_query_is_in_the_users_zone_with_an_overlap() {
        let fake = FakeJira::start("t").await;
        let a = adapter(&fake).await;
        // 18:30 UTC is 10:30 in Los Angeles in November (PST); minus 2 min.
        let at: Timestamp = "2026-11-04T18:30:00Z".parse().unwrap();
        assert_eq!(
            a.changes_jql("assignee = currentUser()", Some(at)),
            "(assignee = currentUser()) AND updated >= \"2026/11/04 10:28\" ORDER BY updated ASC, key ASC"
        );
        assert_eq!(
            a.changes_jql("project = PLAT", None),
            "(project = PLAT) ORDER BY updated ASC, key ASC"
        );
    }

    /// The fake reads JQL dates in UTC: give the user that zone.
    async fn utc_adapter(fake: &FakeJira) -> Adapter {
        fake.route("/rest/api/2/myself", |_| {
            (
                200,
                json!({"key": "JIRAUSER10100", "name": "dana", "timeZone": "UTC"}),
                vec![],
            )
        });
        adapter(fake).await
    }

    fn at(n: u32) -> String {
        format!("2026-11-0{}T{:02}:00:00.000+0000", 1 + n / 24, n % 24)
    }

    #[tokio::test]
    async fn pages_follow_what_the_server_returned() {
        let fake = FakeJira::start("t").await;
        let a = utc_adapter(&fake).await;
        let issues = std::sync::Arc::new(std::sync::Mutex::new(
            (1..=5)
                .map(|n| {
                    issue_json(
                        n,
                        &format!("issue {n}"),
                        if n == 2 { "indeterminate" } else { "new" },
                        &at(n),
                    )
                })
                .collect::<Vec<_>>(),
        ));
        // The server caps a page at 2 although 100 were asked for.
        fake.serve_search(issues, 2);
        let mut all = Vec::new();
        let mut cursors = Vec::new();
        let n = a
            .pull_scope("assignee = currentUser()", None, |page, cursor| {
                all.extend(page);
                cursors.push(cursor);
                async { Ok::<(), ()>(()) }
            })
            .await
            .unwrap();
        assert_eq!(n, 5);
        assert_eq!(
            all.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
            ["PLAT-1", "PLAT-2", "PLAT-3", "PLAT-4", "PLAT-5"]
        );
        assert_eq!(all[1].status_category, StatusCategory::InProgress);
        assert!(
            all.iter()
                .all(|i| i.is_mine && i.url.ends_with(&format!("/browse/{}", i.key)))
        );
        assert_eq!(cursors.last().unwrap().unwrap(), parse(&at(5)));
        // The request asked for the custom fields and a page of 100.
        let search = fake
            .seen()
            .into_iter()
            .find(|s| s.target.starts_with("/rest/api/2/search"))
            .unwrap();
        let body: Value = serde_json::from_str(&search.body).unwrap();
        assert_eq!(body["maxResults"], 100);
        assert!(
            body["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f == "customfield_10002")
        );
        assert_eq!(
            a.scope_ids("assignee = currentUser()").await.unwrap(),
            ["1", "2", "3", "4", "5"]
        );
    }

    fn parse(s: &str) -> Timestamp {
        crate::issue::parse_time(s).unwrap()
    }

    #[tokio::test]
    async fn an_issue_changing_mid_pull_skips_no_other() {
        let fake = FakeJira::start("t").await;
        let a = utc_adapter(&fake).await;
        let issues = std::sync::Arc::new(std::sync::Mutex::new(
            (1..=6)
                .map(|n| issue_json(n, "x", "new", &at(n)))
                .collect::<Vec<_>>(),
        ));
        fake.serve_search(issues.clone(), 2);
        let mut keys = Vec::new();
        let mut first = true;
        a.pull_scope("scope", None, |page, _| {
            keys.extend(page.iter().map(|i| i.key.clone()));
            // After the first page, PLAT-1 changes (moves to the end) and
            // PLAT-2 leaves: an offset would now skip PLAT-3.
            if std::mem::take(&mut first) {
                let mut l = issues.lock().unwrap();
                l.retain(|v| v["id"] != "2");
                l[0]["fields"]["updated"] = json!(at(20));
            }
            async { Ok::<(), ()>(()) }
        })
        .await
        .unwrap();
        assert_eq!(
            keys,
            [
                "PLAT-1", "PLAT-2", "PLAT-3", "PLAT-4", "PLAT-5", "PLAT-6", "PLAT-1"
            ]
        );
    }

    #[tokio::test]
    async fn the_bound_is_safely_early_when_the_zone_is_unknown_or_repeated() {
        let fake = FakeJira::start("t").await;
        fake.route("/rest/api/2/myself", |_| {
            (
                200,
                json!({"key": "K", "name": "n", "timeZone": "SystemV/PST8PDT"}),
                vec![],
            )
        });
        let a = adapter(&fake).await;
        let at: Timestamp = "2026-11-04T18:30:00Z".parse().unwrap();
        // 14 hours back of 18:28 UTC.
        assert!(
            a.changes_jql("s", Some(at))
                .contains("updated >= \"2026/11/04 04:28\""),
            "{}",
            a.changes_jql("s", Some(at))
        );
        let fake = FakeJira::start("t").await;
        let la = adapter(&fake).await;
        // 08:40 UTC on Nov 1 is 01:40 PDT, in the repeated hour: an hour back.
        let fold: Timestamp = "2026-11-01T08:40:00Z".parse().unwrap();
        assert!(
            la.changes_jql("s", Some(fold))
                .contains("updated >= \"2026/11/01 00:38\""),
            "{}",
            la.changes_jql("s", Some(fold))
        );
        // A saved filter's own ORDER BY is dropped.
        assert_eq!(
            la.changes_jql("project = PLAT ORDER BY priority DESC", None),
            "(project = PLAT) ORDER BY updated ASC, key ASC"
        );
    }

    #[tokio::test]
    async fn fetch_tells_found_from_gone_and_forbidden() {
        let fake = FakeJira::start("t").await;
        let a = adapter(&fake).await;
        fake.route("/rest/api/2/issue/7", |_| {
            (
                200,
                issue_json(7, "seven", "done", "2026-11-01T10:00:00.000+0000"),
                vec![],
            )
        });
        fake.route("/rest/api/2/issue/8", |_| {
            (403, json!({"errorMessages": ["no"]}), vec![])
        });
        let Fetched::Found(i) = a.fetch("7").await.unwrap() else {
            panic!()
        };
        assert_eq!(i.status_category, StatusCategory::Done);
        assert_eq!(a.fetch("8").await.unwrap(), Fetched::Forbidden);
        assert_eq!(a.fetch("9").await.unwrap(), Fetched::NotFound);
    }

    /// A fake with one issue, its writes served; and the adapter.
    async fn writable(push: Push) -> (FakeJira, Arc<Mutex<Vec<Value>>>, Adapter) {
        let fake = FakeJira::start("t").await;
        fake.serve_discovery();
        let issues = Arc::new(Mutex::new(vec![issue_json(
            1,
            "one",
            "new",
            "2026-11-04T09:00:00.000+0000",
        )]));
        fake.serve_writes(issues.clone(), || "2026-11-05T10:00:00.000+0000".to_owned());
        let client = Client::new(&fake.base_url(), Credential::new("t")).unwrap();
        let a = Adapter::connect(client, "work-jira", vec!["Won't Do".into()], push)
            .await
            .unwrap();
        (fake, issues, a)
    }

    #[tokio::test]
    async fn field_pushes_send_jira_json_and_reach_the_issue() {
        let (fake, issues, a) = writable(Push::default()).await;
        a.update_fields(
            "1",
            &[
                FieldPatch::Title("renamed".into()),
                FieldPatch::Priority(Priority::High),
                FieldPatch::Labels(vec!["a".into(), "b".into()]),
                FieldPatch::DueDate(Some(jiff::civil::date(2026, 11, 6))),
            ],
        )
        .await
        .unwrap();
        let put = fake.seen().into_iter().find(|s| s.method == "PUT").unwrap();
        assert_eq!(put.target, "/rest/api/2/issue/1");
        let body: Value = serde_json::from_str(&put.body).unwrap();
        assert_eq!(
            body,
            json!({"fields": {"summary": "renamed", "priority": {"name": "Critical"}, "labels": ["a", "b"], "duedate": "2026-11-06"}})
        );
        // And a pull reads it back.
        let Fetched::Found(i) = a.fetch("1").await.unwrap() else {
            panic!()
        };
        assert_eq!((i.title.as_str(), i.priority), ("renamed", Priority::High));
        // Clearing the due date sends null; nothing to send sends nothing.
        a.update_fields("1", &[FieldPatch::DueDate(None)])
            .await
            .unwrap();
        assert_eq!(issues.lock().unwrap()[0]["fields"]["duedate"], Value::Null);
        let before = fake.seen().len();
        a.update_fields("1", &[]).await.unwrap();
        assert_eq!(fake.seen().len(), before);
        // No level for "none": refused here, not sent.
        assert!(matches!(
            a.update_fields("1", &[FieldPatch::Priority(Priority::None)])
                .await,
            Err(PushError::Jira(JiraError::Refused(400, _)))
        ));
    }

    #[tokio::test]
    async fn transitions_go_by_category_and_close_with_the_configured_resolution() {
        let (fake, issues, a) = writable(Push::default()).await;
        a.transition("1", Target::InProgress).await.unwrap();
        assert_eq!(
            issues.lock().unwrap()[0]["fields"]["status"]["name"],
            "In Progress"
        );
        a.transition("1", Target::Done(ResolutionKind::Done))
            .await
            .unwrap();
        let last = fake
            .seen()
            .into_iter()
            .rev()
            .find(|s| s.method == "POST")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&last.body).unwrap(),
            json!({"transition": {"id": "71"}, "fields": {"resolution": {"id": "10000"}}})
        );
        let Fetched::Found(i) = a.fetch("1").await.unwrap() else {
            panic!()
        };
        assert_eq!(i.resolution_kind, Some(ResolutionKind::Done));
        a.transition("1", Target::Done(ResolutionKind::Cancelled))
            .await
            .unwrap();
        let Fetched::Found(i) = a.fetch("1").await.unwrap() else {
            panic!()
        };
        assert_eq!(
            (i.resolution.as_deref(), i.resolution_kind),
            (Some("Won't Do"), Some(ResolutionKind::Cancelled))
        );
        a.transition("1", Target::Todo).await.unwrap();
        let Fetched::Found(i) = a.fetch("1").await.unwrap() else {
            panic!()
        };
        assert_eq!(
            (i.status_category, i.resolution),
            (StatusCategory::Todo, None)
        );
    }

    #[tokio::test]
    async fn a_transition_jira_does_not_offer_or_cannot_take_is_named() {
        // By the target status config names: one this workflow lacks.
        let push = Push {
            transitions: [(Category::InProgress, "Doing".to_owned())].into(),
            done_resolution: "Completed".into(),
            ..Push::default()
        };
        let (_fake, _issues, a) = writable(push).await;
        let Err(PushError::NoTransition {
            wanted, available, ..
        }) = a.transition("1", Target::InProgress).await
        else {
            panic!()
        };
        assert_eq!(wanted, "Doing");
        assert_eq!(available, ["Open", "In Progress (2)", "Closed"]);
        // A resolution the screen does not allow.
        let Err(e) = a.transition("1", Target::Done(ResolutionKind::Done)).await else {
            panic!()
        };
        assert!(
            e.to_string()
                .contains("\"Completed\" is not one Jira allows"),
            "{e}"
        );
        // Gone: Jira's 404.
        assert_eq!(
            a.transition("9", Target::Todo).await,
            Err(PushError::Jira(JiraError::NotFound))
        );
    }

    #[tokio::test]
    async fn only_a_close_sets_a_resolution_in_jiras_own_value() {
        // The config's case does not matter: Jira's value (by id) is sent.
        let push = Push {
            cancel_resolution: "won't do".into(),
            ..Push::default()
        };
        let (fake, _issues, a) = writable(push).await;
        a.transition("1", Target::Done(ResolutionKind::Cancelled))
            .await
            .unwrap();
        let last = fake
            .seen()
            .into_iter()
            .rev()
            .find(|s| s.method == "POST")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&last.body).unwrap()["fields"],
            json!({"resolution": {"id": "10101"}})
        );
        // A reopen screen with an optional resolution: left empty.
        let reopen = |required: bool| {
            json!({"transitions": [{"id": "3", "name": "Reopen", "to": {"name": "Open", "statusCategory": {"key": "new"}},
                "fields": {"resolution": {"required": required, "allowedValues": [{"id": "10000", "name": "Done"}]}}}]})
        };
        let optional = reopen(false);
        fake.route("/rest/api/2/issue/2/transitions", move |seen| {
            if seen.method == "GET" {
                (200, optional.clone(), vec![])
            } else {
                (204, Value::Null, vec![])
            }
        });
        a.transition("2", Target::Todo).await.unwrap();
        let last = fake
            .seen()
            .into_iter()
            .rev()
            .find(|s| s.method == "POST")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&last.body).unwrap(),
            json!({"transition": {"id": "3"}})
        );
        // Required there: refused, rather than leave the issue resolved.
        let required = reopen(true);
        fake.route("/rest/api/2/issue/3/transitions", move |_| {
            (200, required.clone(), vec![])
        });
        let Err(PushError::NoTransition { why, .. }) = a.transition("3", Target::Todo).await else {
            panic!()
        };
        assert!(why.contains("leave the issue resolved"), "{why}");
    }

    #[tokio::test]
    async fn a_refused_field_comes_back_named() {
        let fake = FakeJira::start("t").await;
        fake.serve_discovery();
        fake.route("/rest/api/2/issue/1", |_| {
            (
                400,
                json!({"errorMessages": [], "errors": {"duedate": "Due date must be after start"}}),
                vec![],
            )
        });
        let client = Client::new(&fake.base_url(), Credential::new("t")).unwrap();
        let a = Adapter::connect(client, "work-jira", vec![], Push::default())
            .await
            .unwrap();
        assert_eq!(
            a.update_fields("1", &[FieldPatch::DueDate(None)]).await,
            Err(PushError::Jira(JiraError::Fields(
                [(
                    "duedate".to_owned(),
                    "Due date must be after start".to_owned()
                )]
                .into()
            )))
        );
    }

    #[tokio::test]
    async fn a_create_sends_the_fields_reads_the_issue_back_and_names_what_is_missing() {
        let (fake, issues, a) = writable(Push::default()).await;
        fake.serve_search(issues.clone(), 50);
        let new = NewIssue {
            project: "PLAT".into(),
            issue_type: "Task".into(),
            summary: "Ship the report".into(),
            description: "## Steps\n- tag `v2`".into(),
            priority: Some(Priority::High),
            labels: vec!["work".into()],
            due_date: Some(jiff::civil::date(2026, 11, 20)),
            parent: None,
        };
        let made = a.create(&new).await.unwrap();
        assert_eq!(
            (made.key.as_str(), made.title.as_str()),
            ("PLAT-2", "Ship the report")
        );
        let post = fake
            .seen()
            .into_iter()
            .find(|s| s.method == "POST" && s.target == "/rest/api/2/issue")
            .unwrap();
        let body: Value = serde_json::from_str(&post.body).unwrap();
        assert_eq!(
            body["fields"],
            json!({"project": {"key": "PLAT"}, "issuetype": {"name": "Task"}, "summary": "Ship the report",
                   "description": "h2. Steps\n* tag {{v2}}", "priority": {"name": "Critical"},
                   "labels": ["work"], "duedate": "2026-11-20"})
        );
        // A project Jira has no such type for: what is missing, by field.
        let err = a
            .create(&NewIssue {
                project: "OPS".into(),
                ..new.clone()
            })
            .await
            .unwrap_err();
        let CreateError::MissingFields(f) = err else {
            panic!("{err:?}")
        };
        assert_eq!(f.keys().collect::<Vec<_>>(), ["issuetype"]);
        // The lost-answer search finds an issue by its exact summary, in its
        // project and of its type, made since the create was sent.
        let sent = made.created_at;
        let found = a.find_created(&new, sent).await.unwrap();
        assert_eq!(found.map(|i| i.key), Some("PLAT-2".to_owned()));
        let seen = fake.seen();
        let jql = seen
            .iter()
            .rev()
            .find(|s| s.target == "/rest/api/2/search")
            .unwrap();
        assert!(
            jql.body.contains(
                r#"project = \"PLAT\" AND issuetype = \"Task\" AND reporter = currentUser() AND created >= -10m AND summary ~ \"ship the report\""#
            ),
            "{}",
            jql.body
        );
        // One made before the create was sent is not the one it made.
        let later = sent + SignedDuration::from_mins(5);
        assert_eq!(a.find_created(&new, later).await.unwrap(), None);
        // Upper-case operators are words to the search.
        let not = NewIssue {
            summary: "Do NOT merge (yet)".into(),
            parent: Some("PLAT-1".into()),
            ..new.clone()
        };
        let _ = a.find_created(&not, sent).await.unwrap();
        let seen = fake.seen();
        let jql = &seen.last().unwrap().body;
        assert!(
            jql.contains(r#"summary ~ \"do not merge yet\" AND parent = \"PLAT-1\""#),
            "{jql}"
        );
        // A key reads as an id does (attach).
        let Fetched::Found(by_key) = a.fetch("PLAT-1").await.unwrap() else {
            panic!()
        };
        assert_eq!(by_key.remote_id, "1");
    }
}
