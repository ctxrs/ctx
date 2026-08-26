use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::ui::{
    fields, hint, outcome, section, Action, Document, Field, Hint, Outcome, OutcomeState,
    RenderContext, Ui,
};
use ctx_upgrade_engine::InstallMarker;

pub struct UpgradeStatusView<'a> {
    pub current_version: &'a str,
    pub auto_upgrade: &'a str,
    pub auto_enabled: bool,
    pub state: &'a Value,
    pub install: &'a Value,
}

pub fn render_status(view: UpgradeStatusView<'_>, json_output: bool, ui: &mut Ui) -> Result<()> {
    let value = json!({
        "schema_version": 1,
        "command": "upgrade_status",
        "current_version": view.current_version,
        "auto_upgrade": {
            "mode": view.auto_upgrade,
            "enabled": view.auto_enabled,
        },
        "state": view.state,
        "install": view.install,
        "warnings": [],
    });
    if json_output {
        let output = format!("{}\n", serde_json::to_string_pretty(&value)?);
        ui.write_stdout_bytes(output.as_bytes())?;
        return Ok(());
    }

    let document = render_upgrade_status_human(
        ui.stdout_context(),
        view.current_version,
        view.auto_upgrade,
        view.state,
        view.install,
    );
    ui.write_stdout(&document)?;
    Ok(())
}

fn render_upgrade_status_human(
    context: &RenderContext,
    current_version: &str,
    auto_upgrade: &str,
    state: &Value,
    install: &Value,
) -> Document {
    let managed = install.get("managed").and_then(Value::as_bool) == Some(true);
    let marker = install
        .get("marker")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    let inconsistent = !managed && marker != "absent";
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let upgrade_error = managed && status == "error";
    let (outcome_state, title) = if inconsistent {
        (OutcomeState::Error, "ctx install marker needs attention")
    } else if !managed {
        (
            OutcomeState::Warning,
            "ctx is not managed by the hosted installer",
        )
    } else if upgrade_error {
        (OutcomeState::Error, "Upgrade needs attention")
    } else {
        match status {
            "up_to_date" | "applied" => (OutcomeState::Success, "ctx is up to date"),
            "available" => (OutcomeState::Neutral, "A ctx update is available"),
            "scheduled" => (OutcomeState::Neutral, "A ctx update is scheduled"),
            "never_checked" => (OutcomeState::Neutral, "ctx has not checked for updates yet"),
            _ => (OutcomeState::Neutral, "ctx upgrade status"),
        }
    };
    let mut document = outcome(
        context,
        Outcome {
            state: outcome_state,
            title,
            detail: None,
        },
    );
    document.push_blank();
    document.append(section(
        "Upgrade",
        fields(
            context,
            &[
                Field::new("Version", current_version),
                Field::new(
                    "State",
                    if inconsistent {
                        "inconsistent"
                    } else if managed {
                        human_upgrade_state(status)
                    } else {
                        "unmanaged"
                    },
                ),
                Field::new("Automatic upgrades", auto_upgrade),
            ],
        ),
    ));

    if let Some(error) = state
        .get("error")
        .and_then(Value::as_str)
        .filter(|_| status == "error")
    {
        document.push_blank();
        document.append(section(
            "Issue",
            fields(context, &[Field::new("Detail", error)]),
        ));
    } else if !managed {
        if let Some(reason) = install.get("reason").and_then(Value::as_str) {
            document.push_blank();
            document.append(section(
                "Install",
                fields(context, &[Field::new("Reason", reason)]),
            ));
        }
    }

    let action = if upgrade_error || inconsistent {
        Some((
            "Inspect the local installation and upgrade state.",
            "ctx doctor",
        ))
    } else if managed && status == "available" {
        Some(("Apply the signed update when you are ready.", "ctx upgrade"))
    } else {
        None
    };
    if let Some((text, command)) = action {
        document.push_blank();
        document.append(hint(context, Hint { text }, Some(Action { command })));
    }
    document
}

fn human_upgrade_state(status: &str) -> &str {
    match status {
        "up_to_date" => "up to date",
        _ => status,
    }
}

pub fn reconcile_scheduled_state(mut state: Value, marker: Option<&InstallMarker>) -> Value {
    if state.get("status").and_then(Value::as_str) != Some("scheduled") {
        return state;
    }
    let Some(marker) = marker else {
        return state;
    };
    let Some(latest_version) = state
        .get("latest_version")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return state;
    };
    let Some(install_path) = state.get("install_path").and_then(Value::as_str) else {
        return state;
    };
    if Path::new(install_path) != marker.install_path {
        return state;
    }
    if marker.version == latest_version {
        if let Some(object) = state.as_object_mut() {
            let update_was_available = object
                .get("update_was_available")
                .and_then(Value::as_bool)
                .or_else(|| object.get("update_available").and_then(Value::as_bool))
                .unwrap_or(false);
            object.insert("status".to_owned(), Value::String("applied".to_owned()));
            object.insert("applied".to_owned(), Value::Bool(true));
            object.insert("current_version".to_owned(), Value::String(latest_version));
            object.insert("update_available".to_owned(), Value::Bool(false));
            object.insert(
                "update_was_available".to_owned(),
                Value::Bool(update_was_available),
            );
            object.insert(
                "reconciled_from".to_owned(),
                Value::String("scheduled".to_owned()),
            );
        }
    }
    state
}

#[cfg(test)]
mod ui_tests {
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let available = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(
                line.width() <= available,
                "{line:?} exceeded {available} columns"
            );
        }
    }

    #[test]
    fn managed_healthy_status_omits_routine_paths() {
        let state = json!({"status": "up_to_date"});
        let install = json!({
            "managed": true,
            "install_path": "/opt/ctx/bin/ctx"
        });
        let document =
            render_upgrade_status_human(&context(80), "1.0.0", "apply", &state, &install);
        let rendered = document.render_plain();
        assert!(rendered.starts_with("✓ ctx is up to date\n"));
        assert!(
            rendered.contains("State               up to date"),
            "{rendered}"
        );
        assert!(!rendered.contains("up_to_date"), "{rendered}");
        assert!(rendered.contains("Automatic upgrades  apply"));
        assert!(!rendered.contains("/opt/ctx"));
    }

    #[test]
    fn error_and_unmanaged_states_remain_actionable_at_narrow_widths() {
        for width in [32, 48, 80, 120] {
            let context = context(width);
            let error = render_upgrade_status_human(
                &context,
                "1.0.0",
                "off",
                &json!({"status": "error", "error": "replacement verification failed"}),
                &json!({"managed": true}),
            );
            let error_text = error.render_plain();
            assert!(error_text.starts_with("✗ Upgrade needs attention\n"));
            assert!(error_text.contains("ctx doctor\n"));
            assert_fits(&error, &context);

            let unmanaged = render_upgrade_status_human(
                &context,
                "1.0.0",
                "off",
                &json!({"status": "never_checked"}),
                &json!({
                    "managed": false,
                    "marker": "absent",
                    "reason": "ctx was not installed by the hosted installer"
                }),
            );
            let unmanaged_text = unmanaged.render_plain();
            assert!(unmanaged_text.starts_with("! ctx is not managed"));
            assert!(unmanaged_text.contains("hosted installer"));
            assert_fits(&unmanaged, &context);

            let inconsistent = render_upgrade_status_human(
                &context,
                "1.0.0",
                "off",
                &json!({"status": "never_checked"}),
                &json!({
                    "managed": false,
                    "marker": "corrupt",
                    "reason": "ctx install marker hash mismatch"
                }),
            );
            let inconsistent_text = inconsistent.render_plain();
            assert!(inconsistent_text.starts_with("✗ ctx install marker"));
            assert!(inconsistent_text.contains("inconsistent"));
            assert!(inconsistent_text.contains("ctx doctor\n"));
            assert_fits(&inconsistent, &context);
        }
    }
}
