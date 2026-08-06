use ctx_pro_host_protocol::{
    CommitLineage, CommitLineageBounds, CommitLineageEdge, CommitLineageOmission,
    CommitLineageState, CommitLineageYield, ExactCommitRef, ScopedCommitEndpoint,
};

use crate::ui::{Document, RenderContext, Token};

use super::layout::{
    enum_heading, enum_text, push_field, push_heading, push_notice, push_references,
    push_role_resource, timestamp_text, METADATA_LABEL_WIDTH,
};

pub(super) fn render(document: &mut Document, context: &RenderContext, lineage: &CommitLineage) {
    let completeness = if lineage.complete {
        "complete"
    } else {
        "partial"
    };
    let status = if lineage.ambiguous {
        format!("Lineage · {completeness} · ambiguous")
    } else {
        format!("Lineage · {completeness}")
    };
    push_heading(document, 0, &status);

    for edge in &lineage.edges {
        render_edge(document, context, edge);
    }
    for yielded_by in &lineage.yielded_by {
        render_yield(document, context, yielded_by);
    }

    if lineage.edges.is_empty() && lineage.yielded_by.is_empty() {
        push_notice(
            document,
            context,
            2,
            "No exact operation or yield evidence was returned.",
        );
    }

    if let Some(origin) = &lineage.origin {
        document.push_blank();
        push_heading(document, 2, "Origin");
        push_exact_commit(document, context, 4, "commit", origin);
    }
    if let Some(endpoint) = &lineage.endpoint {
        document.push_blank();
        render_endpoint(document, context, endpoint);
    }

    if !lineage.complete {
        document.push_blank();
        push_notice(
            document,
            context,
            2,
            &omission_notice(&lineage.bounds.omission),
        );
        render_bounds(document, context, &lineage.bounds);
    }
    if lineage.ambiguous {
        document.push_blank();
        push_notice(
            document,
            context,
            2,
            "Lineage is ambiguous. No unique origin or endpoint is claimed.",
        );
    } else if !lineage.complete {
        push_notice(
            document,
            context,
            2,
            "No unique origin or endpoint is claimed for this partial result.",
        );
    }

    document.push_blank();
    push_field(
        document,
        context,
        2,
        "implementation",
        METADATA_LABEL_WIDTH,
        "commit lineage does not establish who implemented the code",
        Token::Text,
        false,
    );
}

fn render_edge(document: &mut Document, context: &RenderContext, edge: &CommitLineageEdge) {
    let heading = format!(
        "{} · {}",
        enum_heading(edge.kind),
        enum_text(edge.relation_class)
    );
    push_heading(document, 2, &heading);
    push_exact_commit(document, context, 4, "source", &edge.source);
    push_exact_commit(document, context, 4, "result", &edge.result);
    push_field(
        document,
        context,
        4,
        "outcome",
        METADATA_LABEL_WIDTH,
        "operation yielded this commit",
        state_token(edge.state),
        false,
    );
    push_role_resource(document, context, 4, "actor", &edge.actor);
    push_field(
        document,
        context,
        4,
        "proof",
        METADATA_LABEL_WIDTH,
        &enum_text(edge.proof_class),
        Token::Text,
        false,
    );
    render_non_asserted_state(document, context, 4, edge.state);
    if let Some(observed_at_ms) = edge.observed_at_ms {
        push_field(
            document,
            context,
            4,
            "observed",
            METADATA_LABEL_WIDTH,
            &timestamp_text(observed_at_ms),
            Token::Text,
            true,
        );
    }
    push_field(
        document,
        context,
        4,
        "operation id",
        METADATA_LABEL_WIDTH,
        &edge.operation_id,
        Token::Text,
        true,
    );
    push_references(
        document,
        context,
        4,
        "evidence",
        METADATA_LABEL_WIDTH,
        &edge.evidence_numbers,
    );
}

fn render_yield(document: &mut Document, context: &RenderContext, yielded_by: &CommitLineageYield) {
    push_heading(document, 2, "Yield record");
    push_field(
        document,
        context,
        4,
        "outcome",
        METADATA_LABEL_WIDTH,
        "operation yielded this commit",
        state_token(yielded_by.state),
        false,
    );
    push_role_resource(document, context, 4, "actor", &yielded_by.actor);
    push_field(
        document,
        context,
        4,
        "proof",
        METADATA_LABEL_WIDTH,
        &enum_text(yielded_by.proof_class),
        Token::Text,
        false,
    );
    render_non_asserted_state(document, context, 4, yielded_by.state);
    if let Some(observed_at_ms) = yielded_by.observed_at_ms {
        push_field(
            document,
            context,
            4,
            "observed",
            METADATA_LABEL_WIDTH,
            &timestamp_text(observed_at_ms),
            Token::Text,
            true,
        );
    }
    push_field(
        document,
        context,
        4,
        "yield id",
        METADATA_LABEL_WIDTH,
        &yielded_by.yield_id,
        Token::Text,
        true,
    );
    push_references(
        document,
        context,
        4,
        "evidence",
        METADATA_LABEL_WIDTH,
        &yielded_by.evidence_numbers,
    );
}

fn render_endpoint(
    document: &mut Document,
    context: &RenderContext,
    endpoint: &ScopedCommitEndpoint,
) {
    let (heading, commit, scope, observation_id, observed_at_ms, evidence_numbers) = match endpoint
    {
        ScopedCommitEndpoint::CurrentAtRef {
            commit,
            scope,
            observation_id,
            observed_at_ms,
            evidence_numbers,
        } => (
            "Current at ref",
            commit,
            scope,
            observation_id,
            observed_at_ms,
            evidence_numbers,
        ),
        ScopedCommitEndpoint::CurrentForPr {
            commit,
            scope,
            observation_id,
            observed_at_ms,
            evidence_numbers,
        } => (
            "Current for PR",
            commit,
            scope,
            observation_id,
            observed_at_ms,
            evidence_numbers,
        ),
    };
    push_heading(document, 2, heading);
    push_exact_commit(document, context, 4, "commit", commit);
    push_role_resource(document, context, 4, "scope", scope);
    push_field(
        document,
        context,
        4,
        "observation",
        METADATA_LABEL_WIDTH,
        observation_id,
        Token::Text,
        true,
    );
    push_field(
        document,
        context,
        4,
        "observed",
        METADATA_LABEL_WIDTH,
        &timestamp_text(*observed_at_ms),
        Token::Text,
        true,
    );
    push_references(
        document,
        context,
        4,
        "evidence",
        METADATA_LABEL_WIDTH,
        evidence_numbers,
    );
}

fn push_exact_commit(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    label: &str,
    commit: &ExactCommitRef,
) {
    let value = if label == "commit" {
        format!("{} ({})", commit.oid, enum_text(commit.object_format))
    } else {
        format!(
            "commit {} ({})",
            commit.oid,
            enum_text(commit.object_format)
        )
    };
    push_field(
        document,
        context,
        indent,
        label,
        METADATA_LABEL_WIDTH,
        &value,
        Token::Text,
        true,
    );
}

fn render_non_asserted_state(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    state: CommitLineageState,
) {
    if state != CommitLineageState::Asserted {
        push_field(
            document,
            context,
            indent,
            "state",
            METADATA_LABEL_WIDTH,
            &enum_text(state),
            Token::Warning,
            false,
        );
    }
}

const fn state_token(state: CommitLineageState) -> Token {
    match state {
        CommitLineageState::Asserted => Token::Success,
        CommitLineageState::Ambiguous => Token::Warning,
        CommitLineageState::Contradicted => Token::Error,
    }
}

fn omission_notice(omission: &CommitLineageOmission) -> String {
    match omission {
        CommitLineageOmission::Exact(count) => format!(
            "More proven lineage may be omitted: {count} {}.",
            if *count == 1 { "event" } else { "events" }
        ),
        CommitLineageOmission::AtLeast(count) => format!(
            "More proven lineage may be omitted: at least {count} {}.",
            if *count == 1 { "event" } else { "events" }
        ),
        CommitLineageOmission::Unknown => "More proven lineage may be omitted.".to_owned(),
    }
}

fn render_bounds(document: &mut Document, context: &RenderContext, bounds: &CommitLineageBounds) {
    let reason = bounds.truncation_reason.map(enum_text).unwrap_or_default();
    let value = format!(
        "returned {}/{} · examined {}/{} · {reason}",
        bounds.returned_events,
        bounds.returned_event_limit,
        bounds.examined_events,
        bounds.examined_event_limit,
    );
    push_field(
        document,
        context,
        4,
        "bounds",
        METADATA_LABEL_WIDTH,
        &value,
        Token::Warning,
        false,
    );
}
