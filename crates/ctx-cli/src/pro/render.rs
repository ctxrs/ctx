use anyhow::Result;
use ctx_pro_host_protocol::BlameResult;
use serde_json::Value;

use crate::ui::{canonical_human_output_bytes, Document, RenderContext, Ui};

use super::evidence_preview::EvidencePreviewModel;
use super::{diagnostic::BlameNextAction, BlameResultFreshness, HostedBlameResult};

mod commit;
mod evidence;
mod file;
mod human;
mod layout;
mod lineage;
mod pull_request;
mod relationships;
mod target;

#[must_use]
pub(crate) fn blame_result_json<T: BlameOutput>(
    output: &T,
    previews: Option<&EvidencePreviewModel>,
) -> Value {
    let result = output.result();
    let evidence_context = BlameEvidenceContext::for_result(result, previews);
    blame_result_json_with_context(output, &evidence_context)
}

fn blame_result_json_with_context<T: BlameOutput>(
    output: &T,
    evidence_context: &BlameEvidenceContext,
) -> Value {
    let mut value = serde_json::to_value(output.result()).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.insert("evidence_context".to_owned(), evidence_context.json_value());
        if let Some(freshness) = output.freshness() {
            object.insert(
                "freshness".to_owned(),
                serde_json::json!({ "state": freshness }),
            );
        }
        if let Some(action) = successful_next_action(output) {
            object.insert(
                "next_action".to_owned(),
                serde_json::to_value(action).unwrap_or(Value::Null),
            );
        }
    }
    value
}

pub(crate) trait BlameOutput {
    fn result(&self) -> &BlameResult;
    fn freshness(&self) -> Option<BlameResultFreshness>;
}

impl BlameOutput for BlameResult {
    fn result(&self) -> &BlameResult {
        self
    }

    fn freshness(&self) -> Option<BlameResultFreshness> {
        None
    }
}

impl BlameOutput for HostedBlameResult {
    fn result(&self) -> &BlameResult {
        &self.result
    }

    fn freshness(&self) -> Option<BlameResultFreshness> {
        Some(self.freshness)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceContextStatus {
    Available,
    Unavailable,
    NotApplicable,
}

impl EvidenceContextStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unavailable => "unavailable",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlameEvidenceContext {
    status: EvidenceContextStatus,
    model: EvidencePreviewModel,
}

impl BlameEvidenceContext {
    fn for_result(result: &BlameResult, previews: Option<&EvidencePreviewModel>) -> Self {
        if matches!(
            &result.target,
            ctx_pro_host_protocol::ResolvedBlameTarget::File { .. }
        ) {
            Self::for_file(previews.cloned().unwrap_or(EvidencePreviewModel {
                previews: Vec::new(),
            }))
        } else {
            Self::not_applicable()
        }
    }

    #[must_use]
    pub(crate) fn for_file(model: EvidencePreviewModel) -> Self {
        let model = evidence::admitted_previews(&model);
        let status = if model.previews.is_empty() {
            EvidenceContextStatus::Unavailable
        } else {
            EvidenceContextStatus::Available
        };
        Self { status, model }
    }

    #[must_use]
    pub(crate) fn not_applicable() -> Self {
        Self {
            status: EvidenceContextStatus::NotApplicable,
            model: EvidencePreviewModel {
                previews: Vec::new(),
            },
        }
    }

    fn json_value(&self) -> Value {
        serde_json::json!({
            "status": self.status.as_str(),
            "items": &self.model.previews,
        })
    }

    const fn model(&self) -> &EvidencePreviewModel {
        &self.model
    }

    const fn is_available(&self) -> bool {
        matches!(self.status, EvidenceContextStatus::Available)
    }
}

/// Emits one blame result and returns its canonical, color-independent byte
/// count. Machine output intentionally bypasses the terminal UI.
pub(crate) fn print_blame_result(
    result: &HostedBlameResult,
    json_output: bool,
    ui: &mut Ui,
) -> Result<usize> {
    let evidence_context = BlameEvidenceContext::for_result(result, None);
    print_blame_result_with_context(result, json_output, &evidence_context, ui)
}

pub(crate) fn print_blame_result_with_evidence_preview(
    result: &HostedBlameResult,
    json_output: bool,
    previews: &EvidencePreviewModel,
    ui: &mut Ui,
) -> Result<usize> {
    let evidence_context = BlameEvidenceContext::for_result(result, Some(previews));
    print_blame_result_with_context(result, json_output, &evidence_context, ui)
}

fn print_blame_result_with_context(
    result: &HostedBlameResult,
    json_output: bool,
    evidence_context: &BlameEvidenceContext,
    ui: &mut Ui,
) -> Result<usize> {
    if json_output {
        let mut rendered =
            serde_json::to_vec_pretty(&blame_result_json_with_context(result, evidence_context))?;
        rendered.push(b'\n');
        ui.stdout_writer().write_all(&rendered)?;
        return Ok(rendered.len());
    }

    let document = render_blame_document(result, ui.stdout_context(), evidence_context);
    let plain_bytes = canonical_human_output_bytes(|context| {
        render_blame_document(result, context, evidence_context)
    });
    ui.write_stdout(&document)?;
    Ok(plain_bytes)
}

fn render_blame_document<T: BlameOutput>(
    output: &T,
    context: &RenderContext,
    evidence_context: &BlameEvidenceContext,
) -> Document {
    human::render(
        output.result(),
        output.freshness(),
        successful_next_action(output).as_ref(),
        context,
        evidence_context,
    )
}

fn successful_next_action<T: BlameOutput>(output: &T) -> Option<BlameNextAction> {
    (output.freshness() == Some(BlameResultFreshness::Current)
        && output.result().outcome.attribution == ctx_pro_host_protocol::BlameAttribution::None)
        .then(|| BlameNextAction::core_search_for_resolved(&output.result().target))
        .flatten()
}

#[cfg(test)]
mod tests;
