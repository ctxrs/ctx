use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ctx_history_index::{EventRecord, SessionRecord, VerifiedIndex};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    output::compact_json,
    transcript::normalize_uuid_prefix,
    ui::{diagnostic, Action, Diagnostic, DiagnosticLevel, Field, RenderContext, Ui},
};

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";

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
            "{} {requested} was not found in the source-backed Core generation",
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
            "{} id prefix {requested:?} was not found in the source-backed Core generation",
            kind.noun()
        );
        Self {
            kind,
            requested,
            message,
        }
    }
}

pub(super) fn resolve_event(index: &VerifiedIndex, id: &str) -> Result<EventRecord> {
    if let Ok(uuid) = Uuid::parse_str(id.trim()) {
        return index.event_by_id(uuid)?.ok_or_else(|| {
            MissingLookupError::exact(MissingLookupKind::Event, uuid.to_string()).into()
        });
    }
    let prefix = validate_ctx_id(id, "event")?;
    match index.events_by_id_prefix(&prefix)?.as_slice() {
        [] => Err(MissingLookupError::prefix(MissingLookupKind::Event, prefix).into()),
        [event] => Ok(event.clone()),
        matches => Err(anyhow!(
            "event id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_event_id",
            matches[0].event_id,
            matches[1].event_id
        )),
    }
}

pub(super) fn resolve_session(index: &VerifiedIndex, id: &str) -> Result<SessionRecord> {
    if let Ok(uuid) = Uuid::parse_str(id.trim()) {
        return index.session_by_id(uuid)?.ok_or_else(|| {
            MissingLookupError::exact(MissingLookupKind::Session, uuid.to_string()).into()
        });
    }
    let prefix = validate_ctx_id(id, "session")?;
    match index.sessions_by_id_prefix(&prefix)?.as_slice() {
        [] => Err(MissingLookupError::prefix(MissingLookupKind::Session, prefix).into()),
        [session] => Ok(session.clone()),
        matches => Err(anyhow!(
            "session id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_session_id",
            matches[0].session_id,
            matches[1].session_id
        )),
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
            "source-backed session lookup requires a ctx session ID or --provider-session"
        )),
    }
}

pub(super) fn source_path_exists(path: Option<&str>) -> Option<bool> {
    path.map(|path| Path::new(path).exists())
}

pub(super) fn event_source_json(event: &EventRecord) -> Value {
    compact_json(json!({
        "source_id": event.locator.source().identity().as_uuid(),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "path": event.source_path,
        "exists": source_path_exists(event.source_path.as_deref()),
        "workspace": event.workspace,
        "cwd": event.cwd,
        "source_format": event.source_format,
    }))
}

pub(super) fn session_source_json(
    session: &SessionRecord,
    first_event: Option<&EventRecord>,
) -> Value {
    compact_json(json!({
        "source_id": first_event.map(|event| event.locator.source().identity().as_uuid()),
        "provider": session.provider,
        "provider_session_id": session.provider_session_id,
        "path": session.source_path,
        "exists": source_path_exists(session.source_path.as_deref()),
        "workspace": session.workspace,
        "cwd": session.cwd,
        "source_format": session.source_format,
    }))
}

pub(super) fn open_index(data_root: &Path) -> Result<VerifiedIndex> {
    let root = index_root(data_root);
    if !root.join("meta.json").is_file() {
        return Err(anyhow!(
            "source-backed Core index is not initialized at {}",
            root.display()
        ));
    }
    VerifiedIndex::open_pinned(&root)
        .with_context(|| format!("open verified source-backed Core index {}", root.display()))
}

pub(super) fn index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}
