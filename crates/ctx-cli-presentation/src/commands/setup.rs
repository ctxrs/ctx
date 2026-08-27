use std::path::Path;

use clap::Args;
use ctx_history_read_application::HistoryHealthReport;
use serde_json::Value;

use crate::ui::{
    fields, outcome, section, Document, Field, Line, Outcome, OutcomeState, RenderContext, Span,
    Token,
};
use crate::{output::JsonOutputFormat, progress::ProgressArg};

use super::history_health::{history_partial_cause, setup_history_fields};

#[derive(Debug, Args)]
pub struct SetupArgs {
    #[arg(
        long,
        alias = "no-import",
        help = "Deprecated and ignored; setup follows its normal refresh lifecycle"
    )]
    pub catalog_only: bool,
    #[arg(long, hide = true, help = "Deprecated; use `ctx semantic enable`")]
    pub semantic: bool,
    #[arg(long, help = "Do not start daemon maintenance after setup")]
    pub no_daemon: bool,
    #[arg(
        long,
        help = "Wait for the daemon-owned lexical refresh to publish before returning"
    )]
    pub wait: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    pub progress: ProgressArg,
}

pub struct SetupDaemonState<'a> {
    pub requested: bool,
    pub reason: Option<&'a str>,
    pub started: bool,
    pub persistent_supervisor_verified: bool,
}

pub fn render_setup_human(
    context: &RenderContext,
    data_root: &Path,
    mode: &str,
    source: &Value,
    health: Option<&HistoryHealthReport>,
    refresh_request: &Value,
    daemon: SetupDaemonState<'_>,
) -> Document {
    let refresh_status = refresh_request["status"].as_str().unwrap_or("unavailable");
    let queued = mode == "pending"
        || matches!(
            refresh_status,
            "accepted" | "pending" | "queued" | "running"
        );
    let partial_cause = history_partial_cause(health);
    let refresh_partial = source["refresh"]["status"] == "partial";
    let partial_detail = partial_cause.as_ref().map(|cause| {
        if refresh_partial {
            format!("{cause}. Healthy prior history remains searchable.")
        } else {
            format!("{cause}. Indexed history remains searchable.")
        }
    });
    let (state, title, detail) = if partial_cause.is_some() {
        (
            OutcomeState::Warning,
            "History is searchable with exclusions",
            partial_detail.as_deref(),
        )
    } else if mode == "ready" {
        (
            OutcomeState::Success,
            "History is ready to search",
            queued.then_some("A refresh is running; the current index remains searchable."),
        )
    } else if queued {
        (
            OutcomeState::Neutral,
            "History indexing is queued",
            Some("Background indexing will publish the first searchable index."),
        )
    } else {
        (
            OutcomeState::Warning,
            "History is not ready",
            Some("Setup completed, but no verified search index is available."),
        )
    };
    let mut document = outcome(
        context,
        Outcome {
            state,
            title,
            detail,
        },
    );

    let mut history_values = setup_history_fields(health);
    history_values.push(("Semantic", component_status(&source["semantic"]).to_owned()));
    if let Some(status) = daemon_human_status(&daemon) {
        history_values.push(("Background", status));
    }
    let history_fields = history_values
        .iter()
        .map(|(label, value)| Field::new(label, value.as_str()))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("History", fields(context, &history_fields)));

    if mode != "ready" && !queued {
        let data_root = data_root.display().to_string();
        document.push_blank();
        document.append(section(
            "Data",
            fields(context, &[Field::new("Root", &data_root)]),
        ));
    }

    let next_command = if partial_cause.is_some() {
        "ctx doctor"
    } else if mode == "ready" {
        "ctx search \"test failure\""
    } else if queued {
        "ctx index watch"
    } else if daemon.requested && !daemon.started {
        "ctx status"
    } else if matches!(daemon.reason, Some("daemon_disabled" | "explicit_opt_out")) {
        "ctx index mode auto"
    } else {
        "ctx doctor"
    };
    document.push_blank();
    document.append(section(
        "Next",
        Document::from_line(
            Line::new()
                .with(Span::text("  "))
                .with(Span::new(next_command, Token::Command)),
        ),
    ));
    document
}

fn component_status(component: &Value) -> &str {
    component
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable")
}

fn daemon_human_status(daemon: &SetupDaemonState<'_>) -> Option<String> {
    match (daemon.started, daemon.persistent_supervisor_verified) {
        (true, true) => None,
        (true, false) => Some("persistent daemon (automatic restart unavailable)".to_owned()),
        (false, _) if daemon.requested => {
            Some("startup was not verified; run ctx status".to_owned())
        }
        (false, _) if daemon.reason == Some("explicit_opt_out") => {
            Some("skipped because --no-daemon was used".to_owned())
        }
        (false, _) if daemon.reason == Some("daemon_disabled") => Some("disabled".to_owned()),
        (false, _) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use ctx_history_read_application::{
        HistoryDataCoverage, HistoryHealthReport, HistoryRootCoverage,
    };
    use serde_json::json;
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(line.width() <= width, "{line:?} exceeded {width} columns");
        }
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    fn ready_source() -> Value {
        json!({
            "indexed_sources": 1,
            "indexed_sessions": 2,
            "indexed_events": 1000,
            "lexical": {
                "status": "ready",
                "certified_sources": 9,
                "indexed_documents": 9,
            },
            "refresh": {"status": "ready"},
            "semantic": {"status": "disabled"},
        })
    }

    fn ready_health() -> HistoryHealthReport {
        HistoryHealthReport {
            contributing_agent_histories: vec!["codex".to_owned()],
            provider_roots: Some(HistoryRootCoverage {
                included: 1,
                partial: 0,
                excluded: 0,
                unknown: 0,
            }),
            sessions: 2,
            messages: 1_000,
            tool_calls: 300,
            data: HistoryDataCoverage {
                processed: 4 * 1024 * 1024,
                excluded: Some(0),
            },
            source_failures: 0,
            rejected_records: 0,
        }
    }

    fn render_ready(context: &RenderContext) -> Document {
        let health = ready_health();
        render_setup_human(
            context,
            Path::new("/tmp/ctx"),
            "ready",
            &ready_source(),
            Some(&health),
            &json!({"status": "published"}),
            SetupDaemonState {
                requested: false,
                reason: None,
                started: false,
                persistent_supervisor_verified: true,
            },
        )
    }

    #[test]
    fn setup_ready_is_outcome_first_and_has_one_search_action() {
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_ready(&context);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("✓ History is ready to search\n\nHistory\n"));
            assert!(normalized.contains("Agent histories Codex"));
            assert!(normalized.contains("Roots 1 included root"));
            assert!(normalized
                .contains("Indexed 2 sessions; 1,000 messages; 300 tool calls; 4.0 MiB processed"));
            assert!(!rendered.contains("747"));
            assert!(!rendered.contains("\nSessions"));
            assert!(!rendered.contains("indexed source"));
            assert!(rendered.contains("Next\n  ctx search \"test failure\"\n"));
            assert!(!rendered.contains("Generation"));
            assert!(!rendered.contains("PID"));
            assert!(!rendered.contains("\nData\n"));
            assert!(!rendered.contains("/tmp/ctx"));
            assert_eq!(rendered.matches("\nNext\n").count(), 1);
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_omits_empty_agent_histories() {
        let source = json!({
            "lexical": {
                "status": "ready",
                "indexed_documents": 1,
            },
            "refresh": {"status": "ready"},
            "semantic": {"status": "disabled"},
        });
        let document = render_setup_human(
            &context(80, ColorMode::Never),
            Path::new("/tmp/ctx"),
            "ready",
            &source,
            Some(&HistoryHealthReport {
                provider_roots: Some(HistoryRootCoverage {
                    included: 0,
                    partial: 0,
                    excluded: 0,
                    unknown: 0,
                }),
                ..HistoryHealthReport::default()
            }),
            &json!({"status": "published"}),
            SetupDaemonState {
                requested: false,
                reason: None,
                started: false,
                persistent_supervisor_verified: true,
            },
        );
        let rendered = document.render_plain();
        assert!(!rendered.contains("Agent histories"));
        assert!(!rendered.contains("Gemini"));
    }

    #[test]
    fn setup_queued_has_watch_as_its_primary_action_without_an_eta() {
        let source = json!({
            "lexical": {"status": "pending"},
            "refresh": {"status": "pending"},
            "semantic": {"status": "disabled"},
        });
        let refresh = json!({"status": "pending"});
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_setup_human(
                &context,
                Path::new("/tmp/ctx"),
                "pending",
                &source,
                None,
                &refresh,
                SetupDaemonState {
                    requested: true,
                    reason: None,
                    started: true,
                    persistent_supervisor_verified: true,
                },
            );
            let rendered = document.render_plain();
            assert!(rendered.starts_with("History indexing is queued\n"));
            assert!(!rendered.contains("Refresh"));
            assert!(rendered.contains("Next\n  ctx index watch\n"));
            assert!(!rendered.contains("Estimated"));
            assert!(!rendered.contains("ctx search"));
            assert!(!rendered.contains("42"));
            assert!(!rendered.contains("\nData\n"));
            assert!(!rendered.contains("/tmp/ctx"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_degraded_explains_disabled_background_work_and_recovery() {
        let source = json!({
            "lexical": {"status": "unavailable"},
            "refresh": {"status": "unavailable"},
            "semantic": {"status": "disabled"},
        });
        let refresh = json!({
            "status": "unavailable",
            "reason": "daemon_disabled",
        });
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_setup_human(
                &context,
                Path::new("/tmp/ctx"),
                "unavailable",
                &source,
                None,
                &refresh,
                SetupDaemonState {
                    requested: false,
                    reason: Some("daemon_disabled"),
                    started: false,
                    persistent_supervisor_verified: false,
                },
            );
            let rendered = document.render_plain();
            assert!(rendered.starts_with("! History is not ready\n"));
            assert!(rendered.contains("Background  disabled\n"));
            assert!(rendered.contains("Data\nRoot  /tmp/ctx\n"));
            assert!(rendered.contains("Next\n  ctx index mode auto\n"));
            assert!(!rendered.contains("ctx index watch"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_manager_unavailable_reports_a_persistent_daemon_without_native_restart() {
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_setup_human(
                &context,
                Path::new("/tmp/ctx"),
                "ready",
                &ready_source(),
                Some(&ready_health()),
                &json!({"status": "published"}),
                SetupDaemonState {
                    requested: true,
                    reason: None,
                    started: true,
                    persistent_supervisor_verified: false,
                },
            );
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("✓ History is ready to search\n"));
            assert!(
                normalized.contains("Background persistent daemon (automatic restart unavailable)")
            );
            assert!(!normalized.contains("Continuous refresh is unavailable"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_fallback_reports_a_persistent_daemon_without_a_bounded_limitation() {
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_setup_human(
                &context,
                Path::new("/tmp/ctx"),
                "ready",
                &ready_source(),
                Some(&ready_health()),
                &json!({"status": "published"}),
                SetupDaemonState {
                    requested: true,
                    reason: None,
                    started: true,
                    persistent_supervisor_verified: false,
                },
            );
            let normalized = document
                .render_plain()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                normalized.contains("Background persistent daemon (automatic restart unavailable)")
            );
            assert!(!normalized.contains("Continuous refresh is unavailable"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn setup_plain_output_equals_ansi_stripped_styled_output() {
        let context = context(80, ColorMode::Always);
        let document = render_ready(&context);
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }
}
