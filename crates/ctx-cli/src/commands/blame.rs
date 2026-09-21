use std::{path::Path, time::Instant};

use anyhow::Result;
use ctx_attribution::presentation::{mcp_text, print_blame_result_with_evidence_preview};
use ctx_attribution_model::{
    BlameAttribution, BlameDiagnostic, BlameDiagnosticReason, BlameResultFreshness, BlameTarget,
    EvidencePreviewModel, HostedBlameResult,
};
use ctx_client_observability::analytics::{
    BlameFailure, BlameFailureClass, BlameFailurePhase, BlameFreshness, BlameRequestKind,
    BlameResultFacts, BlameResultState, BlameTargetKind, BlameTerminalFacts,
};

use crate::{
    tool_backend::{
        StructuredToolError, ToolBackendError, ToolExecutionError, ToolOutcome, ToolUsageFacts,
    },
    ui::Ui,
};

mod args;
pub(crate) use args::BlameArgs;

pub(crate) fn target_kind(target: &BlameTarget) -> BlameTargetKind {
    match target {
        BlameTarget::File { .. } => BlameTargetKind::File,
        BlameTarget::Commit { .. } => BlameTargetKind::Commit,
        BlameTarget::PullRequest { .. } => BlameTargetKind::PullRequest,
    }
}

fn query(
    data_root: &Path,
    target: &BlameTarget,
    limit: u32,
    cursor: Option<&str>,
    facts: &mut BlameTerminalFacts,
) -> Result<(HostedBlameResult, EvidencePreviewModel), BlameDiagnostic> {
    facts.request_kind = Some(if cursor.is_some() {
        BlameRequestKind::Continuation
    } else {
        BlameRequestKind::FirstRequest
    });
    let started = Instant::now();
    let result = ctx_attribution::query(data_root, target, limit, cursor);
    facts.query_duration = Some(started.elapsed());
    match result {
        Ok(result) => {
            facts.result = Some(BlameResultFacts {
                state: match result.result.outcome.attribution {
                    BlameAttribution::Proven => BlameResultState::Proven,
                    BlameAttribution::Possible => BlameResultState::Possible,
                    BlameAttribution::Conflicting => BlameResultState::Conflicting,
                    BlameAttribution::None => BlameResultState::None,
                },
                evaluated: u64::from(result.result.outcome.coverage.evaluated),
                freshness: match result.freshness {
                    BlameResultFreshness::Current => BlameFreshness::Current,
                    BlameResultFreshness::StaleCommitted => BlameFreshness::StaleCommitted,
                },
                has_more: result.result.next.is_some(),
            });
            let previews = ctx_attribution::hydrate_evidence_previews(data_root, &result.result);
            Ok((result, previews))
        }
        Err(error) => {
            facts.failure = Some(BlameFailure {
                class: failure_class(error.reason),
                phase: BlameFailurePhase::Query,
            });
            Err(error)
        }
    }
}

fn failure_class(reason: BlameDiagnosticReason) -> BlameFailureClass {
    use BlameDiagnosticReason::*;
    match reason {
        RequestInvalid | InvalidBounds => BlameFailureClass::InvalidRequest,
        GraphCorrupt => BlameFailureClass::Corruption,
        ProjectionStale | EvidenceStale => BlameFailureClass::Stale,
        TargetOrRepositoryAmbiguous
        | TargetAmbiguous
        | RepositoryAmbiguous
        | CommitRewriteAmbiguous => BlameFailureClass::Ambiguous,
        ProjectionAbsent | ProjectionPartial | ProjectionIncompatible | SourceUnavailable => {
            BlameFailureClass::Source
        }
        _ => BlameFailureClass::Repository,
    }
}

fn diagnostic_text(error: &BlameDiagnostic) -> String {
    let mut text = format!("{}\n", error.message);
    if let Some(action) = &error.next_action {
        let argv = action
            .argv
            .iter()
            .map(|arg| ctx_attribution::presentation::shell_quote_arg(arg))
            .collect::<Vec<_>>()
            .join(" ");
        text.push_str(&format!("\nNext\n  {argv}\n"));
    }
    text
}

pub(crate) fn run(
    args: BlameArgs,
    data_root: &Path,
    facts: &mut BlameTerminalFacts,
    ui: &mut Ui,
) -> Result<()> {
    let target = args.target().map_err(anyhow::Error::msg)?;
    match query(data_root, &target, args.limit(), args.cursor(), facts) {
        Ok((result, previews)) => {
            let rendered = print_blame_result_with_evidence_preview(
                &result,
                args.json_output(),
                &previews,
                ui,
            );
            if let Err(error) = &rendered {
                record_presentation_failure(facts, error);
            }
            rendered.map(|_| ())
        }
        Err(error) => {
            let rendered = if args.json_output() {
                serde_json::to_vec(&error)
                    .map_err(anyhow::Error::from)
                    .and_then(|mut bytes| {
                        bytes.push(b'\n');
                        ui.write_stderr_bytes(&bytes).map_err(Into::into)
                    })
            } else {
                ui.write_stderr_bytes(diagnostic_text(&error).as_bytes())
                    .map_err(Into::into)
            };
            if let Err(error) = &rendered {
                record_presentation_failure(facts, error);
            }
            rendered?;
            Err(crate::dispatch::rendered_cli_error())
        }
    }
}

fn record_presentation_failure(facts: &mut BlameTerminalFacts, error: &anyhow::Error) {
    facts.output_served = Some(false);
    facts.failure = Some(BlameFailure {
        class: BlameFailureClass::Output,
        phase: if error.downcast_ref::<std::io::Error>().is_some() {
            BlameFailurePhase::Output
        } else {
            BlameFailurePhase::Presentation
        },
    });
}

pub(crate) fn tool(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
) -> Result<ToolOutcome, ToolExecutionError> {
    let mut facts = BlameTerminalFacts::new(target_kind(&target));
    match query(data_root, &target, limit, cursor.as_deref(), &mut facts) {
        Ok((result, previews)) => {
            let (structured, text) = mcp_text::render_blame_tool(&result, Some(&previews));
            Ok(ToolOutcome {
                structured,
                compact: None,
                text: Some(text),
                usage: ToolUsageFacts {
                    blame: Some(facts),
                    ..ToolUsageFacts::default()
                },
            })
        }
        Err(error) => Err(ToolExecutionError {
            error: Box::new(ToolBackendError::Blame(StructuredToolError {
                structured: serde_json::to_value(&error)
                    .expect("canonical Blame diagnostic is serializable"),
                detail: diagnostic_text(&error),
            })),
            usage: Box::new(ToolUsageFacts {
                blame: Some(facts),
                ..ToolUsageFacts::default()
            }),
        }),
    }
}
