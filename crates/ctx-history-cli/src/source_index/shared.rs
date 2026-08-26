use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ctx_history_index::{IndexError, VerifiedIndex};
use ctx_history_refresh::{verify_generation_query_authority, GenerationQueryAuthorityError};
use serde_json::{json, Value};

use crate::ui::{diagnostic, Action, Diagnostic, DiagnosticLevel, Field, RenderContext, Ui};

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

#[cfg(test)]
pub(super) use ctx_history_read_application::{resolve_core_event, resolve_session};
pub(super) use ctx_history_read_application::{
    validate_ctx_id, validate_session_selector, MissingLookupError, MissingLookupKind,
};

pub(super) fn externalize_query_error(error: anyhow::Error) -> anyhow::Error {
    if let Some(limit) =
        error.downcast_ref::<ctx_history_read_application::ContentQueryLimitError>()
    {
        return anyhow::Error::new(crate::presentation_limit::PresentationOutputLimitError {
            event_id: limit.event_id,
            actual_bytes: limit.actual_bytes,
            maximum_bytes: limit.maximum_bytes,
        });
    }
    let detail = error
        .downcast_ref::<ctx_history_read_application::SelectorError>()
        .map(selector_error_detail)
        .or_else(|| {
            error
                .downcast_ref::<ctx_history_read_application::CompactRefResolveError>()
                .and_then(compact_ref_error_detail)
        })
        .or_else(|| {
            error
                .downcast_ref::<ctx_history_read_application::EventWindowLimitError>()
                .map(|limit| {
                    format!(
                        "Core presentation selected at least {} events; the presentation limit is {} events",
                        limit.actual_events, limit.maximum_events
                    )
                })
        })
        .or_else(|| {
            error
                .downcast_ref::<ctx_history_read_application::EncodedCoreQueryLimitError>()
                .map(|limit| {
                    format!(
                        "stored Core encoding through ctx event {} requires {} bytes; the presentation retention limit is {} bytes",
                        limit.event_id, limit.actual_bytes, limit.maximum_bytes
                    )
                })
        })
        .or_else(|| {
            error
                .downcast_ref::<ctx_history_read_application::SourceIdentityFilterError>()
                .map(source_identity_filter_error_detail)
        });
    detail.map(anyhow::Error::msg).unwrap_or(error)
}

fn source_identity_filter_error_detail(
    error: &ctx_history_read_application::SourceIdentityFilterError,
) -> String {
    match error {
        ctx_history_read_application::SourceIdentityFilterError::InvalidHistorySource => {
            "--history-source expects plugin/source or provider_key/source_id".to_owned()
        }
        ctx_history_read_application::SourceIdentityFilterError::CustomProviderRequired => {
            "custom history source filters can only be combined with --provider custom".to_owned()
        }
    }
}

fn selector_error_detail(error: &ctx_history_read_application::SelectorError) -> String {
    use ctx_history_read_application::SelectorError;

    match error {
        SelectorError::PrefixTooShort { kind, minimum } => format!(
            "{kind} id prefix must be at least {minimum} hex characters, or pass a full ctx UUID"
        ),
        SelectorError::InvalidId { kind } => format!(
            "{kind} id must be a full ctx UUID or an unambiguous hex prefix from verbose search output"
        ),
        SelectorError::EmptyProviderSession => "provider session ID must not be empty".to_owned(),
        SelectorError::ConflictingSessionSelectors => {
            "pass either a ctx session ID or --provider-session, not both".to_owned()
        }
        SelectorError::MissingSessionSelector => {
            "Core session lookup requires a ctx session ID or --provider-session".to_owned()
        }
        SelectorError::IncompleteCustomSourceSelector => {
            "--provider-key and --source-id must be passed together".to_owned()
        }
        SelectorError::EmptyCustomSourceSelector { field } => {
            format!("--{} must not be empty", field.replace('_', "-"))
        }
        SelectorError::CustomSourceSelectorRequiresProviderSession => {
            "--provider-key/--source-id requires --provider-session".to_owned()
        }
        SelectorError::CustomSourceSelectorRequiresCustomProvider => {
            "--provider-key/--source-id can only be combined with --provider custom".to_owned()
        }
        SelectorError::ProviderSessionNotFound {
            provider_session_id,
        } => format!(
            "provider session {provider_session_id:?} was not found in the Core generation"
        ),
        SelectorError::ProviderSessionAmbiguous {
            provider_session_id,
            first,
            second,
            first_route,
            second_route,
        } => match (first_route, second_route) {
            (Some(first_route), Some(second_route)) => format!(
                "provider session {provider_session_id:?} is ambiguous between custom routes {first_route} ({first}) and {second_route} ({second}); pass --provider-key and --source-id, or a ctx session ID"
            ),
            _ => format!(
                "provider session {provider_session_id:?} is ambiguous; first matches are {first} and {second}; pass --provider, --provider-key/--source-id for custom history, or a ctx session ID"
            ),
        },
        SelectorError::ProviderMismatch {
            session_id,
            actual,
            requested,
        } => format!(
            "Core session {session_id} belongs to provider {actual}, not {requested}"
        ),
    }
}

fn compact_ref_error_detail(
    error: &ctx_history_read_application::CompactRefResolveError,
) -> Option<String> {
    let ctx_history_read_application::CompactRefResolveError::Ambiguous {
        namespace,
        reference,
        first,
        second,
    } = error
    else {
        return None;
    };
    let ctx_id_name = match namespace {
        ctx_history_read_application::CompactRefNamespace::Event => "ctx_event_id",
        ctx_history_read_application::CompactRefNamespace::Session => "ctx_session_id",
    };
    Some(format!(
        "{namespace} id prefix {reference:?} is ambiguous; conflicting full IDs are {first} and {second}; use a longer {ctx_id_name} or a full UUID"
    ))
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

pub fn generation_query_authority_error_json(error: &GenerationQueryAuthorityError) -> Value {
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
    let (summary, detail, label) = match missing.kind() {
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
            fields: &[Field::new(label, missing.requested())],
            action: Some(Action {
                command: recovery_command,
            }),
        },
    )
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
