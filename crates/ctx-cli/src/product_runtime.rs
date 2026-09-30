//! Final-binary ownership of optional Server/Sharing observations. Producers
//! only touch bounded memory; existing runtime owners perform consent and IO.
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use crate::{
    analytics_summary::{Summary, Window},
    hosted::HostedObservers,
};
use ctx_client_observability::analytics::{self as wire, PublicEventV1};

mod daemon;
mod hosted;
mod server;
mod sharing;
#[cfg(test)]
mod tests;

pub(crate) use daemon::DaemonCollector;

const MAX_TERMINALS: usize = 16;
const MAX_SUMMARIES: usize = 50;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Accumulator {
    window: Window,
    terminals: Vec<PublicEventV1>,
}

impl Accumulator {
    fn terminal(&mut self, event: PublicEventV1) {
        // Keep recent lifecycle transitions, including shutdown, under storms.
        if self.terminals.len() == MAX_TERMINALS {
            self.terminals.remove(0);
        }
        self.terminals.push(event);
    }
}

struct Collector {
    root: PathBuf,
    owner: String,
    endpoint: String,
    started: Instant,
    memory: Mutex<Accumulator>,
    flushing: Mutex<()>,
    active: AtomicBool,
    limited: AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Binding {
    Current,
    Deferred,
    Changed,
    OptedOut,
}

impl Collector {
    fn capture(root: &Path) -> Option<Arc<Self>> {
        Self::capture_with_identity(root, true)
    }

    fn capture_with_identity(root: &Path, create_identity: bool) -> Option<Arc<Self>> {
        let endpoint = crate::observability_composition::optional_analytics_endpoint(root)?;
        let owner = if create_identity {
            crate::identity::try_installation_id(root)
        } else {
            crate::identity::try_existing_installation_id(root)
        }
        .ok()
        .flatten()?;
        Some(Arc::new(Self {
            root: root.to_path_buf(),
            owner,
            endpoint,
            started: Instant::now(),
            memory: Mutex::new(Accumulator::default()),
            flushing: Mutex::new(()),
            active: AtomicBool::new(true),
            limited: AtomicBool::new(false),
        }))
    }

    fn observe(&self, record: impl FnOnce(&mut Accumulator)) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut memory) = self.memory.try_lock() else {
            self.limited.store(true, Ordering::Relaxed);
            return;
        };
        if self.active.load(Ordering::Relaxed) {
            record(&mut memory);
        }
    }

    fn server(&self, observation: ctx_history_server::ServerObservation) {
        self.observe(|memory| server::record(memory, observation, self.started.elapsed(), now()));
    }

    fn sharing(&self, observation: ctx_history_sharing::SharingObservation) {
        self.observe(|memory| {
            memory
                .window
                .record(Summary::Sharing(sharing::project(observation)), now())
        });
    }

    fn sharing_observer(self: &Arc<Self>) -> ctx_history_sharing::SharingObserver {
        let collector = self.clone();
        Arc::new(move |observation| collector.sharing(observation))
    }

    fn binding(&self) -> Binding {
        if ctx_app_config::normalized_analytics_environment_override() == Some(false) {
            return Binding::OptedOut;
        }
        let Ok(config) = ctx_app_config::AppConfig::load_read_only(&self.root) else {
            return Binding::Deferred;
        };
        if !crate::analytics::effective_analytics_enabled(&config) {
            return Binding::OptedOut;
        }
        if std::env::var_os("CTX_ANALYTICS_DRY_RUN").is_some() {
            return Binding::Deferred;
        }
        if config.analytics.endpoint != self.endpoint {
            return Binding::Changed;
        }
        match crate::identity::try_existing_installation_id(&self.root) {
            Ok(Some(owner)) if owner == self.owner => Binding::Current,
            Ok(Some(_)) => Binding::Changed,
            // A nonwaiting lookup cannot distinguish absence/contention. Skip
            // this flush; never repair identity or rebind captured observations.
            Ok(None) | Err(_) => Binding::Deferred,
        }
    }

    fn deactivate(&self) {
        self.active.store(false, Ordering::Relaxed);
        if let Ok(mut memory) = self.memory.try_lock() {
            *memory = Accumulator::default();
        }
    }

    fn take(&self) -> Option<(Vec<PublicEventV1>, Vec<PublicEventV1>)> {
        let mut memory = self.memory.try_lock().ok()?;
        let mut summaries = memory.window.take(now());
        if self.limited.swap(false, Ordering::Relaxed) {
            mark_limited(&mut summaries);
        }
        Some((summaries, std::mem::take(&mut memory.terminals)))
    }

    fn flush(&self, drain: bool) {
        let Ok(_flushing) = self.flushing.try_lock() else {
            return;
        };
        match self.binding() {
            Binding::Deferred => return,
            Binding::OptedOut => {
                self.deactivate();
                purge_captured_owner(&self.root, &self.owner, &self.endpoint);
                return;
            }
            Binding::Changed => {
                self.deactivate();
                let _ = crate::analytics_summary::take_saved(&self.root, &self.owner);
                return;
            }
            Binding::Current => {}
        }
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        let Some((summaries, terminals)) = self.take() else {
            return;
        };
        let saved = crate::analytics_summary::take_saved(&self.root, &self.owner);
        let summaries = combine_summaries(saved, summaries);
        // Both append seams recheck captured owner/endpoint and consent under
        // admission. Counters are retired even if the optional outbox is full.
        let _ = crate::observability_composition::append_analytics_summary_for_owner(
            &self.root,
            &self.owner,
            &self.endpoint,
            &summaries,
        );
        let _ = crate::observability_composition::append_optional_analytics_batch_for_owner(
            &self.root,
            &self.owner,
            &self.endpoint,
            &terminals,
        );
        if drain && self.binding() == Binding::Current {
            let _ = crate::observability_composition::drain_analytics_outbox_for_owner(
                &self.root,
                &self.owner,
                DRAIN_TIMEOUT,
            );
        }
    }

    fn runtime_tick(&self, tick: ctx_history_server::ServerRuntimeTick) {
        use ctx_history_server::ServerRuntimeTick as Tick;
        let drain = match tick {
            Tick::Ready | Tick::Interval => true,
            Tick::Stopped | Tick::Failed => false,
        };
        self.flush(drain);
        // Finite finish and terminal runtime hooks admit locally and use the
        // existing throttled child; only live runtime ticks own a network drain.
        if !drain && self.active.load(Ordering::Relaxed) && self.binding() == Binding::Current {
            let _ = crate::analytics_delivery::schedule(&self.root);
        }
    }
}

fn opted_out(root: &Path) -> bool {
    ctx_app_config::normalized_analytics_environment_override() == Some(false)
        || ctx_app_config::AppConfig::load_read_only(root)
            .is_ok_and(|config| !crate::analytics::effective_analytics_enabled(&config))
}

fn purge_captured_owner(root: &Path, owner: &str, endpoint: &str) {
    let _ = crate::analytics_summary::take_saved(root, owner);
    // The existing policy seam rechecks consent, purges only this captured
    // owner on explicit opt-out and never performs telemetry HTTP.
    let _ = crate::observability_composition::append_optional_analytics_batch_for_owner(
        root,
        owner,
        endpoint,
        &[],
    );
}

fn now() -> i64 {
    ctx_history_core::utc_now().timestamp()
}

fn mark_limited(events: &mut [PublicEventV1]) {
    for event in events {
        match event {
            PublicEventV1::ServerSummary(v) => v.collection_limited = true,
            PublicEventV1::SharingSummary(v) => v.collection_limited = true,
            PublicEventV1::SiftSummary(v) => v.collection_limited = true,
            _ => {}
        }
    }
}

fn combine_summaries(
    mut saved: Vec<PublicEventV1>,
    memory: Vec<PublicEventV1>,
) -> Vec<PublicEventV1> {
    let limited = saved.len().saturating_add(memory.len()) > MAX_SUMMARIES;
    saved.extend(
        memory
            .into_iter()
            .take(MAX_SUMMARIES.saturating_sub(saved.len())),
    );
    saved.truncate(MAX_SUMMARIES);
    if limited {
        mark_limited(&mut saved);
    }
    saved
}

/// Keep a clone until `finish` after `hosted::run_with_observers` returns.
/// Runtime ticks already flush long-lived servers every thirty seconds.
pub(crate) fn hosted(root: Option<&Path>) -> HostedObservers {
    hosted_with_identity(root, true)
}

fn hosted_with_identity(root: Option<&Path>, create_identity: bool) -> HostedObservers {
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => match ctx_history_platform::default_data_root() {
            Ok(root) => root,
            Err(_) => return disabled(),
        },
    };
    let Some(collector) = Collector::capture_with_identity(&root, create_identity) else {
        // Disabled startup must honor an observed opt-out, but malformed
        // configuration/dry-run does not authorize destructive cleanup.
        if opted_out(&root) {
            if let Ok(Some(owner)) = crate::identity::try_existing_installation_id(&root) {
                purge_captured_owner(&root, &owner, "");
            }
        }
        return disabled();
    };
    let server = collector.clone();
    let completion = collector.clone();
    let sharing = collector.sharing_observer();
    HostedObservers {
        server: Some(Arc::new(move |fact| server.server(fact))),
        sharing: Some(sharing),
        completion: Some(Arc::new(move |fact| {
            completion.observe(|memory| memory.terminal(hosted::project(fact)));
        })),
        runtime: Some(Arc::new(move |tick| collector.runtime_tick(tick))),
    }
}

/// Terminal-only commands need no observer or consent identity before completion.
pub(crate) fn hosted_after_completion(
    root: Option<&Path>,
    command: &crate::hosted::HostedCommand,
) -> HostedObservers {
    let restore_marker = command.restore_ownership_marker(root);
    let root = root.map(Path::to_path_buf);
    HostedObservers {
        completion: Some(Arc::new(move |fact| {
            let failed_restore = fact.result.is_err()
                && matches!(
                    fact.operation,
                    crate::hosted::HostedOperation::Existing(
                        wire::HostedOperationV1::ArchiveRestore
                            | wire::HostedOperationV1::ServerRestore
                    )
                );
            // A malformed input (even if its error output fails) must leave an
            // unowned target retryable. An existing identity can record failure;
            // ownership also permits capture after restore or output failure.
            let create_identity =
                !failed_restore || restore_marker.as_ref().is_some_and(|path| path.is_file());
            let observers = hosted_with_identity(root.as_deref(), create_identity);
            if let Some(completion) = &observers.completion {
                completion(fact);
            }
            finish(&observers);
        })),
        ..Default::default()
    }
}

fn disabled() -> HostedObservers {
    // Suppress the hosted adapter's legacy fallback even when consent is off.
    HostedObservers {
        completion: Some(Arc::new(|_| {})),
        ..Default::default()
    }
}

/// Flush after producer/worker joins, including failure before runtime startup.
/// Stopped only admits locally and schedules the existing child; it cannot
/// fabricate a lifecycle event or perform foreground telemetry HTTP.
pub(crate) fn finish(observers: &HostedObservers) {
    if let Some(flush) = &observers.runtime {
        flush(ctx_history_server::ServerRuntimeTick::Stopped);
    }
}

fn measured(total: u64) -> wire::MeasuredTotal {
    wire::MeasuredTotal { samples: 1, total }
}
fn histogram(duration: Duration) -> [u64; 14] {
    let mut bins = [0; 14];
    bins[wire::native_duration_index(duration)] = 1;
    bins
}
fn counts(duration: Option<Duration>, failed: bool) -> wire::WindowCounts {
    wire::WindowCounts {
        observed: 1,
        failed: u64::from(failed),
        latency: duration.map(histogram),
    }
}
