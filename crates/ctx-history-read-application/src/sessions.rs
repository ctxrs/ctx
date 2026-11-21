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
    target: GenerationReadTarget,
    generation_id: String,
    retained_generation_id: Option<String>,
    pub page: SessionListPage,
    pub read_model: Value,
    pub compact_read_model: Option<Value>,
}

impl SessionsApplicationResult {
    pub fn receipt(&self) -> GenerationReadReceipt<'_> {
        GenerationReadReceipt {
            target: &self.target,
            generation_id: &self.generation_id,
            retained_generation_id: self.retained_generation_id.as_deref(),
        }
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

fn normalize_workspace_filter(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("workspace filter cannot be empty"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(anyhow!(
            "workspace filter cannot contain control characters"
        ));
    }
    let normalized = if trimmed.chars().all(|ch| ch == '/' || ch == '\\') {
        if trimmed.starts_with('\\') {
            "\\"
        } else {
            "/"
        }
    } else {
        let stripped = trimmed.trim_end_matches(['/', '\\']);
        if stripped.len() == 2
            && stripped.as_bytes()[0].is_ascii_alphabetic()
            && stripped.as_bytes()[1] == b':'
            && trimmed
                .as_bytes()
                .get(2)
                .is_some_and(|byte| *byte == b'\\' || *byte == b'/')
        {
            &trimmed[..3]
        } else {
            stripped
        }
    };
    Ok(normalized.to_owned())
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
    let workspace = request
        .workspace
        .as_deref()
        .map(normalize_workspace_filter)
        .transpose()?;
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
        RetainedPeerRead::Deferred
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
    let target = generation.receipt().target.clone();
    let generation_id = generation.index().generation_id().to_owned();
    let page = PinnedHistoryQuery::new(generation.index(), generation.retained_peer())
        .sessions(&filters, limit)
        .map_err(SessionsApplicationError::Query)?;
    let read_model = sessions_read_model(&page, generation.archive_provenance());
    let (compact_read_model, retained_generation_id) = if !compact_projection {
        (None, None)
    } else if page.sessions.is_empty() {
        (Some(read_model.clone()), None)
    } else if generation.is_deferred_without_peer() {
        let current =
            crate::compact_presentation::prepare_deferred_current(generation.index(), &read_model)
                .map_err(SessionsApplicationError::Projection)?;
        let retained_peer = generation
            .into_deferred_peer_for_projection()
            .map_err(SessionsApplicationError::Projection)?;
        let retained_id = retained_peer
            .as_ref()
            .map(|peer| peer.generation_id().to_owned());
        let projected = crate::compact_presentation::project_deferred(
            &read_model,
            current,
            retained_peer.as_ref(),
        )
        .map_err(SessionsApplicationError::Projection)?;
        (Some(projected), retained_id)
    } else {
        let retained_id = generation
            .retained_peer()
            .map(|peer| peer.generation_id().to_owned());
        let projected =
            CompactPresentationProjection::new(generation.index(), generation.retained_peer())
                .project(&read_model)
                .map_err(SessionsApplicationError::Projection)?;
        (Some(projected), retained_id)
    };
    Ok(SessionsApplicationResult {
        target,
        generation_id,
        retained_generation_id,
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

    #[test]
    fn prepare_session_list_filters_normalizes_workspace_trailing_slashes_and_rejects_controls() {
        for (input, expected) in [
            ("/work/app/", "/work/app"),
            ("  /work/app///  ", "/work/app"),
            ("/", "/"),
            ("///", "/"),
            (r"C:\work\app\", r"C:\work\app"),
            (r"C:\", r"C:\"),
            ("my-repo", "my-repo"),
        ] {
            let (filters, _) = prepare_session_list_filters(&SessionsRequest {
                workspace: Some(input.to_owned()),
                ..SessionsRequest::default()
            })
            .unwrap();
            assert_eq!(
                filters.workspace.as_deref(),
                Some(expected),
                "input={input:?}"
            );
        }

        for invalid in ["foo\nbar", "foo\rbar", "foo\tbar", "foo\x1b[31mbar", "a\0b"] {
            let err = prepare_session_list_filters(&SessionsRequest {
                workspace: Some(invalid.to_owned()),
                ..SessionsRequest::default()
            })
            .unwrap_err();
            assert!(
                err.to_string().contains("control characters"),
                "expected control-char error for {invalid:?}, got {err}"
            );
        }
    }

    #[test]
    fn execute_sessions_uses_deferred_peer_and_skips_peer_open_when_empty() {
        use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};

        let temp = tempfile::tempdir().unwrap();
        let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.commit(|_| true).unwrap();

        let mut seen_retained_peer = None;
        let mut port = |req: &GenerationReadRequest| -> anyhow::Result<crate::GenerationRead> {
            seen_retained_peer = Some(req.retained_peer);
            let index = VerifiedIndex::open_pinned_with_retained_peer(temp.path())?;
            Ok(crate::GenerationRead::new(index, None))
        };

        let compact = execute_sessions(
            SessionsApplicationRequest {
                request: SessionsRequest::default(),
                generation_target: GenerationReadTarget::Active,
                compact_projection: true,
            },
            &mut port,
        )
        .unwrap();
        assert_eq!(seen_retained_peer, Some(RetainedPeerRead::Deferred));
        assert!(compact.page().sessions.is_empty());
        assert!(compact.receipt().retained_generation_id.is_none());

        let mut json_retained_peer = None;
        let mut json_port = |req: &GenerationReadRequest| -> anyhow::Result<crate::GenerationRead> {
            json_retained_peer = Some(req.retained_peer);
            let index = VerifiedIndex::open_pinned(temp.path())?;
            Ok(crate::GenerationRead::new(index, None))
        };
        let json_res = execute_sessions(
            SessionsApplicationRequest {
                request: SessionsRequest::default(),
                generation_target: GenerationReadTarget::Active,
                compact_projection: false,
            },
            &mut json_port,
        )
        .unwrap();
        assert_eq!(json_retained_peer, Some(RetainedPeerRead::Omit));
        assert!(json_res.compact_read_model.is_none());
    }
}
