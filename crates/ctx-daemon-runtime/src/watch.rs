use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use notify::{
    event::{AccessKind, AccessMode, CreateKind, MetadataKind, ModifyKind, RemoveKind},
    Event, EventKind, RecursiveMode,
};

use crate::CoalescingWakePayload;
mod callback_injection;
#[cfg(target_os = "macos")]
mod metadata_poll;
mod native_subscription;

pub const fn native_watch_backend() -> &'static str {
    if cfg!(target_os = "macos") {
        "notify_recommended_with_metadata_poll"
    } else {
        "notify_recommended"
    }
}

pub const WATCH_EVENT_QUEUE_CAPACITY: usize = 256;
pub const WATCH_DEBOUNCE_QUIET: Duration = Duration::from_millis(250);
pub const WATCH_DEBOUNCE_MAX: Duration = Duration::from_secs(2);
static NEXT_WATCHER_EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWatchIgnore {
    Access,
    AccessTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeWatchEvent {
    pub paths: Vec<PathBuf>,
    needs_rescan: bool,
    requires_rearm: bool,
    ignored: Option<NativeWatchIgnore>,
}

impl NativeWatchEvent {
    pub fn ordinary(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            needs_rescan: false,
            requires_rearm: false,
            ignored: None,
        }
    }

    pub fn requiring_rearm(paths: Vec<PathBuf>) -> Self {
        Self {
            requires_rearm: true,
            ..Self::ordinary(paths)
        }
    }

    pub fn rescan(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            needs_rescan: true,
            requires_rearm: true,
            ignored: None,
        }
    }

    pub fn ignored(paths: Vec<PathBuf>, ignored: NativeWatchIgnore) -> Self {
        Self {
            ignored: Some(ignored),
            ..Self::ordinary(paths)
        }
    }

    pub fn needs_rescan(&self) -> bool {
        self.needs_rescan
    }

    pub fn requires_rearm(&self) -> bool {
        self.requires_rearm
    }

    pub fn ignored_kind(&self) -> Option<NativeWatchIgnore> {
        self.ignored
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeWatchError;

pub type NativeWatchResult = std::result::Result<NativeWatchEvent, NativeWatchError>;

fn normalize_native_watch_event(event: notify::Result<Event>) -> NativeWatchResult {
    let event = event.map_err(|_| NativeWatchError)?;
    let ignored = if matches!(
        event.kind,
        EventKind::Access(kind) if !matches!(kind, AccessKind::Close(AccessMode::Write))
    ) {
        Some(NativeWatchIgnore::Access)
    } else if matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime))
    ) {
        Some(NativeWatchIgnore::AccessTime)
    } else {
        None
    };
    let requires_rearm = matches!(
        event.kind,
        EventKind::Any
            | EventKind::Other
            | EventKind::Create(CreateKind::Any | CreateKind::Folder | CreateKind::Other)
            | EventKind::Modify(ModifyKind::Any | ModifyKind::Name(_) | ModifyKind::Other)
            | EventKind::Remove(RemoveKind::Any | RemoveKind::Folder | RemoveKind::Other)
    );
    let needs_rescan = event.need_rescan();
    Ok(NativeWatchEvent {
        paths: event.paths,
        needs_rescan,
        requires_rearm,
        ignored,
    })
}

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct WatchWatermark {
    pub epoch: u64,
    pub sequence: u64,
}

impl WatchWatermark {
    fn new(epoch: u64, sequence: u64) -> Self {
        Self { epoch, sequence }
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct NativeWatcherSnapshot {
    pub ingress_overflows: u64,
    pub ingress_disconnects: u64,
    pub coalesced_wakeups: u64,
    pub reconciliations: u64,
    pub forced_rearms: u64,
    pub registration_attempts: u64,
    pub watched_roots: usize,
}

#[derive(Debug, Default)]
struct NativeWatcherCounters {
    coalesced_wakeups: u64,
    reconciliations: u64,
    forced_rearms: u64,
    registration_attempts: u64,
}

enum WatchMessage {
    Event {
        event: NativeWatchResult,
        watermark: WatchWatermark,
    },
    DrainIngress,
    Stop,
}

type EventClassifier<P> = Arc<dyn Fn(NativeWatchResult, WatchWatermark) -> P + Send + Sync>;
type ReconciliationFactory<P> = Arc<dyn Fn(WatchWatermark) -> P + Send + Sync>;
type IgnoreEvent = Arc<dyn Fn(&NativeWatchEvent) -> bool + Send + Sync>;
type ObservePayload<P> = Arc<dyn Fn(&P) + Send + Sync>;
type SignalPayload<P> = Arc<dyn Fn(P) + Send + Sync>;
type OverflowFence = Arc<dyn Fn(WatchWatermark) + Send + Sync>;
type RearmOverlapHook = Box<dyn FnMut(&Path)>;
type RegistrationAttemptHook = Box<dyn FnMut(&Path) -> Result<()>>;

#[derive(Debug, Default)]
struct RawWatchIngress {
    lost_sequence: AtomicU64,
    overflows: AtomicU64,
    disconnects: AtomicU64,
}

impl RawWatchIngress {
    fn record_loss(&self, loss: WatchWatermark) {
        self.lost_sequence
            .fetch_max(loss.sequence, Ordering::AcqRel);
    }

    fn take_loss(&self, epoch: u64) -> Option<WatchWatermark> {
        let sequence = self.lost_sequence.swap(0, Ordering::AcqRel);
        (sequence != 0).then(|| WatchWatermark::new(epoch, sequence))
    }
}

pub struct NativeFileWatcher {
    watcher: native_subscription::ReliableWatcher,
    watched: BTreeMap<PathBuf, bool>,
    counters: Arc<Mutex<NativeWatcherCounters>>,
    sender: mpsc::SyncSender<WatchMessage>,
    ingress: Arc<RawWatchIngress>,
    accepting_events: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    watcher_epoch: u64,
    callback_sequence: Arc<AtomicU64>,
    ignore_event: IgnoreEvent,
    overflow_fence: OverflowFence,
    rearm_pending: bool,
    rearm_overlap_hook: Option<RearmOverlapHook>,
    registration_attempt_hook: Option<RegistrationAttemptHook>,
}

impl NativeFileWatcher {
    pub fn start<P: CoalescingWakePayload>(
        thread_name: &str,
        ignore_event: IgnoreEvent,
        classify_event: EventClassifier<P>,
        overflow_fence: Arc<dyn Fn(WatchWatermark) + Send + Sync>,
        reconciliation: ReconciliationFactory<P>,
        observe_payload: ObservePayload<P>,
        signal_payload: SignalPayload<P>,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(WATCH_EVENT_QUEUE_CAPACITY);
        let counters = Arc::new(Mutex::new(NativeWatcherCounters::default()));
        let ingress = Arc::new(RawWatchIngress::default());
        let accepting_events = Arc::new(AtomicBool::new(true));
        let watcher_epoch = NEXT_WATCHER_EPOCH.fetch_add(1, Ordering::Relaxed);
        let callback_sequence = Arc::new(AtomicU64::new(0));
        let watcher = native_file_watcher(
            &sender,
            &ingress,
            &accepting_events,
            watcher_epoch,
            &callback_sequence,
            &ignore_event,
            &overflow_fence,
        )?;
        let thread_counters = Arc::clone(&counters);
        let thread_ingress = Arc::clone(&ingress);
        let thread_reconciliation = Arc::clone(&reconciliation);
        let thread_signal_payload = Arc::clone(&signal_payload);
        let thread = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                watch_event_loop(
                    receiver,
                    thread_ingress,
                    thread_counters,
                    watcher_epoch,
                    classify_event,
                    thread_reconciliation,
                    observe_payload,
                    thread_signal_payload,
                );
            })
            .context("start native filesystem debounce worker")?;
        Ok(Self {
            watcher,
            watched: BTreeMap::new(),
            counters,
            sender,
            ingress,
            accepting_events,
            thread: Some(thread),
            watcher_epoch,
            callback_sequence,
            ignore_event,
            overflow_fence,
            rearm_pending: false,
            rearm_overlap_hook: None,
            registration_attempt_hook: None,
        })
    }

    pub fn startup_watermark(&self) -> WatchWatermark {
        WatchWatermark::new(self.watcher_epoch, 0)
    }

    pub fn next_watermark(&self) -> WatchWatermark {
        WatchWatermark::new(
            self.watcher_epoch,
            self.callback_sequence
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_add(1))
                })
                .unwrap_or_else(|current| current)
                .saturating_add(1),
        )
    }

    pub fn needs_registration(&self, desired: &BTreeMap<PathBuf, bool>, force_rearm: bool) -> bool {
        self.replacement_required(force_rearm)
            || self.watched.len() != desired.len()
            || desired
                .iter()
                .any(|(path, recursive)| self.watched.get(path).copied() != Some(*recursive))
    }

    pub fn replacement_required(&self, force_rearm: bool) -> bool {
        force_rearm
            || self.rearm_pending
            || self
                .thread
                .as_ref()
                .is_some_and(thread::JoinHandle::is_finished)
    }

    pub fn reconcile_paths(
        &mut self,
        desired: BTreeMap<PathBuf, bool>,
        force_rearm: bool,
    ) -> Result<()> {
        let mut last_error = None;
        let mut registration_attempts = 0_u64;
        if self.worker_failed() {
            anyhow::bail!("native filesystem watcher worker is unavailable");
        }
        self.rearm_pending |= force_rearm;
        if self.rearm_pending {
            match native_file_watcher(
                &self.sender,
                &self.ingress,
                &self.accepting_events,
                self.watcher_epoch,
                &self.callback_sequence,
                &self.ignore_event,
                &self.overflow_fence,
            ) {
                Ok(mut replacement) => {
                    let mut replacement_ready = true;
                    for (path, recursive) in &desired {
                        registration_attempts = registration_attempts.saturating_add(1);
                        let registration = self
                            .registration_attempt_hook
                            .as_mut()
                            .map_or(Ok(()), |hook| hook(path))
                            .and_then(|()| {
                                replacement
                                    .watch(path, recursive_mode(*recursive))
                                    .map_err(Into::into)
                            });
                        if let Err(error) = registration {
                            replacement_ready = false;
                            last_error = Some(anyhow::anyhow!("watch {}: {error}", path.display()));
                        }
                    }
                    if replacement_ready {
                        for path in desired.keys() {
                            if let Some(hook) = self.rearm_overlap_hook.as_mut() {
                                hook(path);
                            }
                        }
                        self.watcher = replacement;
                        self.watched = desired;
                        self.rearm_pending = false;
                    }
                }
                Err(error) => last_error = Some(error),
            }
        } else {
            let stale = self
                .watched
                .keys()
                .filter(|path| !desired.contains_key(*path))
                .cloned()
                .collect::<Vec<_>>();
            for path in stale {
                if let Err(error) = self.watcher.unwatch(&path) {
                    last_error = Some(anyhow::anyhow!("unwatch {}: {error}", path.display()));
                }
                self.watched.remove(&path);
            }
            for (path, recursive) in &desired {
                let current = self.watched.get(path).copied();
                if current == Some(*recursive) {
                    continue;
                }
                if current.is_some() {
                    if let Err(error) = self.watcher.unwatch(path) {
                        last_error = Some(anyhow::anyhow!("unwatch {}: {error}", path.display()));
                    }
                    self.watched.remove(path);
                }
                registration_attempts = registration_attempts.saturating_add(1);
                let registration = self
                    .registration_attempt_hook
                    .as_mut()
                    .map_or(Ok(()), |hook| hook(path))
                    .and_then(|()| {
                        self.watcher
                            .watch(path, recursive_mode(*recursive))
                            .map_err(Into::into)
                    });
                match registration {
                    Ok(()) => {
                        self.watched.insert(path.clone(), *recursive);
                    }
                    Err(error) => {
                        last_error = Some(anyhow::anyhow!("watch {}: {error}", path.display()));
                    }
                }
            }
        }
        let mut counters = self.lock_counters();
        counters.reconciliations = counters.reconciliations.saturating_add(1);
        counters.registration_attempts = counters
            .registration_attempts
            .saturating_add(registration_attempts);
        if force_rearm {
            counters.forced_rearms = counters.forced_rearms.saturating_add(1);
        }
        drop(counters);
        match last_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn snapshot(&self) -> NativeWatcherSnapshot {
        let counters = self.lock_counters();
        NativeWatcherSnapshot {
            ingress_overflows: self.ingress.overflows.load(Ordering::Acquire),
            ingress_disconnects: self.ingress.disconnects.load(Ordering::Acquire),
            coalesced_wakeups: counters.coalesced_wakeups,
            reconciliations: counters.reconciliations,
            forced_rearms: counters.forced_rearms,
            registration_attempts: counters.registration_attempts,
            watched_roots: self.watched.len(),
        }
    }

    pub fn worker_failed(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
    }

    #[doc(hidden)]
    pub fn install_rearm_overlap_hook(&mut self, hook: impl FnMut(&Path) + 'static) {
        self.rearm_overlap_hook = Some(Box::new(hook));
    }

    #[doc(hidden)]
    pub fn install_registration_attempt_hook(
        &mut self,
        hook: impl FnMut(&Path) -> Result<()> + 'static,
    ) {
        self.registration_attempt_hook = Some(Box::new(hook));
    }

    pub fn stop(&mut self) {
        if self.accepting_events.swap(false, Ordering::AcqRel) {
            self.watcher.stop();
            let _ = self.sender.send(WatchMessage::Stop);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn lock_counters(&self) -> std::sync::MutexGuard<'_, NativeWatcherCounters> {
        self.counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for NativeFileWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

fn recursive_mode(recursive: bool) -> RecursiveMode {
    if recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    }
}

fn native_file_watcher(
    sender: &mpsc::SyncSender<WatchMessage>,
    ingress: &Arc<RawWatchIngress>,
    accepting_events: &Arc<AtomicBool>,
    watcher_epoch: u64,
    callback_sequence: &Arc<AtomicU64>,
    ignore_event: &IgnoreEvent,
    overflow_fence: &OverflowFence,
) -> Result<native_subscription::ReliableWatcher> {
    let sender = sender.clone();
    let ingress = Arc::clone(ingress);
    let accepting_events = Arc::clone(accepting_events);
    let sequence = Arc::clone(callback_sequence);
    let ignore_event = Arc::clone(ignore_event);
    let overflow_fence = Arc::clone(overflow_fence);
    native_subscription::ReliableWatcher::new(move |event: notify::Result<Event>| {
        forward_native_watch_event(
            &sender,
            &ingress,
            &accepting_events,
            watcher_epoch,
            &sequence,
            &ignore_event,
            &overflow_fence,
            normalize_native_watch_event(event),
        );
    })
    .context("start native filesystem watcher")
}

#[allow(clippy::too_many_arguments)]
fn forward_native_watch_event(
    sender: &mpsc::SyncSender<WatchMessage>,
    ingress: &RawWatchIngress,
    accepting_events: &AtomicBool,
    watcher_epoch: u64,
    sequence: &AtomicU64,
    ignore_event: &IgnoreEvent,
    overflow_fence: &OverflowFence,
    event: NativeWatchResult,
) {
    if !accepting_events.load(Ordering::Acquire)
        || event.as_ref().is_ok_and(|event| ignore_event(event))
    {
        return;
    }
    let watermark = WatchWatermark::new(
        watcher_epoch,
        sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(1))
            })
            .unwrap_or_else(|current| current)
            .saturating_add(1),
    );
    match sender.try_send(WatchMessage::Event { event, watermark }) {
        Ok(()) => {}
        Err(mpsc::TrySendError::Full(WatchMessage::Event { watermark, .. })) => {
            overflow_fence(watermark);
            ingress.record_loss(watermark);
            ingress.overflows.fetch_add(1, Ordering::Relaxed);
            match sender.try_send(WatchMessage::DrainIngress) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    ingress.disconnects.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        Err(mpsc::TrySendError::Full(_)) => unreachable!("callback sends only raw events"),
        Err(mpsc::TrySendError::Disconnected(_)) => {
            overflow_fence(watermark);
            ingress.record_loss(watermark);
            ingress.disconnects.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn observe_pending_raw_events<P: CoalescingWakePayload>(
    first: Option<(NativeWatchResult, WatchWatermark)>,
    receiver: &mpsc::Receiver<WatchMessage>,
    ingress: &RawWatchIngress,
    watcher_epoch: u64,
    classify_event: &EventClassifier<P>,
    reconciliation: &ReconciliationFactory<P>,
    observe_payload: &ObservePayload<P>,
    relevant: &mut P,
) -> bool {
    let mut stop = false;
    let mut drained = 0_usize;
    let mut observe_event = |event, watermark| {
        let payload = classify_event(event, watermark);
        if !payload.is_empty() {
            observe_payload(&payload);
            relevant.merge(payload);
        }
    };
    if let Some((event, watermark)) = first {
        observe_event(event, watermark);
        drained = 1;
    }
    while drained < WATCH_EVENT_QUEUE_CAPACITY {
        let message = match receiver.try_recv() {
            Ok(message) => message,
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
        };
        drained = drained.saturating_add(1);
        match message {
            WatchMessage::Event { event, watermark } => observe_event(event, watermark),
            WatchMessage::DrainIngress => {}
            WatchMessage::Stop => {
                stop = true;
                break;
            }
        }
    }
    if let Some(watermark) = ingress.take_loss(watcher_epoch) {
        let payload = reconciliation(watermark);
        if !payload.is_empty() {
            observe_payload(&payload);
            relevant.merge(payload);
        }
    }
    stop
}

#[allow(clippy::too_many_arguments)]
fn watch_event_loop<P: CoalescingWakePayload>(
    receiver: mpsc::Receiver<WatchMessage>,
    ingress: Arc<RawWatchIngress>,
    counters: Arc<Mutex<NativeWatcherCounters>>,
    watcher_epoch: u64,
    classify_event: EventClassifier<P>,
    reconciliation: ReconciliationFactory<P>,
    observe_payload: ObservePayload<P>,
    signal_payload: SignalPayload<P>,
) {
    loop {
        let first = match receiver.recv() {
            Ok(WatchMessage::Event { event, watermark }) => Some((event, watermark)),
            Ok(WatchMessage::DrainIngress) => None,
            Ok(WatchMessage::Stop) | Err(_) => return,
        };
        let started = Instant::now();
        let mut relevant = P::default();
        if observe_pending_raw_events(
            first,
            &receiver,
            &ingress,
            watcher_epoch,
            &classify_event,
            &reconciliation,
            &observe_payload,
            &mut relevant,
        ) {
            return;
        }
        loop {
            let elapsed = started.elapsed();
            if elapsed >= WATCH_DEBOUNCE_MAX {
                break;
            }
            let timeout = WATCH_DEBOUNCE_QUIET.min(WATCH_DEBOUNCE_MAX - elapsed);
            match receiver.recv_timeout(timeout) {
                Ok(WatchMessage::Event { event, watermark }) => {
                    if observe_pending_raw_events(
                        Some((event, watermark)),
                        &receiver,
                        &ingress,
                        watcher_epoch,
                        &classify_event,
                        &reconciliation,
                        &observe_payload,
                        &mut relevant,
                    ) {
                        return;
                    }
                }
                Ok(WatchMessage::DrainIngress) => {
                    if observe_pending_raw_events(
                        None,
                        &receiver,
                        &ingress,
                        watcher_epoch,
                        &classify_event,
                        &reconciliation,
                        &observe_payload,
                        &mut relevant,
                    ) {
                        return;
                    }
                }
                Ok(WatchMessage::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
        if !relevant.is_empty() {
            let mut counters = counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counters.coalesced_wakeups = counters.coalesced_wakeups.saturating_add(1);
            drop(counters);
            signal_payload(relevant);
        }
    }
}

pub fn watch_roots<'a>(targets: impl IntoIterator<Item = &'a Path>) -> BTreeMap<PathBuf, bool> {
    let mut roots = BTreeMap::new();
    for target in targets {
        if target.is_dir() {
            roots
                .entry(target.to_path_buf())
                .and_modify(|recursive| *recursive = true)
                .or_insert(true);
        } else if target.is_file() {
            if let Some(parent) = target.parent() {
                roots.entry(parent.to_path_buf()).or_insert(false);
            }
        } else if let Some(existing) = target.ancestors().find(|candidate| candidate.is_dir()) {
            roots.entry(existing.to_path_buf()).or_insert(false);
        }
    }
    roots
}

#[cfg(test)]
mod tests;
