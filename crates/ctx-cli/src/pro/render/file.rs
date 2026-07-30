use ctx_pro_host_protocol::{BlameMatch, FileBlameMatch};

use crate::ui::{Document, RenderContext, Token};

use super::{
    layout::{
        line_range_text, push_field, push_heading, push_references, push_resource_primary,
        METADATA_LABEL_WIDTH,
    },
    relationships::render_attribution_groups,
};

pub(super) fn render(document: &mut Document, context: &RenderContext, matches: &[BlameMatch]) {
    if matches.is_empty() {
        push_heading(document, 0, "No committed line matches found");
        return;
    }
    for (index, value) in matches
        .iter()
        .filter_map(|value| match value {
            BlameMatch::File(value) => Some(value),
            BlameMatch::Commit(_) | BlameMatch::PullRequest(_) => None,
        })
        .enumerate()
    {
        if index > 0 {
            document.push_blank();
        }
        render_match(document, context, value);
    }
}

fn render_match(document: &mut Document, context: &RenderContext, value: &FileBlameMatch) {
    push_heading(
        document,
        0,
        &format!("Lines {}", line_range_text(&value.lines)),
    );
    push_resource_primary(document, context, 2, &value.commit);
    push_references(
        document,
        context,
        4,
        "evidence",
        METADATA_LABEL_WIDTH,
        &value.line_evidence_numbers,
    );
    if value.production.is_empty() {
        push_field(
            document,
            context,
            2,
            "Agent production",
            "Agent production".len(),
            "not proven",
            Token::Warning,
            false,
        );
        return;
    }
    document.push_blank();
    render_attribution_groups(document, context, 2, &value.production);
}
