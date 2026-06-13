pub(super) fn snapshot_timing_enabled() -> bool {
    std::env::var("CTX_SNAPSHOT_TIMING")
        .ok()
        .as_deref()
        .and_then(parse_boolish)
        .unwrap_or(false)
}

pub(super) const WRITE_METRICS_INTERVAL_SECS: u64 = 10;
pub(super) const WRITE_METRICS_TABLE_COUNT: usize = 7;
pub(super) const I64_BYTES: u64 = 8;
pub(super) const BOOL_BYTES: u64 = 1;

#[derive(Clone, Copy, Debug)]
pub(super) enum WriteMetricTable {
    SessionEvents,
    SessionTurns,
    SessionTurnTools,
    Messages,
    SessionHeadMaterializations,
    SessionActiveSnapshotHeads,
    SessionSnapshotSummaries,
}

impl WriteMetricTable {
    const ALL: [WriteMetricTable; WRITE_METRICS_TABLE_COUNT] = [
        WriteMetricTable::SessionEvents,
        WriteMetricTable::SessionTurns,
        WriteMetricTable::SessionTurnTools,
        WriteMetricTable::Messages,
        WriteMetricTable::SessionHeadMaterializations,
        WriteMetricTable::SessionActiveSnapshotHeads,
        WriteMetricTable::SessionSnapshotSummaries,
    ];

    fn index(self) -> usize {
        match self {
            WriteMetricTable::SessionEvents => 0,
            WriteMetricTable::SessionTurns => 1,
            WriteMetricTable::SessionTurnTools => 2,
            WriteMetricTable::Messages => 3,
            WriteMetricTable::SessionHeadMaterializations => 4,
            WriteMetricTable::SessionActiveSnapshotHeads => 5,
            WriteMetricTable::SessionSnapshotSummaries => 6,
        }
    }

    fn is_base(self) -> bool {
        matches!(
            self,
            WriteMetricTable::SessionEvents
                | WriteMetricTable::SessionTurns
                | WriteMetricTable::SessionTurnTools
                | WriteMetricTable::Messages
        )
    }
}

pub(super) struct WriteMetrics {
    bytes: [AtomicU64; WRITE_METRICS_TABLE_COUNT],
    writes: [AtomicU64; WRITE_METRICS_TABLE_COUNT],
}

impl WriteMetrics {
    fn new() -> Self {
        Self {
            bytes: std::array::from_fn(|_| AtomicU64::new(0)),
            writes: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn record(&self, table: WriteMetricTable, rows: u64, bytes: u64) {
        let index = table.index();
        self.bytes[index].fetch_add(bytes, Ordering::Relaxed);
        self.writes[index].fetch_add(rows, Ordering::Relaxed);
    }

    fn snapshot(&self) -> WriteMetricsSnapshot {
        WriteMetricsSnapshot {
            bytes: std::array::from_fn(|i| self.bytes[i].load(Ordering::Relaxed)),
            writes: std::array::from_fn(|i| self.writes[i].load(Ordering::Relaxed)),
        }
    }
}

#[derive(Clone)]
pub(super) struct WriteMetricsSnapshot {
    bytes: [u64; WRITE_METRICS_TABLE_COUNT],
    writes: [u64; WRITE_METRICS_TABLE_COUNT],
}

impl WriteMetricsSnapshot {
    fn delta(&self, previous: &WriteMetricsSnapshot) -> WriteMetricsSnapshot {
        WriteMetricsSnapshot {
            bytes: std::array::from_fn(|i| self.bytes[i].saturating_sub(previous.bytes[i])),
            writes: std::array::from_fn(|i| self.writes[i].saturating_sub(previous.writes[i])),
        }
    }

    fn total_bytes(&self) -> u64 {
        self.bytes.iter().sum()
    }

    fn total_writes(&self) -> u64 {
        self.writes.iter().sum()
    }

    fn base_bytes(&self) -> u64 {
        WriteMetricTable::ALL
            .iter()
            .filter(|table| table.is_base())
            .map(|table| self.bytes[table.index()])
            .sum()
    }
}

pub(super) fn write_metrics_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("CTX_WRITE_METRICS")
            .ok()
            .as_deref()
            .and_then(parse_boolish)
            .unwrap_or(false)
    })
}

pub(super) fn write_metrics() -> Option<&'static WriteMetrics> {
    if !write_metrics_enabled() {
        return None;
    }
    static METRICS: OnceLock<WriteMetrics> = OnceLock::new();
    static LOGGER: OnceLock<()> = OnceLock::new();
    let metrics = METRICS.get_or_init(WriteMetrics::new);
    LOGGER.get_or_init(|| {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut interval =
                    tokio::time::interval(Duration::from_secs(WRITE_METRICS_INTERVAL_SECS));
                let mut previous = metrics.snapshot();
                loop {
                    interval.tick().await;
                    let current = metrics.snapshot();
                    let delta = current.delta(&previous);
                    previous = current;
                    let total_bytes = delta.total_bytes();
                    let total_writes = delta.total_writes();
                    if total_bytes == 0 && total_writes == 0 {
                        continue;
                    }
                    let base_bytes = delta.base_bytes();
                    let derived_bytes = total_bytes.saturating_sub(base_bytes);
                    let write_amplification =
                        (base_bytes > 0).then(|| total_bytes as f64 / base_bytes as f64);

                    info!(
                        target: "ctx_store.write_metrics",
                        interval_s = WRITE_METRICS_INTERVAL_SECS,
                        total_writes,
                        total_bytes,
                        base_bytes,
                        derived_bytes,
                        write_amplification,
                        session_events_writes =
                            delta.writes[WriteMetricTable::SessionEvents.index()],
                        session_events_bytes =
                            delta.bytes[WriteMetricTable::SessionEvents.index()],
                        session_turns_writes = delta.writes[WriteMetricTable::SessionTurns.index()],
                        session_turns_bytes = delta.bytes[WriteMetricTable::SessionTurns.index()],
                        session_turn_tools_writes =
                            delta.writes[WriteMetricTable::SessionTurnTools.index()],
                        session_turn_tools_bytes =
                            delta.bytes[WriteMetricTable::SessionTurnTools.index()],
                        messages_writes = delta.writes[WriteMetricTable::Messages.index()],
                        messages_bytes = delta.bytes[WriteMetricTable::Messages.index()],
                        session_head_materializations_writes = delta.writes
                            [WriteMetricTable::SessionHeadMaterializations.index()],
                        session_head_materializations_bytes = delta.bytes
                            [WriteMetricTable::SessionHeadMaterializations.index()],
                        session_active_snapshot_heads_writes = delta.writes
                            [WriteMetricTable::SessionActiveSnapshotHeads.index()],
                        session_active_snapshot_heads_bytes = delta.bytes
                            [WriteMetricTable::SessionActiveSnapshotHeads.index()],
                        session_snapshot_summaries_writes = delta.writes
                            [WriteMetricTable::SessionSnapshotSummaries.index()],
                        session_snapshot_summaries_bytes = delta.bytes
                            [WriteMetricTable::SessionSnapshotSummaries.index()],
                    );
                }
            });
        } else {
            info!(
                target: "ctx_store.write_metrics",
                "CTX_WRITE_METRICS enabled without a tokio runtime; write metrics logging disabled",
            );
        }
    });
    Some(metrics)
}

pub(super) fn record_write(table: WriteMetricTable, rows: u64, bytes_per_row: u64) {
    if rows == 0 {
        return;
    }
    if let Some(metrics) = write_metrics() {
        let bytes = bytes_per_row.saturating_mul(rows);
        metrics.record(table, rows, bytes);
    }
}

pub(super) fn bytes_str(value: &str) -> u64 {
    value.len() as u64
}

pub(super) fn bytes_opt_str(value: Option<&str>) -> u64 {
    value.map(bytes_str).unwrap_or(0)
}

pub(super) fn bytes_opt_i64(value: Option<i64>) -> u64 {
    value.map(|_| I64_BYTES).unwrap_or(0)
}

pub(super) fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

pub(super) fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
}

pub(super) fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .as_deref()
        .and_then(parse_boolish)
        .unwrap_or(false)
}

pub(super) fn disable_tool_summary_persistence() -> bool {
    env_flag_enabled("CTX_DISABLE_TOOL_SUMMARY_PERSISTENCE")
}
