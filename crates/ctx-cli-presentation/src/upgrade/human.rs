use anyhow::Result;
use serde_json::{json, Value};

use crate::output::print_json;
use crate::ui::{
    fields, hint, outcome, section, Action, Document, Field, Hint, Outcome as UiOutcome,
    OutcomeState, RenderContext, Ui,
};

use ctx_upgrade_engine::UpgradeOutcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoModeInstallAuthority {
    Hosted,
    External,
    Inconsistent,
}

pub fn render_auto_mode(
    enabled: bool,
    authority: AutoModeInstallAuthority,
    json_output: bool,
    ui: &mut Ui,
) -> Result<()> {
    if json_output {
        return print_json(auto_mode_json(enabled));
    }
    let document = outcome(
        ui.stdout_context(),
        UiOutcome {
            state: OutcomeState::Success,
            title: if enabled {
                "Automatic upgrades enabled"
            } else {
                "Automatic upgrades disabled"
            },
            detail: Some(match (enabled, authority) {
                (true, _) => "ctx will apply signed updates in the background.",
                (false, AutoModeInstallAuthority::Hosted) => {
                    "Run `ctx upgrade` whenever you want to update."
                }
                (false, AutoModeInstallAuthority::External) => {
                    "Use the tool or process that installed ctx when you want to update."
                }
                (false, AutoModeInstallAuthority::Inconsistent) => {
                    "Run `ctx doctor` before changing or updating this installation."
                }
            }),
        },
    );
    ui.write_stdout(&document)?;
    Ok(())
}

fn auto_mode_json(enabled: bool) -> Value {
    json!({
        "schema_version": 1,
        "command": if enabled { "upgrade_enable" } else { "upgrade_disable" },
        "ok": true,
        "status": if enabled { "enabled" } else { "disabled" },
        "auto": if enabled { "apply" } else { "off" },
        "enabled": enabled,
    })
}

pub fn render_outcome(upgrade: &UpgradeOutcome, json_output: bool, ui: &mut Ui) -> Result<()> {
    if json_output {
        let output = format!(
            "{}\n",
            serde_json::to_string_pretty(&outcome_json(upgrade))?
        );
        ui.write_stdout_bytes(output.as_bytes())?;
        return Ok(());
    }

    let document = render_upgrade_outcome_human(ui.stdout_context(), upgrade);
    ui.write_stdout(&document)?;
    for warning in upgrade.warnings() {
        let warning = outcome(
            ui.stderr_context(),
            UiOutcome {
                state: OutcomeState::Warning,
                title: warning,
                detail: None,
            },
        );
        ui.write_stderr(&warning)?;
    }
    Ok(())
}

fn outcome_json(upgrade: &UpgradeOutcome) -> Value {
    let plan = upgrade.plan();
    json!({
        "schema_version": 1,
        "command": upgrade.command(),
        "ok": true,
        "status": upgrade.status(),
        "message": upgrade.message(),
        "current_version": plan.map(|plan| if upgrade.applied() {
            plan.latest_version()
        } else {
            plan.current_version()
        }),
        "latest_version": plan.map(ctx_upgrade_engine::UpgradePlan::latest_version),
        "update_available": plan
            .map(|plan| !upgrade.applied() && plan.update_available())
            .unwrap_or(false),
        "update_was_available": plan
            .map(ctx_upgrade_engine::UpgradePlan::update_available)
            .unwrap_or(false),
        "channel": plan.map(ctx_upgrade_engine::UpgradePlan::channel),
        "platform": plan.map(ctx_upgrade_engine::UpgradePlan::platform),
        "metadata_url": plan.map(ctx_upgrade_engine::UpgradePlan::metadata_url),
        "artifact_url": plan.map(ctx_upgrade_engine::UpgradePlan::artifact_url),
        "install_path": plan.map(|plan| plan.install_path().display().to_string()),
        "managed": plan.map(ctx_upgrade_engine::UpgradePlan::managed).unwrap_or(false),
        "applied": upgrade.applied(),
        "dry_run": upgrade.dry_run(),
        "warnings": upgrade.warnings(),
        "upgrade_attempt_id": upgrade.attempt_id(),
    })
}

pub fn render_error(result: Result<()>, human_output: bool, ui: &mut Ui) -> Result<()> {
    if !human_output {
        return result;
    }
    let error = match result {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let Some(document) = render_upgrade_integrity_error_human(ui.stderr_context(), &error) else {
        return Err(error);
    };
    ui.write_stderr(&document)?;
    Err(crate::rendered_cli_error())
}

fn render_upgrade_integrity_error_human(
    context: &RenderContext,
    error: &anyhow::Error,
) -> Option<Document> {
    let integrity_failure = error.chain().any(|cause| {
        let message = cause.to_string().to_ascii_lowercase();
        message.contains("artifact checksum mismatch")
            || message.contains("checksum does not match signed metadata")
    });
    if !integrity_failure {
        return None;
    }

    let mut document = outcome(
        context,
        UiOutcome {
            state: OutcomeState::Error,
            title: "Upgrade integrity check failed",
            detail: Some(
                "The artifact did not match signed release metadata. The installed ctx version was not changed.",
            ),
        },
    );
    document.push_blank();
    document.append(hint(
        context,
        Hint {
            text: "Retry the signed download; ctx will verify it before installation.",
        },
        Some(Action {
            command: "ctx upgrade",
        }),
    ));
    Some(document)
}

fn render_upgrade_outcome_human(context: &RenderContext, upgrade: &UpgradeOutcome) -> Document {
    let state = match upgrade.status() {
        "up_to_date" | "applied" => OutcomeState::Success,
        "available" | "dry_run" | "scheduled" => OutcomeState::Neutral,
        _ => OutcomeState::Neutral,
    };
    let mut document = outcome(
        context,
        UiOutcome {
            state,
            title: upgrade.message(),
            detail: None,
        },
    );

    if let Some(plan) = upgrade.plan() {
        let current_version = displayed_current_version(
            upgrade.applied(),
            plan.current_version(),
            plan.latest_version(),
        );
        document.push_blank();
        document.append(section(
            "Release",
            fields(
                context,
                &[
                    Field::new("Current", current_version),
                    Field::new("Latest", plan.latest_version()),
                    Field::new("Channel", plan.channel()),
                ],
            ),
        ));
    }

    let managed = upgrade.plan().map(|plan| plan.managed()).unwrap_or(true);
    if let Some((text, command)) = upgrade_next_step(upgrade.status(), managed) {
        document.push_blank();
        document.append(hint(
            context,
            Hint { text },
            command.map(|command| Action { command }),
        ));
    }
    document
}

fn upgrade_next_step(status: &str, managed: bool) -> Option<(&'static str, Option<&'static str>)> {
    match (status, managed) {
        ("available" | "dry_run", true) => Some((
            "Apply the signed update when you are ready.",
            Some("ctx upgrade"),
        )),
        ("available", false) => Some((
            "Use the tool or process that installed ctx to apply this update.",
            None,
        )),
        _ => None,
    }
}

fn displayed_current_version<'a>(
    applied: bool,
    current_version: &'a str,
    latest_version: &'a str,
) -> &'a str {
    if applied {
        latest_version
    } else {
        current_version
    }
}

#[cfg(test)]
mod tests {
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
    fn available_upgrade_is_outcome_first_and_actionable() {
        let upgrade = UpgradeOutcome::for_test(
            "upgrade_check",
            "available",
            "ctx 1.1.0 is available (current 1.0.0, channel stable).",
            false,
        );
        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document = render_upgrade_outcome_human(&context, &upgrade);
            let rendered = document.render_plain();
            assert!(rendered.starts_with("ctx 1.1.0 is available"));
            assert!(rendered.contains("ctx upgrade\n"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn applied_upgrade_has_no_redundant_next_step() {
        let upgrade =
            UpgradeOutcome::for_test("upgrade", "applied", "Upgraded ctx 1.0.0 to 1.1.0.", true);
        let rendered = render_upgrade_outcome_human(&context(80), &upgrade).render_plain();
        assert_eq!(rendered, "✓ Upgraded ctx 1.0.0 to 1.1.0.\n");
        assert_eq!(displayed_current_version(true, "1.0.0", "1.1.0"), "1.1.0");
        assert_eq!(displayed_current_version(false, "1.0.0", "1.1.0"), "1.0.0");
    }

    #[test]
    fn checksum_failure_reports_signed_metadata_mismatch_and_unchanged_install() {
        let error = anyhow::anyhow!(
            "artifact checksum mismatch: expected {}, got {}",
            "a".repeat(64),
            "b".repeat(64)
        )
        .context("download https://cli.ctx.rs/releases/ctx-linux-x64.tar.gz");
        let expected_words = concat!(
            "✗ Upgrade integrity check failed ",
            "The artifact did not match signed release metadata. ",
            "The installed ctx version was not changed. ",
            "Hint: Retry the signed download; ctx will verify it before installation. ",
            "Next ctx upgrade"
        );

        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document = render_upgrade_integrity_error_human(&context, &error).unwrap();
            let rendered = document.render_plain();
            assert_eq!(
                rendered.split_whitespace().collect::<Vec<_>>().join(" "),
                expected_words,
                "width {width}"
            );
            assert_eq!(rendered.matches("ctx upgrade").count(), 1, "width {width}");
            assert!(!rendered.contains("https://"), "{rendered}");
            assert!(!rendered.contains(&"a".repeat(64)), "{rendered}");
            assert_fits(&document, &context);
        }

        assert!(render_upgrade_integrity_error_human(
            &context(80),
            &anyhow::anyhow!("download release metadata")
        )
        .is_none());
        assert!(render_upgrade_integrity_error_human(
            &context(80),
            &anyhow::anyhow!("ctx artifact checksum does not match signed metadata")
        )
        .is_some());
    }

    #[test]
    fn machine_integrity_error_remains_unrendered() {
        let stdout_context =
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Never));
        let stderr_context =
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Never));
        let mut ui = Ui::with_writers(
            std::io::sink(),
            stdout_context,
            std::io::sink(),
            stderr_context,
        );
        let error = render_error(
            Err(anyhow::anyhow!(
                "artifact checksum mismatch: machine output sentinel"
            )),
            false,
            &mut ui,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "artifact checksum mismatch: machine output sentinel"
        );
    }

    #[test]
    fn automatic_mode_json_is_one_complete_machine_receipt() {
        assert_eq!(
            auto_mode_json(true),
            serde_json::json!({
                "schema_version": 1,
                "command": "upgrade_enable",
                "ok": true,
                "status": "enabled",
                "auto": "apply",
                "enabled": true,
            })
        );
        assert_eq!(
            auto_mode_json(false),
            serde_json::json!({
                "schema_version": 1,
                "command": "upgrade_disable",
                "ok": true,
                "status": "disabled",
                "auto": "off",
                "enabled": false,
            })
        );
    }

    #[test]
    fn external_available_upgrade_uses_owner_neutral_guidance() {
        assert_eq!(
            upgrade_next_step("available", false),
            Some((
                "Use the tool or process that installed ctx to apply this update.",
                None,
            ))
        );
        assert_eq!(upgrade_next_step("dry_run", false), None);
    }
}
