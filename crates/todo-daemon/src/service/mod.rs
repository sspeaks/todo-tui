//! tonic service implementations over a shared [`Ctx`].

pub(crate) mod item;
pub(crate) mod system;
pub(crate) mod view;
pub(crate) mod watch;

use todo_store::Store;
use tonic::{Code, Status};

use crate::ShutdownHandle;
use crate::conn::Clients;
use crate::error;

#[derive(Clone)]
pub(crate) struct Ctx {
    pub(crate) store: Store,
    pub(crate) clients: Clients,
    pub(crate) shutdown: ShutdownHandle,
}

/// UNIMPLEMENTED for an RPC this daemon does not serve yet.
fn not_implemented(rpc: &str) -> Status {
    error::error_info(
        Code::Unimplemented,
        "NOT_IMPLEMENTED",
        format!("{rpc} is not implemented yet"),
        [],
    )
}
