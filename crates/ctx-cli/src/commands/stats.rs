use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    local_usage::{self, CoveredTokenEstimate, EstimateModel, UsageEstimates, UsageReport},
    output::print_json,
    StatsArgs,
};

const METHODOLOGY: &str =
    "Methodology: estimates use the versioned model and published 50× raw-search benchmark.";

/// Read and render local product statistics.
///
/// Dispatch owns command exclusion and passes the already resolved effective
/// local-usage setting. This report-to-view conversion is the single consumer
/// boundary for the central usage report and estimate DTO.
pub(crate) fn run(args: StatsArgs, data_root: PathBuf, local_usage_enabled: bool) -> Result<()> {
    let detailed = args.detail || args.format.is_json();
    let report = local_usage::read_report(&data_root, local_usage_enabled, detailed);
    render_report(&args, &report)
}

pub(crate) fn malformed_config_failure(json_output: bool) -> Result<()> {
    let report = UsageReport::config_error();
    if json_output {
        eprintln!("{}", serde_json::to_string(&stats_json(&report))?);
    } else {
        eprintln!("local_usage_config_unavailable: local usage configuration could not be read");
    }
    Err(crate::dispatch::rendered_cli_error())
}

fn render_report(args: &StatsArgs, report: &UsageReport) -> Result<()> {
    if args.format.is_json() {
        return print_json(stats_json(report));
    }
    render_human(report, args.detail);
    Ok(())
}

fn stats_json(report: &UsageReport) -> Value {
    let view = StatsView::from_report(report);
    json!({
        "schema_version": 2,
        "local_usage": report,
        "measured": view.measured,
        "estimated": view.estimated,
        "local_only": true,
        "read_only": true,
    })
}

#[derive(Debug, Serialize)]
struct MeasuredStats {
    history_retrieval: HistoryRetrieval,
    code_provenance: CodeProvenance,
    delivery: MeasuredDelivery,
}

#[derive(Debug, Serialize)]
struct HistoryRetrieval {
    searches: u64,
    result_bearing_searches: u64,
    sessions_or_events_opened: u64,
    records_located: u64,
    discovery_proxy: DiscoveryProxy,
}

#[derive(Debug, Serialize)]
struct DiscoveryProxy {
    context_searches: u64,
    context_found: u64,
    context_opened: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_cited: Option<u64>,
    context_cited_coverage: &'static str,
    validated_discoveries: u64,
}

#[derive(Debug, Serialize)]
struct CodeProvenance {
    blame_investigations: u64,
    origins_identified: u64,
    possible_leads: u64,
    no_attribution: u64,
    errors: u64,
    citations: u64,
}

#[derive(Debug, Serialize)]
struct MeasuredDelivery {
    calls: u64,
    results: u64,
    citations: u64,
    cli_output_bytes: ByteMeasurement,
    mcp_transport_response_bytes: ByteMeasurement,
    semantic_context_bytes: ByteMeasurement,
    semantic_search_result_bytes: ByteMeasurement,
    approximate_context_tokens: Option<CoveredTokenEstimate>,
    latency: LatencyMeasurement,
    active_days: u64,
    first_day_utc: Option<String>,
    last_day_utc: Option<String>,
}

#[derive(Debug, Serialize)]
struct ByteMeasurement {
    bytes: u64,
    measured_samples: u64,
}

#[derive(Debug, Serialize)]
struct LatencyMeasurement {
    milliseconds: u64,
    measured_samples: u64,
    buckets: Vec<LatencyBucket>,
}

#[derive(Debug, Serialize)]
struct LatencyBucket {
    bucket: String,
    calls: u64,
}

#[derive(Debug, Serialize)]
struct EstimatedSavings {
    model: EstimateModel,
    approximate_avoided_context_tokens: CoveredTokenEstimate,
    estimated_time_saved_seconds: u64,
}

struct StatsView {
    measured: Option<MeasuredStats>,
    estimated: Option<EstimatedSavings>,
}

impl StatsView {
    fn from_report(report: &UsageReport) -> Self {
        let Some(summary) = &report.summary else {
            return Self {
                measured: None,
                estimated: None,
            };
        };

        let actions = &summary.result_actions;
        let context = &summary.context;
        let blame = &summary.pro_blame;
        let latency_buckets = report
            .details
            .as_ref()
            .into_iter()
            .flat_map(|details| details.duration_buckets.iter())
            .map(|duration| LatencyBucket {
                bucket: duration.duration_bucket.clone(),
                calls: duration.calls,
            })
            .collect();
        let measured = MeasuredStats {
            history_retrieval: HistoryRetrieval {
                searches: actions.searches,
                result_bearing_searches: actions.result_bearing_searches,
                sessions_or_events_opened: actions
                    .sessions_opened
                    .saturating_add(actions.events_opened),
                records_located: actions.records_located,
                discovery_proxy: DiscoveryProxy {
                    context_searches: context.context_searches,
                    context_found: context.context_found,
                    context_opened: context.context_opened,
                    context_cited: (context.context_cited_coverage == "measured")
                        .then_some(context.context_cited),
                    context_cited_coverage: context.context_cited_coverage,
                    validated_discoveries: context.validated_discoveries,
                },
            },
            code_provenance: CodeProvenance {
                blame_investigations: blame.requests,
                origins_identified: blame.produced_attribution_requests,
                possible_leads: blame.possible_or_reference_only_requests,
                no_attribution: blame.no_confident_attribution_requests,
                errors: blame.error_requests,
                citations: blame.citation_count,
            },
            delivery: MeasuredDelivery {
                calls: summary.calls,
                results: summary.result_count,
                citations: summary.citation_count,
                cli_output_bytes: ByteMeasurement {
                    bytes: summary.cli_output_bytes,
                    measured_samples: summary.cli_output_byte_samples,
                },
                mcp_transport_response_bytes: ByteMeasurement {
                    bytes: summary.mcp_response_bytes,
                    measured_samples: summary.mcp_response_byte_samples,
                },
                semantic_context_bytes: ByteMeasurement {
                    bytes: summary.semantic_context_bytes,
                    measured_samples: summary.semantic_context_byte_samples,
                },
                semantic_search_result_bytes: ByteMeasurement {
                    bytes: summary.semantic_search_result_bytes,
                    measured_samples: summary.semantic_search_result_byte_samples,
                },
                approximate_context_tokens: report
                    .estimates
                    .as_ref()
                    .map(|estimates| estimates.approximate_context_tokens),
                latency: LatencyMeasurement {
                    milliseconds: summary.measured_latency_ms,
                    measured_samples: summary.measured_latency_samples,
                    buckets: latency_buckets,
                },
                active_days: summary.active_days,
                first_day_utc: summary.first_day_utc.clone(),
                last_day_utc: summary.last_day_utc.clone(),
            },
        };
        Self {
            measured: Some(measured),
            estimated: report.estimates.as_ref().map(EstimatedSavings::from),
        }
    }
}

impl From<&UsageEstimates> for EstimatedSavings {
    fn from(estimates: &UsageEstimates) -> Self {
        Self {
            model: estimates.model,
            approximate_avoided_context_tokens: estimates.approximate_avoided_context_tokens,
            estimated_time_saved_seconds: estimates.estimated_time_saved_seconds,
        }
    }
}

fn render_human(report: &UsageReport, detailed: bool) {
    println!("Local usage: {}", report.state);
    let view = StatsView::from_report(report);
    let Some(measured) = view.measured else {
        let message = report
            .error
            .as_ref()
            .map(|error| format!("{} ({})", error.code, error.message))
            .unwrap_or_else(|| "measurements unavailable while local usage is disabled".to_owned());
        println!("{message}");
        return;
    };

    println!();
    println!("History retrieval");
    println!("  Searches: {}", measured.history_retrieval.searches);
    println!(
        "  Result-bearing searches: {}",
        measured.history_retrieval.result_bearing_searches
    );
    println!(
        "  Sessions/events opened: {}",
        measured.history_retrieval.sessions_or_events_opened
    );
    println!(
        "  Records located: {}",
        measured.history_retrieval.records_located
    );
    let discovery = &measured.history_retrieval.discovery_proxy;
    println!("  Context searches: {}", discovery.context_searches);
    println!("  Context found: {}", discovery.context_found);
    println!("  Context opened: {}", discovery.context_opened);
    match discovery.context_cited {
        Some(context_cited) => println!("  Context cited: {context_cited}"),
        None => println!("  Context cited: {}", discovery.context_cited_coverage),
    }
    println!(
        "  Validated discoveries: {}",
        discovery.validated_discoveries
    );

    println!();
    println!("Code provenance");
    println!(
        "  Blame investigations: {}",
        measured.code_provenance.blame_investigations
    );
    println!(
        "  Origins identified: {}",
        measured.code_provenance.origins_identified
    );
    println!(
        "  Possible leads: {}",
        measured.code_provenance.possible_leads
    );
    println!(
        "  No attribution: {}",
        measured.code_provenance.no_attribution
    );
    println!("  Errors: {}", measured.code_provenance.errors);
    println!("  Citations: {}", measured.code_provenance.citations);

    println!();
    println!("Measured delivery");
    println!("  Results: {}", measured.delivery.results);
    println!("  Citations: {}", measured.delivery.citations);
    render_bytes("CLI output bytes", &measured.delivery.cli_output_bytes);
    render_bytes(
        "MCP transport response bytes",
        &measured.delivery.mcp_transport_response_bytes,
    );
    render_bytes(
        "Semantic/context bytes",
        &measured.delivery.semantic_context_bytes,
    );
    render_bytes(
        "Result-bearing search bytes",
        &measured.delivery.semantic_search_result_bytes,
    );
    match measured.delivery.approximate_context_tokens {
        Some(estimate) => render_token_estimate("Approximate context tokens", &estimate),
        None => println!("  Approximate context tokens: unavailable"),
    }
    println!(
        "  Latency: {} ms across {} measured calls",
        measured.delivery.latency.milliseconds, measured.delivery.latency.measured_samples,
    );
    println!("  Active days: {}", measured.delivery.active_days);

    println!();
    println!("Estimated savings");
    if let Some(estimated) = &view.estimated {
        render_token_estimate(
            "Approximate context tokens avoided",
            &estimated.approximate_avoided_context_tokens,
        );
        println!(
            "  Estimated time saved: {} seconds",
            estimated.estimated_time_saved_seconds
        );
    } else {
        println!("  Approximate context tokens avoided: unavailable");
        println!("  Estimated time saved: unavailable");
    }
    println!("{METHODOLOGY}");

    if detailed {
        println!();
        println!("CLI / MCP detail");
        local_usage::render_human_summary(report, true);
    }
}

fn render_bytes(label: &str, measurement: &ByteMeasurement) {
    println!(
        "  {label}: {} across {} measured calls",
        measurement.bytes, measurement.measured_samples
    );
}

fn render_token_estimate(label: &str, estimate: &CoveredTokenEstimate) {
    let coverage = estimate.coverage.as_str();
    let samples = format!(
        "{}/{} measured samples",
        estimate.measured_samples, estimate.eligible_samples
    );
    match estimate.approximate_tokens {
        Some(tokens) => println!("  {label}: {tokens} ({coverage}; {samples})"),
        None => println!("  {label}: unavailable ({coverage}; {samples})"),
    }
}
