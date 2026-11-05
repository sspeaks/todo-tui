//! The HTTP side: one authenticated client per instance, and the errors a
//! Data Center server answers with.

use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use todo_sync::Credential;

/// How long one request may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Why a request failed, in the terms sync status uses (design 5.11, 5.12).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JiraError {
    /// 401: the token is wrong, expired or revoked.
    #[error("the token was refused (401): check it, or make a new one")]
    Auth,
    /// 403: signed in, but not allowed (or a CAPTCHA after failed logins).
    #[error("forbidden (403): {0}")]
    Forbidden(String),
    #[error("not found (404)")]
    NotFound,
    /// 429: rate limited; wait this long.
    #[error("rate limited (429); retry after {0:?}")]
    RateLimited(Duration),
    /// Another 4xx: the request is wrong (a bad JQL query, a value Jira
    /// will not take).
    #[error("refused ({0}): {1}")]
    Refused(u16, String),
    /// A 400 naming fields: what Jira said about each (a push, design 5.6).
    #[error("refused (400): {}", field_text(.0))]
    Fields(std::collections::BTreeMap<String, String>),
    /// 5xx: the server failed; try again later.
    #[error("server error ({0}): {1}")]
    Server(u16, String),
    /// No answer: DNS, TLS, a timeout, the network.
    #[error("cannot reach Jira: {0}")]
    Offline(String),
    /// An answer that is not what the API documents.
    #[error("unexpected answer: {0}")]
    Malformed(String),
}

impl JiraError {
    /// Whether waiting and trying again can help.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            JiraError::RateLimited(_) | JiraError::Server(..) | JiraError::Offline(_)
        )
    }
}

/// `GET /rest/api/2/myself`: who the token is.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Myself {
    /// Stable (`JIRAUSER10100`); `name` can be renamed (ADR-0006).
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub email_address: String,
    /// The user's profile time zone, which JQL dates are read in.
    #[serde(default)]
    pub time_zone: String,
}

/// An authenticated client for one Data Center instance.
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    token: Credential,
}

impl Client {
    /// `base_url` is `https://host[/context]`, without a trailing slash.
    pub fn new(base_url: &str, token: Credential) -> Result<Self, JiraError> {
        // No environment proxy (it would see a plain-http request to a
        // loopback test server, token and all), and no redirects (one could
        // take the request, Authorization header and all, elsewhere).
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("todo-tui/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| JiraError::Offline(e.to_string()))?;
        Ok(Client {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `<base>/browse/<KEY>`.
    pub fn browse_url(&self, key: &str) -> String {
        format!("{}/browse/{key}", self.base_url)
    }

    pub async fn myself(&self) -> Result<Myself, JiraError> {
        self.get("/rest/api/2/myself", &[]).await
    }

    /// A GET of `path` (from `/rest/…`) with `query`, as JSON.
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, JiraError> {
        let req = self
            .http
            .get(format!("{}{path}", self.base_url))
            .query(query);
        self.send(req).await
    }

    /// A POST of `body` to `path`, as JSON both ways.
    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, JiraError> {
        let req = self
            .http
            .post(format!("{}{path}", self.base_url))
            .json(body);
        self.send(req).await
    }

    /// A PUT of `body` to `path`; Jira answers 204 with no body.
    pub async fn put_empty(&self, path: &str, body: &serde_json::Value) -> Result<(), JiraError> {
        let req = self.http.put(format!("{}{path}", self.base_url)).json(body);
        self.send_raw(req).await.map(drop)
    }

    /// A POST of `body` to `path` that answers 204 with no body (a
    /// transition).
    pub async fn post_empty(&self, path: &str, body: &serde_json::Value) -> Result<(), JiraError> {
        let req = self
            .http
            .post(format!("{}{path}", self.base_url))
            .json(body);
        self.send_raw(req).await.map(drop)
    }

    async fn send<T: DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<T, JiraError> {
        let body = self.send_raw(req).await?;
        serde_json::from_slice(&body).map_err(|e| JiraError::Malformed(e.to_string()))
    }

    /// The 2xx body, or the error the status says.
    async fn send_raw(&self, req: reqwest::RequestBuilder) -> Result<Vec<u8>, JiraError> {
        let res = req
            .bearer_auth(self.token.expose())
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| JiraError::Offline(without_url(&e)))?;
        let status = res.status().as_u16();
        if (200..300).contains(&status) {
            let body = res
                .bytes()
                .await
                .map_err(|e| JiraError::Offline(without_url(&e)))?;
            return Ok(body.to_vec());
        }
        let retry_after = res
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .and_then(retry_after_secs);
        let body = res.text().await.unwrap_or_default();
        Err(match status {
            401 => JiraError::Auth,
            403 => JiraError::Forbidden(messages(&body)),
            404 => JiraError::NotFound,
            429 => JiraError::RateLimited(Duration::from_secs(retry_after.unwrap_or(1).max(1))),
            500..=599 => JiraError::Server(status, messages(&body)),
            400 => match field_errors(&body) {
                Some(fields) => JiraError::Fields(fields),
                None => JiraError::Refused(status, messages(&body)),
            },
            _ => JiraError::Refused(status, messages(&body)),
        })
    }
}

/// `{"errors": {"field": "message"}}` with no general message: a refusal
/// of named fields.
fn field_errors(body: &str) -> Option<std::collections::BTreeMap<String, String>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let general = v
        .get("errorMessages")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|a| !a.is_empty());
    let errors: std::collections::BTreeMap<String, String> = v
        .get("errors")?
        .as_object()?
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect();
    (!general && !errors.is_empty()).then_some(errors)
}

fn field_text(fields: &std::collections::BTreeMap<String, String>) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// `Retry-After` as seconds: delta-seconds, or an HTTP-date (RFC 9110) from
/// now, at least 0.
fn retry_after_secs(v: &str) -> Option<u64> {
    let v = v.trim();
    if let Ok(n) = v.parse::<u64>() {
        return Some(n);
    }
    let at = jiff::fmt::rfc2822::parse(v).ok()?.timestamp();
    let wait = jiff::Timestamp::now().duration_until(at).as_secs();
    Some(u64::try_from(wait).unwrap_or(0))
}

/// The error text of an error body (`{"errorMessages": […], "errors": {…}}`);
/// a body that is not JSON (an HTML error page) is cut short.
fn messages(body: &str) -> String {
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct ErrorBody {
        #[serde(default)]
        error_messages: Vec<String>,
        #[serde(default)]
        errors: std::collections::BTreeMap<String, String>,
    }
    if let Ok(e) = serde_json::from_str::<ErrorBody>(body) {
        let all: Vec<String> = e
            .error_messages
            .into_iter()
            .chain(e.errors.into_iter().map(|(k, v)| format!("{k}: {v}")))
            .collect();
        if !all.is_empty() {
            return all.join("; ");
        }
    }
    let text: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(200) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None if text.is_empty() => "no details".to_owned(),
        None => text,
    }
}

/// A reqwest error's text without its URL's query (which may hold JQL).
fn without_url(e: &reqwest::Error) -> String {
    e.without_url_ref().to_string()
}

trait WithoutUrl {
    fn without_url_ref(&self) -> String;
}

impl WithoutUrl for reqwest::Error {
    /// With the causes reqwest's own text leaves out (the TLS or DNS error),
    /// which is what says what to fix.
    fn without_url_ref(&self) -> String {
        let mut s = self.to_string();
        if let Some(url) = self.url() {
            s = s.replace(url.as_str(), url.path());
        }
        let mut last = s.clone();
        let mut cause = std::error::Error::source(self);
        while let Some(e) = cause {
            let text = e.to_string();
            if !last.contains(&text) {
                s.push_str(": ");
                s.push_str(&text);
            }
            last = text;
            cause = e.source();
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_bodies_read() {
        assert_eq!(
            messages(
                r#"{"errorMessages":["The value 'X' does not exist for the field 'project'."],"errors":{}}"#
            ),
            "The value 'X' does not exist for the field 'project'."
        );
        assert_eq!(
            messages(r#"{"errorMessages":[],"errors":{"summary":"required"}}"#),
            "summary: required"
        );
        assert_eq!(
            messages("<html>\n<body>Bad   Gateway</body></html>"),
            "<html> <body>Bad Gateway</body></html>"
        );
        assert_eq!(messages(""), "no details");
        assert!(messages(&"x ".repeat(300)).ends_with('…'));
    }

    #[test]
    fn transient_errors_are_the_retryable_ones() {
        assert!(JiraError::Offline("x".into()).is_transient());
        assert!(JiraError::Server(502, String::new()).is_transient());
        assert!(JiraError::RateLimited(Duration::from_secs(1)).is_transient());
        assert!(!JiraError::Auth.is_transient());
        assert!(!JiraError::Refused(400, String::new()).is_transient());
        assert!(!JiraError::Fields(Default::default()).is_transient());
    }

    #[test]
    fn a_refusal_naming_fields_keeps_them() {
        let f = field_errors(r#"{"errorMessages":[],"errors":{"duedate":"Due date is invalid"}}"#);
        assert_eq!(
            f,
            Some([("duedate".to_owned(), "Due date is invalid".to_owned())].into())
        );
        assert_eq!(
            JiraError::Fields(f.unwrap()).to_string(),
            "refused (400): duedate: Due date is invalid"
        );
        // A general message (or none named) is a plain refusal.
        assert_eq!(
            field_errors(r#"{"errorMessages":["no"],"errors":{"a":"b"}}"#),
            None
        );
        assert_eq!(field_errors(r#"{"errorMessages":[],"errors":{}}"#), None);
        assert_eq!(field_errors("<html>"), None);
    }

    #[test]
    fn retry_after_takes_seconds_or_a_date() {
        assert_eq!(retry_after_secs(" 7 "), Some(7));
        assert_eq!(retry_after_secs("Wed, 21 Oct 2015 07:28:00 GMT"), Some(0));
        let soon = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(120);
        let text = jiff::fmt::rfc2822::to_string(&soon.to_zoned(jiff::tz::TimeZone::UTC)).unwrap();
        assert!(
            (100..=120).contains(&retry_after_secs(&text).unwrap()),
            "{text}"
        );
        assert_eq!(retry_after_secs("soon"), None);
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use crate::fake::FakeJira;

    #[tokio::test]
    async fn myself_with_the_token_and_without() {
        let fake = FakeJira::start("tok").await;
        let good = Client::new(&fake.base_url(), Credential::new("tok")).unwrap();
        let me = good.myself().await.unwrap();
        assert_eq!(
            (me.key.as_str(), me.time_zone.as_str()),
            ("JIRAUSER10100", "America/Los_Angeles")
        );
        assert_eq!(fake.seen()[0].authorization.as_deref(), Some("Bearer tok"));
        let bad = Client::new(&fake.base_url(), Credential::new("nope")).unwrap();
        assert_eq!(bad.myself().await, Err(JiraError::Auth));
        assert_eq!(
            good.browse_url("PLAT-1"),
            format!("{}/browse/PLAT-1", fake.base_url())
        );
    }

    #[tokio::test]
    async fn errors_map_by_status() {
        let fake = FakeJira::start("t").await;
        fake.route("/rest/api/2/busy", |_| {
            (
                429,
                serde_json::json!({}),
                vec![("Retry-After".into(), "7".into())],
            )
        });
        fake.route("/rest/api/2/down", |_| {
            (
                503,
                serde_json::json!({"errorMessages": ["maintenance"]}),
                vec![],
            )
        });
        fake.route("/rest/api/2/bad", |_| {
            (
                400,
                serde_json::json!({"errorMessages": ["bad JQL"]}),
                vec![],
            )
        });
        fake.route("/rest/api/2/no", |_| {
            (403, serde_json::json!({"errorMessages": ["no"]}), vec![])
        });
        let c = Client::new(&fake.base_url(), Credential::new("t")).unwrap();
        let get = |p: &'static str| {
            let c = &c;
            async move { c.get::<serde_json::Value>(p, &[]).await }
        };
        assert_eq!(
            get("/rest/api/2/busy").await,
            Err(JiraError::RateLimited(Duration::from_secs(7)))
        );
        assert_eq!(
            get("/rest/api/2/down").await,
            Err(JiraError::Server(503, "maintenance".into()))
        );
        assert_eq!(
            get("/rest/api/2/bad").await,
            Err(JiraError::Refused(400, "bad JQL".into()))
        );
        assert_eq!(
            get("/rest/api/2/no").await,
            Err(JiraError::Forbidden("no".into()))
        );
        assert_eq!(get("/rest/api/2/missing").await, Err(JiraError::NotFound));
        let gone = Client::new("http://127.0.0.1:9", Credential::new("t")).unwrap();
        let Err(JiraError::Offline(why)) = gone.myself().await else {
            panic!("not offline")
        };
        // The cause says what failed.
        assert!(
            why.to_lowercase().contains("refused") || why.contains("connect"),
            "{why}"
        );
        // A redirect is not followed, so the token stays here.
        fake.route("/rest/api/2/moved", |_| {
            (
                302,
                serde_json::json!({}),
                vec![("Location".into(), "http://127.0.0.1:9/x".into())],
            )
        });
        assert_eq!(
            get("/rest/api/2/moved").await,
            Err(JiraError::Refused(302, "{}".into()))
        );
    }
}
