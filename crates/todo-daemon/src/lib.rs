//! The todo-tui daemon: tonic services, scheduler and watch fan-out.

pub mod client;
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

pub use scheduler::ARCHIVE_DONE_AFTER_DAYS;
pub use server::{Daemon, DaemonOptions, Exit, Started, run};
pub use shutdown::ShutdownHandle;
