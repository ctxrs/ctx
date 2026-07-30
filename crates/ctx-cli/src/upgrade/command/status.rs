use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::{
    config::AppConfig,
    ui::{
        fields, hint, outcome, section, Action, Document, Field, Hint, Outcome, OutcomeState,
        RenderContext, Ui,
    },
};

use super::super::{
    install::{
        current_install_path, managed_install_marker_for_current_exe, InstallMarker,
        ManagedInstallMarker,
    },
    path::{path_diagnostics, PathDiagnostics},
    state::{read_state_json, STATE_SCHEMA_VERSION},
};

pub(super) fn render_status(
    data_root: &Path,
    config: &AppConfig,
    json_output: bool,
    ui: &mut Ui,
) -> Result<()> {
    let state = read_state_json().unwrap_or_else(|| {
        json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "status": "never_checked"
        })
    });
    let current_version = env!("CARGO_PKG_VERSION");
    let current_exe = current_install_path().ok();
    let path_diagnostics = current_exe
        .as_ref()
        .map(|path| path_diagnostics(path, current_version));
    let marker_result = managed_install_marker_for_current_exe();
    let valid_marker = match &marker_result {
        Ok(ManagedInstallMarker::Valid(marker)) => Some(marker),
        _ => None,
    };
    let state = reconcile_scheduled_state(state, valid_marker);
    let marker = match marker_result {
        Ok(ManagedInstallMarker::Valid(marker)) => json!({
            "managed": true,
            "marker": "valid",
            "install_path": marker.install_path,
            "platform": marker.platform,
            "channel": marker.channel,
            "version": marker.version,
            "sha256": marker.sha256,
        }),
        Ok(ManagedInstallMarker::Absent) => json!({
            "managed": false,
            "marker": "absent",
            "reason": "ctx was not installed by the hosted installer"
        }),
        Ok(ManagedInstallMarker::Invalid { reason }) => json!({
            "managed": false,
            "marker": "corrupt",
            "reason": reason,
            "action": "reinstall ctx from https://ctx.rs/install",
        }),
        Err(error) => json!({
            "managed": false,
            "marker": "unavailable",
            "reason": format!("{error:#}"),
        }),
    };
    let pro = crate::pro::lifecycle_status_json(data_root);
    let value = json!({
        "schema_version": 1,
        "command": "upgrade_status",
        "current_version": current_version,
        "auto_upgrade": {
            "mode": config.auto_upgrade_mode().as_str(),
            "enabled": config.auto_upgrade_enabled(),
        },
        "state": state,
        "install": marker,
        "path": path_diagnostics.as_ref().map(PathDiagnostics::json),
        "warnings": path_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.warnings.clone())
            .unwrap_or_default(),
        "pro": pro,
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let document = render_upgrade_status_human(
        ui.stdout_context(),
        current_version,
        config.auto_upgrade_mode().as_str(),
        &state,
        &marker,
        &pro,
    );
    ui.write_stdout(&document)?;
    if let Some(diagnostics) = &path_diagnostics {
        for warning in &diagnostics.warnings {
            let warning = outcome(
                ui.stderr_context(),
                Outcome {
                    state: OutcomeState::Warning,
                    title: warning,
                    detail: None,
                },
            );
            ui.write_stderr(&warning)?;
        }
    }
    Ok(())
}

fn render_upgrade_status_human(
    context: &RenderContext,
    current_version: &str,
    auto_upgrade: &str,
    state: &Value,
    install: &Value,
    pro: &Value,
) -> Document {
    let managed = install.get("managed").and_then(Value::as_bool) == Some(true);
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let (outcome_state, title) = if !managed {
        (
            OutcomeState::Warning,
            "ctx is not managed by the hosted installer",
        )
    } else {
        match status {
            "up_to_date" | "applied" => (OutcomeState::Success, "ctx is up to date"),
            "available" => (OutcomeState::Neutral, "A ctx update is available"),
            "scheduled" => (OutcomeState::Neutral, "A ctx update is scheduled"),
            "error" => (OutcomeState::Error, "Upgrade needs attention"),
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
                Field::new("State", if managed { status } else { "unmanaged" }),
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

    if pro.get("installed").and_then(Value::as_bool) == Some(true) {
        document.push_blank();
        document.append(section(
            "Pro",
            fields(
                context,
                &[
                    Field::new(
                        "State",
                        pro.get("state")
                            .and_then(Value::as_str)
                            .unwrap_or("unavailable"),
                    ),
                    Field::new("Updates", "managed by ctx pro"),
                ],
            ),
        ));
    }

    let action = if managed && status == "available" {
        Some(("Apply the signed update when you are ready.", "ctx upgrade"))
    } else if managed && status == "error" {
        Some((
            "Inspect the local installation and upgrade state.",
            "ctx doctor",
        ))
    } else {
        None
    };
    if let Some((text, command)) = action {
        document.push_blank();
        document.append(hint(context, Hint { text }, Some(Action { command })));
    }
    document
}

fn reconcile_scheduled_state(mut state: Value, marker: Option<&InstallMarker>) -> Value {
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
        let pro = json!({"installed": false});
        let document =
            render_upgrade_status_human(&context(80), "1.0.0", "apply", &state, &install, &pro);
        let rendered = document.render_plain();
        assert!(rendered.starts_with("✓ ctx is up to date\n"));
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
                &json!({"installed": true, "state": "ready"}),
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
                    "reason": "ctx was not installed by the hosted installer"
                }),
                &json!({"installed": false}),
            );
            let unmanaged_text = unmanaged.render_plain();
            assert!(unmanaged_text.starts_with("! ctx is not managed"));
            assert!(unmanaged_text.contains("hosted installer"));
            assert_fits(&unmanaged, &context);
        }
    }
}
