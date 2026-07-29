use std::path::Path;

use serde::Serialize;

use super::store::{open_read_only, usage_path, usage_store_exists};
use super::{estimate_usage, EstimateFacts, UsageEstimates, DEFINITION_VERSION, RETENTION_DAYS};

mod query;
mod render;
mod validation;

use query::{estimate_facts, query_report};
pub(crate) use render::{pro_conversion_action, render_human_summary};
pub(super) use validation::{validate_rows, validate_rows_for_schema};

const CONTEXT_CITED_COVERAGE_UNSUPPORTED: &str = "unsupported";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageReport {
    pub(crate) schema_version: i64,
    pub(crate) enabled: bool,
    pub(crate) state: &'static str,
    pub(crate) definition_version: i64,
    pub(crate) retention_days: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<UsageSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) details: Option<UsageDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) estimates: Option<UsageEstimates>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<UsageReportError>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct UsageSummary {
    pub(crate) first_day_utc: Option<String>,
    pub(crate) last_day_utc: Option<String>,
    pub(crate) active_days: u64,
    pub(crate) ctx_versions: Vec<String>,
    pub(crate) calls: u64,
    pub(crate) successful_calls: u64,
    pub(crate) failed_calls: u64,
    pub(crate) result_bearing_calls: u64,
    pub(crate) empty_calls: u64,
    pub(crate) not_applicable_calls: u64,
    pub(crate) result_count: u64,
    pub(crate) citation_count: u64,
    pub(crate) mcp_response_bytes: u64,
    pub(crate) mcp_response_byte_samples: u64,
    pub(crate) cli_output_bytes: u64,
    pub(crate) cli_output_byte_samples: u64,
    pub(crate) measured_latency_ms: u64,
    pub(crate) measured_latency_samples: u64,
    pub(crate) semantic_context_bytes: u64,
    pub(crate) semantic_context_byte_samples: u64,
    pub(crate) semantic_search_result_bytes: u64,
    pub(crate) semantic_search_result_byte_samples: u64,
    pub(crate) context: ContextProxySummary,
    pub(crate) result_actions: ResultActionSummary,
    pub(crate) pro_blame: ProBlameSummary,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ContextProxySummary {
    pub(crate) context_searches: u64,
    pub(crate) context_found: u64,
    pub(crate) context_opened: u64,
    #[serde(skip)]
    pub(crate) context_cited: u64,
    pub(crate) context_cited_coverage: &'static str,
    pub(crate) validated_discoveries: u64,
}

impl Default for ContextProxySummary {
    fn default() -> Self {
        Self {
            context_searches: 0,
            context_found: 0,
            context_opened: 0,
            context_cited: 0,
            context_cited_coverage: CONTEXT_CITED_COVERAGE_UNSUPPORTED,
            validated_discoveries: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ResultActionSummary {
    pub(crate) searches: u64,
    pub(crate) result_bearing_searches: u64,
    pub(crate) sessions_opened: u64,
    pub(crate) events_opened: u64,
    pub(crate) locate_requests: u64,
    pub(crate) records_located: u64,
    pub(crate) sources_requests: u64,
    pub(crate) sql_requests: u64,
    pub(crate) blame_requests: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ProBlameSummary {
    pub(crate) requests: u64,
    pub(crate) citation_count: u64,
    pub(crate) produced_attribution_requests: u64,
    pub(crate) possible_or_reference_only_requests: u64,
    pub(crate) no_confident_attribution_requests: u64,
    pub(crate) error_requests: u64,
    pub(crate) by_target: Vec<ProBlameTargetSummary>,
    #[serde(skip)]
    unclassified_target_errors: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProBlameTargetSummary {
    pub(crate) target_type: String,
    pub(crate) requests: u64,
    pub(crate) produced: u64,
    pub(crate) possible_or_reference_only: u64,
    pub(crate) none: u64,
    pub(crate) error: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct UsageDetails {
    pub(crate) by_operation: Vec<OperationSummary>,
    pub(crate) duration_buckets: Vec<DurationSummary>,
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
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DurationSummary {
    pub(crate) duration_bucket: String,
    pub(crate) calls: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageReportError {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

impl UsageReport {
    pub(crate) fn config_error() -> Self {
        Self {
            schema_version: 2,
            enabled: false,
            state: "error",
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: None,
            details: None,
            estimates: None,
            error: Some(UsageReportError {
                code: "local_usage_config_unavailable",
                message: "local usage configuration could not be read",
            }),
        }
    }
}

pub(crate) fn read_report(data_root: &Path, enabled: bool, detailed: bool) -> UsageReport {
    let path = usage_path(data_root);
    if !enabled {
        return UsageReport {
            schema_version: 2,
            enabled,
            state: "disabled",
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: None,
            details: None,
            estimates: None,
            error: None,
        };
    }
    let exists = match usage_store_exists(data_root) {
        Ok(exists) => exists,
        Err(error) => return error_report(enabled, error.public_message()),
    };
    if !exists {
        let estimates = match estimate_usage(EstimateFacts::default()) {
            Ok(estimates) => estimates,
            Err(error) => return error_report(enabled, error.public_message()),
        };
        return UsageReport {
            schema_version: 2,
            enabled,
            state: "empty",
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: Some(UsageSummary::default()),
            details: detailed.then(UsageDetails::default),
            estimates: Some(estimates),
            error: None,
        };
    }
    match open_read_only(&path).and_then(|mut store| {
        let (summary, details) = query_report(store.connection_mut(), detailed)?;
        let estimates = estimate_usage(estimate_facts(&summary)?)?;
        store.verify_unchanged()?;
        Ok((summary, details, estimates))
    }) {
        Ok((summary, details, estimates)) => UsageReport {
            schema_version: 2,
            enabled,
            state: if summary.calls == 0 { "empty" } else { "ready" },
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: Some(summary),
            details,
            estimates: Some(estimates),
            error: None,
        },
        Err(error) => error_report(enabled, error.public_message()),
    }
}

fn error_report(enabled: bool, message: &'static str) -> UsageReport {
    UsageReport {
        schema_version: 2,
        enabled,
        state: "error",
        definition_version: DEFINITION_VERSION,
        retention_days: RETENTION_DAYS,
        summary: None,
        details: None,
        estimates: None,
        error: Some(UsageReportError {
            code: "usage_store_unavailable",
            message,
        }),
    }
}
