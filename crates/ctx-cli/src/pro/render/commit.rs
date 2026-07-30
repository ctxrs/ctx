use ctx_pro_host_protocol::{
    BlameMatch, CommitBlameMatch, CommitPredicate, FactState, ResourceRef,
};

use crate::ui::{Document, RenderContext, Token};

use super::layout::{
    confidence_token, enum_text, push_authored, push_enum_field, push_field, push_heading,
    push_notice, push_references, push_resource_primary, push_role_resource, same_resource,
    state_token, timestamp_text, METADATA_LABEL_WIDTH,
};

pub(super) fn render(
    document: &mut Document,
    context: &RenderContext,
    target: &ResourceRef,
    matches: &[BlameMatch],
) {
    let commits = matches
        .iter()
        .filter_map(|value| match value {
            BlameMatch::Commit(value) => Some(value),
            BlameMatch::File(_) | BlameMatch::PullRequest(_) => None,
        })
        .collect::<Vec<_>>();
    if commits.is_empty() {
        push_heading(document, 0, "No cited agent attribution found");
        return;
    }
    let (produced, remaining): (Vec<_>, Vec<_>) = commits.into_iter().partition(|value| {
        value.predicate == CommitPredicate::ProducedBy && value.state == FactState::Asserted
    });
    let (possible, also_recorded): (Vec<_>, Vec<_>) = remaining
        .into_iter()
        .partition(|value| value.predicate == CommitPredicate::PossiblyProducedBy);
    let has_produced = !produced.is_empty();
    let has_possible = !possible.is_empty();

    if has_produced {
        push_heading(document, 0, "Produced by");
        for value in produced {
            render_match(document, context, target, value, false);
        }
    }
    if has_possible {
        if has_produced {
            document.push_blank();
        }
        push_heading(document, 0, "Possible producers");
        for value in possible {
            render_match(document, context, target, value, false);
        }
    }
    if !also_recorded.is_empty() {
        if has_produced || has_possible {
            document.push_blank();
        }
        push_heading(document, 0, "Also recorded");
        for value in also_recorded {
            render_match(document, context, target, value, true);
        }
    }
    if matches.iter().any(|value| {
        matches!(
            value,
            BlameMatch::Commit(CommitBlameMatch {
                state: FactState::Ambiguous,
                ..
            })
        )
    }) && !matches.iter().any(|value| {
        matches!(
            value,
            BlameMatch::Commit(CommitBlameMatch {
                predicate: CommitPredicate::ProducedBy,
                state: FactState::Asserted,
                ..
            })
        )
    }) {
        document.push_blank();
        push_notice(document, context, 0, "No producing session is asserted.");
    }
}

fn render_match(
    document: &mut Document,
    context: &RenderContext,
    target: &ResourceRef,
    value: &CommitBlameMatch,
    show_predicate: bool,
) {
    let metadata_indent;
    if show_predicate {
        push_heading(document, 2, &enum_text(value.predicate));
        push_role_resource(document, context, 4, "subject", &value.subject);
        render_object(document, context, value);
        metadata_indent = 4;
    } else if same_resource(&value.subject, target) {
        match &value.object {
            Some(object) => push_resource_primary(document, context, 2, object),
            None => push_authored(
                document,
                context,
                2,
                "Source commit not resolved",
                Token::Text,
            ),
        }
        metadata_indent = 4;
    } else {
        push_heading(document, 2, "Relationship");
        push_role_resource(document, context, 4, "subject", &value.subject);
        render_object(document, context, value);
        metadata_indent = 4;
    }

    if let Some(actor) = &value.direct_actor {
        push_role_resource(document, context, metadata_indent, "direct actor", actor);
    }
    if let Some(root) = &value.owning_root {
        push_role_resource(document, context, metadata_indent, "owning root", root);
    }
    if let Some(time) = value.fact_occurred_at_ms {
        let time = timestamp_text(time);
        push_field(
            document,
            context,
            metadata_indent,
            "occurred",
            METADATA_LABEL_WIDTH,
            &time,
            Token::Text,
            true,
        );
    }
    push_enum_field(
        document,
        context,
        metadata_indent,
        "confidence",
        value.confidence,
        confidence_token(value.confidence),
    );
    push_enum_field(
        document,
        context,
        metadata_indent,
        "state",
        value.state,
        state_token(value.state),
    );
    push_references(
        document,
        context,
        metadata_indent,
        "evidence",
        METADATA_LABEL_WIDTH,
        &value.evidence_numbers,
    );
}

fn render_object(document: &mut Document, context: &RenderContext, value: &CommitBlameMatch) {
    match &value.object {
        Some(object) => push_role_resource(document, context, 4, "object", object),
        None => push_field(
            document,
            context,
            4,
            "object",
            METADATA_LABEL_WIDTH,
            "source commit not resolved",
            Token::Text,
            false,
        ),
    }
}
