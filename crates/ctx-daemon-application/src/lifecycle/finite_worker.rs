use std::{
    io,
    path::PathBuf,
    process::Child,
    time::{Duration, Instant},
};

use super::DaemonHandoff;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const MIN_ESCALATION_REAP_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_ESCALATION_REAP_TIMEOUT: Duration = Duration::from_secs(2);

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
    data_root: PathBuf,
    owner_id: String,
}

impl FiniteCoreWorkerLease {
    pub(super) fn from_handoff(
        data_root: PathBuf,
        handoff: DaemonHandoff,
        child: Option<Child>,
        owner_id: Option<String>,
    ) -> io::Result<Self> {
        Ok(match child {
            Some(child) if child.id() == handoff.pid => {
                let Some(owner_id) = owner_id else {
                    let mut child = child;
                    reap_owned_candidate(&mut child)?;
                    return Err(io::Error::other(
                        "owned finite worker has no stable daemon owner identity",
                    ));
                };
                Self::Owned(FiniteWorkerLease {
                    handoff,
                    child,
                    data_root,
                    owner_id,
                })
            }
            Some(mut child) => {
                reap_owned_candidate(&mut child)?;
                Self::Joined(handoff)
            }
            None => Self::Joined(handoff),
        })
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
        self.interrupt_and_reap_with(
            timeout,
            ctx_daemon_runtime::interrupt_attached_child_group,
            Child::kill,
            Child::try_wait,
        )
    }

    fn interrupt_and_reap_with(
        &mut self,
        timeout: Duration,
        signal: impl FnOnce(&Child) -> io::Result<()>,
        mut kill: impl FnMut(&mut Child) -> io::Result<()>,
        mut try_wait: impl FnMut(&mut Child) -> io::Result<Option<std::process::ExitStatus>>,
    ) -> io::Result<()> {
        let mut first_error = match try_wait(&mut self.child) {
            Ok(Some(_)) => return self.finish_reaped_cleanup(None),
            Ok(None) => None,
            // Even a failed non-blocking status probe cannot waive the exact
            // child's mandatory kill/reap cleanup.
            Err(error) => Some(error),
        };
        let graceful_wait = match signal(&self.child) {
            Ok(()) => true,
            // A child can exit between try_wait and the group signal. The
            // mandatory reap below is authoritative for that race.
            Err(error) if interrupted_group_is_already_gone(&error) => true,
            Err(error) => {
                first_error.get_or_insert(error);
                false
            }
        };
        if graceful_wait {
            let deadline = Instant::now() + timeout;
            loop {
                match try_wait(&mut self.child) {
                    Ok(Some(_)) => return self.finish_reaped_cleanup(first_error),
                    Ok(None) => {}
                    Err(error) => {
                        first_error.get_or_insert(error);
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(
                    POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }

        let escalation_timeout = timeout
            .max(MIN_ESCALATION_REAP_TIMEOUT)
            .min(MAX_ESCALATION_REAP_TIMEOUT);
        let escalation_deadline = Instant::now() + escalation_timeout;
        let mut kill_succeeded = false;
        loop {
            if !kill_succeeded {
                match kill(&mut self.child) {
                    Ok(()) => kill_succeeded = true,
                    // InvalidInput commonly means the process won the kill
                    // race. Status probing below remains authoritative.
                    Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
            match try_wait(&mut self.child) {
                Ok(Some(_)) => return self.finish_reaped_cleanup(first_error),
                Ok(None) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
            if Instant::now() >= escalation_deadline {
                return Err(first_error.unwrap_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "finite worker did not exit after bounded kill escalation",
                    )
                }));
            }
            std::thread::sleep(
                POLL_INTERVAL.min(escalation_deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    fn finish_reaped_cleanup(&self, mut first_error: Option<io::Error>) -> io::Result<()> {
        let daemon_root = ctx_daemon_runtime::daemon_root_path(&self.data_root);
        let endpoints = [
            daemon_root.join("source-refresh-endpoint.json"),
            daemon_root.join("query-endpoint.json"),
        ];
        if let Err(error) = ctx_daemon_runtime::cleanup_reaped_daemon_owner(
            &self.data_root,
            self.handoff.pid,
            &self.owner_id,
            &endpoints,
        ) {
            first_error.get_or_insert_with(|| io::Error::other(error.to_string()));
        }
        first_error.map_or(Ok(()), Err)
    }

    #[cfg(test)]
    pub(super) fn interrupt_and_reap_with_signal_for_test(
        &mut self,
        timeout: Duration,
        signal: impl FnOnce(&Child) -> io::Result<()>,
    ) -> io::Result<()> {
        self.interrupt_and_reap_with(timeout, signal, Child::kill, Child::try_wait)
    }

    #[cfg(test)]
    pub(super) fn interrupt_and_reap_with_actions_for_test(
        &mut self,
        timeout: Duration,
        signal: impl FnOnce(&Child) -> io::Result<()>,
        kill: impl FnMut(&mut Child) -> io::Result<()>,
    ) -> io::Result<()> {
        self.interrupt_and_reap_with(timeout, signal, kill, Child::try_wait)
    }

    #[cfg(test)]
    pub(super) fn interrupt_and_reap_with_probe_for_test(
        &mut self,
        timeout: Duration,
        signal: impl FnOnce(&Child) -> io::Result<()>,
        kill: impl FnMut(&mut Child) -> io::Result<()>,
        try_wait: impl FnMut(&mut Child) -> io::Result<Option<std::process::ExitStatus>>,
    ) -> io::Result<()> {
        self.interrupt_and_reap_with(timeout, signal, kill, try_wait)
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

pub(super) fn reap_owned_candidate(child: &mut Child) -> io::Result<()> {
    reap_owned_candidate_with(
        child,
        ctx_daemon_runtime::interrupt_attached_child_group,
        Child::kill,
        Child::try_wait,
    )
}

fn reap_owned_candidate_with(
    child: &mut Child,
    signal: impl FnOnce(&Child) -> io::Result<()>,
    mut kill: impl FnMut(&mut Child) -> io::Result<()>,
    mut try_wait: impl FnMut(&mut Child) -> io::Result<Option<std::process::ExitStatus>>,
) -> io::Result<()> {
    let mut first_error = match try_wait(child) {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => None,
        Err(error) => Some(error),
    };
    let graceful_wait = match signal(child) {
        Ok(()) => true,
        Err(error) if interrupted_group_is_already_gone(&error) => true,
        Err(error) => {
            first_error.get_or_insert(error);
            false
        }
    };
    if graceful_wait {
        let deadline = Instant::now() + MIN_ESCALATION_REAP_TIMEOUT;
        loop {
            match try_wait(child) {
                Ok(Some(_)) => return first_error.map_or(Ok(()), Err),
                Ok(None) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                    break;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    let deadline = Instant::now() + MIN_ESCALATION_REAP_TIMEOUT;
    let mut kill_succeeded = false;
    loop {
        if !kill_succeeded {
            match kill(child) {
                Ok(()) => kill_succeeded = true,
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match try_wait(child) {
            Ok(Some(_)) => return first_error.map_or(Ok(()), Err),
            Ok(None) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
        if Instant::now() >= deadline {
            return Err(first_error.unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "finite worker candidate did not exit after bounded kill escalation",
                )
            }));
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
pub(super) fn reap_owned_candidate_with_actions_for_test(
    child: &mut Child,
    signal: impl FnOnce(&Child) -> io::Result<()>,
    kill: impl FnMut(&mut Child) -> io::Result<()>,
) -> io::Result<()> {
    reap_owned_candidate_with(child, signal, kill, Child::try_wait)
}

#[cfg(test)]
pub(super) fn reap_owned_candidate_with_probe_for_test(
    child: &mut Child,
    signal: impl FnOnce(&Child) -> io::Result<()>,
    kill: impl FnMut(&mut Child) -> io::Result<()>,
    try_wait: impl FnMut(&mut Child) -> io::Result<Option<std::process::ExitStatus>>,
) -> io::Result<()> {
    reap_owned_candidate_with(child, signal, kill, try_wait)
}
