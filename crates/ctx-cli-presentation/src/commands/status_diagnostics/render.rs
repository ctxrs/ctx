use crate::ui::{fields, Document, Field, RenderContext};

use super::*;

fn number(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

pub fn render_status_diagnostics(
    context: &RenderContext,
    report: &StatusDiagnosticReport,
) -> Document {
    let duration = report.elapsed_ms.map_or_else(
        || "unknown".to_owned(),
        |ms| format!("{:.3} s", ms as f64 / 1000.0),
    );
    let cpu = match (report.cpu_ms, report.cpu_percent) {
        (Some(ms), Some(percent)) => format!("{:.3} s; {percent:.2}% of one core", ms / 1000.0),
        _ => format!("unknown ({})", report.cpu_status.as_str().replace('_', " ")),
    };
    let cycles = match (report.counters[5], report.cycles_per_second) {
        (Some(count), Some(rate)) => format!("{count} observed; {rate:.2}/s"),
        _ => format!(
            "unknown ({})",
            report.scheduler_status.as_str().replace('_', " ")
        ),
    };
    let activity = &report.refresh;
    let refresh = if activity.active() && report.daemon != DaemonState::Running {
        format!(
            "recorded {}; daemon {}",
            activity.state.as_str().replace('_', " "),
            report.daemon.as_str()
        )
    } else {
        activity.state.as_str().replace('_', " ")
    };
    let mut values = vec![
        ("ctx version", report.ctx_version.to_owned()),
        ("OS", OS.to_owned()),
        ("Architecture", ARCH.to_owned()),
        ("Activity sample", duration),
        ("Daemon", report.daemon.as_str().to_owned()),
        (
            "Heartbeat age",
            report
                .heartbeat_age_ms
                .map_or_else(|| "unknown".to_owned(), |ms| format!("{ms} ms")),
        ),
        ("CPU", cpu),
        ("Scheduler cycles", cycles),
        ("Last observed refresh", refresh),
        ("History work", activity.work_description()),
        (
            "Providers",
            activity
                .provider_names()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "unknown".to_owned()),
        ),
    ];
    if matches!(
        activity.state,
        RefreshState::Skipped | RefreshState::Disabled
    ) || activity.reason != RefreshReason::Unknown
    {
        values.push((
            "Refresh reason",
            match activity.reason {
                RefreshReason::Backoff => "waiting before retrying a failed refresh",
                RefreshReason::Deadline => "daemon work deadline reached",
                RefreshReason::Disabled => "daemon disabled",
                RefreshReason::Paused => "automatic refresh retries paused",
                RefreshReason::PartiallyPaused => {
                    "automatic refresh retries paused for some sources"
                }
                RefreshReason::Confirming => "retrying to confirm a refresh failure",
                RefreshReason::Unknown => "unknown",
            }
            .to_owned(),
        ));
    }
    values.extend([
        ("Processed records", number(activity.processed_records)),
        ("Processed bytes", number(activity.processed_bytes)),
        ("Scanned routes", number(activity.scanned_routes)),
        ("Certified sources", number(activity.certified_source_count)),
        ("Certified bytes", number(activity.certified_source_bytes)),
        (
            "Generation changed",
            activity
                .generation_changed
                .map_or_else(|| "unknown".to_owned(), |changed| changed.to_string()),
        ),
    ]);
    let mut document = fields(
        context,
        &values
            .iter()
            .map(|(label, value)| Field::new(label, value))
            .collect::<Vec<_>>(),
    );
    document.push_blank();
    let timing_values = ["Discovery", "Read and stage", "Commit", "Publication check"]
        .into_iter()
        .zip(activity.timings)
        .map(|(label, value)| {
            (
                label,
                value.map_or_else(
                    || "unknown".to_owned(),
                    |us| format!("{:.6} s", us as f64 / 1_000_000.0),
                ),
            )
        })
        .collect::<Vec<_>>();
    document.append(crate::ui::section(
        "Recorded refresh time",
        fields(
            context,
            &timing_values
                .iter()
                .map(|(label, value)| Field::new(label, value))
                .collect::<Vec<_>>(),
        ),
    ));
    let wakeup_labels = [
        "Filesystem signals",
        "IPC signals",
        "Timeout wakeups",
        "Retry wakeups",
        "Refresh wakeups",
        "Work cycles",
        "No-work cycles",
    ];
    let wakeup_values = wakeup_labels
        .into_iter()
        .zip(report.counters)
        .map(|(label, value)| (label, number(value)))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(crate::ui::section(
        "Observed scheduler increments",
        fields(
            context,
            &wakeup_values
                .iter()
                .map(|(label, value)| Field::new(label, value))
                .collect::<Vec<_>>(),
        ),
    ));
    let context_values = [
        ("Blame writer observed", report.blame_writer_observed.map_or_else(|| "unknown".to_owned(), |value| value.to_string())),
        ("Blame recorded phase", report.blame_phase.as_str().replace('_', " ")),
        ("Blame completed sources", number(report.blame_counters[0])),
        ("Blame total sources", number(report.blame_counters[1])),
        ("Blame applied changes", number(report.blame_counters[2])),
        ("Last recorded semantic job", report.semantic_status.as_str().replace('_', " ")),
        ("Semantic work remaining", report.semantic_work_remaining.map_or_else(|| "unknown".to_owned(), |value| value.to_string())),
        ("Scope", "Blame is store-wide, not tied to the daemon PID. Progress and semantic job data may be stale. CPU is aggregate process time without activity attribution.".to_owned()),
    ];
    document.push_blank();
    document.append(crate::ui::section(
        "Advisory work context",
        fields(
            context,
            &context_values
                .iter()
                .map(|(label, value)| Field::new(label, value))
                .collect::<Vec<_>>(),
        ),
    ));
    document
}
