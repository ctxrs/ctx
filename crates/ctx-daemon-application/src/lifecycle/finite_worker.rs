use std::{
    io,
    process::Child,
    time::{Duration, Instant},
};

use super::DaemonHandoff;

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A finite-worker result has two intentionally different authorities.
///
/// `Owned` is the exact direct child that won the singleton handoff.  Only the
/// foreground client which received this value can interrupt and reap it.
/// `Joined` describes an already-running owner (including a singleton loser)
/// and is observational: it has no process-control capability at all.
#[derive(Debug)]
pub enum FiniteCoreWorkerLease {
    Owned(FiniteWorkerLease),
    Joined(DaemonHandoff),
}

/// Capability for one authenticated direct finite-worker child.
#[derive(Debug)]
pub struct FiniteWorkerLease {
    handoff: DaemonHandoff,
    child: Child,
}

impl FiniteCoreWorkerLease {
    pub(super) fn from_handoff(handoff: DaemonHandoff, child: Option<Child>) -> Self {
        match child {
            Some(child) if child.id() == handoff.pid => {
                Self::Owned(FiniteWorkerLease { handoff, child })
            }
            Some(mut child) => {
                reap_owned_candidate(&mut child);
                Self::Joined(handoff)
            }
            None => Self::Joined(handoff),
        }
    }

    pub fn handoff(&self) -> DaemonHandoff {
        match self {
            Self::Owned(lease) => lease.handoff,
            Self::Joined(handoff) => *handoff,
        }
    }

    pub fn into_owned(self) -> Option<FiniteWorkerLease> {
        match self {
            Self::Owned(lease) => Some(lease),
            Self::Joined(_) => None,
        }
    }
}

impl FiniteWorkerLease {
    pub fn handoff(&self) -> DaemonHandoff {
        self.handoff
    }

    /// Reap a completed exact child without changing the state of an active
    /// finite worker. Normal foreground success must not shorten coalesced
    /// successor work just because the client finished observing it.
    pub fn reap_if_exited(&mut self) -> io::Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    /// Gracefully interrupt only the child's private process group, then reap
    /// that exact direct child. A bounded kill is an escalation for this owned
    /// child only; it never targets a lock pid, endpoint owner, or joiner.
    pub fn interrupt_and_reap(&mut self, timeout: Duration) -> io::Result<()> {
        if self.reap_if_exited()? {
            return Ok(());
        }
        match ctx_daemon_runtime::interrupt_attached_child_group(&self.child) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => return Err(error),
            // A child can exit between try_wait and the group signal. The
            // mandatory reap below is authoritative for that race.
            Err(error) if interrupted_group_is_already_gone(&error) => {}
            Err(error) => return Err(error),
        }
        let deadline = Instant::now() + timeout;
        loop {
            if self.reap_if_exited()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                match self.child.kill() {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
                    Err(error) => return Err(error),
                }
                let _ = self.child.wait();
                return Ok(());
            }
            std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}

fn interrupted_group_is_already_gone(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(windows)]
    {
        // The authoritative child reap below determines whether a console
        // group disappeared during delivery; Windows has no ESRCH equivalent.
        let _ = error;
        false
    }
}

pub(super) fn reap_owned_candidate(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) | Err(_) => return,
        Ok(None) => {}
    }
    let _ = child.kill();
    let _ = child.wait();
}
