//! Jira Data Center tracker adapter (ADR-0006): REST API v2, a Personal
//! Access Token as a bearer token.

pub mod adapter;
pub mod client;
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod issue;
pub mod wiki;

pub use adapter::{
    Adapter, CreateError, Fetched, FieldPatch, NewIssue, PullError, PushError, Target,
};
pub use client::{Client, JiraError, Myself};
