use super::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{anyhow, Result};
use tokio::sync::{mpsc as tokio_mpsc, Notify};

pub(super) struct WorkspaceStoreLeaseRegistry {
    state: StdMutex<WorkspaceStoreLeaseState>,
    close_executor: StoreCloseExecutor,
}

#[derive(Default)]
struct WorkspaceStoreLeaseState {
    entries: HashMap<u64, WorkspaceStoreLeaseEntry>,
    closing_workspaces: HashMap<WorkspaceId, Arc<WorkspaceCloseSignal>>,
}

struct WorkspaceStoreLeaseEntry {
    workspace_id: WorkspaceId,
    in_use: usize,
    pending_close: Option<Store>,
}

struct WorkspaceStoreLease {
    instance_id: u64,
    registry: Arc<WorkspaceStoreLeaseRegistry>,
}

pub(super) struct PendingWorkspaceStoreClose {
    pub(super) workspace_id: WorkspaceId,
    pub(super) store: Store,
    pub(super) notify: Arc<WorkspaceCloseSignal>,
}

pub(super) struct ReactivatedWorkspaceStore {
    pub(super) store: Store,
    pub(super) instance_id: u64,
    pub(super) notify: Arc<WorkspaceCloseSignal>,
}

struct StoreCloseJob {
    store: Store,
    registry: Arc<WorkspaceStoreLeaseRegistry>,
    workspace_id: WorkspaceId,
    notify: Arc<WorkspaceCloseSignal>,
}

struct StoreCloseExecutor {
    tx: tokio_mpsc::UnboundedSender<StoreCloseJob>,
}

pub(super) struct WorkspaceCloseSignal {
    finished: AtomicBool,
    notify: Notify,
}

include!("close_signal.rs");
include!("executor.rs");
include!("registry.rs");
include!("close_helpers.rs");
#[cfg(test)]
include!("tests_mod.rs");
