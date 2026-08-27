use clap::{Args, Subcommand};
use serde_json::Value;

use crate::output::JsonOutputFormat;
use crate::ui::{
    fields, outcome, section, Document, Field, Line, Outcome, OutcomeState, Span, Token,
};

#[derive(Debug, Args)]
pub struct SemanticArgs {
    #[command(subcommand)]
    pub command: SemanticCommand,
}

impl SemanticArgs {
    pub fn json_output(&self) -> bool {
        match &self.command {
            SemanticCommand::Enable(args) => args.format.is_json(),
            SemanticCommand::Status(args) | SemanticCommand::Disable(args) => args.format.is_json(),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum SemanticCommand {
    #[command(about = "Enable local semantic search and start model indexing")]
    Enable(SemanticEnableArgs),
    #[command(about = "Show local semantic search readiness")]
    Status(SemanticFormatArgs),
    #[command(about = "Disable local semantic search and retain downloaded assets")]
    Disable(SemanticFormatArgs),
}

#[derive(Debug, Args)]
pub struct SemanticEnableArgs {
    #[arg(
        long,
        help = "Wait until semantic search is ready for the current index"
    )]
    pub wait: bool,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

#[derive(Debug, Args)]
pub struct SemanticFormatArgs {
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub format: JsonOutputFormat,
}

pub fn render_semantic_status(context: &crate::ui::RenderContext, report: &Value) -> Document {
    let enabled = bool_at(report, "/enabled");
    let status = str_at(report, "/status", "unavailable");
    let indexing_mode = str_at(report, "/indexing/mode", "unknown");
    let (state, title, detail) = if !enabled {
        if status == "disabling" {
            (
                OutcomeState::Neutral,
                "Semantic search is disabling",
                Some("The opt-out is saved; background semantic serving is stopping."),
            )
        } else {
            (
                OutcomeState::Neutral,
                "Semantic search is disabled",
                Some("No model acquisition or semantic indexing will run."),
            )
        }
    } else {
        match status {
            "ready" => (
                OutcomeState::Success,
                "Semantic search is ready",
                Some("Hybrid and semantic search can use the current index."),
            ),
            "pending" if indexing_mode == "manual" => (
                OutcomeState::Neutral,
                "Semantic search is enabled",
                Some("Automatic model acquisition and indexing are paused in manual mode."),
            ),
            "pending" => (
                OutcomeState::Neutral,
                "Semantic search is preparing",
                Some("Model acquisition or semantic indexing is still in progress."),
            ),
            "failed" | "unavailable" => (
                OutcomeState::Warning,
                "Semantic search needs attention",
                Some("Inspect the reported reason or run ctx doctor."),
            ),
            _ => (
                OutcomeState::Neutral,
                "Semantic search is enabled",
                Some("Background maintenance has not reported a ready index yet."),
            ),
        }
    };
    let mut document = outcome(
        context,
        Outcome {
            state,
            title,
            detail,
        },
    );
    let daemon_status = str_at(report, "/daemon/status", "unavailable");
    let reason = report
        .pointer("/reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty());
    let mut values = vec![
        Field::new("Status", status),
        Field::new("Indexing", indexing_mode),
        Field::new("Background", daemon_status),
    ];
    if let Some(reason) = reason {
        values.push(Field::new("Reason", reason));
    }
    document.push_blank();
    document.append(section("Semantic", fields(context, &values)));

    let next = if !enabled {
        Some("ctx semantic enable")
    } else if status == "ready" {
        Some("ctx search \"your query\"")
    } else if indexing_mode == "manual" {
        Some("ctx index mode auto")
    } else if matches!(status, "failed" | "unavailable") {
        Some("ctx doctor")
    } else {
        Some("ctx semantic status")
    };
    if let Some(next) = next {
        document.push_blank();
        document.append(section(
            "Next",
            Document::from_line(
                Line::new()
                    .with(Span::text("  "))
                    .with(Span::new(next, Token::Command)),
            ),
        ));
    }
    document
}

pub fn render_semantic_disabled(context: &crate::ui::RenderContext, report: &Value) -> Document {
    let status = str_at(report, "/status", "disabled");
    let pending = status == "disabling";
    let mut document = outcome(
        context,
        Outcome {
            state: if pending {
                OutcomeState::Neutral
            } else {
                OutcomeState::Success
            },
            title: if pending {
                "Semantic search is disabling"
            } else {
                "Semantic search disabled"
            },
            detail: Some(if pending {
                "The opt-out is saved; background serving is stopping. Downloaded assets are retained."
            } else {
                "Downloaded model, runtime, and semantic index data were retained."
            }),
        },
    );
    document.push_blank();
    document.append(section(
        "Semantic",
        fields(context, &[Field::new("Status", status)]),
    ));
    document
}

fn bool_at(report: &Value, pointer: &str) -> bool {
    report
        .pointer(pointer)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn str_at<'a>(report: &'a Value, pointer: &str, fallback: &'a str) -> &'a str {
    report
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use ctx_terminal::{RenderContext, StreamKind, TestContext};
    use serde_json::json;

    use super::*;

    fn context() -> RenderContext {
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout))
    }

    #[test]
    fn disabled_status_points_to_the_namespace() {
        let rendered = render_semantic_status(
            &context(),
            &json!({
                "enabled": false,
                "status": "disabled",
                "indexing": {"mode": "auto"},
                "daemon": {"status": "running"},
            }),
        )
        .render_plain();

        assert!(
            rendered.contains("Semantic search is disabled"),
            "{rendered}"
        );
        assert!(rendered.contains("ctx semantic enable"), "{rendered}");
    }

    #[test]
    fn manual_pending_status_explains_the_required_lifecycle() {
        let rendered = render_semantic_status(
            &context(),
            &json!({
                "enabled": true,
                "status": "pending",
                "reason": "flat_f32_projection_missing",
                "indexing": {"mode": "manual"},
                "daemon": {"status": "disabled"},
            }),
        )
        .render_plain();

        assert!(
            rendered.contains("Semantic search is enabled"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Automatic model acquisition and indexing are paused"),
            "{rendered}"
        );
        assert!(rendered.contains("ctx index mode auto"), "{rendered}");
    }

    #[test]
    fn pending_disable_reports_saved_policy_and_background_shutdown() {
        let rendered = render_semantic_disabled(
            &context(),
            &json!({
                "enabled": false,
                "status": "disabling",
            }),
        )
        .render_plain();

        assert!(
            rendered.contains("Semantic search is disabling"),
            "{rendered}"
        );
        assert!(rendered.contains("opt-out is saved"), "{rendered}");
        assert!(rendered.contains("Status  disabling"), "{rendered}");
    }
}
