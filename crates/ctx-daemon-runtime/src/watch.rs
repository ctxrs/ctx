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
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};

use crate::CoalescingWakePayload;

pub const WATCH_EVENT_QUEUE_CAPACITY: usize = 256;
pub const WATCH_DEBOUNCE_QUIET: Duration = Duration::from_millis(250);
pub const WATCH_DEBOUNCE_MAX: Duration = Duration::from_secs(2);
const WATCH_EVENT_OVERFLOW_CAPACITY: usize = WATCH_EVENT_QUEUE_CAPACITY / 2;

static NEXT_WATCHER_EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NativeWatchIgnore {
    Access,
    AccessTime,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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
    pending: Mutex<BTreeMap<NativeWatchResult, WatchWatermark>>,
    lost_sequence: AtomicU64,
    overflows: AtomicU64,
    disconnects: AtomicU64,
}

impl RawWatchIngress {
    fn merge_nonblocking(&self, event: NativeWatchResult, watermark: WatchWatermark) -> bool {
        let merged = match self.pending.try_lock() {
            Ok(mut pending) => merge_raw_event(&mut pending, event, watermark),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                merge_raw_event(&mut poisoned.into_inner(), event, watermark)
            }
            Err(std::sync::TryLockError::WouldBlock) => false,
        };
        if !merged {
            self.lost_sequence
                .fetch_max(watermark.sequence, Ordering::AcqRel);
        }
        merged
    }

    fn take_nonblocking(
        &self,
        epoch: u64,
    ) -> Option<(
        BTreeMap<NativeWatchResult, WatchWatermark>,
        Option<WatchWatermark>,
    )> {
        let mut pending = match self.pending.try_lock() {
            Ok(pending) => pending,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        let events = std::mem::take(&mut *pending);
        let sequence = self.lost_sequence.swap(0, Ordering::AcqRel);
        Some((
            events,
            (sequence != 0).then(|| WatchWatermark::new(epoch, sequence)),
        ))
    }
}

fn merge_raw_event(
    pending: &mut BTreeMap<NativeWatchResult, WatchWatermark>,
    event: NativeWatchResult,
    watermark: WatchWatermark,
) -> bool {
    if pending.len() >= WATCH_EVENT_OVERFLOW_CAPACITY && !pending.contains_key(&event) {
        return false;
    }
    pending
        .entry(event)
        .and_modify(|current| *current = (*current).max(watermark))
        .or_insert(watermark);
    true
}

pub struct NativeFileWatcher {
    watcher: RecommendedWatcher,
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
        reconciliation: ReconciliationFactory<P>,
        observe_payload: ObservePayload<P>,
        signal_payload: SignalPayload<P>,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(WATCH_EVENT_OVERFLOW_CAPACITY);
        let counters = Arc::new(Mutex::new(NativeWatcherCounters::default()));
        let ingress = Arc::new(RawWatchIngress::default());
        let accepting_events = Arc::new(AtomicBool::new(true));
        let watcher_epoch = NEXT_WATCHER_EPOCH.fetch_add(1, Ordering::Relaxed);
        let callback_sequence = Arc::new(AtomicU64::new(0));
        let callback_reconciliation = Arc::clone(&reconciliation);
        let callback_observe = Arc::clone(&observe_payload);
        let overflow_fence: OverflowFence = Arc::new(move |watermark| {
            callback_observe(&callback_reconciliation(watermark));
        });
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
) -> Result<RecommendedWatcher> {
    let sender = sender.clone();
    let ingress = Arc::clone(ingress);
    let accepting_events = Arc::clone(accepting_events);
    let sequence = Arc::clone(callback_sequence);
    let ignore_event = Arc::clone(ignore_event);
    let overflow_fence = Arc::clone(overflow_fence);
    RecommendedWatcher::new(
        move |event: notify::Result<Event>| {
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
        },
        Config::default(),
    )
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
        Err(mpsc::TrySendError::Full(WatchMessage::Event { event, watermark })) => {
            overflow_fence(watermark);
            if !ingress.merge_nonblocking(event, watermark) {
                ingress.overflows.fetch_add(1, Ordering::Relaxed);
            }
            match sender.try_send(WatchMessage::DrainIngress) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    overflow_fence(watermark);
                    ingress.disconnects.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        Err(mpsc::TrySendError::Full(_)) => unreachable!("callback sends only raw events"),
        Err(mpsc::TrySendError::Disconnected(_)) => {
            overflow_fence(watermark);
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
    let mut events = first.into_iter().collect::<Vec<_>>();
    let mut stop = false;
    for message in receiver.try_iter() {
        match message {
            WatchMessage::Event { event, watermark } => events.push((event, watermark)),
            WatchMessage::DrainIngress => {}
            WatchMessage::Stop => stop = true,
        }
    }
    let Some((overflow, loss)) = ingress.take_nonblocking(watcher_epoch) else {
        return stop;
    };
    events.extend(overflow);
    events.sort_by_key(|(_, watermark)| *watermark);
    if let Some(watermark) = loss {
        let payload = reconciliation(watermark);
        if !payload.is_empty() {
            observe_payload(&payload);
            relevant.merge(payload);
        }
    }
    for (event, watermark) in events {
        let payload = classify_event(event, watermark);
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
mod tests {
    use notify::event::{DataChange, Flag};

    use super::*;

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    struct TestPayload(Option<WatchWatermark>);

    impl CoalescingWakePayload for TestPayload {
        fn is_empty(&self) -> bool {
            self.0.is_none()
        }

        fn merge(&mut self, other: Self) {
            if let Some(watermark) = other.0 {
                self.0 = Some(self.0.map_or(watermark, |current| current.max(watermark)));
            }
        }
    }

    #[test]
    fn notify_events_are_normalized_without_product_policy() {
        let path = PathBuf::from("/tmp/history.jsonl");
        let access = normalize_native_watch_event(Ok(Event::new(EventKind::Access(
            AccessKind::Read,
        ))
        .add_path(path.clone())))
        .unwrap();
        assert_eq!(access.paths, vec![path.clone()]);
        assert_eq!(access.ignored_kind(), Some(NativeWatchIgnore::Access));
        assert!(!access.needs_rescan());
        assert!(!access.requires_rearm());

        let access_time = normalize_native_watch_event(Ok(Event::new(EventKind::Modify(
            ModifyKind::Metadata(MetadataKind::AccessTime),
        ))
        .add_path(path.clone())))
        .unwrap();
        assert_eq!(
            access_time.ignored_kind(),
            Some(NativeWatchIgnore::AccessTime)
        );

        let write_close = normalize_native_watch_event(Ok(Event::new(EventKind::Access(
            AccessKind::Close(AccessMode::Write),
        ))
        .add_path(path.clone())))
        .unwrap();
        assert_eq!(write_close.ignored_kind(), None);

        let rename = normalize_native_watch_event(Ok(Event::new(EventKind::Modify(
            ModifyKind::Name(notify::event::RenameMode::Both),
        ))
        .add_path(path.clone())))
        .unwrap();
        assert!(rename.requires_rearm());

        let rescan = normalize_native_watch_event(Ok(Event::new(EventKind::Modify(
            ModifyKind::Data(DataChange::Content),
        ))
        .add_path(path)
        .set_flag(Flag::Rescan)))
        .unwrap();
        assert!(rescan.needs_rescan());
    }

    #[test]
    fn raw_ingress_coalesces_identity_and_fences_capacity_loss() {
        let ingress = RawWatchIngress::default();
        for sequence in 1..=WATCH_EVENT_QUEUE_CAPACITY as u64 {
            assert!(ingress.merge_nonblocking(
                Ok(NativeWatchEvent::ordinary(vec![PathBuf::from(
                    "/tmp/config.toml",
                )])),
                WatchWatermark::new(9, sequence),
            ));
        }
        let (events, loss) = ingress.take_nonblocking(9).unwrap();
        assert!(loss.is_none());
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.values().next(),
            Some(&WatchWatermark::new(9, WATCH_EVENT_QUEUE_CAPACITY as u64))
        );

        for ordinal in 1..=WATCH_EVENT_OVERFLOW_CAPACITY {
            assert!(ingress.merge_nonblocking(
                Ok(NativeWatchEvent::ordinary(vec![PathBuf::from(format!(
                    "/tmp/{ordinal}.jsonl"
                ))])),
                WatchWatermark::new(11, ordinal as u64),
            ));
        }
        assert!(!ingress.merge_nonblocking(
            Err(NativeWatchError),
            WatchWatermark::new(11, WATCH_EVENT_OVERFLOW_CAPACITY as u64 + 1),
        ));
        let (events, loss) = ingress.take_nonblocking(11).unwrap();
        assert_eq!(events.len(), WATCH_EVENT_OVERFLOW_CAPACITY);
        assert_eq!(
            loss,
            Some(WatchWatermark::new(
                11,
                WATCH_EVENT_OVERFLOW_CAPACITY as u64 + 1
            ))
        );
    }

    #[test]
    fn full_and_disconnected_callback_channels_fence_synchronously() {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(WatchMessage::DrainIngress).unwrap();
        let ingress = RawWatchIngress::default();
        let accepting = AtomicBool::new(true);
        let sequence = AtomicU64::new(0);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let fence_observed = Arc::clone(&observed);
        let fence: OverflowFence = Arc::new(move |watermark| {
            fence_observed.lock().unwrap().push(watermark);
        });
        let ignore: IgnoreEvent = Arc::new(|_| false);

        forward_native_watch_event(
            &sender,
            &ingress,
            &accepting,
            17,
            &sequence,
            &ignore,
            &fence,
            Ok(NativeWatchEvent::ordinary(vec![PathBuf::from("/tmp/full")])),
        );
        assert_eq!(
            observed.lock().unwrap().as_slice(),
            &[WatchWatermark::new(17, 1)]
        );
        assert!(ingress.take_nonblocking(17).is_some());

        drop(receiver);
        forward_native_watch_event(
            &sender,
            &ingress,
            &accepting,
            17,
            &sequence,
            &ignore,
            &fence,
            Ok(NativeWatchEvent::ordinary(vec![PathBuf::from(
                "/tmp/disconnected",
            )])),
        );
        assert_eq!(
            observed.lock().unwrap().as_slice(),
            &[WatchWatermark::new(17, 1), WatchWatermark::new(17, 2)]
        );
        assert_eq!(ingress.disconnects.load(Ordering::Acquire), 1);
    }

    #[test]
    fn worker_observes_payloads_before_debounce_and_empty_events_extend_activity() {
        let (classified_tx, classified_rx) = mpsc::channel();
        let (observed_tx, observed_rx) = mpsc::channel();
        let (signal_tx, signal_rx) = mpsc::channel();
        let watcher = NativeFileWatcher::start(
            "ctx-watch-activity-test",
            Arc::new(|_| false),
            Arc::new(move |event, watermark| {
                classified_tx.send(watermark).unwrap();
                TestPayload(
                    event
                        .is_ok_and(|event| event.paths != [PathBuf::from("/tmp/empty")])
                        .then_some(watermark),
                )
            }),
            Arc::new(|watermark| TestPayload(Some(watermark))),
            Arc::new(move |payload| observed_tx.send(payload.clone()).unwrap()),
            Arc::new(move |payload| signal_tx.send(payload).unwrap()),
        )
        .unwrap();
        let send = |path| {
            forward_native_watch_event(
                &watcher.sender,
                &watcher.ingress,
                &watcher.accepting_events,
                watcher.watcher_epoch,
                &watcher.callback_sequence,
                &watcher.ignore_event,
                &watcher.overflow_fence,
                Ok(NativeWatchEvent::ordinary(vec![PathBuf::from(path)])),
            )
        };

        send("/tmp/first");
        assert_eq!(
            classified_rx.recv().unwrap(),
            WatchWatermark::new(watcher.watcher_epoch, 1)
        );
        assert_eq!(observed_rx.recv().unwrap().0.unwrap().sequence, 1);
        thread::sleep(Duration::from_millis(100));
        send("/tmp/empty");
        assert_eq!(classified_rx.recv().unwrap().sequence, 2);
        assert!(observed_rx.try_recv().is_err());
        thread::sleep(Duration::from_millis(175));
        assert!(
            signal_rx.try_recv().is_err(),
            "empty activity did not extend debounce"
        );
        send("/tmp/later");
        assert_eq!(classified_rx.recv().unwrap().sequence, 3);
        assert_eq!(observed_rx.recv().unwrap().0.unwrap().sequence, 3);
        assert_eq!(
            signal_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .0
                .unwrap()
                .sequence,
            3
        );
    }

    #[test]
    fn worker_exit_requires_replacement() {
        let watcher = NativeFileWatcher::start(
            "ctx-watch-worker-death-test",
            Arc::new(|_| false),
            Arc::new(|_, _| TestPayload::default()),
            Arc::new(|watermark| TestPayload(Some(watermark))),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        )
        .unwrap();
        watcher.sender.send(WatchMessage::Stop).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !watcher.worker_failed() {
            assert!(Instant::now() < deadline, "worker did not stop");
            thread::yield_now();
        }

        assert!(watcher.replacement_required(false));
    }
}
