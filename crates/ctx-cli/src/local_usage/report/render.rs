use serde_json::{json, Value};

use crate::pro::PRO_MONTHLY_PRICE_DISPLAY;

use super::UsageReport;

pub(crate) fn render_human_summary(report: &UsageReport, detailed: bool) {
    println!("Local usage: {}", report.state);
    if let Some(error) = &report.error {
        println!("{} ({})", error.code, error.message);
        return;
    }
    let Some(definitions) = &report.definitions else {
        return;
    };
    for definition in definitions {
        let summary = &definition.summary;
        println!();
        println!(
            "Measured local facts — definition {}",
            definition.definition_version
        );
        println!(
            "  Active UTC days: {} ({} through {})",
            definition.active_days, definition.first_day_utc, definition.last_day_utc
        );
        println!("  ctx versions: {}", definition.ctx_versions.join(", "));
        println!(
            "  Calls: {} ({} success, {} failure)",
            summary.calls, summary.successful_calls, summary.failed_calls
        );
        println!(
            "  Classified result sets: {} nonempty, {} empty",
            summary.result_bearing_calls, summary.empty_calls
        );
        println!(
            "  No result-set classification: {} calls",
            summary.not_applicable_calls
        );
        println!(
            "  Results: {}; unique blame citations: {}",
            summary.result_count, summary.citation_count
        );
        println!(
            "  Delivered output bytes: {}",
            summary.delivered_output_bytes
        );
        println!(
            "  Covered delivered context bytes: {}",
            summary.delivered_context_bytes
        );
        println!(
            "  Matched normalized session bytes: {}",
            summary.matched_normalized_session_bytes
        );
        println!(
            "  Eligible search coverage: {} complete, {} unavailable",
            summary.complete_context_eligible_calls, summary.unavailable_context_eligible_calls
        );
        let blame = &summary.pro_blame;
        if blame.requests > 0 {
            println!(
                "  Blame outcomes: {} produced-attribution, {} possible-only, {} none, {} error",
                blame.produced_attribution_requests,
                blame.possible_only_requests,
                blame.none_requests,
                blame.error_requests
            );
        }
        if detailed {
            for operation in &definition.by_operation {
                println!(
                    "  {}/{} {}: calls={} success={} failure={} nonempty={} empty={} n/a={} output_bytes={} context_bytes={} complete={} unavailable={}",
                    operation.surface,
                    operation.operation,
                    operation.ctx_version,
                    operation.calls,
                    operation.successful_calls,
                    operation.failed_calls,
                    operation.result_bearing_calls,
                    operation.empty_calls,
                    operation.not_applicable_calls,
                    operation.delivered_output_bytes,
                    operation.delivered_context_bytes,
                    operation.complete_context_eligible_calls,
                    operation.unavailable_context_eligible_calls,
                );
            }
            for duration in &definition.duration_buckets {
                println!(
                    "  Duration {}: {} calls",
                    duration.duration_bucket, duration.calls
                );
            }
        }
    }
    if let Some(estimates) = &report.estimates {
        let tokens = estimates.approximate_context_tokens;
        println!();
        println!("Approximate token-equivalents");
        println!(
            "  Covered context: {} bytes; low={} central={} high={} ({})",
            tokens.delivered_context_bytes,
            tokens.token_equivalents.low,
            tokens.token_equivalents.central,
            tokens.token_equivalents.high,
            tokens.coefficient_version
        );
        let reduction = estimates.estimated_context_reduction;
        println!();
        println!("Estimated context reduction");
        println!(
            "  Baseline={} bytes, observed={} bytes, estimated reduction={} bytes",
            reduction.comparison_baseline_bytes,
            reduction.observed_delivered_context_bytes,
            reduction.estimated_avoided_context_bytes
        );
        println!(
            "  Approximate reduction: low={} central={} high={} ({}; {}; covered={} unavailable={})",
            reduction.approximate_token_equivalents.low,
            reduction.approximate_token_equivalents.central,
            reduction.approximate_token_equivalents.high,
            reduction.estimate_model_version,
            reduction.coefficient_version,
            reduction.covered_calls,
            reduction.unavailable_calls
        );
    }
}

pub(crate) fn pro_conversion_action(access_state: Option<&str>) -> Option<Value> {
    match access_state {
        Some("trial") => Some(json!({
            "kind": "pro_monthly_conversion",
            "price": PRO_MONTHLY_PRICE_DISPLAY,
            "command": "ctx pro manage",
            "reason": "trial_active",
        })),
        Some("locked") => Some(json!({
            "kind": "pro_restore_access",
            "command": "ctx pro manage",
            "reason": "access_locked",
            "graph_preserved": true,
        })),
        Some("active" | "canceling_paid" | "offline_grace") | None | Some(_) => None,
    }
}
