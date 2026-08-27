use serde_json::{json, Map, Value};

use crate::{
    local_usage::{self, UsageReport},
    ui::{
        diagnostic, fields, outcome, Action, Diagnostic, DiagnosticLevel, Document, Field, Outcome,
        OutcomeState, RenderContext,
    },
};

use super::UsageStatusMode;

pub fn compact_usage_health_json(report: &UsageReport) -> Value {
    json!({
        "schema_version": report.schema_version,
        "enabled": report.enabled,
        "state": report.state,
        "definition_version": report.definition_version,
        "retention_days": report.retention_days,
        "error": report.error,
    })
}

pub fn malformed_status_config_json() -> Value {
    json!({
        "schema_version": 1,
        "local_usage": compact_usage_health_json(&local_usage::UsageReport::config_error()),
        "local_only": true,
        "read_only": true,
    })
}

pub fn removed_cloud_config_json() -> Value {
    json!({
        "schema_version": 1,
        "error": {
            "code": "removed_config_key",
            "config_key": "cloud.mode",
            "message": "cloud history configuration is no longer supported",
        },
        "local_only": true,
        "read_only": true,
    })
}

pub fn usage_action_json(action: &Map<String, Value>) -> Value {
    json!({
        "schema_version": 1,
        "local_usage_action": action,
        "local_only": true,
        "read_only": false,
    })
}

pub fn usage_action_error_json(
    mode: UsageStatusMode,
    code: &'static str,
    message: &'static str,
) -> Value {
    json!({
        "schema_version": 1,
        "local_usage_action": {
            "action": mode.as_str(),
            "ok": false,
            "error": {
                "code": code,
                "message": message,
            },
        },
        "local_only": true,
        "read_only": false,
    })
}

pub fn render_usage_action_human(
    context: &RenderContext,
    mode: UsageStatusMode,
    action: &Map<String, Value>,
) -> Document {
    match mode {
        UsageStatusMode::Enable => render_enable_action(context, action),
        UsageStatusMode::Disable => outcome(
            context,
            Outcome {
                state: OutcomeState::Success,
                title: "Local usage disabled",
                detail: None,
            },
        ),
        UsageStatusMode::Reset => {
            let cleared = action.get("store_state").and_then(Value::as_str) == Some("cleared");
            outcome(
                context,
                Outcome {
                    state: if cleared {
                        OutcomeState::Success
                    } else {
                        OutcomeState::Neutral
                    },
                    title: if cleared {
                        "Local usage history cleared"
                    } else {
                        "Local usage history is already empty"
                    },
                    detail: None,
                },
            )
        }
    }
}

pub fn render_usage_failure(
    context: &RenderContext,
    mode: UsageStatusMode,
    code: &'static str,
    message: &'static str,
) -> Document {
    let command = format!("ctx status --usage {}", mode.as_str());
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Error,
            summary: usage_failure_title(mode),
            detail: Some(message),
            fields: &[Field::new("Code", code)],
            action: Some(Action { command: &command }),
        },
    )
}

pub fn render_malformed_status_config_failure(context: &RenderContext) -> Document {
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Error,
            summary: "Local usage configuration could not be read",
            detail: None,
            fields: &[Field::new("Code", "local_usage_config_unavailable")],
            action: Some(Action {
                command: "ctx doctor",
            }),
        },
    )
}

pub fn render_removed_cloud_config_failure(context: &RenderContext) -> Document {
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Error,
            summary: "cloud.mode is no longer supported",
            detail: Some("Remove it from config.toml and run the command again."),
            fields: &[],
            action: None,
        },
    )
}

fn render_enable_action(context: &RenderContext, action: &Map<String, Value>) -> Document {
    if action
        .get("effective_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return outcome(
            context,
            Outcome {
                state: OutcomeState::Success,
                title: "Local usage enabled",
                detail: None,
            },
        );
    }

    let environment = action
        .get("environment_override")
        .and_then(Value::as_str)
        .unwrap_or("invalid");
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Warning,
            title: "Local usage remains disabled",
            detail: Some("The saved setting was updated, but the environment still disables it."),
        },
    );
    document.push_blank();
    document.append(fields(
        context,
        &[
            Field::new("Saved setting", "enabled"),
            Field::new("Environment override", environment),
        ],
    ));
    document
}

fn usage_failure_title(mode: UsageStatusMode) -> &'static str {
    match mode {
        UsageStatusMode::Enable => "Local usage could not be enabled",
        UsageStatusMode::Disable => "Local usage could not be disabled",
        UsageStatusMode::Reset => "Local usage history could not be cleared",
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn action(fields: Value) -> Map<String, Value> {
        fields.as_object().cloned().unwrap()
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    #[test]
    fn usage_actions_are_outcome_first_and_hide_machine_field_names() {
        let cases = [
            (
                UsageStatusMode::Enable,
                action(json!({
                    "persisted_enabled": true,
                    "effective_enabled": true,
                    "environment_override": "none",
                })),
                "✓ Local usage enabled\n",
            ),
            (
                UsageStatusMode::Disable,
                action(json!({
                    "persisted_enabled": false,
                    "effective_enabled": false,
                    "environment_override": "none",
                })),
                "✓ Local usage disabled\n",
            ),
            (
                UsageStatusMode::Reset,
                action(json!({"store_state": "cleared"})),
                "✓ Local usage history cleared\n",
            ),
            (
                UsageStatusMode::Reset,
                action(json!({"store_state": "missing"})),
                "Local usage history is already empty\n",
            ),
        ];

        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            for (mode, action, expected) in &cases {
                let rendered = render_usage_action_human(&context, *mode, action).render_plain();
                if width == 80 {
                    assert_eq!(&rendered, expected);
                } else {
                    assert_eq!(
                        rendered.split_whitespace().collect::<Vec<_>>().join(" "),
                        expected.split_whitespace().collect::<Vec<_>>().join(" ")
                    );
                }
                assert!(!rendered.contains("local_usage_"));
            }
        }
    }

    #[test]
    fn usage_enable_override_is_truthful_responsive_and_style_equivalent() {
        let action = action(json!({
            "persisted_enabled": true,
            "effective_enabled": false,
            "environment_override": "disabled",
        }));
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Always);
            let document = render_usage_action_human(&context, UsageStatusMode::Enable, &action);
            let plain = document.render_plain();
            assert!(plain.starts_with("! Local usage remains disabled\n"));
            assert!(plain.contains("Saved setting"));
            assert!(plain.contains("Environment override"));
            assert!(plain.contains("disabled"));
            assert!(!plain.contains("local_usage_"));
            assert_eq!(strip_ansi(&document.render(&context)), plain);
        }
    }

    #[test]
    fn usage_failures_use_shared_diagnostics_and_copyable_recovery_commands() {
        let rendered = render_usage_failure(
            &context(32, ColorMode::Never),
            UsageStatusMode::Reset,
            "usage_reset_failed",
            "local usage could not be reset",
        )
        .render_plain();
        assert!(rendered.starts_with("✗ Local usage history could not\n  be cleared\n"));
        assert!(rendered.contains("Code  usage_reset_failed\n"));
        assert!(rendered.contains("ctx status --usage reset\n"));
        assert!(!rendered.starts_with("usage_reset_failed:"));
    }

    #[test]
    fn status_failures_have_exact_machine_and_human_presentation() {
        assert_eq!(
            malformed_status_config_json(),
            json!({
                "schema_version": 1,
                "local_usage": {
                    "schema_version": 3,
                    "enabled": false,
                    "state": "error",
                    "definition_version": 3,
                    "retention_days": 400,
                    "error": {
                        "code": "local_usage_config_unavailable",
                        "message": "local usage configuration could not be read",
                    },
                },
                "local_only": true,
                "read_only": true,
            })
        );
        assert_eq!(
            removed_cloud_config_json()["error"],
            json!({
                "code": "removed_config_key",
                "config_key": "cloud.mode",
                "message": "cloud history configuration is no longer supported",
            })
        );
        assert!(
            render_malformed_status_config_failure(&context(80, ColorMode::Never))
                .render_plain()
                .starts_with("✗ Local usage configuration could not be read\n")
        );
        assert!(
            render_removed_cloud_config_failure(&context(80, ColorMode::Never))
                .render_plain()
                .starts_with("✗ cloud.mode is no longer supported\n")
        );
    }

    #[test]
    fn usage_machine_receipts_keep_the_exact_public_schema() {
        let mut success = action(json!({
            "persisted_enabled": true,
            "effective_enabled": true,
            "environment_override": "none",
        }));
        success.insert("action".to_owned(), json!("enable"));
        success.insert("ok".to_owned(), json!(true));
        assert_eq!(
            usage_action_json(&success),
            json!({
                "schema_version": 1,
                "local_usage_action": {
                    "action": "enable",
                    "ok": true,
                    "persisted_enabled": true,
                    "effective_enabled": true,
                    "environment_override": "none",
                },
                "local_only": true,
                "read_only": false,
            })
        );
        assert_eq!(
            usage_action_error_json(
                UsageStatusMode::Reset,
                "usage_reset_failed",
                "local usage could not be reset",
            ),
            json!({
                "schema_version": 1,
                "local_usage_action": {
                    "action": "reset",
                    "ok": false,
                    "error": {
                        "code": "usage_reset_failed",
                        "message": "local usage could not be reset",
                    },
                },
                "local_only": true,
                "read_only": false,
            })
        );
    }
}
