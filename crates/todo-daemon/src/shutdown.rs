use tokio::sync::watch;

/// Triggers a graceful daemon shutdown. Cheap to clone.
#[derive(Clone, Debug)]
pub struct ShutdownHandle(watch::Sender<bool>);

impl ShutdownHandle {
    pub(crate) fn new() -> Self {
        Self(watch::Sender::new(false))
    }

    pub fn trigger(&self) {
        self.0.send_replace(true);
    }

    pub(crate) fn signal(&self) -> ShutdownSignal {
        ShutdownSignal(self.0.subscribe())
    }
}

/// Resolves once shutdown has been triggered.
#[derive(Clone, Debug)]
pub(crate) struct ShutdownSignal(watch::Receiver<bool>);

impl ShutdownSignal {
    pub(crate) async fn wait(&mut self) {
        // An Err means every handle is gone, which also means shutdown.
        let _ = self.0.wait_for(|&stop| stop).await;
    }
}
