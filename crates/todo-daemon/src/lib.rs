//! The todo-tui daemon: tonic services, scheduler and watch fan-out.

pub mod client;
mod conn;
mod error;
mod logging;
pub mod paths;
mod server;
mod service;
mod shutdown;

pub use server::{Daemon, Exit, Started, run};
pub use shutdown::ShutdownHandle;
