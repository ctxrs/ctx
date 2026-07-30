use std::time::{Duration, Instant};

use serde_json::Value;

use crate::progress::{format_byte_progress, format_bytes, format_count};
use crate::ui::{
    fields, outcome, progress, section, Document, Field, Line, Outcome, OutcomeState, Progress,
    RenderContext, Span, Token,
};

const RATE_SMOOTHING_WEIGHT: f64 = 0.35;

#[derive(Debug, Clone, Copy)]
struct DashboardSample {
    at: Instant,
    completed_bytes: u64,
    indexed_records: u64,
    semantic_records: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct DashboardRates {
    bytes_per_second: Option<f64>,
    records_per_second: Option<f64>,
    semantic_records_per_second: Option<f64>,
}

#[derive(Debug, Default)]
pub(super) struct IndexDashboard {
    previous: Option<DashboardSample>,
    rates: DashboardRates,
}

impl IndexDashboard {
    pub(super) fn render(&mut self, status: &Value, context: &RenderContext) -> Document {
        self.render_at(status, context, Instant::now())
    }

    fn render_at(&mut self, status: &Value, context: &RenderContext, now: Instant) -> Document {
        self.observe(status, now);
        render_dashboard(status, context, self.rates)
    }

    fn observe(&mut self, status: &Value, now: Instant) {
        let sample = DashboardSample {
            at: now,
            completed_bytes: u64_at(status, &["lexical", "completed_source_bytes"]),
            indexed_records: u64_at(status, &["lexical", "indexed_items"]),
            semantic_records: u64_at(status, &["semantic", "coverage", "embedded_items"]),
        };
        if let Some(previous) = self.previous {
            let elapsed = now.saturating_duration_since(previous.at).as_secs_f64();
            if elapsed >= 0.1 {
                update_rate(
                    &mut self.rates.bytes_per_second,
                    sample.completed_bytes,
                    previous.completed_bytes,
                    elapsed,
                );
                update_rate(
                    &mut self.rates.records_per_second,
                    sample.indexed_records,
                    previous.indexed_records,
                    elapsed,
                );
                update_rate(
                    &mut self.rates.semantic_records_per_second,
                    sample.semantic_records,
                    previous.semantic_records,
                    elapsed,
                );
            }
        }
        self.previous = Some(sample);
    }
}

fn update_rate(rate: &mut Option<f64>, current: u64, previous: u64, elapsed: f64) {
    if current < previous {
        *rate = None;
        return;
    }
    let observed = current.saturating_sub(previous) as f64 / elapsed;
    *rate = Some(match *rate {
        Some(existing) => {
            existing * (1.0 - RATE_SMOOTHING_WEIGHT) + observed * RATE_SMOOTHING_WEIGHT
        }
        None => observed,
    });
}

fn render_dashboard(status: &Value, context: &RenderContext, rates: DashboardRates) -> Document {
    let lexical_status = string_at(status, &["lexical", "status"], "unknown");
    if lexical_status == "missing" {
        return render_missing(context);
    }
    if lexical_status == "failed" {
        return render_lexical_failure(status, context);
    }

    let mut document = if matches!(lexical_status.as_str(), "ready" | "empty") {
        render_ready(status, context)
    } else {
        render_active(status, context, rates)
    };
    append_separated(&mut document, render_semantic(status, context, rates));
    append_separated(&mut document, render_health(status, context));
    document
}

fn render_missing(context: &RenderContext) -> Document {
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Error,
            title: "Search index is not set up",
            detail: None,
        },
    );
    append_action(&mut document, "ctx setup");
    document
}

fn render_lexical_failure(status: &Value, context: &RenderContext) -> Document {
    let failed = u64_at(status, &["lexical", "failed_inventory_units"]);
    let title = if failed == 0 {
        "History indexing failed".to_owned()
    } else {
        format!(
            "Could not index {}",
            pluralized_count(failed, "history file", "history files")
        )
    };
    let sessions = format_count(usize_at(status, &["lexical", "indexed_sessions"]));
    let records = format!(
        "{} searchable",
        format_count(usize_at(status, &["lexical", "indexed_items"]))
    );
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Error,
            title: &title,
            detail: None,
        },
    );
    document.push_blank();
    document.append(section(
        "Still searchable",
        fields(
            context,
            &[
                Field::new("Sessions", &sessions),
                Field::new("Records", &records),
            ],
        ),
    ));
    append_action(&mut document, "ctx doctor");
    document
}

fn render_ready(status: &Value, context: &RenderContext) -> Document {
    let completed_bytes = u64_at(status, &["lexical", "completed_source_bytes"]);
    let total_bytes = u64_at(status, &["lexical", "total_source_bytes"]);
    let processed = format_bytes(total_bytes.max(completed_bytes));
    let sessions = format_count(usize_at(status, &["lexical", "indexed_sessions"]));
    let records = format!(
        "{} searchable",
        format_count(usize_at(status, &["lexical", "indexed_items"]))
    );
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Success,
            title: "Your history is searchable",
            detail: None,
        },
    );
    document.push_blank();
    document.append(fields(
        context,
        &[
            Field::new("Processed", &processed),
            Field::new("Sessions", &sessions),
            Field::new("Records", &records),
        ],
    ));

    let failed = u64_at(status, &["lexical", "failed_inventory_units"]);
    if failed > 0 {
        append_attention(&mut document, context, failed);
    }
    document
}

fn render_active(status: &Value, context: &RenderContext, rates: DashboardRates) -> Document {
    let completed_bytes = u64_at(status, &["lexical", "completed_source_bytes"]);
    let reported_total_bytes = u64_at(status, &["lexical", "total_source_bytes"]);
    let total_bytes =
        (reported_total_bytes > 0).then_some(reported_total_bytes.max(completed_bytes));
    let label = if completed_bytes == 0 && total_bytes.is_none() {
        indeterminate(context, "Discovering your history")
    } else if total_bytes.is_some_and(|total| completed_bytes >= total) {
        "Finalizing your search index".to_owned()
    } else {
        "Indexing your history".to_owned()
    };
    let mut document = progress(
        context,
        Progress {
            label: &label,
            current: completed_bytes,
            total: total_bytes,
            detail: None,
        },
    );

    let processed = match total_bytes {
        Some(total) => format_byte_progress(completed_bytes, total),
        None if completed_bytes > 0 => format!(
            "{} processed; total {}",
            format_bytes(completed_bytes),
            indeterminate(context, "measuring")
        ),
        None => indeterminate(context, "measuring"),
    };
    let sessions = format!(
        "{} indexed",
        format_count(usize_at(status, &["lexical", "indexed_sessions"]))
    );
    let records = format!(
        "{} searchable",
        format_count(usize_at(status, &["lexical", "indexed_items"]))
    );
    let throughput = format_rate(context, rates.records_per_second, "records/sec");
    let remaining = format_remaining(
        context,
        completed_bytes,
        total_bytes,
        rates.bytes_per_second,
    );
    document.push_blank();
    document.append(fields(
        context,
        &[
            Field::new("Processed", &processed),
            Field::new("Sessions", &sessions),
            Field::new("Records", &records),
            Field::new("Throughput", &throughput),
            Field::new("Remaining", &remaining),
        ],
    ));

    let failed = u64_at(status, &["lexical", "failed_inventory_units"]);
    if failed > 0 {
        append_attention(&mut document, context, failed);
    }
    document
}

fn append_attention(document: &mut Document, context: &RenderContext, failed: u64) {
    let title = format!(
        "{} {} attention",
        pluralized_count(failed, "history file", "history files"),
        if failed == 1 { "needs" } else { "need" }
    );
    append_separated(
        document,
        outcome(
            context,
            Outcome {
                state: OutcomeState::Warning,
                title: &title,
                detail: None,
            },
        ),
    );
    append_action(document, "ctx doctor");
}

fn render_semantic(status: &Value, context: &RenderContext, rates: DashboardRates) -> Document {
    if !bool_at(status, &["semantic", "enabled"]) {
        return fields(context, &[Field::new("Semantic search", "Off")]);
    }

    let semantic_status = string_at(status, &["semantic", "status"], "unknown");
    let embedded = u64_at(status, &["semantic", "coverage", "embedded_items"]);
    let searchable = u64_at(status, &["semantic", "coverage", "searchable_items"]);
    if matches!(semantic_status.as_str(), "ready" | "empty") {
        return fields(context, &[Field::new("Semantic search", "On")]);
    }

    if let Some(reason) = semantic_failure_reason(status) {
        let reason = humanize(&reason);
        let mut document = outcome(
            context,
            Outcome {
                state: OutcomeState::Error,
                title: "Semantic search needs attention",
                detail: None,
            },
        );
        document.push_blank();
        document.append(fields(context, &[Field::new("Reason", &reason)]));
        append_action(&mut document, "ctx doctor");
        return document;
    }

    let total = (searchable > 0).then_some(searchable.max(embedded));
    let mut document = progress(
        context,
        Progress {
            label: "Semantic search",
            current: embedded,
            total,
            detail: None,
        },
    );
    let embedded_value = match total {
        Some(total) => format!(
            "{} / {} records",
            format_count_u64(embedded),
            format_count_u64(total)
        ),
        None if embedded > 0 => format!(
            "{} records; total {}",
            format_count_u64(embedded),
            indeterminate(context, "measuring")
        ),
        None => indeterminate(context, "measuring"),
    };
    let throughput = format_rate(context, rates.semantic_records_per_second, "records/sec");
    let remaining = format_remaining(context, embedded, total, rates.semantic_records_per_second);
    document.push_blank();
    document.append(fields(
        context,
        &[
            Field::new("Embedded", &embedded_value),
            Field::new("Throughput", &throughput),
            Field::new("Remaining", &remaining),
        ],
    ));
    document
}

fn semantic_failure_reason(status: &Value) -> Option<String> {
    let semantic_status = string_at(status, &["semantic", "status"], "unknown");
    if matches!(
        semantic_status.as_str(),
        "failed" | "stale_lock" | "unavailable" | "blocked"
    ) {
        return Some(string_at(status, &["semantic", "reason"], &semantic_status));
    }

    let job_status = string_at(
        status,
        &["daemon", "jobs", "semantic_index", "status"],
        "unknown",
    );
    let reason = string_at(
        status,
        &["daemon", "jobs", "semantic_index", "reason"],
        &job_status,
    );
    if matches!(
        job_status.as_str(),
        "failed" | "stale_lock" | "unavailable" | "blocked"
    ) || (job_status == "skipped" && reason == "model_cache_missing")
    {
        Some(reason)
    } else {
        None
    }
}

fn render_health(status: &Value, context: &RenderContext) -> Document {
    if semantic_failure_reason(status).is_some() {
        return Document::new();
    }
    let lexical_pending = u64_at(status, &["lexical", "pending_inventory_units"]) > 0;
    let semantic_status = string_at(status, &["semantic", "status"], "unknown");
    let semantic_pending = bool_at(status, &["semantic", "enabled"])
        && !matches!(semantic_status.as_str(), "ready" | "empty");
    if (!lexical_pending && !semantic_pending) || bool_at(status, &["daemon", "running"]) {
        return Document::new();
    }

    outcome(
        context,
        Outcome {
            state: OutcomeState::Warning,
            title: "Background indexing stopped",
            detail: Some("Run `ctx doctor` for details."),
        },
    )
}

fn append_action(document: &mut Document, command: &str) {
    let action = Document::from_line(
        Line::new()
            .with(Span::text("  "))
            .with(Span::new(command, Token::Command)),
    );
    document.push_blank();
    document.append(section("Next", action));
}

fn append_separated(document: &mut Document, other: Document) {
    if other.is_empty() {
        return;
    }
    if !document.is_empty() {
        document.push_blank();
    }
    document.append(other);
}

fn format_rate(context: &RenderContext, rate: Option<f64>, unit: &str) -> String {
    match rate.filter(|rate| rate.is_finite() && *rate >= 0.05) {
        Some(rate) if rate < 10.0 => format!("{rate:.1} {unit}"),
        Some(rate) => format!("{} {unit}", format_count_u64(rate.round() as u64)),
        None => indeterminate(context, "measuring"),
    }
}

fn format_remaining(
    context: &RenderContext,
    completed: u64,
    total: Option<u64>,
    rate: Option<f64>,
) -> String {
    let Some(total) = total else {
        return indeterminate(context, "estimating");
    };
    if completed >= total {
        return indeterminate(context, "finalizing");
    }
    let Some(rate) = rate.filter(|rate| rate.is_finite() && *rate >= 0.05) else {
        return indeterminate(context, "estimating");
    };
    let seconds = ((total - completed) as f64 / rate)
        .max(1.0)
        .min(u64::MAX as f64) as u64;
    format_duration(Duration::from_secs(seconds))
}

fn indeterminate(context: &RenderContext, value: &str) -> String {
    if context.unicode() {
        format!("{value}…")
    } else {
        format!("{value}...")
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs().max(1);
    if seconds < 60 {
        format!("about {seconds} seconds")
    } else if seconds < 3_600 {
        let minutes = (seconds + 30) / 60;
        format!(
            "about {minutes} {}",
            if minutes == 1 { "minute" } else { "minutes" }
        )
    } else {
        let hours = (seconds + 1_800) / 3_600;
        format!(
            "about {hours} {}",
            if hours == 1 { "hour" } else { "hours" }
        )
    }
}

fn pluralized_count(value: u64, singular: &str, plural: &str) -> String {
    format!(
        "{} {}",
        format_count_u64(value),
        if value == 1 { singular } else { plural }
    )
}

fn format_count_u64(value: u64) -> String {
    usize::try_from(value)
        .map(format_count)
        .unwrap_or_else(|_| value.to_string())
}

fn humanize(value: &str) -> String {
    value.replace('_', " ")
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn string_at(value: &Value, path: &[&str], default: &str) -> String {
    value_at(value, path)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn bool_at(value: &Value, path: &[&str]) -> bool {
    value_at(value, path)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn u64_at(value: &Value, path: &[&str]) -> u64 {
    value_at(value, path).and_then(Value::as_u64).unwrap_or(0)
}

fn usize_at(value: &Value, path: &[&str]) -> usize {
    u64_at(value, path).try_into().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
    }

    fn status(semantic_enabled: bool) -> Value {
        json!({
            "lexical": {
                "status": "partial",
                "indexed_items": 854_466,
                "indexed_sessions": 3_486,
                "completed_source_bytes": 10_700_000_000_u64,
                "total_source_bytes": 13_600_000_000_u64,
                "pending_inventory_units": 947,
                "failed_inventory_units": 0,
            },
            "semantic": {
                "status": if semantic_enabled { "pending" } else { "disabled" },
                "reason": if semantic_enabled {
                    "generation_not_acknowledged"
                } else {
                    "semantic_disabled"
                },
                "enabled": semantic_enabled,
                "coverage": {
                    "embedded_items": 357_421,
                    "searchable_items": 854_466,
                },
            },
            "daemon": {
                "status": "running",
                "running": true,
                "jobs": {
                    "semantic_index": {
                        "status": if semantic_enabled { "pending" } else { "disabled" },
                    },
                },
            },
        })
    }

    fn rates() -> DashboardRates {
        DashboardRates {
            bytes_per_second: Some(20_000_000.0),
            records_per_second: Some(5_200.0),
            semantic_records_per_second: Some(2_100.0),
        }
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(
                line.width() <= width,
                "{line:?} is {} columns in a {width}-column content area",
                line.width()
            );
        }
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    #[test]
    fn active_dashboard_uses_approved_fields_without_internal_statuses() {
        let context = context(80);
        let rendered = render_dashboard(&status(false), &context, rates()).render_plain();

        assert!(rendered.starts_with("Indexing your history"));
        assert!(rendered.lines().next().unwrap().ends_with("78%"));
        assert!(rendered.contains("Processed   10.0 / 12.7 GiB"));
        assert!(rendered.contains("Sessions    3,486 indexed"));
        assert!(rendered.contains("Records     854,466 searchable"));
        assert!(rendered.contains("Throughput  5,200 records/sec"));
        assert!(rendered.contains("Remaining   about 2 minutes"));
        assert!(rendered.contains("Semantic search  Off"));
        assert!(!rendered.contains("partial"));
        assert!(!rendered.contains("daemon"));
        assert_fits(
            &render_dashboard(&status(false), &context, rates()),
            &context,
        );
    }

    #[test]
    fn ready_dashboard_is_a_concise_completion_summary() {
        let mut ready = status(false);
        ready["lexical"]["status"] = json!("ready");
        ready["lexical"]["pending_inventory_units"] = json!(0);
        ready["lexical"]["completed_source_bytes"] = json!(13_600_000_000_u64);
        let rendered =
            render_dashboard(&ready, &context(80), DashboardRates::default()).render_plain();

        assert_eq!(
            rendered,
            "✓ Your history is searchable\n\
\n\
Processed  12.7 GiB\n\
Sessions   3,486\n\
Records    854,466 searchable\n\
\n\
Semantic search  Off\n"
        );
        assert!(!rendered.contains("Remaining"));
        assert!(!rendered.contains('%'));
    }

    #[test]
    fn semantic_off_on_progress_and_failure_are_distinct() {
        let context = context(80);
        let off = render_dashboard(&status(false), &context, rates()).render_plain();
        assert!(off.contains("Semantic search  Off"));

        let mut on = status(true);
        on["semantic"]["status"] = json!("ready");
        on["semantic"]["reason"] = Value::Null;
        let on = render_dashboard(&on, &context, rates()).render_plain();
        assert!(on.contains("Semantic search  On"));

        let progressing = render_dashboard(&status(true), &context, rates()).render_plain();
        assert!(progressing.contains("Semantic search"));
        assert!(progressing.contains("41%"));
        assert!(progressing.contains("Embedded    357,421 / 854,466 records"));

        let mut failed = status(true);
        failed["semantic"]["status"] = json!("failed");
        failed["semantic"]["reason"] = json!("embedding_runtime_failed");
        let failed = render_dashboard(&failed, &context, rates()).render_plain();
        assert!(failed.contains("✗ Semantic search needs attention"));
        assert!(failed.contains("Reason  embedding runtime failed"));
        assert!(failed.contains("ctx doctor"));

        let mut missing_model = status(true);
        missing_model["daemon"]["jobs"]["semantic_index"] = json!({
            "status": "skipped",
            "reason": "model_cache_missing",
        });
        let missing_model = render_dashboard(&missing_model, &context, rates()).render_plain();
        assert!(missing_model.contains("Semantic search needs attention"));
        assert!(missing_model.contains("Reason  model cache missing"));
        assert!(!missing_model.contains("Embedded    357,421"));
    }

    #[test]
    fn unknown_totals_are_indeterminate_without_fake_percentages() {
        let mut unknown = status(false);
        unknown["lexical"]["completed_source_bytes"] = json!(0);
        unknown["lexical"]["total_source_bytes"] = json!(0);
        let rendered =
            render_dashboard(&unknown, &context(48), DashboardRates::default()).render_plain();
        let lexical = rendered.split("\n\n").next().unwrap_or_default();

        assert!(lexical.starts_with("Discovering your history…\n…"));
        assert!(!lexical.contains('%'));
        assert!(rendered.contains("Processed   measuring…"));
        assert!(rendered.contains("Throughput  measuring…"));
        assert!(rendered.contains("Remaining   estimating…"));
    }

    #[test]
    fn failures_keep_searchable_facts_actions_and_one_file_grammar() {
        let context = context(80);
        let mut attention = status(false);
        attention["lexical"]["failed_inventory_units"] = json!(1);
        let attention =
            render_dashboard(&attention, &context, DashboardRates::default()).render_plain();
        assert!(attention.contains("1 history file needs attention"));
        assert!(!attention.contains("1 history file need attention"));

        let mut failed = status(false);
        failed["lexical"]["status"] = json!("failed");
        failed["lexical"]["failed_inventory_units"] = json!(1);
        let failed = render_dashboard(&failed, &context, DashboardRates::default()).render_plain();
        assert!(failed.starts_with("✗ Could not index 1 history file"));
        assert!(failed.contains("Still searchable"));
        assert!(failed.contains("Sessions  3,486"));
        assert!(failed.contains("Records   854,466 searchable"));
        assert!(failed.contains("ctx doctor"));
    }

    #[test]
    fn missing_store_is_a_terminal_setup_state_not_ready_progress() {
        let mut missing = status(false);
        missing["lexical"]["status"] = json!("missing");
        missing["lexical"]["pending_inventory_units"] = json!(0);
        missing["lexical"]["completed_source_bytes"] = json!(0);
        missing["lexical"]["total_source_bytes"] = json!(0);
        let rendered =
            render_dashboard(&missing, &context(80), DashboardRates::default()).render_plain();

        assert!(rendered.starts_with("✗ Search index is not set up"));
        assert!(rendered.contains("ctx setup"));
        assert!(!rendered.contains("Your history is searchable"));
        assert!(!rendered.contains("Indexing your history"));
    }

    #[test]
    fn dashboard_fits_supported_widths_and_reserves_the_last_column() {
        let active_off = status(false);
        let active_on = status(true);
        let mut ready = status(false);
        ready["lexical"]["status"] = json!("ready");
        ready["lexical"]["pending_inventory_units"] = json!(0);
        let mut lexical_failure = status(false);
        lexical_failure["lexical"]["status"] = json!("failed");
        lexical_failure["lexical"]["failed_inventory_units"] = json!(1);
        let mut semantic_failure = status(true);
        semantic_failure["semantic"]["status"] = json!("failed");
        semantic_failure["semantic"]["reason"] =
            json!("embedding_runtime_failed_after_model_initialization");

        for width in [32, 48, 80, 120] {
            let context = context(width);
            for status in [
                &active_off,
                &active_on,
                &ready,
                &lexical_failure,
                &semantic_failure,
            ] {
                let document = render_dashboard(status, &context, rates());
                assert_fits(&document, &context);
            }
        }
    }

    #[test]
    fn ascii_context_uses_ascii_markers_bars_and_ellipsis() {
        let active_context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, 48)
                .color(ColorMode::Never)
                .unicode(false),
        );
        let active = render_dashboard(&status(false), &active_context, DashboardRates::default())
            .render_plain();
        assert!(active.contains('='));
        assert!(active.contains('-'));
        assert!(!active.contains('━'));
        assert!(active.contains("measuring..."));

        let mut discovering = status(false);
        discovering["lexical"]["completed_source_bytes"] = json!(0);
        discovering["lexical"]["total_source_bytes"] = json!(0);
        let discovering =
            render_dashboard(&discovering, &active_context, DashboardRates::default())
                .render_plain();
        assert!(discovering.starts_with("Discovering your history...\n..."));

        let mut ready = status(false);
        ready["lexical"]["status"] = json!("ready");
        ready["lexical"]["pending_inventory_units"] = json!(0);
        let ready =
            render_dashboard(&ready, &active_context, DashboardRates::default()).render_plain();
        assert!(ready.starts_with("OK Your history is searchable"));
    }

    #[test]
    fn styled_rendering_strips_to_the_exact_plain_bytes() {
        let context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Always),
        );
        for status in [status(false), status(true)] {
            let document = render_dashboard(&status, &context, rates());
            let styled = document.render(&context);
            assert!(styled.contains("\u{1b}["));
            assert_eq!(strip_ansi(&styled), document.render_plain());
        }
    }

    #[test]
    fn dashboard_rates_are_derived_from_successive_snapshots() {
        let mut dashboard = IndexDashboard::default();
        let first = status(false);
        let mut second = first.clone();
        second["lexical"]["completed_source_bytes"] = json!(10_740_000_000_u64);
        second["lexical"]["indexed_items"] = json!(864_866);
        let started = Instant::now();
        let context = context(80);

        let first_render = dashboard
            .render_at(&first, &context, started)
            .render_plain();
        let second_render = dashboard
            .render_at(&second, &context, started + Duration::from_secs(2))
            .render_plain();

        assert!(first_render.contains("Throughput  measuring…"));
        assert!(second_render.contains("Throughput  5,200 records/sec"));
        assert!(second_render.contains("Remaining   about 2 minutes"));
    }
}
