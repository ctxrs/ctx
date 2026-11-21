use anyhow::{anyhow, Result};
use ctx_history_core::CaptureProvider;
use ctx_history_index_query::{
    SessionListFilters, SessionListPage, DEFAULT_SESSION_LIST_LIMIT, MAX_SESSION_LIST_LIMIT,
};
use serde_json::{json, Value};

use crate::generation::PinnedGenerationRead;
use crate::json::compact_json;
use crate::{
    parse_since_filter, timestamp_json, CompactPresentationProjection, GenerationReadError,
    GenerationReadPort, GenerationReadReceipt, GenerationReadRequest, GenerationReadTarget,
    PinnedHistoryQuery, RetainedPeerRead,
};

pub const DEFAULT_SESSIONS_LIMIT: usize = DEFAULT_SESSION_LIST_LIMIT;
pub const MAX_SESSIONS_LIMIT: usize = MAX_SESSION_LIST_LIMIT;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsRequest {
    pub since: Option<String>,
    pub provider: Option<CaptureProvider>,
    pub workspace: Option<String>,
    pub no_subagents: bool,
    pub limit: usize,
}

impl Default for SessionsRequest {
    fn default() -> Self {
        Self {
            since: None,
            provider: None,
            workspace: None,
            no_subagents: false,
            limit: DEFAULT_SESSIONS_LIMIT,
        }
    }
}

pub struct SessionsApplicationRequest {
    pub request: SessionsRequest,
    pub generation_target: GenerationReadTarget,
    pub compact_projection: bool,
}

#[derive(Debug)]
pub enum SessionsApplicationError<GenerationError> {
    Generation(GenerationReadError<GenerationError>),
    Query(anyhow::Error),
    Projection(anyhow::Error),
}

pub struct SessionsApplicationResult {
    generation: PinnedGenerationRead,
    pub page: SessionListPage,
    pub read_model: Value,
    pub compact_read_model: Option<Value>,
}

impl SessionsApplicationResult {
    pub fn receipt(&self) -> GenerationReadReceipt<'_> {
        self.generation.receipt()
    }

    pub const fn page(&self) -> &SessionListPage {
        &self.page
    }

    pub fn into_read_models(self) -> (Value, Option<Value>) {
        (self.read_model, self.compact_read_model)
    }
}

impl PinnedHistoryQuery<'_> {
    pub(crate) fn sessions(
        &self,
        filters: &SessionListFilters,
        limit: usize,
    ) -> Result<SessionListPage> {
        Ok(self.index.list_sessions(filters, limit)?)
    }
}

pub fn validate_sessions_limit(limit: usize) -> Result<usize> {
    if !(1..=MAX_SESSIONS_LIMIT).contains(&limit) {
        return Err(anyhow!(
            "limit must be between 1 and {MAX_SESSIONS_LIMIT}, got {limit}"
        ));
    }
    Ok(limit)
}

pub fn prepare_session_list_filters(
    request: &SessionsRequest,
) -> Result<(SessionListFilters, usize)> {
    let limit = validate_sessions_limit(request.limit)?;
    let since_unix_ms = request
        .since
        .as_deref()
        .map(parse_since_filter)
        .transpose()?
        .map(|since| since.timestamp_millis());
    let workspace = match request.workspace.as_deref() {
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(anyhow!("workspace filter cannot be empty"));
            }
            Some(trimmed.to_owned())
        }
        None => None,
    };
    let filters = SessionListFilters {
        since_unix_ms,
        provider: request.provider.map(|p| p.as_str().to_owned()),
        workspace,
        no_subagents: request.no_subagents,
    };
    Ok((filters, limit))
}

pub fn execute_sessions<Generation: GenerationReadPort>(
    request: SessionsApplicationRequest,
    generation_port: &mut Generation,
) -> std::result::Result<SessionsApplicationResult, SessionsApplicationError<Generation::Error>> {
    let SessionsApplicationRequest {
        request,
        generation_target,
        compact_projection,
    } = request;
    let (filters, limit) =
        prepare_session_list_filters(&request).map_err(SessionsApplicationError::Query)?;
    let retained_peer = if compact_projection {
        RetainedPeerRead::IfAvailable
    } else {
        RetainedPeerRead::Omit
    };
    let generation = PinnedGenerationRead::open(
        generation_port,
        GenerationReadRequest {
            target: generation_target,
            retained_peer,
        },
    )
    .map_err(SessionsApplicationError::Generation)?;
    let page = PinnedHistoryQuery::new(generation.index(), generation.retained_peer())
        .sessions(&filters, limit)
        .map_err(SessionsApplicationError::Query)?;
    let read_model = sessions_read_model(&page, generation.archive_provenance());
    let compact_read_model = compact_projection
        .then(|| {
            CompactPresentationProjection::new(generation.index(), generation.retained_peer())
                .project(&read_model)
        })
        .transpose()
        .map_err(SessionsApplicationError::Projection)?;
    Ok(SessionsApplicationResult {
        generation,
        page,
        read_model,
        compact_read_model,
    })
}

pub fn sessions_read_model(
    page: &SessionListPage,
    provenance: &crate::ArchiveProvenanceMap,
) -> Value {
    let sessions = page
        .sessions
        .iter()
        .map(|session| {
            compact_json(json!({
                "ctx_session_id": session.session_id.as_uuid(),
                "provider": session.provider,
                "provider_key": session.provider_key,
                "source_id": session.source_id,
                "provider_session_id": session.provider_session_id,
                "parent_ctx_session_id": session.parent_session_id.map(|id| id.as_uuid()),
                "root_ctx_session_id": session.root_session_id.map(|id| id.as_uuid()),
                "cwd": session.cwd,
                "project": session.project,
                "branch": session.branch,
                "started_at": timestamp_json(session.started_at_unix_ms),
                "last_activity_at": timestamp_json(session.last_activity_at_unix_ms),
                "events": session.events,
                "archive_provenance": provenance.for_session(session.session_id),
            }))
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": 1,
        "payload_type": "session_list",
        "generation_id": page.generation_id,
        "truncated": page.truncated,
        "sessions": sessions,
    })
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        derive_session_id, NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey,
        TypedKey,
    };
    use ctx_history_index_query::SessionSummaryRecord;

    use super::*;
    use crate::ArchiveProvenanceMap;

    fn sample_session_id(label: &str) -> ctx_history_core::StableEntityId {
        let source = SourceKey::derive(
            "claude",
            "claude_code_jsonl",
            "session",
            1,
            SourceAnchor::provider_native("session-file", TypedKey::utf8(label).unwrap()).unwrap(),
        )
        .unwrap();
        let key = NativeSessionKey::native_id("session", TypedKey::utf8(label).unwrap()).unwrap();
        derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &key,
        })
        .unwrap()
    }

    #[test]
    fn sessions_read_model_emits_schema_v1_envelope_and_omits_null_fields() {
        let root_id = sample_session_id("root");
        let child_id = sample_session_id("child");
        let page = SessionListPage {
            generation_id: "gen-42".to_owned(),
            truncated: true,
            sessions: vec![
                SessionSummaryRecord {
                    session_id: root_id,
                    provider: "claude".to_owned(),
                    provider_key: None,
                    source_id: None,
                    provider_session_id: Some("native-root".to_owned()),
                    parent_session_id: None,
                    root_session_id: None,
                    cwd: Some("/work/repo".to_owned()),
                    project: Some("repo".to_owned()),
                    branch: Some("main".to_owned()),
                    started_at_unix_ms: Some(1_700_000_000_000),
                    last_activity_at_unix_ms: Some(1_700_000_060_000),
                    events: 42,
                },
                SessionSummaryRecord {
                    session_id: child_id,
                    provider: "custom".to_owned(),
                    provider_key: Some("amp".to_owned()),
                    source_id: Some("threads".to_owned()),
                    provider_session_id: None,
                    parent_session_id: Some(root_id),
                    root_session_id: Some(root_id),
                    cwd: None,
                    project: None,
                    branch: None,
                    started_at_unix_ms: None,
                    last_activity_at_unix_ms: None,
                    events: 3,
                },
            ],
        };

        let value = sessions_read_model(&page, &ArchiveProvenanceMap::default());
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["payload_type"], "session_list");
        assert_eq!(value["generation_id"], "gen-42");
        assert_eq!(value["truncated"], true);

        let sessions = value["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);

        assert_eq!(sessions[0]["ctx_session_id"], root_id.as_uuid().to_string());
        assert_eq!(sessions[0]["provider"], "claude");
        assert_eq!(sessions[0]["provider_session_id"], "native-root");
        assert_eq!(sessions[0]["cwd"], "/work/repo");
        assert_eq!(sessions[0]["project"], "repo");
        assert_eq!(sessions[0]["branch"], "main");
        assert_eq!(sessions[0]["started_at"], "2023-11-14T22:13:20.000Z");
        assert_eq!(sessions[0]["last_activity_at"], "2023-11-14T22:14:20.000Z");
        assert_eq!(sessions[0]["events"], 42);
        assert!(sessions[0].get("provider_key").is_none());
        assert!(sessions[0].get("parent_ctx_session_id").is_none());

        assert_eq!(
            sessions[1]["ctx_session_id"],
            child_id.as_uuid().to_string()
        );
        assert_eq!(sessions[1]["provider"], "custom");
        assert_eq!(sessions[1]["provider_key"], "amp");
        assert_eq!(sessions[1]["source_id"], "threads");
        assert_eq!(
            sessions[1]["parent_ctx_session_id"],
            root_id.as_uuid().to_string()
        );
        assert_eq!(
            sessions[1]["root_ctx_session_id"],
            root_id.as_uuid().to_string()
        );
        assert_eq!(sessions[1]["events"], 3);
        assert!(sessions[1].get("cwd").is_none());
        assert!(sessions[1].get("started_at").is_none());
        assert!(sessions[1].get("last_activity_at").is_none());
    }

    #[test]
    fn prepare_session_list_filters_validates_limit_since_and_workspace() {
        let (filters, limit) = prepare_session_list_filters(&SessionsRequest {
            since: Some("2026-11-20".to_owned()),
            provider: Some(CaptureProvider::Claude),
            workspace: Some("  my-repo  ".to_owned()),
            no_subagents: true,
            limit: 100,
        })
        .unwrap();
        assert_eq!(limit, 100);
        assert_eq!(filters.provider.as_deref(), Some("claude"));
        assert_eq!(filters.workspace.as_deref(), Some("my-repo"));
        assert!(filters.no_subagents);
        assert!(filters.since_unix_ms.is_some());

        assert!(prepare_session_list_filters(&SessionsRequest {
            limit: 0,
            ..SessionsRequest::default()
        })
        .is_err());
        assert!(prepare_session_list_filters(&SessionsRequest {
            limit: MAX_SESSIONS_LIMIT + 1,
            ..SessionsRequest::default()
        })
        .is_err());
        assert!(prepare_session_list_filters(&SessionsRequest {
            workspace: Some("   ".to_owned()),
            ..SessionsRequest::default()
        })
        .is_err());
        assert!(prepare_session_list_filters(&SessionsRequest {
            since: Some("not-a-time".to_owned()),
            ..SessionsRequest::default()
        })
        .is_err());
    }
}
