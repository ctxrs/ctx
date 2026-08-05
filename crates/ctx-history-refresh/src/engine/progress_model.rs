use super::*;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct SourceBackedRefreshTimings {
    pub discovery_us: u64,
    pub scan_stage_us: u64,
    pub commit_us: u64,
}

impl SourceBackedRefreshTimings {
    pub(crate) fn to_json(self) -> Value {
        json!({
            "discovery": self.discovery_us,
            "scan_stage": self.scan_stage_us,
            "commit": self.commit_us,
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceBackedRefreshState {
    AdmissionPending,
    Queued,
    Running,
    Published,
    Failed,
}

impl SourceBackedRefreshState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionPending => "admission_pending",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Published => "published",
            Self::Failed => "failed",
        }
    }

    pub(super) fn is_active(self) -> bool {
        matches!(self, Self::AdmissionPending | Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SourceBackedCurrentSourceProgressStage {
    SourceFamilyCopy,
    OnlineBackup,
    LogicalFingerprint,
    LogicalScan,
}

impl SourceBackedCurrentSourceProgressStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceFamilyCopy => "source_family_copy",
            Self::OnlineBackup => "online_backup",
            Self::LogicalFingerprint => "logical_fingerprint",
            Self::LogicalScan => "logical_scan",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "source_family_copy" => Some(Self::SourceFamilyCopy),
            "online_backup" => Some(Self::OnlineBackup),
            "logical_fingerprint" => Some(Self::LogicalFingerprint),
            "logical_scan" => Some(Self::LogicalScan),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SourceBackedCurrentSourceProgress {
    pub stage: SourceBackedCurrentSourceProgressStage,
    pub snapshot_pages_completed: Option<u64>,
    pub snapshot_pages_total: Option<u64>,
    pub snapshot_bytes_completed: Option<u64>,
    pub snapshot_bytes_total: Option<u64>,
    pub logical_rows_scanned: Option<u64>,
    pub logical_certified_bytes: Option<u64>,
}

impl SourceBackedCurrentSourceProgress {
    pub fn to_json(self) -> Value {
        compact_json(json!({
            "stage": self.stage.as_str(),
            "snapshot_pages_completed": self.snapshot_pages_completed,
            "snapshot_pages_total": self.snapshot_pages_total,
            "snapshot_bytes_completed": self.snapshot_bytes_completed,
            "snapshot_bytes_total": self.snapshot_bytes_total,
            "logical_rows_scanned": self.logical_rows_scanned,
            "logical_certified_bytes": self.logical_certified_bytes,
        }))
    }

    fn from_json(value: &Value) -> Result<Self> {
        let fields = value.as_object().ok_or_else(|| {
            anyhow!("daemon source refresh current-source progress is not an object")
        })?;
        let stage = fields
            .get("stage")
            .and_then(Value::as_str)
            .and_then(SourceBackedCurrentSourceProgressStage::parse)
            .ok_or_else(|| {
                anyhow!("daemon source refresh current-source progress has an invalid stage")
            })?;
        Ok(Self {
            stage,
            snapshot_pages_completed: optional_progress_u64(fields, "snapshot_pages_completed")?,
            snapshot_pages_total: optional_progress_u64(fields, "snapshot_pages_total")?,
            snapshot_bytes_completed: optional_progress_u64(fields, "snapshot_bytes_completed")?,
            snapshot_bytes_total: optional_progress_u64(fields, "snapshot_bytes_total")?,
            logical_rows_scanned: optional_progress_u64(fields, "logical_rows_scanned")?,
            logical_certified_bytes: optional_progress_u64(fields, "logical_certified_bytes")?,
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedRefreshProgress {
    pub phase: String,
    pub completed_sources: usize,
    pub total_sources: usize,
    pub current_source: Option<String>,
    pub completed_records: Option<u64>,
    pub completed_bytes: Option<u64>,
    pub current_source_progress: Option<SourceBackedCurrentSourceProgress>,
}

impl Default for SourceBackedRefreshProgress {
    fn default() -> Self {
        Self {
            phase: "queued".to_owned(),
            completed_sources: 0,
            total_sources: 0,
            current_source: None,
            completed_records: None,
            completed_bytes: None,
            current_source_progress: None,
        }
    }
}

impl SourceBackedRefreshProgress {
    pub(super) fn to_json_with_total_known(&self, total_sources_known: bool) -> Value {
        compact_json(json!({
            "phase": self.phase,
            "completed_sources": self.completed_sources,
            "total_sources": total_sources_known.then_some(self.total_sources),
            "current_source": self.current_source,
            "completed_records": self.completed_records,
            "completed_bytes": self.completed_bytes,
            "current_source_progress": self.current_source_progress
                .map(SourceBackedCurrentSourceProgress::to_json),
        }))
    }

    pub fn from_status_json(response: &Value) -> Result<Self> {
        let progress = response
            .get("progress")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("daemon source refresh status has no progress object"))?;
        let phase = progress
            .get("phase")
            .and_then(Value::as_str)
            .filter(|phase| !phase.is_empty())
            .ok_or_else(|| anyhow!("daemon source refresh progress has an invalid phase"))?
            .to_owned();
        let current_source = match progress.get("current_source") {
            None | Some(Value::Null) => None,
            Some(Value::String(source)) => Some(source.clone()),
            Some(_) => bail!("daemon source refresh progress has an invalid current_source"),
        };
        let current_source_progress = match progress.get("current_source_progress") {
            None | Some(Value::Null) => None,
            Some(value) => Some(SourceBackedCurrentSourceProgress::from_json(value)?),
        };
        Ok(Self {
            phase,
            completed_sources: required_progress_usize(progress, "completed_sources")?,
            // Legacy callers expose unknown totals as zero in this typed
            // compatibility view. New wire responses omit the total until it
            // is known, while old responses with an explicit zero continue to
            // parse unchanged.
            total_sources: optional_progress_usize(progress, "total_sources")?.unwrap_or_default(),
            current_source,
            completed_records: optional_progress_u64(progress, "completed_records")?,
            completed_bytes: optional_progress_u64(progress, "completed_bytes")?,
            current_source_progress,
        })
    }
}

pub(super) fn status_progress_total_sources_known(response: &Value) -> bool {
    response
        .get("progress")
        .and_then(|progress| progress.get("total_sources"))
        .is_some_and(Value::is_number)
}

fn required_progress_usize(fields: &serde_json::Map<String, Value>, field: &str) -> Result<usize> {
    fields
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| anyhow!("daemon source refresh progress has an invalid {field}"))
}

fn optional_progress_u64(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<u64>> {
    match fields.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            anyhow!("daemon source refresh current-source progress has an invalid {field}")
        }),
    }
}

fn optional_progress_usize(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<usize>> {
    optional_progress_u64(fields, field)?
        .map(|value| {
            usize::try_from(value)
                .map_err(|_| anyhow!("daemon source refresh progress has an invalid {field}"))
        })
        .transpose()
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn progress_parser_accepts_legacy_zero_and_additive_unknown_total_shapes() {
        let legacy = json!({
            "progress": {
                "phase": "queued",
                "completed_sources": 0,
                "total_sources": 0,
            }
        });
        let unknown = json!({
            "progress": {
                "phase": "queued",
                "completed_sources": 0,
            }
        });

        assert_eq!(
            SourceBackedRefreshProgress::from_status_json(&legacy)
                .unwrap()
                .total_sources,
            0
        );
        assert_eq!(
            SourceBackedRefreshProgress::from_status_json(&unknown)
                .unwrap()
                .total_sources,
            0
        );
        assert!(status_progress_total_sources_known(&legacy));
        assert!(!status_progress_total_sources_known(&unknown));
    }
}
