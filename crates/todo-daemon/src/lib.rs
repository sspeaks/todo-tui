//! The todo-tui daemon: tonic services, scheduler and watch fan-out.

mod backup;
pub mod client;
pub mod config;
mod conn;
mod error;
mod logging;
pub mod paths;
pub mod remote;
mod scheduler;
mod server;
mod service;
mod shutdown;
pub mod spool;
mod supervise;
mod sync;

pub use config::ARCHIVE_DONE_AFTER_DAYS;
pub use server::{Daemon, DaemonOptions, Exit, ReloadHandle, Started, run};
pub use shutdown::ShutdownHandle;
