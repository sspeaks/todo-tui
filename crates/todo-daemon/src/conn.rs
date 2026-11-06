//! Accepted connections, tagged with an id so services can tell which
//! client is calling (the interactive-client count of design 4.2).

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use todo_proto::v1::ClientKind;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tonic::transport::server::Connected;

/// Request extension identifying the connection a call arrived on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConnId(u64);

/// Open connections and the client kind each declared in `GetServerInfo`.
#[derive(Clone, Default, Debug)]
pub(crate) struct Clients {
    next_id: Arc<AtomicU64>,
    kinds: Arc<Mutex<HashMap<ConnId, ClientKind>>>,
    /// Told when an interactive client arrives with none connected.
    arrived: Arc<tokio::sync::Notify>,
}

impl Clients {
    pub(crate) fn accept<IO>(&self, io: IO) -> Conn<IO> {
        let id = ConnId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.lock().insert(id, ClientKind::Unspecified);
        Conn {
            io,
            guard: ConnGuard {
                id,
                clients: self.clone(),
            },
        }
    }

    pub(crate) fn set_kind(&self, id: ConnId, kind: ClientKind) {
        let first = kind == ClientKind::Interactive && self.other_interactive(Some(id)) == 0;
        if let Some(k) = self.lock().get_mut(&id) {
            *k = kind;
        }
        if first {
            self.arrived.notify_waiters();
        }
    }

    /// Resolves when an interactive client arrives with none connected.
    pub(crate) async fn interactive_arrived(&self) {
        self.arrived.notified().await;
    }

    /// Interactive clients on connections other than `except`.
    pub(crate) fn other_interactive(&self, except: Option<ConnId>) -> usize {
        self.lock()
            .iter()
            .filter(|&(id, kind)| {
                Some(*id) != except && matches!(kind, ClientKind::Interactive | ClientKind::Mobile)
            })
            .count()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ConnId, ClientKind>> {
        self.kinds.lock().unwrap_or_else(|e| e.into_inner())
    }
}

struct ConnGuard {
    id: ConnId,
    clients: Clients,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.clients.lock().remove(&self.id);
    }
}

/// A stream (the Unix socket's, or the remote listener's TCP) that
/// unregisters itself from [`Clients`] when hyper drops it.
pub(crate) struct Conn<IO> {
    io: IO,
    guard: ConnGuard,
}

impl<IO> Connected for Conn<IO> {
    type ConnectInfo = ConnId;

    fn connect_info(&self) -> ConnId {
        self.guard.id
    }
}

impl<IO: AsyncRead + Unpin> AsyncRead for Conn<IO> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_read(cx, buf)
    }
}

impl<IO: AsyncWrite + Unpin> AsyncWrite for Conn<IO> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}
