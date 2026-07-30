use crate::{
    pro::PRO_MONTHLY_PRICE_DISPLAY,
    ui::{
        fields, hint, outcome, progress, Action, Document, Field, Hint, Outcome as UiOutcome,
        OutcomeState, Progress, RenderContext,
    },
};

pub(super) fn device_sign_in(
    context: &RenderContext,
    verification_uri: &str,
    user_code: &str,
) -> Document {
    let mut document = outcome(
        context,
        UiOutcome {
            state: OutcomeState::Neutral,
            title: "Sign in to ctx Pro",
            detail: None,
        },
    );
    document.push_blank();
    document.append(fields(
        context,
        &[
            Field::new("Sign-in link", verification_uri),
            Field::new("Code", user_code),
        ],
    ));
    document.push_blank();
    document.append(hint(
        context,
        Hint {
            text: "Open the sign-in link and enter the code.",
        },
        Some(Action {
            command: verification_uri,
        }),
    ));
    document
}

pub(super) fn paid_checkout_prompt(context: &RenderContext, url: &str) -> Document {
    let mut document = outcome(
        context,
        UiOutcome {
            state: OutcomeState::Neutral,
            title: "Complete checkout to continue",
            detail: None,
        },
    );
    document.push_blank();
    document.append(fields(
        context,
        &[
            Field::new("Product", PRO_MONTHLY_PRICE_DISPLAY),
            Field::new("Checkout link", url),
        ],
    ));
    document.push_blank();
    document.append(hint(
        context,
        Hint {
            text: "Complete the Stripe-hosted checkout.",
        },
        Some(Action { command: url }),
    ));
    document
}

pub(super) fn trial_conversion(context: &RenderContext) -> Document {
    outcome(
        context,
        UiOutcome {
            state: OutcomeState::Warning,
            title: "The free Pro trial is unavailable for this device",
            detail: Some("Sign in to continue with paid Pro."),
        },
    )
}

pub(super) fn checkout_progress(
    context: &RenderContext,
    label: &str,
    detail: Option<&str>,
) -> Document {
    progress(
        context,
        Progress {
            label,
            current: 0,
            total: None,
            detail,
        },
    )
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
            detail: (!browser_opened).then_some("Use the link shown above."),
        },
    )
}
