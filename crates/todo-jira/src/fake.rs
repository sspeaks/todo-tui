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
            } else if let Some(h) = st.routes.get(&path).cloned() {
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
