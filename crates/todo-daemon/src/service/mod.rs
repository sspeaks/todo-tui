//! tonic service implementations over a shared [`Ctx`].

pub(crate) mod item;
pub(crate) mod system;
pub(crate) mod view;
pub(crate) mod watch;

use todo_store::Store;

use crate::ShutdownHandle;
use crate::conn::Clients;

#[derive(Clone)]
pub(crate) struct Ctx {
    pub(crate) store: Store,
    pub(crate) clients: Clients,
    pub(crate) shutdown: ShutdownHandle,
}
