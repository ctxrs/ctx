use super::*;

/// Content-free work and concurrency counters for one retained SQLite
/// directory authority and all snapshots opened through its clones.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SqliteSourceSnapshotCounters {
    immutable_snapshot_opens: u64,
    pinned_read_only_wal_snapshot_opens: u64,
    copied_snapshot_opens: u64,
    source_bytes_copied: u64,
    terminal_fences: u64,
    terminal_revalidations: u64,
    explicit_aborts: u64,
    unfinished_drops: u64,
    scratch_admissions: u64,
    max_route_scratch_bytes: u64,
    active_snapshots: u64,
    active_snapshot_bytes: u64,
    max_active_snapshots: u64,
    max_active_snapshot_bytes: u64,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct SqliteSourceSnapshotCounterObserver {
    pub(super) context: Arc<SqliteSourceSnapshotContext>,
}

#[cfg(any(test, feature = "test-support"))]
impl SqliteSourceSnapshotCounterObserver {
    pub fn snapshot(&self) -> SqliteSourceSnapshotCounters {
        self.context.snapshot()
    }
}

impl SqliteSourceSnapshotCounters {
    pub const fn immutable_snapshot_opens(self) -> u64 {
        self.immutable_snapshot_opens
    }

    pub const fn copied_snapshot_opens(self) -> u64 {
        self.copied_snapshot_opens
    }

    pub const fn pinned_read_only_wal_snapshot_opens(self) -> u64 {
        self.pinned_read_only_wal_snapshot_opens
    }

    pub const fn source_bytes_copied(self) -> u64 {
        self.source_bytes_copied
    }

    pub const fn terminal_fences(self) -> u64 {
        self.terminal_fences
    }

    pub const fn terminal_revalidations(self) -> u64 {
        self.terminal_revalidations
    }

    pub const fn explicit_aborts(self) -> u64 {
        self.explicit_aborts
    }

    pub const fn unfinished_drops(self) -> u64 {
        self.unfinished_drops
    }

    pub const fn scratch_admissions(self) -> u64 {
        self.scratch_admissions
    }

    pub const fn max_route_scratch_bytes(self) -> u64 {
        self.max_route_scratch_bytes
    }

    pub const fn active_snapshots(self) -> u64 {
        self.active_snapshots
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn active_snapshot_bytes(self) -> u64 {
        self.active_snapshot_bytes
    }

    pub const fn max_active_snapshots(self) -> u64 {
        self.max_active_snapshots
    }

    #[cfg(any(test, feature = "test-support"))]
    pub const fn max_active_snapshot_bytes(self) -> u64 {
        self.max_active_snapshot_bytes
    }
}

#[derive(Debug)]
pub(super) struct SqliteSourceSnapshotContext {
    pub(super) data_root: PathBuf,
    pub(super) counters: Mutex<SqliteSourceSnapshotCounters>,
}

impl SqliteSourceSnapshotContext {
    pub(super) fn snapshot(&self) -> SqliteSourceSnapshotCounters {
        *self.lock()
    }

    pub(super) fn record_source_bytes_copied(&self, bytes: u64) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.source_bytes_copied =
            checked_counter_add(counters.source_bytes_copied, bytes, "source bytes copied")?;
        Ok(())
    }

    pub(super) fn record_open(
        self: &Arc<Self>,
        strategy: SqliteSourceSnapshotStrategy,
        active_bytes: u64,
    ) -> SqliteSourceAccessResult<SqliteSourceSnapshotActivity> {
        let mut counters = self.lock();
        let mut next = *counters;
        match strategy {
            #[cfg(target_os = "linux")]
            SqliteSourceSnapshotStrategy::ImmutableMain => {
                next.immutable_snapshot_opens = checked_counter_add(
                    next.immutable_snapshot_opens,
                    1,
                    "immutable snapshot opens",
                )?;
            }
            SqliteSourceSnapshotStrategy::CopiedFamily
            | SqliteSourceSnapshotStrategy::SelectiveTables => {
                next.copied_snapshot_opens =
                    checked_counter_add(next.copied_snapshot_opens, 1, "copied snapshot opens")?;
            }
            #[cfg(target_os = "linux")]
            SqliteSourceSnapshotStrategy::PinnedReadOnlyWal => {
                next.pinned_read_only_wal_snapshot_opens = checked_counter_add(
                    next.pinned_read_only_wal_snapshot_opens,
                    1,
                    "direct read-only snapshot opens",
                )?;
            }
        }
        next.active_snapshots = checked_counter_add(next.active_snapshots, 1, "active snapshots")?;
        next.active_snapshot_bytes = checked_counter_add(
            next.active_snapshot_bytes,
            active_bytes,
            "active snapshot bytes",
        )?;
        next.max_active_snapshots = next.max_active_snapshots.max(next.active_snapshots);
        next.max_active_snapshot_bytes = next
            .max_active_snapshot_bytes
            .max(next.active_snapshot_bytes);
        *counters = next;
        drop(counters);
        Ok(SqliteSourceSnapshotActivity {
            context: Arc::clone(self),
            active_bytes,
        })
    }

    pub(super) fn record_terminal_fence(&self) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.terminal_fences =
            checked_counter_add(counters.terminal_fences, 1, "terminal fences")?;
        Ok(())
    }

    pub(super) fn record_terminal_revalidation(&self) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.terminal_revalidations =
            checked_counter_add(counters.terminal_revalidations, 1, "terminal revalidations")?;
        Ok(())
    }

    pub(super) fn record_explicit_abort(&self) {
        let mut counters = self.lock();
        counters.explicit_aborts = counters.explicit_aborts.saturating_add(1);
    }

    pub(super) fn record_unfinished_drop(&self) {
        let mut counters = self.lock();
        counters.unfinished_drops = counters.unfinished_drops.saturating_add(1);
    }

    pub(super) fn record_scratch_admission(&self) -> SqliteSourceAccessResult<()> {
        let mut counters = self.lock();
        counters.scratch_admissions =
            checked_counter_add(counters.scratch_admissions, 1, "scratch admissions")?;
        Ok(())
    }

    pub(super) fn record_route_scratch_peak(&self, bytes: u64) {
        let mut counters = self.lock();
        counters.max_route_scratch_bytes = counters.max_route_scratch_bytes.max(bytes);
    }

    fn lock(&self) -> MutexGuard<'_, SqliteSourceSnapshotCounters> {
        match self.counters.lock() {
            Ok(counters) => counters,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[derive(Debug)]
pub(super) struct SqliteSourceSnapshotActivity {
    context: Arc<SqliteSourceSnapshotContext>,
    active_bytes: u64,
}

impl Drop for SqliteSourceSnapshotActivity {
    fn drop(&mut self) {
        let mut counters = self.context.lock();
        counters.active_snapshots = counters.active_snapshots.saturating_sub(1);
        counters.active_snapshot_bytes = counters
            .active_snapshot_bytes
            .saturating_sub(self.active_bytes);
    }
}

#[derive(Debug, Default)]
struct SqliteRouteScratchState {
    retained_bytes: u64,
    reserved_transient_bytes: u64,
    exact_peak_bytes: u64,
}

/// One physical scratch ledger for every retained and transient file created
/// by a single SQLite route.
#[derive(Debug)]
pub(super) struct SqliteRouteScratch {
    context: Arc<SqliteSourceSnapshotContext>,
    pub(super) maximum_bytes: Option<u64>,
    state: Mutex<SqliteRouteScratchState>,
}

impl SqliteRouteScratch {
    pub(super) fn new(
        context: &Arc<SqliteSourceSnapshotContext>,
        maximum_bytes: Option<u64>,
    ) -> Arc<Self> {
        Arc::new(Self {
            context: Arc::clone(context),
            maximum_bytes,
            state: Mutex::new(SqliteRouteScratchState::default()),
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn admit_available_capacity(&self) -> SqliteSourceAccessResult<u64> {
        let available = scratch_available_space(&self.context.data_root)?;
        let headroom = sqlite_snapshot_free_headroom_bytes(available);
        if available <= headroom {
            return Err(SqliteSourceAccessError::InsufficientScratchSpace {
                path: self.context.data_root.clone(),
                required: headroom + 4096,
                available,
            });
        }
        let capacity = self
            .maximum_bytes
            .unwrap_or(u64::MAX)
            .min(available - headroom);
        if capacity < 4096 {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: 4096,
                maximum: capacity,
            });
        }
        self.context.record_scratch_admission()?;
        Ok(capacity)
    }

    pub(super) fn admit_capacity(&self, capacity_bytes: u64) -> SqliteSourceAccessResult<()> {
        if let Some(maximum) = self.maximum_bytes {
            if capacity_bytes > maximum {
                return Err(SqliteSourceAccessError::SnapshotTooLarge {
                    path: self.context.data_root.clone(),
                    length: capacity_bytes,
                    maximum,
                });
            }
        }
        let required = capacity_bytes
            .checked_add(sqlite_snapshot_free_headroom_bytes(capacity_bytes))
            .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
                reason: "SQLite snapshot capacity admission overflowed".to_owned(),
            })?;
        let available = scratch_available_space(&self.context.data_root)?;
        if available < required {
            return Err(SqliteSourceAccessError::InsufficientScratchSpace {
                path: self.context.data_root.clone(),
                required,
                available,
            });
        }
        self.context.record_scratch_admission()
    }

    pub(super) fn set_retained_bytes(&self, retained_bytes: u64) -> SqliteSourceAccessResult<()> {
        let mut state = self.lock();
        let aggregate = retained_bytes
            .checked_add(state.reserved_transient_bytes)
            .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
                reason: "SQLite retained scratch accounting overflowed".to_owned(),
            })?;
        if let Some(maximum) = self.maximum_bytes {
            if aggregate > maximum {
                return Err(SqliteSourceAccessError::SnapshotTooLarge {
                    path: self.context.data_root.clone(),
                    length: aggregate,
                    maximum,
                });
            }
        }
        state.retained_bytes = retained_bytes;
        state.exact_peak_bytes = state.exact_peak_bytes.max(retained_bytes);
        self.context
            .record_route_scratch_peak(state.exact_peak_bytes);
        Ok(())
    }

    pub(super) fn reserve_transient(
        self: &Arc<Self>,
        requested_bytes: u64,
    ) -> SqliteSourceAccessResult<SqliteRouteScratchReservation> {
        let state = self.lock();
        let used = state
            .retained_bytes
            .checked_add(state.reserved_transient_bytes)
            .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
                reason: "SQLite transient scratch accounting overflowed".to_owned(),
            })?;
        let available_capacity = self
            .maximum_bytes
            .map_or(requested_bytes, |maximum| maximum.saturating_sub(used));
        let reserved_bytes = requested_bytes.min(available_capacity);
        if reserved_bytes == 0 {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: requested_bytes,
                maximum: available_capacity,
            });
        }
        drop(state);
        self.admit_capacity(reserved_bytes)?;
        let mut state = self.lock();
        let reserved_transient_bytes =
            state
                .reserved_transient_bytes
                .checked_add(reserved_bytes)
                .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "SQLite transient scratch reservation overflowed".to_owned(),
                })?;
        let aggregate = state
            .retained_bytes
            .checked_add(reserved_transient_bytes)
            .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
                reason: "SQLite aggregate scratch reservation overflowed".to_owned(),
            })?;
        if let Some(maximum) = self.maximum_bytes {
            if aggregate > maximum {
                return Err(SqliteSourceAccessError::SnapshotTooLarge {
                    path: self.context.data_root.clone(),
                    length: aggregate,
                    maximum,
                });
            }
        }
        state.reserved_transient_bytes = reserved_transient_bytes;
        Ok(SqliteRouteScratchReservation {
            account: Arc::clone(self),
            reserved_bytes,
        })
    }

    pub(super) fn record_transient_bytes(&self, bytes: u64) -> SqliteSourceAccessResult<()> {
        let mut state = self.lock();
        if bytes > state.reserved_transient_bytes {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: self.context.data_root.clone(),
                length: bytes,
                maximum: state.reserved_transient_bytes,
            });
        }
        let aggregate = state.retained_bytes.checked_add(bytes).ok_or_else(|| {
            SqliteSourceAccessError::SnapshotUnavailable {
                reason: "SQLite exact scratch accounting overflowed".to_owned(),
            }
        })?;
        state.exact_peak_bytes = state.exact_peak_bytes.max(aggregate);
        self.context
            .record_route_scratch_peak(state.exact_peak_bytes);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, SqliteRouteScratchState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[derive(Debug)]
pub(super) struct SqliteRouteScratchReservation {
    account: Arc<SqliteRouteScratch>,
    reserved_bytes: u64,
}

impl SqliteRouteScratchReservation {
    pub(super) fn maximum_bytes(&self) -> u64 {
        self.reserved_bytes
    }

    pub(super) fn record_exact_bytes(&self, bytes: u64) -> SqliteSourceAccessResult<()> {
        self.account.record_transient_bytes(bytes)
    }
}

impl Drop for SqliteRouteScratchReservation {
    fn drop(&mut self) {
        let mut state = self.account.lock();
        state.reserved_transient_bytes = state
            .reserved_transient_bytes
            .saturating_sub(self.reserved_bytes);
    }
}

fn scratch_available_space(path: &Path) -> SqliteSourceAccessResult<u64> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(available) = take_scratch_available_space_override() {
        return Ok(available);
    }
    let mut measurement_path = path;
    loop {
        match std::fs::metadata(measurement_path) {
            Ok(_) => break,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                measurement_path = measurement_path.parent().ok_or_else(|| {
                    SqliteSourceAccessError::ScratchIoUnavailable {
                        operation: "locating an existing SQLite scratch filesystem ancestor",
                        path: path.to_path_buf(),
                        source,
                    }
                })?;
            }
            Err(source) => {
                return Err(SqliteSourceAccessError::ScratchIoUnavailable {
                    operation: "locating an existing SQLite scratch filesystem ancestor",
                    path: measurement_path.to_path_buf(),
                    source,
                });
            }
        }
    }
    fs2::available_space(measurement_path).map_err(|source| {
        SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "measuring available provider SQLite scratch space",
            path: measurement_path.to_path_buf(),
            source,
        }
    })
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SCRATCH_AVAILABLE_SPACE_OVERRIDE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
fn take_scratch_available_space_override() -> Option<u64> {
    SCRATCH_AVAILABLE_SPACE_OVERRIDE.with(|available| available.take())
}

#[cfg(any(test, feature = "test-support"))]
pub fn override_next_scratch_available_space_for_test(available: u64) {
    SCRATCH_AVAILABLE_SPACE_OVERRIDE.with(|slot| slot.set(Some(available)));
}

fn checked_counter_add(
    value: u64,
    increment: u64,
    counter: &'static str,
) -> SqliteSourceAccessResult<u64> {
    value
        .checked_add(increment)
        .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
            reason: format!("SQLite snapshot accounting overflowed {counter}"),
        })
}
