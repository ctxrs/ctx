use std::collections::BTreeMap;

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
    let operations = grouped_operations(lineage);
    let completeness = if lineage.complete {
        "complete"
    } else {
        "partial"
    };
    let operation_count = operations.len();
    let operation_label = if operation_count == 1 {
        "operation"
    } else {
        "operations"
    };
    let status = if lineage.ambiguous {
        format!("Lineage · {completeness} · ambiguous · {operation_count} {operation_label}")
    } else {
        format!("Lineage · {completeness} · {operation_count} {operation_label}")
    };
    push_heading(document, 0, &status);

    for (operation_id, operation) in operations {
        render_operation(document, context, operation_id, &operation);
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

#[derive(Default)]
struct OperationGroup<'a> {
    mappings: Vec<&'a CommitLineageEdge>,
    yields: Vec<&'a CommitLineageYield>,
}

fn grouped_operations(lineage: &CommitLineage) -> BTreeMap<&str, OperationGroup<'_>> {
    let mut operations: BTreeMap<&str, OperationGroup<'_>> = BTreeMap::new();
    for edge in &lineage.edges {
        operations
            .entry(edge.operation_id.as_str())
            .or_default()
            .mappings
            .push(edge);
    }
    for yielded_by in &lineage.yielded_by {
        operations
            .entry(yielded_by.operation_id.as_str())
            .or_default()
            .yields
            .push(yielded_by);
    }
    for operation in operations.values_mut() {
        operation.mappings.sort_by(|left, right| {
            (
                left.source.logical_repository_id.as_str(),
                left.source.object_format,
                left.source.oid.as_str(),
                left.result.object_format,
                left.result.oid.as_str(),
            )
                .cmp(&(
                    right.source.logical_repository_id.as_str(),
                    right.source.object_format,
                    right.source.oid.as_str(),
                    right.result.object_format,
                    right.result.oid.as_str(),
                ))
        });
        operation.yields.sort_by(|left, right| {
            (left.yield_id.as_str(), left.actor.id.as_str())
                .cmp(&(right.yield_id.as_str(), right.actor.id.as_str()))
        });
    }
    operations
}

fn render_operation(
    document: &mut Document,
    context: &RenderContext,
    operation_id: &str,
    operation: &OperationGroup<'_>,
) {
    let heading = operation_heading(operation);
    push_heading(document, 2, &heading);

    if operation.mappings.len() == 1 {
        render_mapping(document, context, 4, operation.mappings[0]);
    } else {
        for (index, mapping) in operation.mappings.iter().enumerate() {
            push_heading(document, 4, &format!("Mapping {}", index + 1));
            render_mapping(document, context, 6, mapping);
        }
    }

    let Some((actor, proof_class, state, observed_at_ms, evidence_numbers)) = operation
        .mappings
        .first()
        .map(|edge| {
            (
                &edge.actor,
                edge.proof_class,
                edge.state,
                edge.observed_at_ms,
                edge.evidence_numbers.as_slice(),
            )
        })
        .or_else(|| {
            operation.yields.first().map(|yielded_by| {
                (
                    &yielded_by.actor,
                    yielded_by.proof_class,
                    yielded_by.state,
                    yielded_by.observed_at_ms,
                    yielded_by.evidence_numbers.as_slice(),
                )
            })
        })
    else {
        return;
    };

    let outcome = yield_outcome(state, operation.mappings.len());
    push_field(
        document,
        context,
        4,
        "outcome",
        METADATA_LABEL_WIDTH,
        &outcome,
        state_token(state),
        false,
    );
    push_role_resource(document, context, 4, "actor", actor);
    push_field(
        document,
        context,
        4,
        "proof",
        METADATA_LABEL_WIDTH,
        &enum_text(proof_class),
        Token::Text,
        false,
    );
    render_non_asserted_state(document, context, 4, state);
    if let Some(observed_at_ms) = observed_at_ms {
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
        operation_id,
        Token::Text,
        true,
    );
    for (index, yielded_by) in operation.yields.iter().enumerate() {
        let label = if operation.yields.len() == 1 {
            "yield id".to_owned()
        } else {
            format!("yield {} id", index + 1)
        };
        push_field(
            document,
            context,
            4,
            &label,
            METADATA_LABEL_WIDTH,
            &yielded_by.yield_id,
            Token::Text,
            true,
        );
    }
    push_references(
        document,
        context,
        4,
        "evidence",
        METADATA_LABEL_WIDTH,
        evidence_numbers,
    );
}

fn operation_heading(operation: &OperationGroup<'_>) -> String {
    let mapping_count = operation.mappings.len();
    let yield_count = operation.yields.len();
    let mapping_label = if mapping_count == 1 {
        "mapping"
    } else {
        "mappings"
    };
    let yield_label = if yield_count == 1 {
        "yield record"
    } else {
        "yield records"
    };
    match (operation.mappings.first(), yield_count) {
        (Some(edge), 0) => format!(
            "{} · {} · {mapping_count} {mapping_label}",
            enum_heading(edge.kind),
            enum_text(edge.relation_class)
        ),
        (Some(edge), _) => format!(
            "{} · {} · {mapping_count} {mapping_label} · {yield_count} {yield_label}",
            enum_heading(edge.kind),
            enum_text(edge.relation_class)
        ),
        (None, _) => format!("Yield operation · {yield_count} {yield_label}"),
    }
}

fn render_mapping(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    edge: &CommitLineageEdge,
) {
    push_exact_commit(document, context, indent, "source", &edge.source);
    push_exact_commit(document, context, indent, "result", &edge.result);
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

fn yield_outcome(state: CommitLineageState, mapping_count: usize) -> String {
    match state {
        CommitLineageState::Asserted if mapping_count > 1 => {
            format!("operation yielded {mapping_count} mapped commits")
        }
        CommitLineageState::Asserted => "operation yielded this commit".to_owned(),
        CommitLineageState::Ambiguous => "operation yield is ambiguous".to_owned(),
        CommitLineageState::Contradicted => "operation yield is contradicted".to_owned(),
    }
}

fn omission_notice(omission: &CommitLineageOmission) -> String {
    match omission {
        CommitLineageOmission::Exact(count) => format!(
            "More proven lineage may be omitted: {count} {}.",
            if *count == 1 {
                "operation event"
            } else {
                "operation events"
            }
        ),
        CommitLineageOmission::AtLeast(count) => format!(
            "More proven lineage may be omitted: at least {count} {}.",
            if *count == 1 {
                "operation event"
            } else {
                "operation events"
            }
        ),
        CommitLineageOmission::Unknown => "More proven lineage may be omitted.".to_owned(),
    }
}

fn render_bounds(document: &mut Document, context: &RenderContext, bounds: &CommitLineageBounds) {
    let reason = bounds.truncation_reason.map(enum_text).unwrap_or_default();
    let value = format!(
        "operations returned {}/{} · events examined {}/{} · {reason}",
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
