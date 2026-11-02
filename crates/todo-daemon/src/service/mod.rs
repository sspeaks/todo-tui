//! tonic service implementations over a shared [`Ctx`].

pub(crate) mod item;
pub(crate) mod system;
pub(crate) mod view;
pub(crate) mod watch;

use todo_store::Store;

use crate::ShutdownHandle;
use crate::config::ConfigHandle;
use crate::conn::Clients;
use crate::scheduler::Wake;

#[derive(Clone)]
pub(crate) struct Ctx {
    pub(crate) store: Store,
    pub(crate) clients: Clients,
    pub(crate) shutdown: ShutdownHandle,
    /// Poked after every user op, which may give the scheduler work.
    pub(crate) wake: Wake,
    pub(crate) config: ConfigHandle,
}
