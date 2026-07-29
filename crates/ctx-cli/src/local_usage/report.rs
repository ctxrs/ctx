use std::path::Path;

use serde::Serialize;

use super::store::{open_read_only, usage_path, usage_store_exists};
use super::{estimate_usage, UsageEstimates, DEFINITION_VERSION, RETENTION_DAYS};

mod query;
mod render;
mod validation;

use query::query_report;
pub(crate) use render::{pro_conversion_action, render_human_summary};
pub(super) use validation::{validate_rows, validate_rows_for_schema};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageReport {
    pub(crate) schema_version: i64,
    pub(crate) local_only: bool,
    pub(crate) read_only: bool,
    pub(crate) enabled: bool,
    pub(crate) state: &'static str,
    pub(crate) retention_days: i64,
    #[serde(skip)]
    pub(crate) definition_version: i64,
    #[serde(skip)]
    pub(crate) summary: Option<UsageSummary>,
    #[serde(skip)]
    pub(crate) details: Option<UsageDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) definitions: Option<Vec<UsageDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) estimates: Option<UsageEstimates>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<UsageReportError>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageDefinition {
    pub(crate) definition_version: i64,
    pub(crate) ctx_versions: Vec<String>,
    pub(crate) first_day_utc: String,
    pub(crate) last_day_utc: String,
    pub(crate) active_days: u64,
    pub(crate) summary: UsageSummary,
    pub(crate) by_operation: Vec<OperationSummary>,
    pub(crate) duration_buckets: Vec<DurationSummary>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct UsageSummary {
    pub(crate) calls: u64,
    pub(crate) successful_calls: u64,
    pub(crate) failed_calls: u64,
    pub(crate) result_bearing_calls: u64,
    pub(crate) empty_calls: u64,
    pub(crate) not_applicable_calls: u64,
    pub(crate) result_count: u64,
    pub(crate) citation_count: u64,
    pub(crate) delivered_output_bytes: u64,
    pub(crate) delivered_context_bytes: u64,
    pub(crate) matched_normalized_session_bytes: u64,
    pub(crate) complete_context_eligible_calls: u64,
    pub(crate) unavailable_context_eligible_calls: u64,
    pub(crate) pro_blame: ProBlameSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ProBlameSummary {
    pub(crate) requests: u64,
    pub(crate) produced_attribution_requests: u64,
    pub(crate) possible_only_requests: u64,
    pub(crate) none_requests: u64,
    pub(crate) error_requests: u64,
    pub(crate) by_target: Vec<ProBlameTargetSummary>,
    #[serde(skip)]
    pub(super) not_applicable_target_errors: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProBlameTargetSummary {
    pub(crate) target_type: String,
    pub(crate) requests: u64,
    pub(crate) produced: u64,
    pub(crate) possible: u64,
    pub(crate) none: u64,
    pub(crate) error: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OperationSummary {
    pub(crate) ctx_version: String,
    pub(crate) surface: String,
    pub(crate) operation: String,
    pub(crate) calls: u64,
    pub(crate) successful_calls: u64,
    pub(crate) failed_calls: u64,
    pub(crate) result_bearing_calls: u64,
    pub(crate) empty_calls: u64,
    pub(crate) not_applicable_calls: u64,
    pub(crate) result_count: u64,
    pub(crate) citation_count: u64,
    pub(crate) delivered_output_bytes: u64,
    pub(crate) delivered_context_bytes: u64,
    pub(crate) matched_normalized_session_bytes: u64,
    pub(crate) complete_context_eligible_calls: u64,
    pub(crate) unavailable_context_eligible_calls: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DurationSummary {
    pub(crate) duration_bucket: String,
    pub(crate) calls: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct UsageDetails {
    pub(crate) by_operation: Vec<OperationSummary>,
    pub(crate) duration_buckets: Vec<DurationSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageReportError {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

impl UsageReport {
    pub(crate) fn config_error() -> Self {
        error_report(
            false,
            "local_usage_config_unavailable",
            "local usage configuration could not be read",
        )
    }
}

pub(crate) fn read_report(data_root: &Path, enabled: bool, detailed: bool) -> UsageReport {
    if !enabled {
        return base_report(false, "disabled", None, None, None);
    }
    let exists = match usage_store_exists(data_root) {
        Ok(exists) => exists,
        Err(error) => {
            return error_report(true, "usage_store_unavailable", error.public_message());
        }
    };
    if !exists {
        return base_report(true, "empty", Some(Vec::new()), None, None);
    }
    let path = usage_path(data_root);
    match open_read_only(&path).and_then(|mut store| {
        let (definitions, estimate_facts) = query_report(store.connection_mut(), detailed)?;
        let estimates = estimate_usage(estimate_facts)?;
        store.verify_unchanged()?;
        Ok((definitions, estimates))
    }) {
        Ok((definitions, estimates)) => {
            let state = if definitions.is_empty() {
                "empty"
            } else {
                "ready"
            };
            base_report(true, state, Some(definitions), estimates, None)
        }
        Err(error) => error_report(true, "usage_store_unavailable", error.public_message()),
    }
}

fn base_report(
    enabled: bool,
    state: &'static str,
    definitions: Option<Vec<UsageDefinition>>,
    estimates: Option<UsageEstimates>,
    error: Option<UsageReportError>,
) -> UsageReport {
    let compatibility_definition = definitions.as_ref().and_then(|definitions| {
        definitions
            .iter()
            .find(|definition| definition.definition_version == DEFINITION_VERSION)
            .or_else(|| definitions.last())
    });
    let summary = compatibility_definition.map(|definition| definition.summary.clone());
    let details = compatibility_definition.map(|definition| UsageDetails {
        by_operation: definition.by_operation.clone(),
        duration_buckets: definition.duration_buckets.clone(),
    });
    UsageReport {
        schema_version: 2,
        local_only: true,
        read_only: true,
        enabled,
        state,
        retention_days: RETENTION_DAYS,
        definition_version: DEFINITION_VERSION,
        summary,
        details,
        definitions,
        estimates,
        error,
    }
}

fn error_report(enabled: bool, code: &'static str, message: &'static str) -> UsageReport {
    base_report(
        enabled,
        "error",
        None,
        None,
        Some(UsageReportError { code, message }),
    )
}
