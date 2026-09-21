use ctx_attribution_model::{BlameResult, ResolvedBlameTarget};
use ctx_attribution_model::{BlameResultFreshness, diagnostic::BlameNextAction};
use ctx_terminal::ui::{Action, Document, Hint, RenderContext, Token, hint};

use super::{
    BlameEvidenceContext, commit, evidence, file,
    layout::{push_authored, push_heading, push_notice},
    lineage, pull_request, target,
};
use crate::presentation::blame_summary;

pub(super) fn render(
    result: &BlameResult,
    freshness: Option<BlameResultFreshness>,
    next_action: Option<&BlameNextAction>,
    context: &RenderContext,
    evidence_context: &BlameEvidenceContext,
) -> Document {
    let mut document = Document::new();
    render_contract_summary(&mut document, context, result, freshness);
    if let Some(action) = next_action {
        let command = action
            .argv
            .iter()
            .map(|argument| crate::presentation::shell_quote_arg(argument))
            .collect::<Vec<_>>()
            .join(" ");
        document.push_blank();
        document.append(hint(
            context,
            Hint {
                text: "Search Core history for supporting context.",
            },
            Some(Action { command: &command }),
        ));
    }
    document.push_blank();
    target::render(&mut document, context, result);
    document.push_blank();
    match &result.target {
        ResolvedBlameTarget::File { .. } => file::render(&mut document, context, &result.matches),
        ResolvedBlameTarget::Commit { commit, .. } => {
            if let Some(lineage) = &result.lineage {
                lineage::render(&mut document, context, lineage);
                if commit::has_observations(commit, &result.matches) {
                    document.push_blank();
                    commit::render_observations(&mut document, context, commit, &result.matches);
                }
            } else {
                commit::render(&mut document, context, commit, &result.matches);
            }
        }
        ResolvedBlameTarget::PullRequest { .. } => {
            pull_request::render(&mut document, context, &result.matches)
        }
    }
    evidence::render_continuation(&mut document, context, result);
    evidence::render_list(&mut document, context, result);
    if evidence_context.is_available() {
        evidence::render_previews(&mut document, context, evidence_context.model());
    }
    document
}

/// Renders only host-integrated outcome fields from the validated protocol result.
fn render_contract_summary(
    document: &mut Document,
    context: &RenderContext,
    result: &BlameResult,
    freshness: Option<BlameResultFreshness>,
) {
    push_heading(
        document,
        0,
        blame_summary::outcome_heading(result.outcome.attribution),
    );

    if matches!(
        &result.target,
        ResolvedBlameTarget::File { .. } | ResolvedBlameTarget::PullRequest { .. }
    ) {
        push_authored(
            document,
            context,
            2,
            &blame_summary::coverage_text(&result.outcome.coverage, " on this page"),
            Token::Text,
        );
    }

    if freshness == Some(BlameResultFreshness::StaleCommitted) {
        push_notice(
            document,
            context,
            0,
            "Result is from stale committed history; newer Core history may still be materializing.",
        );
    }
}
