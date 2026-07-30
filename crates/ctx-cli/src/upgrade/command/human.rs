use anyhow::Result;

use crate::ui::{
    fields, hint, outcome, section, Action, Document, Field, Hint, Outcome as UiOutcome,
    OutcomeState, RenderContext, Ui,
};

use super::UpgradeOutcome;

pub(super) fn render_auto_mode(enabled: bool, json_output: bool, ui: &mut Ui) -> Result<()> {
    if json_output {
        return Ok(());
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
            detail: Some(if enabled {
                "ctx will apply signed updates in the background."
            } else {
                "Run `ctx upgrade` whenever you want to update."
            }),
        },
    );
    ui.write_stdout(&document)?;
    Ok(())
}

pub(super) fn render_outcome(
    upgrade: &UpgradeOutcome,
    json_output: bool,
    ui: &mut Ui,
) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(&upgrade.json())?);
        return Ok(());
    }

    let document = render_upgrade_outcome_human(ui.stdout_context(), upgrade);
    ui.write_stdout(&document)?;
    for warning in &upgrade.warnings {
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

fn render_upgrade_outcome_human(context: &RenderContext, upgrade: &UpgradeOutcome) -> Document {
    let state = match upgrade.status {
        "up_to_date" | "applied" => OutcomeState::Success,
        "available" | "dry_run" | "scheduled" => OutcomeState::Neutral,
        _ => OutcomeState::Neutral,
    };
    let mut document = outcome(
        context,
        UiOutcome {
            state,
            title: &upgrade.message,
            detail: None,
        },
    );

    if let Some(plan) = &upgrade.plan {
        document.push_blank();
        document.append(section(
            "Release",
            fields(
                context,
                &[
                    Field::new("Current", &plan.current_version),
                    Field::new("Latest", &plan.latest_version),
                    Field::new("Channel", &plan.channel),
                ],
            ),
        ));
    }

    let next = match upgrade.status {
        "available" | "dry_run" => Some("ctx upgrade"),
        _ => None,
    };
    if let Some(command) = next {
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Apply the signed update when you are ready.",
            },
            Some(Action { command }),
        ));
    }
    document
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
    }

    #[test]
    fn available_upgrade_is_outcome_first_and_actionable() {
        let upgrade = UpgradeOutcome {
            command: "upgrade_check",
            status: "available",
            message: "ctx 1.1.0 is available (current 1.0.0, channel stable).".to_owned(),
            plan: None,
            applied: false,
            dry_run: false,
            warnings: Vec::new(),
            attempt_id: None,
        };
        for width in [32, 48, 80, 120] {
            let context = context(width);
            let document = render_upgrade_outcome_human(&context, &upgrade);
            let rendered = document.render_plain();
            assert!(rendered.starts_with("ctx 1.1.0 is available"));
            assert!(rendered.contains("ctx upgrade\n"));
            let available = context.content_width().unwrap_or(1);
            for line in rendered.lines() {
                assert!(
                    line.width() <= available,
                    "{line:?} exceeded {available} columns"
                );
            }
        }
    }

    #[test]
    fn applied_upgrade_has_no_redundant_next_step() {
        let upgrade = UpgradeOutcome {
            command: "upgrade",
            status: "applied",
            message: "Upgraded ctx 1.0.0 to 1.1.0.".to_owned(),
            plan: None,
            applied: true,
            dry_run: false,
            warnings: Vec::new(),
            attempt_id: None,
        };
        let rendered = render_upgrade_outcome_human(&context(80), &upgrade).render_plain();
        assert_eq!(rendered, "✓ Upgraded ctx 1.0.0 to 1.1.0.\n");
    }
}
