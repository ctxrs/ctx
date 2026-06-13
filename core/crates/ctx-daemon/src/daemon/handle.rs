use tokio::sync::broadcast;

use super::route_capabilities::DaemonShutdownSignal;

#[derive(Clone)]
pub struct DaemonHandle {
    shutdown_tx: broadcast::Sender<()>,
}

impl DaemonHandle {
    pub fn new(shutdown_tx: broadcast::Sender<()>) -> Self {
        Self { shutdown_tx }
    }

    pub fn subscribe_shutdown(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }

    pub fn shutdown_signal(&self) -> DaemonShutdownSignal {
        DaemonShutdownSignal::new(self.shutdown_tx.clone())
    }
}
