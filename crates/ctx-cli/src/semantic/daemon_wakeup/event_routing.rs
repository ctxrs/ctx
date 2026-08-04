use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, RwLock},
};

use notify::{
    event::{AccessKind, AccessMode, CreateKind, MetadataKind, ModifyKind, RemoveKind},
    Event, EventKind,
};

use super::{DaemonWakeup, EventWatermark, SourceWatchBatch, WatchAuthority, WatchCounters};

pub(super) fn record_and_observe_watch_event(
    authority: &RwLock<WatchAuthority>,
    counters: &Mutex<WatchCounters>,
    wakeup: &DaemonWakeup,
    data_root: &Path,
    daemon_root: &Path,
    event: notify::Result<Event>,
    watermark: EventWatermark,
) -> SourceWatchBatch {
    let batch = record_watch_event(
        authority,
        counters,
        data_root,
        daemon_root,
        event,
        watermark,
    );
    wakeup.observe_source_watch(&batch);
    batch
}

pub(super) fn record_watch_event(
    authority: &RwLock<WatchAuthority>,
    counters: &Mutex<WatchCounters>,
    data_root: &Path,
    daemon_root: &Path,
    event: notify::Result<Event>,
    watermark: EventWatermark,
) -> SourceWatchBatch {
    let event = match event {
        Ok(event) => event,
        Err(_) => {
            let mut counters = counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counters.backend_errors = counters.backend_errors.saturating_add(1);
            return SourceWatchBatch::catalog_reconciliation(watermark);
        }
    };
    if event.need_rescan() {
        let mut counters = counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        counters.raw_events = counters.raw_events.saturating_add(1);
        counters.rescan_notifications = counters.rescan_notifications.saturating_add(1);
        counters.last_relevant_path = event.paths.first().cloned();
        return SourceWatchBatch::catalog_reconciliation(watermark);
    }
    if let Some(kind) = ignored_watch_event(data_root, &event) {
        record_ignored_watch_event(counters, &event, kind);
        return SourceWatchBatch::default();
    }
    let authority = authority
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let catalog = authority.catalog.snapshot();
    let mut batch = SourceWatchBatch::default();
    let mut relevant_path = None;
    for event_path in &event.paths {
        if event_path.as_path() == data_root || event_path.starts_with(daemon_root) {
            continue;
        }
        if authority
            .controls
            .iter()
            .any(|target| declared_control_paths_overlap(target, event_path))
        {
            batch.reconcile = Some(watermark);
            relevant_path.get_or_insert_with(|| event_path.clone());
        }
        if let Some(catalog) = catalog.as_ref() {
            for route in catalog.routes_overlapping_path(event_path) {
                batch.routes.insert(route, watermark);
                relevant_path.get_or_insert_with(|| event_path.clone());
            }
        }
    }
    if event.paths.is_empty() {
        batch.reconcile = Some(watermark);
        batch.rearm = true;
    } else if !batch.is_empty() && watch_event_requires_rearm(&event) {
        batch.rearm = true;
    }
    drop(authority);
    if !batch.is_empty() {
        let mut counters = counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        counters.raw_events = counters.raw_events.saturating_add(1);
        counters.last_relevant_path = relevant_path;
    } else {
        let mut counters = counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        counters.ignored_other_events = counters.ignored_other_events.saturating_add(1);
    }
    batch
}

fn watch_event_requires_rearm(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Any
            | EventKind::Other
            | EventKind::Create(CreateKind::Any | CreateKind::Folder | CreateKind::Other)
            | EventKind::Modify(ModifyKind::Any | ModifyKind::Name(_) | ModifyKind::Other)
            | EventKind::Remove(RemoveKind::Any | RemoveKind::Folder | RemoveKind::Other)
    )
}

#[derive(Clone, Copy)]
pub(super) enum IgnoredWatchEvent {
    Access,
    AccessTime,
}

pub(super) fn ignored_watch_event(_data_root: &Path, event: &Event) -> Option<IgnoredWatchEvent> {
    if matches!(
        event.kind,
        EventKind::Access(kind)
            if !matches!(kind, AccessKind::Close(AccessMode::Write))
    ) {
        return Some(IgnoredWatchEvent::Access);
    }
    if matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime))
    ) {
        return Some(IgnoredWatchEvent::AccessTime);
    }
    None
}

pub(super) fn record_ignored_watch_event(
    counters: &Mutex<WatchCounters>,
    event: &Event,
    kind: IgnoredWatchEvent,
) {
    let mut counters = counters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match kind {
        IgnoredWatchEvent::Access => {
            counters.ignored_access_events = counters.ignored_access_events.saturating_add(1);
            counters.last_ignored_access_path = event.paths.first().cloned();
        }
        IgnoredWatchEvent::AccessTime => {
            counters.ignored_access_time_events =
                counters.ignored_access_time_events.saturating_add(1);
        }
    }
}

fn declared_control_paths_overlap(target: &Path, event: &Path) -> bool {
    target == event || target.starts_with(event) || event.starts_with(target)
}

pub(super) fn watch_roots<'a>(
    targets: impl IntoIterator<Item = &'a Path>,
) -> BTreeMap<PathBuf, bool> {
    let mut roots = BTreeMap::new();
    for target in targets {
        if target.is_dir() {
            roots
                .entry(target.to_path_buf())
                .and_modify(|recursive| *recursive = true)
                .or_insert(true);
            continue;
        }
        if target.is_file() {
            if let Some(parent) = target.parent() {
                roots.entry(parent.to_path_buf()).or_insert(false);
            }
            continue;
        }
        if let Some(existing) = nearest_existing_ancestor(target) {
            roots.entry(existing).or_insert(false);
        }
    }
    roots
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|candidate| candidate.is_dir())
        .map(Path::to_path_buf)
}
