use super::*;

#[derive(Debug, Clone)]
enum SharedWarmupEventKind {
    Phase {
        phase: HarnessSetupPhase,
        message: String,
    },
    Log {
        phase: HarnessSetupPhase,
        level: HarnessSetupLogLevel,
        message: String,
    },
    Progress {
        progress: HarnessSetupProgressUpdate,
    },
}

#[derive(Debug, Clone)]
struct SharedWarmupEvent {
    seq: u64,
    kind: SharedWarmupEventKind,
}

impl SharedWarmupEvent {
    fn emit(&self, observer: &dyn HarnessSetupObserver) {
        match &self.kind {
            SharedWarmupEventKind::Phase { phase, message } => observer.on_phase(*phase, message),
            SharedWarmupEventKind::Log {
                phase,
                level,
                message,
            } => observer.on_log(*phase, *level, message),
            SharedWarmupEventKind::Progress { progress } => observer.on_progress(progress.clone()),
        }
    }
}

#[derive(Debug, Clone)]
enum SharedWarmupStatus {
    Running,
    Ready,
    Error(String),
}

struct SharedWarmupTaskState {
    events: VecDeque<SharedWarmupEvent>,
    next_seq: u64,
}

pub(super) struct SharedWarmupTask {
    inner: StdMutex<SharedWarmupTaskState>,
    tx: broadcast::Sender<SharedWarmupEvent>,
    status_tx: watch::Sender<SharedWarmupStatus>,
}

impl SharedWarmupTask {
    pub(super) fn new() -> Self {
        let (tx, _) = broadcast::channel(SHARED_WARMUP_CHANNEL_CAP);
        let (status_tx, _) = watch::channel(SharedWarmupStatus::Running);
        Self {
            inner: StdMutex::new(SharedWarmupTaskState {
                events: VecDeque::new(),
                next_seq: 0,
            }),
            tx,
            status_tx,
        }
    }

    pub(super) async fn attach(&self, observer: Option<&dyn HarnessSetupObserver>) -> Result<()> {
        let mut rx = self.tx.subscribe();
        let mut status_rx = self.status_tx.subscribe();
        let (events, mut last_seq, initial_status) = {
            let inner = match self.inner.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let events = inner.events.iter().cloned().collect::<Vec<_>>();
            let last_seq = events.last().map(|event| event.seq).unwrap_or(0);
            (events, last_seq, status_rx.borrow().clone())
        };

        if let Some(observer) = observer {
            for event in &events {
                event.emit(observer);
            }
        }

        match initial_status {
            SharedWarmupStatus::Running => {}
            SharedWarmupStatus::Ready => return Ok(()),
            SharedWarmupStatus::Error(message) => return Err(anyhow::anyhow!(message)),
        }

        loop {
            tokio::select! {
                recv = rx.recv() => {
                    match recv {
                        Ok(event) => {
                            if event.seq <= last_seq {
                                continue;
                            }
                            if let Some(observer) = observer {
                                event.emit(observer);
                            }
                            last_seq = event.seq;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => {
                            match status_rx.borrow().clone() {
                                SharedWarmupStatus::Running => continue,
                                SharedWarmupStatus::Ready => return Ok(()),
                                SharedWarmupStatus::Error(message) => {
                                    return Err(anyhow::anyhow!(message));
                                }
                            }
                        }
                    }
                }
                changed = status_rx.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    match status_rx.borrow().clone() {
                        SharedWarmupStatus::Running => {}
                        SharedWarmupStatus::Ready => return Ok(()),
                        SharedWarmupStatus::Error(message) => return Err(anyhow::anyhow!(message)),
                    }
                }
            }
        }
    }

    pub(super) fn finish(&self, result: std::result::Result<(), String>) {
        let status = match result {
            Ok(()) => SharedWarmupStatus::Ready,
            Err(message) => SharedWarmupStatus::Error(message),
        };
        let _ = self.status_tx.send(status);
    }

    fn push_event(&self, kind: SharedWarmupEventKind) {
        let event = {
            let mut inner = match self.inner.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            inner.next_seq += 1;
            let event = SharedWarmupEvent {
                seq: inner.next_seq,
                kind,
            };
            inner.events.push_back(event.clone());
            while inner.events.len() > SHARED_WARMUP_EVENT_CAP {
                inner.events.pop_front();
            }
            event
        };
        let _ = self.tx.send(event);
    }
}

pub(super) struct SharedWarmupObserver {
    task: Arc<SharedWarmupTask>,
}

impl SharedWarmupObserver {
    pub(super) fn new(task: Arc<SharedWarmupTask>) -> Self {
        Self { task }
    }
}

impl HarnessSetupObserver for SharedWarmupObserver {
    fn on_phase(&self, phase: HarnessSetupPhase, message: &str) {
        self.task.push_event(SharedWarmupEventKind::Phase {
            phase,
            message: message.to_string(),
        });
    }

    fn on_log(&self, phase: HarnessSetupPhase, level: HarnessSetupLogLevel, message: &str) {
        self.task.push_event(SharedWarmupEventKind::Log {
            phase,
            level,
            message: message.to_string(),
        });
    }

    fn on_progress(&self, progress: HarnessSetupProgressUpdate) {
        self.task
            .push_event(SharedWarmupEventKind::Progress { progress });
    }
}
