//! Sync engine and tracker adapter trait (design 5).

pub mod credential;
pub mod instance;

pub use credential::{Credential, CredentialError, CredentialRef};
pub use instance::{Category, Instance, PromoteTarget, Push, Scope, TrackerKind};
