//! Jira Data Center tracker adapter (ADR-0006): REST API v2, a Personal
//! Access Token as a bearer token.

pub mod client;
#[cfg(any(test, feature = "testing"))]
pub mod fake;

pub use client::{Client, JiraError, Myself};
