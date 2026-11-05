//! A configured tracker connection (design 5.3, 8.2 `[[instances]]`).

use std::collections::BTreeMap;
use std::time::Duration;

use todo_core::Priority;

use crate::credential::CredentialRef;

/// The trackers an instance can be (ADR-0006: Data Center first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerKind {
    /// Self-hosted Jira Data Center (or Server): REST v2, a Personal Access Token.
    JiraDataCenter,
}

impl TrackerKind {
    /// As `kind` is written in config.
    pub fn name(self) -> &'static str {
        match self {
            TrackerKind::JiraDataCenter => "jira-dc",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "jira-dc" => Some(TrackerKind::JiraDataCenter),
            _ => None,
        }
    }

    /// `RemoteLink.tracker_kind`.
    pub fn link_kind(self) -> &'static str {
        match self {
            TrackerKind::JiraDataCenter => "jira",
        }
    }
}

/// A named tracker-native query that discovers issues (design 5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub name: String,
    pub query: String,
}

/// The default scope: what is assigned to you and not resolved.
pub fn default_scopes() -> Vec<Scope> {
    vec![Scope {
        name: "mine".to_owned(),
        query: "assignee = currentUser() AND resolution = Unresolved".to_owned(),
    }]
}

/// One `[[instances]]` entry, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    /// Stable, as `RemoteLink.instance_id` stores it.
    pub id: String,
    pub kind: TrackerKind,
    /// `https://…`, without a trailing slash.
    pub base_url: String,
    pub credential: CredentialRef,
    /// At least one.
    pub scopes: Vec<Scope>,
    /// Between pulls while an interactive client is connected (design 5.4).
    pub poll_interactive: Duration,
    /// Between pulls otherwise.
    pub poll_idle: Duration,
    /// Resolution names that mean cancelled (design 5.8); any other means done.
    pub cancelled_resolutions: Vec<String>,
    /// How edits reach the tracker (ADR-0007).
    pub push: Push,
}

/// An instance's push settings (ADR-0007).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Push {
    /// False keeps the instance read-only, as M9 was.
    pub enabled: bool,
    /// The resolution a close to done sets (the transition screen asks).
    pub done_resolution: String,
    /// The resolution a close to cancelled sets.
    pub cancel_resolution: String,
    /// The target status, by name, of the transition into a category; the
    /// first Jira offers into it otherwise.
    pub transitions: BTreeMap<Category, String>,
    /// Local level → the Jira priority name to set; the first of that rank
    /// otherwise (design 5.8).
    pub priority_map: BTreeMap<Priority, String>,
}

impl Default for Push {
    fn default() -> Self {
        Push {
            enabled: true,
            done_resolution: "Done".to_owned(),
            cancel_resolution: "Won't Do".to_owned(),
            transitions: BTreeMap::new(),
            priority_map: BTreeMap::new(),
        }
    }
}

/// A status category a transition goes into (design 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Todo,
    InProgress,
    Done,
}

impl Category {
    /// As config names it: `todo`, `in_progress`, `done`.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "todo" => Some(Category::Todo),
            "in_progress" => Some(Category::InProgress),
            "done" => Some(Category::Done),
            _ => None,
        }
    }
}

/// The default `cancelled_resolutions` (design 5.8).
pub fn default_cancelled_resolutions() -> Vec<String> {
    [
        "Won't Do",
        "Won't Fix",
        "Duplicate",
        "Cannot Reproduce",
        "Declined",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// `poll_seconds` by default, with a TUI connected.
pub const POLL_INTERACTIVE: Duration = Duration::from_secs(60);
/// Between pulls with no TUI connected.
pub const POLL_IDLE: Duration = Duration::from_secs(600);

/// A tracker's address, checked (design 5.10: a token never crosses the
/// network in the clear): `https://host[:port][/path]`, or plain `http://`
/// only to this machine (a test server). No user or password, query or
/// fragment, which would leak or break every request path. The result has
/// no trailing slash.
pub fn tracker_base_url(text: &str) -> Result<String, String> {
    let bad = || {
        "must be an https:// address, e.g. \"https://jira.example.com\" (http:// only to this machine)"
            .to_owned()
    };
    let url = url::Url::parse(text.trim()).map_err(|_| bad())?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "must not hold a user name or password: the credential goes in `credential`".to_owned(),
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("must not have a ?query or #fragment".to_owned());
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => return Err(bad()),
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err(bad()),
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tracker_is_https_or_this_machine() {
        for (given, want) in [
            ("https://jira.example.com/", "https://jira.example.com"),
            ("https://j.example.com/jira", "https://j.example.com/jira"),
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
            ("http://localhost:9/", "http://localhost:9"),
            ("http://[::1]", "http://[::1]"),
        ] {
            assert_eq!(tracker_base_url(given).as_deref(), Ok(want), "{given}");
        }
        for bad in [
            "http://jira.example.com",
            "http://127.0.0.1:8080@evil.com",
            "http://localhost:x@evil.com",
            "https://user:pw@jira.example.com",
            "https://jira.example.com/?os_authType=basic",
            "https://jira.example.com/#x",
            "https://",
            "ftp://x",
            "http://127.0.0.1.example.com",
            "jira.example.com",
        ] {
            assert!(tracker_base_url(bad).is_err(), "{bad}");
        }
    }
}
