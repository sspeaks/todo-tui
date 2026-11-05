//! A fake Jira Data Center over plain HTTP on 127.0.0.1, for tests (design
//! 5.13): it answers the requests the adapter makes from synthetic data, and
//! records them. No work data: issues are `PLAT-n` on `jira.example.com`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// One request the fake received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    /// With its query string.
    pub target: String,
    pub authorization: Option<String>,
    pub body: String,
}

/// How the fake answers a path: a status and a JSON body, or a handler.
type Handler = Arc<dyn Fn(&Seen) -> (u16, Value, Vec<(String, String)>) + Send + Sync>;

#[derive(Default)]
struct State {
    token: String,
    seen: Vec<Seen>,
    /// By path without its query.
    routes: HashMap<String, Handler>,
}

/// A running fake; dropped, it stops accepting.
pub struct FakeJira {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeJira {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeJira {
    /// Serves on a free port; requests need `Authorization: Bearer <token>`.
    pub async fn start(token: &str) -> FakeJira {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let state = Arc::new(Mutex::new(State {
            token: token.to_owned(),
            ..State::default()
        }));
        let s = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let s = s.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, s).await;
                });
            }
        });
        let fake = FakeJira { addr, state, task };
        fake.route("/rest/api/2/myself", |_| {
            (
                200,
                json!({
                    "key": "JIRAUSER10100",
                    "name": "dana",
                    "displayName": "Dana Example",
                    "emailAddress": "dana@example.com",
                    "timeZone": "America/Los_Angeles",
                    "active": true
                }),
                vec![],
            )
        });
        fake
    }

    /// `http://127.0.0.1:<port>`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Answers `path` (without a query) with `handler`.
    pub fn route(
        &self,
        path: &str,
        handler: impl Fn(&Seen) -> (u16, Value, Vec<(String, String)>) + Send + Sync + 'static,
    ) {
        self.state
            .lock()
            .unwrap()
            .routes
            .insert(path.to_owned(), Arc::new(handler));
    }

    /// Every request so far, in order.
    pub fn seen(&self) -> Vec<Seen> {
        self.state.lock().unwrap().seen.clone()
    }
}

async fn serve(stream: tokio::net::TcpStream, state: Arc<Mutex<State>>) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let (method, target) = (
            parts.next().unwrap_or_default().to_owned(),
            parts.next().unwrap_or_default().to_owned(),
        );
        let mut length = 0usize;
        let mut authorization = None;
        loop {
            let mut h = String::new();
            reader.read_line(&mut h).await?;
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "content-length" => length = v.trim().parse().unwrap_or(0),
                    "authorization" => authorization = Some(v.trim().to_owned()),
                    _ => {}
                }
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await?;
        let seen = Seen {
            method,
            target,
            authorization,
            body: String::from_utf8_lossy(&body).into_owned(),
        };
        let (status, value, headers) = {
            let mut st = state.lock().unwrap();
            st.seen.push(seen.clone());
            let path = seen.target.split('?').next().unwrap_or_default().to_owned();
            let expected = format!("Bearer {}", st.token);
            if seen.authorization.as_deref() != Some(expected.as_str()) {
                (
                    401,
                    json!({"errorMessages": ["You are not authenticated."]}),
                    vec![],
                )
            } else if let Some(h) = st.routes.get(&path).cloned().or_else(|| {
                // `/a/*` answers every path under `/a/` that has no route.
                st.routes
                    .iter()
                    .filter_map(|(k, h)| {
                        k.strip_suffix('*')
                            .filter(|p| path.starts_with(p))
                            .map(|_| h.clone())
                    })
                    .next()
            }) {
                drop(st);
                h(&seen)
            } else {
                (404, json!({"errorMessages": ["Not found"]}), vec![])
            }
        };
        let text = value.to_string();
        let mut head = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
            text.len()
        );
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        write.write_all(head.as_bytes()).await?;
        write.write_all(text.as_bytes()).await?;
    }
}

/// A synthetic issue as Data Center's search returns one.
pub fn issue_json(id: u32, title: &str, category: &str, updated: &str) -> Value {
    json!({
        "id": id.to_string(),
        "key": format!("PLAT-{id}"),
        "fields": {
            "summary": title,
            "description": null,
            "issuetype": {"name": "Task"},
            "status": {"name": "Open", "statusCategory": {"key": category}},
            "priority": {"name": "Major"},
            "assignee": {"key": "JIRAUSER10100", "name": "dana", "displayName": "Dana Example"},
            "labels": [],
            "components": [],
            "fixVersions": [],
            "created": "2026-10-01T09:00:00.000+0000",
            "updated": updated
        }
    })
}

impl FakeJira {
    /// Serves discovery: five priorities, a Story Points and a Sprint field.
    pub fn serve_discovery(&self) {
        self.route("/rest/api/2/priority", |_| {
            let p: Vec<Value> = ["Blocker", "Critical", "Major", "Minor", "Trivial"]
                .iter()
                .map(|n| json!({"name": n}))
                .collect();
            (200, Value::Array(p), vec![])
        });
        self.route("/rest/api/2/field", |_| {
            (
                200,
                json!([
                    {"id": "summary", "name": "Summary", "custom": false},
                    {"id": "customfield_10002", "name": "Story Points", "custom": true,
                     "schema": {"type": "number", "custom": "com.atlassian.jira.plugin.system.customfieldtypes:float"}},
                    {"id": "customfield_10004", "name": "Sprint", "custom": true,
                     "schema": {"type": "array", "custom": "com.pyxis.greenhopper.jira:gh-sprint"}}
                ]),
                vec![],
            )
        });
    }

    /// Serves `issues` to searches, `page` at a time. It reads the JQL as
    /// the adapter writes it: `updated >= "yyyy/MM/dd HH:mm"` (in UTC, the
    /// zone tests give the user), `id > n`, `id in (…)`, `resolution =
    /// Unresolved` (an issue with a `resolution` field is resolved) and
    /// `ORDER BY id`, else in the list's order. `issues` can change between
    /// calls (a shared list).
    pub fn serve_search(&self, issues: Arc<Mutex<Vec<Value>>>, page: usize) {
        self.route("/rest/api/2/search", move |seen| {
            let body: Value = serde_json::from_str(&seen.body).unwrap_or_default();
            // As Data Center answers a body it cannot read: validateQuery is
            // a boolean in POST /search.
            if !body["validateQuery"].is_null() && !body["validateQuery"].is_boolean() {
                return (
                    400,
                    json!({"errorMessages": ["Cannot deserialize value of type `java.lang.Boolean`"]}),
                    vec![],
                );
            }
            let jql = body["jql"].as_str().unwrap_or_default().to_owned();
            let start = body["startAt"].as_u64().unwrap_or(0) as usize;
            let since = jql
                .split("updated >= \"")
                .nth(1)
                .and_then(|t| t.split('"').next())
                .map(|t| {
                    let wall =
                        jiff::civil::DateTime::strptime("%Y/%m/%d %H:%M", t).expect("a JQL date");
                    wall.to_zoned(jiff::tz::TimeZone::UTC)
                        .expect("UTC")
                        .timestamp()
                });
            let after = jql
                .split("id > ")
                .nth(1)
                .and_then(|t| t.split(' ').next())
                .and_then(|t| t.parse::<u64>().ok());
            let wanted: Option<Vec<String>> = jql
                .split("id in (")
                .nth(1)
                .and_then(|t| t.split(')').next())
                .map(|t| t.split(',').map(|s| s.trim().to_owned()).collect());
            let unresolved = jql.contains("resolution = Unresolved");
            let updated = |v: &Value| {
                v["fields"]["updated"]
                    .as_str()
                    .and_then(crate::issue::parse_time)
            };
            let id = |v: &Value| {
                v["id"]
                    .as_str()
                    .and_then(|i| i.parse::<u64>().ok())
                    .unwrap_or(0)
            };
            let mut all: Vec<Value> = issues
                .lock()
                .unwrap()
                .iter()
                .filter(|v| since.is_none_or(|s| updated(v).is_some_and(|u| u >= s)))
                .filter(|v| after.is_none_or(|a| id(v) > a))
                .filter(|v| {
                    wanted
                        .as_ref()
                        .is_none_or(|w| w.iter().any(|x| v["id"] == x.as_str()))
                })
                .filter(|v| !unresolved || v["fields"]["resolution"].is_null())
                .cloned()
                .collect();
            if jql.contains("ORDER BY id") {
                all.sort_by_key(id);
            } else {
                all.sort_by_key(|v| (updated(v), id(v)));
            }
            let slice: Vec<Value> = all.iter().skip(start).take(page).cloned().collect();
            (
                200,
                json!({"startAt": start, "maxResults": page, "total": all.len(), "issues": slice}),
                vec![],
            )
        });
    }
}
