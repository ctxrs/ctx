use std::{
    path::Path,
    sync::{Mutex, RwLock},
};

use ctx_daemon_runtime::{NativeWatchEvent, NativeWatchIgnore, NativeWatchResult};

#[cfg(test)]
use super::DaemonWakeup;
use super::{EventWatermark, SourceWatchBatch, WatchAuthority, WatchCounters};

#[cfg(test)]
pub(super) fn record_and_observe_watch_event(
    authority: &RwLock<WatchAuthority>,
    counters: &Mutex<WatchCounters>,
    wakeup: &DaemonWakeup,
    data_root: &Path,
    daemon_root: &Path,
    event: NativeWatchResult,
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
    event: NativeWatchResult,
    watermark: EventWatermark,
) -> SourceWatchBatch {
    let event = match event {
        Ok(event) => event,
        Err(_) => {
            let mut counters = counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counters.backend_errors = counters.backend_errors.saturating_add(1);
            drop(counters);
            return fence_catalog_uncertainty(authority, watermark);
        }
    };
    if event.needs_rescan() {
        let mut counters = counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        counters.raw_events = counters.raw_events.saturating_add(1);
        counters.rescan_notifications = counters.rescan_notifications.saturating_add(1);
        counters.last_relevant_path = event.paths.first().cloned();
        drop(counters);
        return fence_catalog_uncertainty(authority, watermark);
    }
    if let Some(kind) = ignored_watch_event(data_root, &event) {
        record_ignored_watch_event(counters, &event, kind);
        return SourceWatchBatch::default();
    }
    if event.paths.is_empty() {
        record_relevant_watch_event(counters, None);
        return fence_catalog_uncertainty(authority, watermark);
    }
    let authority_snapshot = authority
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let catalog_state = authority_snapshot
        .catalog
        .state
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if catalog_state.uncertain_through.is_some() {
        drop(catalog_state);
        drop(authority_snapshot);
        record_relevant_watch_event(counters, event.paths.first().cloned());
        return fence_catalog_uncertainty(authority, watermark);
    }
    let mut batch = SourceWatchBatch::default();
    let mut relevant_path = None;
    let mut matched_path = false;
    let mut unmatched_path = false;
    let mut data_root_invalidated = false;
    for event_path in &event.paths {
        if event_path.as_path() == data_root {
            data_root_invalidated |= event.requires_rearm();
            continue;
        }
        if event_path.starts_with(daemon_root) {
            unmatched_path |= event.requires_rearm();
            continue;
        }
        let mut path_matched = false;
        let control_matched = authority_snapshot
            .controls
            .iter()
            .any(|target| declared_control_paths_overlap(target, event_path));
        if control_matched {
            batch.reconcile = Some(watermark);
            path_matched = true;
            relevant_path.get_or_insert_with(|| event_path.clone());
        }
        if event_path.starts_with(data_root) && !control_matched {
            unmatched_path |= event.requires_rearm();
            continue;
        }
        if let Some(catalog) = catalog_state.snapshot.as_ref() {
            for route in catalog.routes_overlapping_path(event_path) {
                path_matched = true;
                let member = catalog.exact_member_for_event(&route, event_path);
                batch.record_route(route, watermark, member);
                relevant_path.get_or_insert_with(|| event_path.clone());
            }
        }
        matched_path |= path_matched;
        unmatched_path |= !path_matched;
    }
    let uncertain =
        data_root_invalidated || (event.requires_rearm() && matched_path && unmatched_path);
    drop(catalog_state);
    if uncertain {
        drop(authority_snapshot);
        record_relevant_watch_event(counters, event.paths.first().cloned());
        return fence_catalog_uncertainty(authority, watermark);
    }
    if !batch.is_empty() && event.requires_rearm() {
        batch.rearm = true;
    }
    drop(authority_snapshot);
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

fn record_relevant_watch_event(counters: &Mutex<WatchCounters>, path: Option<std::path::PathBuf>) {
    let mut counters = counters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    counters.raw_events = counters.raw_events.saturating_add(1);
    counters.last_relevant_path = path;
}

fn fence_catalog_uncertainty(
    authority: &RwLock<WatchAuthority>,
    watermark: EventWatermark,
) -> SourceWatchBatch {
    authority
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .catalog
        .fence_uncertainty(watermark);
    SourceWatchBatch::uncertainty(watermark)
}

#[derive(Clone, Copy)]
pub(super) enum IgnoredWatchEvent {
    Access,
    AccessTime,
}

pub(super) fn ignored_watch_event(
    _data_root: &Path,
    event: &NativeWatchEvent,
) -> Option<IgnoredWatchEvent> {
    match event.ignored_kind() {
        Some(NativeWatchIgnore::Access) => Some(IgnoredWatchEvent::Access),
        Some(NativeWatchIgnore::AccessTime) => Some(IgnoredWatchEvent::AccessTime),
        None => None,
    }
}

pub(super) fn record_ignored_watch_event(
    counters: &Mutex<WatchCounters>,
    event: &NativeWatchEvent,
    kind: IgnoredWatchEvent,
) {
    let mut counters = match counters.try_lock() {
        Ok(counters) => counters,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
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
