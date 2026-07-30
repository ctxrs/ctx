use ctx_pro_host_protocol::{BlameResult, ResolvedBlameTarget};

use crate::ui::{Document, RenderContext};

use super::{commit, evidence, file, pull_request, target};

pub(super) fn render(result: &BlameResult, context: &RenderContext) -> Document {
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
    document
}
