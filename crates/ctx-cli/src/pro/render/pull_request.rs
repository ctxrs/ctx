use ctx_pro_host_protocol::{BlameMatch, PullRequestBlameMatch, PullRequestBlameRelationship};

use crate::ui::{Document, RenderContext, Token};

use super::{
    layout::{
        confidence_token, enum_text, push_authored, push_enum_field, push_field, push_heading,
        push_references, push_resource_primary, push_role_resource, state_token, timestamp_text,
        METADATA_LABEL_WIDTH,
    },
    relationships::render_attribution_groups,
};

pub(super) fn render(document: &mut Document, context: &RenderContext, matches: &[BlameMatch]) {
    let pull_requests = matches.iter().filter_map(|value| match value {
        BlameMatch::PullRequest(value) => Some(value),
        BlameMatch::File(_) | BlameMatch::Commit(_) => None,
    });
    let (commits, activities): (Vec<_>, Vec<_>) = pull_requests
        .partition(|value| matches!(value.relationship, PullRequestBlameRelationship::Commit(_)));

    push_heading(document, 0, "Code produced");
    if commits.is_empty() {
        push_authored(
            document,
            context,
            2,
            "No associated commits on this page.",
            Token::Text,
        );
    } else {
        for (index, value) in commits.into_iter().enumerate() {
            if index > 0 {
                document.push_blank();
            }
            render_commit(document, context, value);
        }
    }

    document.push_blank();
    push_heading(document, 0, "PR activity");
    if activities.is_empty() {
        push_authored(
            document,
            context,
            2,
            "No cited activity on this page.",
            Token::Text,
        );
    } else {
        for (index, value) in activities.into_iter().enumerate() {
            if index > 0 {
                document.push_blank();
            }
            render_activity(document, context, value);
        }
    }
}

fn render_commit(document: &mut Document, context: &RenderContext, value: &PullRequestBlameMatch) {
    let PullRequestBlameRelationship::Commit(commit) = &value.relationship else {
        return;
    };
    push_heading(document, 2, &enum_text(commit.relationship));
    push_resource_primary(document, context, 4, &commit.commit);
    push_references(
        document,
        context,
        6,
        "evidence",
        METADATA_LABEL_WIDTH,
        &commit.evidence_numbers,
    );
    if commit.production.is_empty() {
        push_field(
            document,
            context,
            6,
            "Agent production",
            "Agent production".len(),
            "not proven",
            Token::Warning,
            false,
        );
    } else {
        render_attribution_groups(document, context, 6, &commit.production);
    }
}

fn render_activity(
    document: &mut Document,
    context: &RenderContext,
    value: &PullRequestBlameMatch,
) {
    let PullRequestBlameRelationship::Activity(activity) = &value.relationship else {
        return;
    };
    push_heading(document, 2, &enum_text(activity.action));
    push_resource_primary(document, context, 4, &activity.session);
    if let Some(actor) = &activity.direct_actor {
        push_role_resource(document, context, 6, "direct actor", actor);
    }
    if let Some(root) = &activity.owning_root {
        push_role_resource(document, context, 6, "owning root", root);
    }
    if let Some(time) = activity.fact_occurred_at_ms {
        let time = timestamp_text(time);
        push_field(
            document,
            context,
            6,
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
        6,
        "confidence",
        activity.confidence,
        confidence_token(activity.confidence),
    );
    push_enum_field(
        document,
        context,
        6,
        "state",
        activity.state,
        state_token(activity.state),
    );
    push_references(
        document,
        context,
        6,
        "evidence",
        METADATA_LABEL_WIDTH,
        &activity.evidence_numbers,
    );
}
