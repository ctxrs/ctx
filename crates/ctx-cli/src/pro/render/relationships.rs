use ctx_pro_host_protocol::{AgentAttribution, ProductionRelationship};

use crate::ui::{Document, RenderContext};

use super::layout::{
    confidence_token, push_enum_field, push_heading, push_references, push_resource_primary,
    push_role_resource, state_token, METADATA_LABEL_WIDTH,
};

pub(super) fn render_attribution_groups(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    attributions: &[AgentAttribution],
) {
    let has_produced = attributions
        .iter()
        .any(|value| value.relationship == ProductionRelationship::ProducedBy);
    let has_possible = attributions
        .iter()
        .any(|value| value.relationship == ProductionRelationship::PossiblyProducedBy);

    if has_produced {
        push_heading(document, indent, "Produced by");
        for value in attributions
            .iter()
            .filter(|value| value.relationship == ProductionRelationship::ProducedBy)
        {
            render_attribution(document, context, indent + 2, value);
        }
    }
    if has_possible {
        if has_produced {
            document.push_blank();
        }
        push_heading(document, indent, "Possible producers");
        for value in attributions
            .iter()
            .filter(|value| value.relationship == ProductionRelationship::PossiblyProducedBy)
        {
            render_attribution(document, context, indent + 2, value);
        }
    }
}

fn render_attribution(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    value: &AgentAttribution,
) {
    push_resource_primary(document, context, indent, &value.producing_session);
    if let Some(actor) = &value.direct_actor {
        push_role_resource(document, context, indent + 2, "direct actor", actor);
    }
    render_session_lineage(
        document,
        context,
        indent + 2,
        value.parent_session.as_ref(),
        value.owning_root.as_ref(),
    );
    push_enum_field(
        document,
        context,
        indent + 2,
        "confidence",
        value.confidence,
        confidence_token(value.confidence),
    );
    push_enum_field(
        document,
        context,
        indent + 2,
        "state",
        value.state,
        state_token(value.state),
    );
    push_references(
        document,
        context,
        indent + 2,
        "evidence",
        METADATA_LABEL_WIDTH,
        &value.evidence_numbers,
    );
}

pub(super) fn render_session_lineage(
    document: &mut Document,
    context: &RenderContext,
    indent: usize,
    parent_session: Option<&ctx_pro_host_protocol::ResourceRef>,
    owning_root: Option<&ctx_pro_host_protocol::ResourceRef>,
) {
    if let Some(parent) = parent_session {
        push_role_resource(document, context, indent, "parent", parent);
    }
    if let Some(root) = owning_root {
        push_role_resource(document, context, indent, "owning root", root);
    }
}

#[cfg(test)]
#[path = "relationships_tests.rs"]
mod tests;
