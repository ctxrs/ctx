use std::io;

use anyhow::{bail, Result};

use super::{LocalProDataOutcome, ProManagePlan, UninstallDataDisposition, UNINSTALL_DATA_PROMPT};
use crate::{
    pro::PRO_MONTHLY_PRICE_DISPLAY,
    ui::{
        fields, hint, outcome, section, Action, Document, Field, Hint, Outcome as UiOutcome,
        OutcomeState, RenderContext, Ui,
    },
};

pub(super) fn prompt_uninstall_data_disposition(
    input: &mut impl io::BufRead,
    ui: &mut Ui,
) -> Result<UninstallDataDisposition> {
    loop {
        ui.write_stderr(&uninstall_prompt(ui.stderr_context()))?;
        ui.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            bail!("cancelled: uninstall confirmation was not provided");
        }
        match answer.trim() {
            "" | "y" | "Y" | "yes" | "YES" => return Ok(UninstallDataDisposition::Delete),
            "n" | "N" | "no" | "NO" => return Ok(UninstallDataDisposition::Keep),
            _ => ui.write_stderr(&uninstall_answer_required(ui.stderr_context()))?,
        }
    }
}

pub(super) fn uninstall_prompt(context: &RenderContext) -> Document {
    outcome(
        context,
        UiOutcome {
            state: OutcomeState::Warning,
            title: UNINSTALL_DATA_PROMPT,
            detail: Some("Canonical ctx history is always preserved."),
        },
    )
}

pub(super) fn uninstall_answer_required(context: &RenderContext) -> Document {
    outcome(
        context,
        UiOutcome {
            state: OutcomeState::Warning,
            title: "Please answer y or n.",
            detail: None,
        },
    )
}

pub(super) fn setup(context: &RenderContext, account_state: &str) -> Document {
    let title = if account_state == "trial" {
        "ctx Pro trial is ready"
    } else {
        "ctx Pro is ready"
    };
    let product = if account_state == "trial" {
        "Free ctx Pro trial"
    } else {
        PRO_MONTHLY_PRICE_DISPLAY
    };
    let mut document = outcome(
        context,
        UiOutcome {
            state: OutcomeState::Success,
            title,
            detail: None,
        },
    );
    document.push_blank();
    document.append(section(
        "Pro",
        fields(
            context,
            &[
                Field::new("Product", product),
                Field::new("Access", access_state(account_state)),
                Field::new("Work graph", "Ready"),
            ],
        ),
    ));
    document.push_blank();
    document.append(hint(
        context,
        Hint {
            text: "Manage trial, account, or billing details.",
        },
        Some(Action {
            command: "ctx pro manage",
        }),
    ));
    document
}

pub(super) fn manage(
    context: &RenderContext,
    plan: &ProManagePlan,
    browser_opened: bool,
) -> Document {
    let product = if plan.access_state == "trial" {
        "Free ctx Pro trial"
    } else {
        PRO_MONTHLY_PRICE_DISPLAY
    };
    let graph = if plan.access_state == "locked" {
        "Preserved locally; access is locked"
    } else {
        "Available locally"
    };
    let field_values = [
        Field::new("Product", product),
        Field::new("Access", access_state(&plan.access_state)),
        Field::new("Work graph", graph),
        Field::new("Management link", &plan.portal_url),
    ];
    let mut document = outcome(
        context,
        UiOutcome {
            state: if plan.access_state == "locked" {
                OutcomeState::Warning
            } else {
                OutcomeState::Success
            },
            title: "ctx Pro account management is ready",
            detail: None,
        },
    );
    document.push_blank();
    document.append(section("Pro", fields(context, &field_values)));
    document.push_blank();
    let next = if browser_opened {
        None
    } else {
        Some(Action {
            command: &plan.portal_url,
        })
    };
    document.append(hint(
        context,
        Hint {
            text: if browser_opened {
                "Finish account or billing changes in the browser."
            } else {
                "Open the management link to change billing or restore access."
            },
        },
        next,
    ));
    document
}

pub(super) fn browser_notice(
    context: &RenderContext,
    browser_opened: bool,
    destination: &str,
) -> Document {
    let title = if browser_opened {
        format!("Browser open requested for {destination}.")
    } else {
        format!("A browser could not be opened for {destination}.")
    };
    outcome(
        context,
        UiOutcome {
            state: if browser_opened {
                OutcomeState::Neutral
            } else {
                OutcomeState::Warning
            },
            title: &title,
            detail: (!browser_opened).then_some("Use the link in the command output."),
        },
    )
}

pub(super) fn uninstall(
    context: &RenderContext,
    helper_removed: bool,
    data_outcome: LocalProDataOutcome,
) -> Document {
    let (title, data) = match data_outcome {
        LocalProDataOutcome::Deleted => ("ctx Pro was removed", "Deleted"),
        LocalProDataOutcome::Preserved => ("ctx Pro was removed", "Preserved locally"),
        LocalProDataOutcome::Absent if helper_removed => {
            ("ctx Pro was removed", "No local Pro data found")
        }
        LocalProDataOutcome::Absent => (
            "No ctx Pro installation or local Pro data was found",
            "No local Pro data found",
        ),
    };
    let mut document = outcome(
        context,
        UiOutcome {
            state: OutcomeState::Success,
            title,
            detail: None,
        },
    );
    document.push_blank();
    document.append(section(
        "Local data",
        fields(
            context,
            &[
                Field::new("Pro graph", data),
                Field::new("Canonical ctx history", "Preserved"),
            ],
        ),
    ));
    document.push_blank();
    match data_outcome {
        LocalProDataOutcome::Deleted => document.append(hint(
            context,
            Hint {
                text: "Set up Pro again to rebuild local Pro data.",
            },
            Some(Action { command: "ctx pro" }),
        )),
        LocalProDataOutcome::Preserved => document.append(hint(
            context,
            Hint {
                text: "Set up Pro again to restore the preserved graph.",
            },
            Some(Action { command: "ctx pro" }),
        )),
        LocalProDataOutcome::Absent => document.append(hint(
            context,
            Hint {
                text: "No further action is required.",
            },
            None,
        )),
    }
    document
}

fn access_state(state: &str) -> &'static str {
    match state {
        "trial" => "Trial active",
        "active" => "Active",
        "canceling_paid" => "Active; cancellation scheduled",
        "offline_grace" => "Offline grace",
        "locked" => "Locked",
        _ => "Unavailable",
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
        let maximum = context.content_width().unwrap_or(1);
        assert!(document
            .render_plain()
            .lines()
            .all(|line| line.width() <= maximum));
    }

    #[test]
    fn setup_is_outcome_first_and_has_one_next_command() {
        assert_eq!(
            setup(&context(80), "trial").render_plain(),
            concat!(
                "✓ ctx Pro trial is ready\n\n",
                "Pro\n",
                "Product     Free ctx Pro trial\n",
                "Access      Trial active\n",
                "Work graph  Ready\n\n",
                "Hint: Manage trial, account, or billing details.\n\n",
                "Next\n",
                "  ctx pro manage\n",
            )
        );
    }

    #[test]
    fn manage_sanitizes_the_link_and_uninstall_states_are_truthful() {
        let plan = ProManagePlan {
            portal_url: "https://billing.example.test/session\u{1b}[2J".to_owned(),
            access_state: "locked".to_owned(),
            refresh_after_unix: None,
            access_deadline_unix: None,
            grace_deadline_unix: None,
        };
        let rendered = manage(&context(120), &plan, false).render_plain();
        assert!(rendered.starts_with("! ctx Pro account management is ready\n"));
        assert!(rendered.contains("Management link"));
        assert!(rendered.contains("\\x1b"));
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.contains("Preserved locally; access is locked"));
        assert!(rendered.contains("\nNext\n  https://billing.example.test/session\\x1b[2J\n"));

        let opened = manage(&context(120), &plan, true).render_plain();
        assert!(opened.contains("Finish account or billing changes in the browser."));
        assert!(!opened.contains("\nNext\n"));

        assert!(uninstall(&context(80), true, LocalProDataOutcome::Deleted)
            .render_plain()
            .contains("Pro graph              Deleted"));
        assert!(
            uninstall(&context(80), true, LocalProDataOutcome::Preserved)
                .render_plain()
                .contains("Pro graph              Preserved locally")
        );
        assert!(uninstall(&context(80), false, LocalProDataOutcome::Absent)
            .render_plain()
            .contains("No further action is required."));
    }

    #[test]
    fn lifecycle_renderers_fit_supported_widths() {
        let plan = ProManagePlan {
            portal_url: "https://billing.example.test/session".to_owned(),
            access_state: "trial".to_owned(),
            refresh_after_unix: Some(100),
            access_deadline_unix: Some(200),
            grace_deadline_unix: None,
        };
        for width in [32, 48, 80, 120] {
            let context = context(width);
            for document in [
                setup(&context, "trial"),
                manage(&context, &plan, false),
                uninstall(&context, true, LocalProDataOutcome::Deleted),
                uninstall_prompt(&context),
            ] {
                assert_fits(&document, &context);
            }
        }
    }
}
