use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ctx_history_index::{CoreEventRecord, IndexError, SessionRecord, VerifiedIndex};
use ctx_history_refresh::{verify_generation_query_authority, GenerationQueryAuthorityError};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    transcript::normalize_uuid_prefix,
    ui::{diagnostic, Action, Diagnostic, DiagnosticLevel, Field, RenderContext, Ui},
};

use super::compact_ref::{CompactRefNamespace, CompactRefResolveError, CompactRefResolver};

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const ACTIVE_GENERATION_RACE_ERROR_CODE: &str = "generation_changed";
const ACTIVE_GENERATION_RACE_FAILURE_KIND: &str = "active_generation_race";
const ACTIVE_GENERATION_RACE_DETAIL: &str =
    "the active searchable generation changed while the command was opening it";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActiveGenerationRaceCommand {
    Search,
    Show,
}

impl ActiveGenerationRaceCommand {
    const fn summary(self) -> &'static str {
        match self {
            Self::Search => "History changed during search",
            Self::Show => "History changed while opening this item",
        }
    }

    const fn retry_detail(self) -> &'static str {
        match self {
            Self::Search => {
                "A refresh published a new searchable generation while ctx was opening the previous one. Retry the same search command."
            }
            Self::Show => {
                "A refresh published a new searchable generation while ctx was opening the previous one. Retry the same show command."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MissingLookupKind {
    Event,
    Session,
}

impl MissingLookupKind {
    const fn noun(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Session => "session",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(super) struct MissingLookupError {
    kind: MissingLookupKind,
    requested: String,
    message: String,
}

impl MissingLookupError {
    fn exact(kind: MissingLookupKind, requested: impl Into<String>) -> Self {
        let requested = requested.into();
        let message = format!(
            "{} {requested} was not found in the Core generation",
            kind.noun()
        );
        Self {
            kind,
            requested,
            message,
        }
    }

    fn prefix(kind: MissingLookupKind, requested: impl Into<String>) -> Self {
        let requested = requested.into();
        let message = format!(
            "{} id prefix {requested:?} was not found in the Core generation",
            kind.noun()
        );
        Self {
            kind,
            requested,
            message,
        }
    }
}

#[cfg(test)]
pub(super) fn resolve_core_event(index: &VerifiedIndex, id: &str) -> Result<CoreEventRecord> {
    let references = CompactRefResolver::new(index, None);
    resolve_core_event_with_refs(&references, id)
}

pub(super) fn resolve_core_event_with_refs(
    references: &CompactRefResolver<'_>,
    id: &str,
) -> Result<CoreEventRecord> {
    let event_id = resolve_compact_id_for_lookup(references, CompactRefNamespace::Event, id)?;
    references
        .current_index()
        .core_event_by_id(event_id)?
        .ok_or_else(|| missing_resolved_lookup(MissingLookupKind::Event, id, event_id).into())
}

#[cfg(test)]
pub(super) fn resolve_session(index: &VerifiedIndex, id: &str) -> Result<SessionRecord> {
    let references = CompactRefResolver::new(index, None);
    resolve_session_with_refs(&references, id)
}

pub(super) fn resolve_session_with_refs(
    references: &CompactRefResolver<'_>,
    id: &str,
) -> Result<SessionRecord> {
    let session_id = resolve_compact_id_for_lookup(references, CompactRefNamespace::Session, id)?;
    references
        .current_index()
        .session_by_id(session_id)?
        .ok_or_else(|| missing_resolved_lookup(MissingLookupKind::Session, id, session_id).into())
}

fn resolve_compact_id_for_lookup(
    references: &CompactRefResolver<'_>,
    namespace: CompactRefNamespace,
    id: &str,
) -> Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(id.trim()) {
        return Ok(id);
    }
    match references.resolve_id(namespace, id) {
        Ok(id) => Ok(id),
        Err(error) => match error.downcast_ref::<CompactRefResolveError>() {
            Some(CompactRefResolveError::ExactNotFound { id, .. }) => {
                let kind = match namespace {
                    CompactRefNamespace::Event => MissingLookupKind::Event,
                    CompactRefNamespace::Session => MissingLookupKind::Session,
                };
                Err(MissingLookupError::exact(kind, id.to_string()).into())
            }
            Some(CompactRefResolveError::NotFound { reference, .. }) => {
                let kind = match namespace {
                    CompactRefNamespace::Event => MissingLookupKind::Event,
                    CompactRefNamespace::Session => MissingLookupKind::Session,
                };
                Err(MissingLookupError::prefix(kind, reference).into())
            }
            _ => Err(error),
        },
    }
}

fn missing_resolved_lookup(
    kind: MissingLookupKind,
    requested: &str,
    resolved: Uuid,
) -> MissingLookupError {
    if Uuid::parse_str(requested.trim()).is_ok() {
        MissingLookupError::exact(kind, resolved.to_string())
    } else {
        MissingLookupError::prefix(kind, requested.trim().to_ascii_lowercase())
    }
}

pub(super) fn resolve_lookup_for_output<T>(
    result: Result<T>,
    human_output: bool,
    recovery_command: &str,
    ui: &mut Ui,
) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if human_output => {
            let Some(missing) = error.downcast_ref::<MissingLookupError>() else {
                return Err(error);
            };
            let document = render_missing_lookup(ui.stderr_context(), missing, recovery_command);
            ui.write_stderr(&document)?;
            Err(crate::dispatch::rendered_cli_error())
        }
        Err(error) => Err(error),
    }
}

pub(super) fn render_active_generation_race<T>(
    result: Result<T>,
    json_output: bool,
    command: ActiveGenerationRaceCommand,
    ui: &mut Ui,
) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if is_active_generation_race(&error) => {
            if json_output {
                let encoded = serde_json::to_string(&active_generation_race_error_json())?;
                writeln!(ui.stderr_writer(), "{encoded}")?;
            } else {
                let document = diagnostic(
                    ui.stderr_context(),
                    Diagnostic {
                        level: DiagnosticLevel::Error,
                        summary: command.summary(),
                        detail: Some(command.retry_detail()),
                        fields: &[],
                        action: None,
                    },
                );
                ui.write_stderr(&document)?;
            }
            Err(crate::dispatch::rendered_cli_error())
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn is_active_generation_race(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<IndexError>(),
            Some(IndexError::ConcurrentGenerationChange)
        )
    })
}

pub(crate) fn active_generation_race_error_json() -> Value {
    json!({
        "error": format!(
            "{ACTIVE_GENERATION_RACE_ERROR_CODE}/{ACTIVE_GENERATION_RACE_FAILURE_KIND}"
        ),
        "error_code": ACTIVE_GENERATION_RACE_ERROR_CODE,
        "failure_kind": ACTIVE_GENERATION_RACE_FAILURE_KIND,
        "detail": ACTIVE_GENERATION_RACE_DETAIL,
        "retryable": true,
    })
}

pub(crate) fn generation_query_authority_error_json(
    error: &GenerationQueryAuthorityError,
) -> Value {
    let detail = error.to_string();
    json!({
        "error": detail.clone(),
        "error_code": error.error_code(),
        "detail": detail,
        "retryable": error.retryable(),
    })
}

pub(super) fn render_missing_lookup(
    context: &RenderContext,
    missing: &MissingLookupError,
    recovery_command: &str,
) -> crate::ui::Document {
    let (summary, detail, label) = match missing.kind {
        MissingLookupKind::Event => (
            "Event not found",
            "This event is not in the current searchable generation. Search for text from the event, then retry with a returned event ID.",
            "Requested event",
        ),
        MissingLookupKind::Session => (
            "Session not found",
            "This session is not in the current searchable generation. Search for text from the session, then retry with a returned session ID.",
            "Requested session",
        ),
    };
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Error,
            summary,
            detail: Some(detail),
            fields: &[Field::new(label, &missing.requested)],
            action: Some(Action {
                command: recovery_command,
            }),
        },
    )
}

pub(super) fn validate_ctx_id(id: &str, kind: &str) -> Result<String> {
    let trimmed = id.trim();
    if Uuid::parse_str(trimmed).is_ok() {
        return Ok(trimmed.to_ascii_lowercase());
    }
    normalize_uuid_prefix(trimmed, kind)
}

pub(super) fn validate_session_selector(
    id: Option<&str>,
    provider_session_id: Option<&str>,
) -> Result<()> {
    match (id, provider_session_id) {
        (Some(id), None) => {
            validate_ctx_id(id, "session")?;
            Ok(())
        }
        (None, Some(provider_session_id)) if provider_session_id.trim().is_empty() => {
            Err(anyhow!("provider session ID must not be empty"))
        }
        (None, Some(_)) => Ok(()),
        (Some(_), Some(_)) => Err(anyhow!(
            "pass either a ctx session ID or --provider-session, not both"
        )),
        (None, None) => Err(anyhow!(
            "Core session lookup requires a ctx session ID or --provider-session"
        )),
    }
}

pub(super) fn open_index(data_root: &Path) -> Result<VerifiedIndex> {
    let root = index_root(data_root);
    let index = match VerifiedIndex::open_pinned(&root) {
        Ok(index) => index,
        Err(ctx_history_index::IndexError::MissingActiveGenerationPointer) => {
            return Err(anyhow!(
                "the Core index does not exist; retry with daemon refresh enabled"
            ));
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open verified Core index {}", root.display()));
        }
    };
    verify_generation_query_authority(&index).map_err(anyhow::Error::new)?;
    Ok(index)
}

pub(super) fn index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}
