//! The Data Center adapter's read half (design 5.2, ADR-0006): discovery,
//! scope pulls by cursor, the scope's ids for reconciliation, fetching one
//! issue.

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};
use todo_core::remote::RemoteIssue;

use crate::client::{Client, JiraError};
use crate::issue::{Discovery, map_priorities, search_fields, to_remote};

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
        let discovery = Discovery {
            me: me.key,
            time_zone: me.time_zone,
            priorities: map_priorities(&names),
            story_points,
            sprint,
            cancelled_resolutions,
        };
        Ok(Adapter {
            client,
            instance: instance.to_owned(),
            discovery,
        })
    }

    pub fn discovery(&self) -> &Discovery {
        &self.discovery
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
    use todo_core::{Priority, StatusCategory};
    use todo_sync::Credential;

    use super::*;
    use crate::fake::{FakeJira, issue_json};

    async fn adapter(fake: &FakeJira) -> Adapter {
        fake.serve_discovery();
        let client = Client::new(&fake.base_url(), Credential::new("t")).unwrap();
        Adapter::connect(client, "work-jira", vec!["Won't Do".into()])
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
}
