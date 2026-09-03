use super::*;

/// Setup-visible work remaining before a verified, durable Core generation is
/// usable. These stages do not imply one shared numerator or denominator.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum SourceBackedRefreshStage {
    #[default]
    Preparing,
    Reading,
    Merging,
    Syncing,
    PhysicalVerification,
    LogicalVerification,
    Activation,
    Complete,
    Failed,
}

impl SourceBackedRefreshStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Reading => "reading",
            Self::Merging => "merging",
            Self::Syncing => "syncing",
            Self::PhysicalVerification => "physical_verification",
            Self::LogicalVerification => "logical_verification",
            Self::Activation => "activation",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }

    pub(super) fn from_phase(phase: &str) -> Self {
        match phase {
            "parsing" | "reading" | "refreshing" | "verifying" => Self::Reading,
            "committing" | "merging" => Self::Merging,
            "syncing" => Self::Syncing,
            "physical_verification" => Self::PhysicalVerification,
            "logical_verification" => Self::LogicalVerification,
            "activation" | "committed" | "persisting_terminal" => Self::Activation,
            "complete" | "published" => Self::Complete,
            "failed" => Self::Failed,
            _ => Self::Preparing,
        }
    }
}

pub(super) type SourceBackedRefreshState = RefreshRequestState;
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedRefreshProgress {
    pub phase: String,
    pub completed_sources: usize,
    pub total_sources: usize,
    pub current_source: Option<String>,
    pub completed_records: Option<u64>,
    pub completed_bytes: Option<u64>,
    pub providers: Vec<String>,
    pub processed_sessions: u64,
    pub processed_messages: u64,
    pub processed_tool_calls: u64,
    pub processed_bytes: u64,
    pub elapsed_millis: Option<u64>,
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
            providers: Vec::new(),
            processed_sessions: 0,
            processed_messages: 0,
            processed_tool_calls: 0,
            processed_bytes: 0,
            elapsed_millis: None,
            current_source_progress: None,
        }
    }
}

impl SourceBackedRefreshProgress {
    /// Typed setup stage for the whole path to a usable Core generation.
    pub fn whole_run_stage(&self) -> SourceBackedRefreshStage {
        SourceBackedRefreshStage::from_phase(&self.phase)
    }

    /// Time until the generation is usable, never time until source reading
    /// alone finishes.
    ///
    /// This remains `None` until direct remaining-work counters exist for all
    /// later stages or a per-stage model is trained and qualified across CPU,
    /// storage contention, and corpus shapes.
    pub(super) fn to_json_with_total_known(
        &self,
        total_sources_known: bool,
        estimated_remaining_millis: Option<u64>,
    ) -> Value {
        let mut value = compact_json(json!({
            "phase": self.phase,
            "whole_run_stage": self.whole_run_stage().as_str(),
            "completed_sources": self.completed_sources,
            "total_sources": self.total_sources,
            "total_sources_known": total_sources_known,
            "current_source": self.current_source,
            "completed_records": self.completed_records,
            "completed_bytes": self.completed_bytes,
            "providers": self.providers,
            "processed_sessions": self.processed_sessions,
            "processed_messages": self.processed_messages,
            "processed_tool_calls": self.processed_tool_calls,
            "processed_bytes": self.processed_bytes,
            "elapsed_millis": self.elapsed_millis,
            "current_source_progress": self.current_source_progress
                .map(SourceBackedCurrentSourceProgress::to_json),
        }));
        // Preserve an explicit unknown whole-run estimate. Consumers must not
        // substitute source-stage progress for this null.
        value["estimated_remaining_millis"] = json!(estimated_remaining_millis);
        value
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
            total_sources: required_progress_usize(progress, "total_sources")?,
            current_source,
            completed_records: optional_progress_u64(progress, "completed_records")?,
            completed_bytes: optional_progress_u64(progress, "completed_bytes")?,
            providers: optional_progress_strings(progress, "providers")?,
            processed_sessions: optional_progress_u64(progress, "processed_sessions")?.unwrap_or(0),
            processed_messages: optional_progress_u64(progress, "processed_messages")?.unwrap_or(0),
            processed_tool_calls: optional_progress_u64(progress, "processed_tool_calls")?
                .unwrap_or(0),
            processed_bytes: optional_progress_u64(progress, "processed_bytes")?.unwrap_or(0),
            elapsed_millis: optional_progress_u64(progress, "elapsed_millis")?,
            current_source_progress,
        })
    }
}

fn optional_progress_strings(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Vec<String>> {
    let Some(value) = fields.get(field) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow!("daemon source refresh progress has an invalid {field}"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| anyhow!("daemon source refresh progress has an invalid {field}"))
        })
        .collect()
}

pub(super) fn status_progress_total_sources_known(response: &Value) -> bool {
    let Some(progress) = response.get("progress") else {
        return false;
    };
    match progress.get("total_sources_known") {
        Some(Value::Bool(known)) => *known,
        // Pre-additive durable records used zero as the unknown placeholder.
        // A new known-zero snapshot carries the explicit boolean above.
        None => progress
            .get("total_sources")
            .and_then(Value::as_u64)
            .is_some_and(|total| total != 0),
        Some(_) => false,
    }
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
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| anyhow!("daemon source refresh progress has an invalid {field}")),
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn progress_parser_distinguishes_legacy_placeholder_and_additive_known_zero() {
        let legacy_unknown = json!({
            "progress": {
                "phase": "queued",
                "completed_sources": 0,
                "total_sources": 0,
            }
        });
        let legacy_known = json!({
            "progress": {
                "phase": "refreshing",
                "completed_sources": 1,
                "total_sources": 2,
            }
        });
        let additive_known_zero = json!({
            "progress": {
                "phase": "published",
                "completed_sources": 0,
                "total_sources": 0,
                "total_sources_known": true,
            }
        });

        assert_eq!(
            SourceBackedRefreshProgress::from_status_json(&legacy_unknown)
                .unwrap()
                .total_sources,
            0
        );
        assert_eq!(
            SourceBackedRefreshProgress::from_status_json(&additive_known_zero)
                .unwrap()
                .total_sources,
            0
        );
        assert!(!status_progress_total_sources_known(&legacy_unknown));
        assert!(status_progress_total_sources_known(&legacy_known));
        assert!(status_progress_total_sources_known(&additive_known_zero));
    }

    #[test]
    fn whole_run_contract_is_additive_and_unknown_eta_stays_explicit() {
        let legacy = json!({
            "progress": {
                "phase": "verifying",
                "completed_sources": 2,
                "total_sources": 2,
            }
        });
        let parsed = SourceBackedRefreshProgress::from_status_json(&legacy).unwrap();
        assert_eq!(parsed.whole_run_stage(), SourceBackedRefreshStage::Reading);

        let progress = SourceBackedRefreshProgress {
            phase: "physical_verification".to_owned(),
            ..Default::default()
        };
        let json = progress.to_json_with_total_known(true, None);
        assert_eq!(json["whole_run_stage"], "physical_verification");
        assert_eq!(json["estimated_remaining_millis"], Value::Null);
        assert_eq!(
            SourceBackedRefreshProgress::from_status_json(&json!({ "progress": json })).unwrap(),
            progress
        );

        let completed = SourceBackedRefreshProgress {
            phase: "published".to_owned(),
            ..Default::default()
        }
        .to_json_with_total_known(true, None);
        assert_eq!(completed["whole_run_stage"], "complete");
        assert_eq!(completed["estimated_remaining_millis"], Value::Null);

        let failed = SourceBackedRefreshProgress {
            phase: "failed".to_owned(),
            ..Default::default()
        }
        .to_json_with_total_known(true, None);
        assert_eq!(failed["whole_run_stage"], "failed");
        assert_eq!(failed["estimated_remaining_millis"], Value::Null);
    }
}
