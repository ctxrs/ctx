use serde_json::{json, Value};

use crate::pro::PRO_MONTHLY_PRICE_DISPLAY;

use super::super::CoveredTokenEstimate;
use super::UsageReport;

const HUMAN_OUTPUT_WIDTH: usize = 80;

pub(crate) fn render_human_summary(report: &UsageReport, detailed: bool) {
    println!("local_usage: {}", report.state);
    let Some(summary) = &report.summary else {
        if let Some(error) = &report.error {
            println!("local_usage_error: {} ({})", error.code, error.message);
        }
        return;
    };
    println!("usage_calls: {}", summary.calls);
    println!("usage_active_utc_days: {}", summary.active_days);
    println!(
        "usage_measured_latency_ms: {} samples={}",
        summary.measured_latency_ms, summary.measured_latency_samples
    );
    println!(
        "usage_cli_output_bytes: {} samples={}",
        summary.cli_output_bytes, summary.cli_output_byte_samples
    );
    println!(
        "usage_mcp_transport_bytes: {} samples={}",
        summary.mcp_response_bytes, summary.mcp_response_byte_samples
    );
    println!(
        "usage_semantic_context_bytes: {} samples={}",
        summary.semantic_context_bytes, summary.semantic_context_byte_samples
    );
    println!(
        "usage_semantic_search_result_bytes: {} samples={}",
        summary.semantic_search_result_bytes, summary.semantic_search_result_byte_samples
    );
    println!(
        "usage_context_proxies: searches={} found={} opened={} cited={} validated={}",
        summary.context.context_searches,
        summary.context.context_found,
        summary.context.context_opened,
        summary.context.context_cited_coverage,
        summary.context.validated_discoveries
    );
    if let Some(estimates) = &report.estimates {
        println!("usage_estimate_model: {}", estimates.model.version);
        print_token_estimate(
            "usage_approximate_context_tokens",
            estimates.approximate_context_tokens,
        );
        print_token_estimate(
            "usage_approximate_avoided_context_tokens",
            estimates.approximate_avoided_context_tokens,
        );
        println!(
            "usage_estimated_time_saved_seconds: {}",
            estimates.estimated_time_saved_seconds
        );
    }
    println!(
        "usage_mcp_pro_result_classification: {} nonempty, {} empty",
        summary.result_bearing_calls, summary.empty_calls
    );
    println!(
        "usage_mcp_pro_result_classification_not_applicable: {} calls",
        summary.not_applicable_calls
    );
    let blame = &summary.pro_blame;
    if blame.requests > 0 {
        println!(
            "Pro returned produced attribution in {} of {} blame requests.",
            blame.produced_attribution_requests, blame.requests
        );
        println!(
            "pro_blame_outcomes: produced-attribution {}, possible-only {}, none {}, error {}",
            blame.produced_attribution_requests,
            blame.possible_or_reference_only_requests,
            blame.no_confident_attribution_requests,
            blame.error_requests
        );
        for target in &blame.by_target {
            println!(
                "  {}: produced-attribution {}, possible-only/reference-only {}, none {}, error {}",
                target.target_type,
                target.produced,
                target.possible_or_reference_only,
                target.none,
                target.error
            );
        }
    }
    if detailed {
        if let Some(details) = &report.details {
            for operation in &details.by_operation {
                println!(
                    "usage_operation: {}/{}",
                    operation.surface, operation.operation
                );
                print_wrapped_fields([
                    format!("ctx_version={}", operation.ctx_version),
                    format!("calls={}", operation.calls),
                    format!("success={}", operation.successful_calls),
                    format!("failure={}", operation.failed_calls),
                    format!("result={}", operation.result_bearing_calls),
                    format!("empty={}", operation.empty_calls),
                    format!("not-applicable={}", operation.not_applicable_calls),
                ]);
            }
            for duration in &details.duration_buckets {
                println!(
                    "usage_duration: {} calls={}",
                    duration.duration_bucket, duration.calls
                );
            }
        }
    }
}

fn print_token_estimate(label: &str, estimate: CoveredTokenEstimate) {
    match estimate.approximate_tokens {
        Some(tokens) => println!(
            "{label}: {tokens} coverage={} samples={}/{}",
            estimate.coverage.as_str(),
            estimate.measured_samples,
            estimate.eligible_samples
        ),
        None => println!(
            "{label}: unavailable coverage={} samples={}/{}",
            estimate.coverage.as_str(),
            estimate.measured_samples,
            estimate.eligible_samples
        ),
    }
}

fn print_wrapped_fields(fields: impl IntoIterator<Item = String>) {
    let mut line = String::from("  ");
    for field in fields {
        let separator_width = usize::from(line.len() > 2);
        if line.len() + separator_width + field.len() > HUMAN_OUTPUT_WIDTH {
            println!("{line}");
            line.truncate(2);
        } else if separator_width > 0 {
            line.push(' ');
        }
        line.push_str(&field);
    }
    if line.len() > 2 {
        println!("{line}");
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
