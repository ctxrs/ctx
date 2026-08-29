use std::{
    io,
    process::Child,
    time::{Duration, Instant},
};

use super::DaemonHandoff;

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Authenticated ownership retained only by the CLI that spawned a finite
/// foreground worker. An existing daemon and a losing singleton candidate
/// intentionally carry no child, and therefore no process-control authority.
#[derive(Debug)]
pub struct FiniteCoreWorkerLease {
    handoff: DaemonHandoff,
    child: Option<Child>,
}

impl FiniteCoreWorkerLease {
    pub(super) fn authenticated(handoff: DaemonHandoff, child: Option<Child>) -> Self {
        Self { handoff, child }
    }

    pub fn handoff(&self) -> DaemonHandoff {
        self.handoff
    }

    /// Control exists only for the exact local child which won the
    /// identity-stable lifecycle handoff.
    pub fn controls_authenticated_child(&self) -> bool {
        self.child
            .as_ref()
            .is_some_and(|child| child.id() == self.handoff.pid)
    }

    /// Wait for the finite worker to retire naturally, then reap it. A bounded
    /// fallback is limited to the retained child and never targets a daemon
    /// discovered through the singleton lock.
    pub fn reap_after_completion(&mut self, timeout: Duration) -> io::Result<()> {
        self.wait_then_force(timeout)
    }

    /// Ctrl-C already reaches an attached finite worker through its normal
    /// terminal/console signal path. This waits for that graceful exit and
    /// forces only the retained child if it remains live.
    pub fn interrupt_and_reap(&mut self, timeout: Duration) -> io::Result<()> {
        self.wait_then_force(timeout)
    }

    fn wait_then_force(&mut self, timeout: Duration) -> io::Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        let deadline = Instant::now() + timeout;
        loop {
            if child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                return Ok(());
            }
            std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    #[cfg(test)]
    pub(super) fn child_for_test(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }
}

pub(super) fn reap_owned_candidate(child: &mut Option<Child>) {
    let Some(child) = child.as_mut() else {
        return;
    };
    match child.try_wait() {
        Ok(Some(_)) | Err(_) => return,
        Ok(None) => {}
    }
    let _ = child.kill();
    let _ = child.wait();
}
