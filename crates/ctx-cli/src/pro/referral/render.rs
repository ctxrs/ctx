use super::share_command;
use crate::{
    pro::commercial_api::{ReferralCreateResult, ReferralPayoutResult, ReferralStatusResult},
    ui::{
        fields, hint, outcome, section, Action, Document, Field, Hint, Outcome as UiOutcome,
        OutcomeState, RenderContext,
    },
};

const PROGRAM: &str =
    "Ordinary customer referrals earn $10/month for 12 paid months, up to $120 per referral.";
const CTA_LEAD: &str = "Refer a developer. Earn $10/month toward your agent bill.";
const CTA_SECONDARY: &str = "Up to $120 per friend.";

pub(super) fn cta(context: &RenderContext) -> Document {
    let mut document = Document::new();
    document.push_blank();
    document.append(outcome(
        context,
        UiOutcome {
            state: OutcomeState::Neutral,
            title: CTA_LEAD,
            detail: Some(CTA_SECONDARY),
        },
    ));
    document.push_blank();
    document.append(hint(
        context,
        Hint {
            text: "Create a stable codename to share.",
        },
        Some(Action {
            command: "ctx referral create <codename>",
        }),
    ));
    document
}

pub(super) fn create(context: &RenderContext, result: &ReferralCreateResult) -> Document {
    let title = if result.disposition == "existing" {
        "Referral codename is already active"
    } else {
        "Referral codename created"
    };
    let command = share_command(&result.codename);
    let mut document = outcome(
        context,
        UiOutcome {
            state: OutcomeState::Success,
            title,
            detail: Some(PROGRAM),
        },
    );
    document.push_blank();
    document.append(section(
        "Referral",
        fields(context, &[Field::new("Codename", &result.codename)]),
    ));
    document.push_blank();
    document.append(hint(
        context,
        Hint {
            text: "Share this command with a new ctx Pro customer. It is a command, not a link.",
        },
        Some(Action { command: &command }),
    ));
    document
}

pub(super) fn status(context: &RenderContext, result: &ReferralStatusResult) -> Document {
    let (state, title) = status_outcome(result);
    let share = share_command(&result.codename);
    let attributed = result.attributed.to_string();
    let subscribed = result.subscribed.to_string();
    let earned = format_usd(result.earned_cents);
    let pending = format_usd(result.pending_cents);
    let manual_review = format_usd(result.manual_review_cents);
    let payable = format_usd(result.payable_cents);
    let processing = format_usd(result.processing_cents);
    let paid = format_usd(result.paid_cents);
    let debt = format_usd(result.debt_cents);
    let mut earnings = vec![Field::new("Earned", &earned)];
    if result.pending_cents > 0 {
        earnings.push(Field::new("Pending", &pending));
    }
    if result.manual_review_cents > 0 {
        earnings.push(Field::new("Under review", &manual_review));
    }
    if result.payable_cents > 0 {
        earnings.push(Field::new("Payable", &payable));
    }
    if result.processing_cents > 0 {
        earnings.push(Field::new("Processing", &processing));
    }
    if result.paid_cents > 0 {
        earnings.push(Field::new("Paid", &paid));
    }
    if result.debt_cents > 0 {
        earnings.push(Field::new("Debt", &debt));
    }
    let mut document = outcome(
        context,
        UiOutcome {
            state,
            title: &title,
            detail: Some(PROGRAM),
        },
    );
    document.push_blank();
    document.append(section(
        "Referral",
        fields(
            context,
            &[
                Field::new("Codename", &result.codename),
                Field::new("Share command", &share),
                Field::new("Attributed", &attributed),
                Field::new("Paid customers", &subscribed),
            ],
        ),
    ));
    document.push_blank();
    document.append(section("Earnings", fields(context, &earnings)));
    document.push_blank();
    document.append(section(
        "Payout",
        fields(
            context,
            &[Field::new(
                "Eligibility",
                payout_eligibility(&result.payout_state),
            )],
        ),
    ));
    document.push_blank();
    if matches!(
        result.payout_state.as_str(),
        "eligible" | "onboarding_pending"
    ) {
        document.append(hint(
            context,
            Hint {
                text: "Set up Stripe-hosted payouts for the payable balance.",
            },
            Some(Action {
                command: "ctx referral payout",
            }),
        ));
    } else if result.pending_cents > 0
        || result.manual_review_cents > 0
        || result.processing_cents > 0
    {
        document.append(hint(
            context,
            Hint {
                text: "No action is required while this balance settles.",
            },
            None,
        ));
    } else {
        document.append(hint(
            context,
            Hint {
                text: "Share the referral command with another new ctx Pro customer.",
            },
            Some(Action { command: &share }),
        ));
    }
    document
}

pub(super) fn payout(
    context: &RenderContext,
    result: &ReferralPayoutResult,
    browser_opened: bool,
) -> Document {
    let mut document = outcome(
        context,
        UiOutcome {
            state: OutcomeState::Neutral,
            title: "Stripe payout setup link created",
            detail: Some(PROGRAM),
        },
    );
    document.push_blank();
    document.append(section(
        "Payout",
        fields(
            context,
            &[
                Field::new("State", payout_state(&result.payout_state)),
                Field::new("Setup link", &result.url),
            ],
        ),
    ));
    document.push_blank();
    let next = if browser_opened {
        None
    } else {
        Some(Action {
            command: &result.url,
        })
    };
    document.append(hint(
        context,
        Hint {
            text: if browser_opened {
                "Finish payout setup in the browser."
            } else {
                "Finish payout setup using the Stripe-hosted link."
            },
        },
        next,
    ));
    document
}

pub(super) fn payout_browser_notice(context: &RenderContext, browser_opened: bool) -> Document {
    outcome(
        context,
        UiOutcome {
            state: if browser_opened {
                OutcomeState::Neutral
            } else {
                OutcomeState::Warning
            },
            title: if browser_opened {
                "Browser open requested for Stripe payout setup."
            } else {
                "A browser could not be opened for Stripe payout setup."
            },
            detail: (!browser_opened).then_some("Use the setup link in the command output."),
        },
    )
}

fn status_outcome(result: &ReferralStatusResult) -> (OutcomeState, String) {
    if result.debt_cents > 0 {
        (
            OutcomeState::Warning,
            format!("Referral debt: {}", format_usd(result.debt_cents)),
        )
    } else if result.manual_review_cents > 0 {
        (
            OutcomeState::Warning,
            format!(
                "Referral earnings under review: {}",
                format_usd(result.manual_review_cents)
            ),
        )
    } else if result.processing_cents > 0 {
        (
            OutcomeState::Neutral,
            format!(
                "Referral payout processing: {}",
                format_usd(result.processing_cents)
            ),
        )
    } else if result.payable_cents > 0 && result.payout_state == "eligible" {
        (
            OutcomeState::Neutral,
            format!(
                "Payout setup is available for {}",
                format_usd(result.payable_cents)
            ),
        )
    } else if result.payable_cents > 0 && result.payout_state == "onboarding_pending" {
        (
            OutcomeState::Warning,
            format!(
                "Payout setup is incomplete for {}",
                format_usd(result.payable_cents)
            ),
        )
    } else if result.payable_cents > 0 && result.payout_state == "ready" {
        (
            OutcomeState::Success,
            "Payout recipient setup is ready".to_owned(),
        )
    } else if result.pending_cents > 0 {
        (
            OutcomeState::Neutral,
            format!(
                "Referral earnings pending: {}",
                format_usd(result.pending_cents)
            ),
        )
    } else if result.paid_cents > 0 {
        (
            OutcomeState::Success,
            format!("Referral payout settled: {}", format_usd(result.paid_cents)),
        )
    } else if result.earned_cents == 0 {
        (OutcomeState::Neutral, "No referral earnings yet".to_owned())
    } else {
        (
            OutcomeState::Success,
            format!("Referral earnings: {}", format_usd(result.earned_cents)),
        )
    }
}

fn payout_eligibility(state: &str) -> &'static str {
    match state {
        "not_eligible" => "Not yet eligible",
        "eligible" => "Eligible; payout setup available",
        "onboarding_pending" => "Eligible; payout setup incomplete",
        "ready" => "Ready for payout",
        "paused" => "Paused",
        _ => "Unavailable",
    }
}

fn payout_state(state: &str) -> &'static str {
    match state {
        "not_eligible" => "Not eligible",
        "eligible" => "Eligible",
        "onboarding_pending" => "Onboarding pending",
        "ready" => "Ready",
        "paused" => "Paused",
        _ => "Unavailable",
    }
}

fn format_usd(cents: u64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
    }

    fn line_fits_or_preserves_copyable_atom(line: &str, maximum: usize) -> bool {
        if line.width() <= maximum {
            return true;
        }
        if line.trim_start().starts_with("ctx ") {
            return true;
        }
        line.split_whitespace().any(|atom| {
            atom.contains("://") || atom.starts_with("--") || uuid::Uuid::parse_str(atom).is_ok()
        })
    }

    fn status_result() -> ReferralStatusResult {
        ReferralStatusResult {
            codename: "agent-smith".to_owned(),
            attributed: 4,
            subscribed: 3,
            earned_cents: 7_000,
            pending_cents: 2_000,
            manual_review_cents: 2_000,
            payable_cents: 2_000,
            processing_cents: 1_000,
            paid_cents: 2_000,
            debt_cents: 2_000,
            currency: "usd".to_owned(),
            payout_state: "paused".to_owned(),
        }
    }

    #[test]
    fn create_distinguishes_the_share_command_from_a_link() {
        let result = ReferralCreateResult {
            codename: "agent-smith".to_owned(),
            disposition: "created".to_owned(),
        };
        assert_eq!(
            create(&context(80), &result).render_plain(),
            concat!(
                "✓ Referral codename created\n",
                "Ordinary customer referrals earn $10/month for 12 paid months, up to $120 per\n",
                "referral.\n\n",
                "Referral\n",
                "Codename  agent-smith\n\n",
                "Hint: Share this command with a new ctx Pro customer. It is a command, not a\n",
                "      link.\n\n",
                "Next\n",
                "  ctx pro --referral agent-smith\n",
            )
        );
    }

    #[test]
    fn status_leads_with_state_and_keeps_earned_pending_and_paid_distinct() {
        let rendered = status(&context(120), &status_result()).render_plain();
        assert!(rendered.starts_with("! Referral debt: $20.00\n"));
        assert!(rendered.contains("Share command   ctx pro --referral agent-smith"));
        assert!(rendered.contains("Earned        $70.00"));
        assert!(rendered.contains("Pending       $20.00"));
        assert!(rendered.contains("Paid          $20.00"));
        assert!(rendered.contains("Eligibility  Paused"));
    }

    #[test]
    fn status_omits_zero_value_ledger_rows_but_keeps_earned() {
        let mut result = status_result();
        result.earned_cents = 0;
        result.pending_cents = 0;
        result.manual_review_cents = 0;
        result.payable_cents = 0;
        result.processing_cents = 0;
        result.paid_cents = 0;
        result.debt_cents = 0;
        result.payout_state = "not_eligible".to_owned();
        let rendered = status(&context(120), &result).render_plain();
        let earnings = rendered
            .split_once("\nEarnings\n")
            .unwrap()
            .1
            .split_once("\nPayout\n")
            .unwrap()
            .0;
        assert_eq!(earnings, "Earned  $0.00\n");
    }

    #[test]
    fn payout_calls_the_dynamic_value_a_link_and_sanitizes_it() {
        let result = ReferralPayoutResult {
            kind: "payout_onboarding_created".to_owned(),
            payout_state: "onboarding_pending".to_owned(),
            url: "https://connect.stripe.test/setup\u{1b}[2J".to_owned(),
            expires_at_unix: 1,
        };
        let rendered = payout(&context(120), &result, false).render_plain();
        assert!(rendered.contains("Setup link  https://connect.stripe.test/setup\\x1b[2J"));
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains("Share link"));
        assert!(rendered.contains("\nNext\n  https://connect.stripe.test/setup\\x1b[2J\n"));

        let opened = payout(&context(120), &result, true).render_plain();
        assert!(opened.contains("Finish payout setup in the browser."));
        assert!(!opened.contains("\nNext\n"));
    }

    #[test]
    fn referral_renderers_fit_supported_widths() {
        let create_result = ReferralCreateResult {
            codename: "agent-smith".to_owned(),
            disposition: "existing".to_owned(),
        };
        let payout_result = ReferralPayoutResult {
            kind: "payout_onboarding_created".to_owned(),
            payout_state: "onboarding_pending".to_owned(),
            url: "https://connect.stripe.test/setup/session".to_owned(),
            expires_at_unix: 1,
        };
        for width in [32, 48, 80, 120] {
            let context = context(width);
            for document in [
                cta(&context),
                create(&context, &create_result),
                status(&context, &status_result()),
                payout(&context, &payout_result, false),
            ] {
                let maximum = context.content_width().unwrap_or(1);
                assert!(document
                    .render_plain()
                    .lines()
                    .all(|line| line_fits_or_preserves_copyable_atom(line, maximum)));
            }
        }
    }
}
