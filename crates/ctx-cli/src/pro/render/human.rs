use ctx_pro_host_protocol::{BlameResult, ResolvedBlameTarget};

use crate::ui::{Document, RenderContext};

use super::{commit, evidence, file, pull_request, target, BlameEvidenceContext};

pub(super) fn render(
    result: &BlameResult,
    context: &RenderContext,
    evidence_context: &BlameEvidenceContext,
) -> Document {
    let mut document = Document::new();
    target::render(&mut document, context, result);
    document.push_blank();
    match &result.target {
        ResolvedBlameTarget::File { .. } => file::render(&mut document, context, &result.matches),
        ResolvedBlameTarget::Commit { commit, .. } => {
            commit::render(&mut document, context, commit, &result.matches)
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
