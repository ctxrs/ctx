use std::collections::VecDeque;

use ctx_history_core::{
    AgentType, Confidence, EventRole, EventType, Fidelity, SessionStatus, StableEntityId,
};
use ctx_history_index::EventRecord;
use ctx_history_relational::{
    RelationalEventMetadata, RelationalFileTouchMetadata, RelationalProjectionRecord,
    RelationalSessionMetadata,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::SourceBackedRelationalCatchUpError;

pub(super) fn records_for_event(
    event: EventRecord,
    provisional_session: Option<RelationalSessionMetadata>,
) -> std::result::Result<VecDeque<RelationalProjectionRecord>, SourceBackedRelationalCatchUpError> {
    let event_type = event.event_type.parse::<EventType>().map_err(|error| {
        SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
            "invalid event type {:?}: {error}",
            event.event_type
        ))
    })?;
    let role = event
        .role
        .as_deref()
        .map(str::parse::<EventRole>)
        .transpose()
        .map_err(|error| {
            SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                "invalid event role for {}: {error}",
                event.event_id
            ))
        })?;
    let mut records = VecDeque::with_capacity(
        event
            .touched_files
            .len()
            .saturating_add(1 + usize::from(provisional_session.is_some())),
    );
    if let Some(session) = provisional_session {
        records.push_back(RelationalProjectionRecord::Session(session));
    }
    records.push_back(RelationalProjectionRecord::Event(RelationalEventMetadata {
        event_id: event.event_id,
        session_id: event.session_id,
        event_sequence: event.event_sequence,
        event_type,
        role,
        occurred_at_unix_ms: event.occurred_at_unix_ms,
        fidelity: Fidelity::Imported,
        locator: event.locator,
    }));
    for (ordinal, path) in event.touched_files.into_iter().enumerate() {
        records.push_back(RelationalProjectionRecord::FileTouch(
            RelationalFileTouchMetadata {
                file_touch_id: file_touch_id(event.event_id, ordinal, &path)?,
                event_id: Some(event.event_id),
                session_id: Some(event.session_id),
                path,
                old_path: None,
                change_kind: None,
                line_count_delta: None,
                confidence: Confidence::Explicit,
                created_at_unix_ms: event.occurred_at_unix_ms,
                updated_at_unix_ms: event.occurred_at_unix_ms,
            },
        ));
    }
    Ok(records)
}

pub(super) struct SourceMetadataSeed {
    pub(super) source_path: Option<String>,
    pub(super) cwd: Option<String>,
}

impl SourceMetadataSeed {
    pub(super) fn new(event: &EventRecord) -> Self {
        Self {
            source_path: event.source_path.clone(),
            cwd: event.cwd.clone(),
        }
    }
}

#[derive(Clone)]
pub(super) struct SessionAggregate {
    first_event_sequence: u64,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    provider_session_id: Option<String>,
    agent_type: String,
    is_primary: bool,
    branch: Option<String>,
    workspace: Option<String>,
    cwd: Option<String>,
    source_path: Option<String>,
    started_at_unix_ms: Option<i64>,
    ended_at_unix_ms: Option<i64>,
}

impl SessionAggregate {
    pub(super) fn new(event: &EventRecord) -> Self {
        Self {
            first_event_sequence: event.event_sequence,
            session_id: event.session_id,
            parent_session_id: event.parent_session_id,
            root_session_id: event.root_session_id,
            provider_session_id: event.provider_session_id.clone(),
            agent_type: event.agent_type.clone(),
            is_primary: event.is_primary,
            branch: event.branch.clone(),
            workspace: event.workspace.clone(),
            cwd: event.cwd.clone(),
            source_path: event.source_path.clone(),
            started_at_unix_ms: event.occurred_at_unix_ms,
            ended_at_unix_ms: event.occurred_at_unix_ms,
        }
    }

    pub(super) fn observe(&mut self, event: &EventRecord) {
        if event.event_sequence < self.first_event_sequence {
            self.first_event_sequence = event.event_sequence;
            self.parent_session_id = event.parent_session_id;
            self.root_session_id = event.root_session_id;
            self.provider_session_id = event.provider_session_id.clone();
            self.agent_type.clone_from(&event.agent_type);
            self.is_primary = event.is_primary;
            self.branch.clone_from(&event.branch);
            self.workspace.clone_from(&event.workspace);
            self.cwd.clone_from(&event.cwd);
            self.source_path.clone_from(&event.source_path);
        }
        self.started_at_unix_ms = option_min(self.started_at_unix_ms, event.occurred_at_unix_ms);
        self.ended_at_unix_ms = option_max(self.ended_at_unix_ms, event.occurred_at_unix_ms);
    }

    pub(super) fn to_metadata(
        &self,
    ) -> std::result::Result<RelationalSessionMetadata, SourceBackedRelationalCatchUpError> {
        self.clone().into_metadata()
    }

    pub(super) fn into_metadata(
        self,
    ) -> std::result::Result<RelationalSessionMetadata, SourceBackedRelationalCatchUpError> {
        let agent_type = self.agent_type.parse::<AgentType>().map_err(|error| {
            SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
                "invalid agent type {:?}: {error}",
                self.agent_type
            ))
        })?;
        Ok(RelationalSessionMetadata {
            session_id: self.session_id,
            parent_session_id: self.parent_session_id,
            root_session_id: self.root_session_id,
            provider_session_id: self.provider_session_id,
            external_agent_id: None,
            agent_type,
            role_hint: None,
            is_primary: self.is_primary,
            branch: self.branch,
            workspace: self.workspace,
            cwd: self.cwd,
            source_path: self.source_path,
            status: SessionStatus::Imported,
            fidelity: Fidelity::Imported,
            started_at_unix_ms: self.started_at_unix_ms,
            ended_at_unix_ms: self.ended_at_unix_ms,
        })
    }
}

fn option_min(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

fn option_max(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn file_touch_id(
    event_id: StableEntityId,
    ordinal: usize,
    path: &str,
) -> std::result::Result<Uuid, SourceBackedRelationalCatchUpError> {
    let identity = event_id.encode_canonical().map_err(|error| {
        SourceBackedRelationalCatchUpError::InvalidMetadata(format!(
            "encode file-touch event identity: {error}"
        ))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-source-relational-file-touch-v1\0");
    hasher.update(identity);
    hasher.update((ordinal as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Uuid::from_bytes(bytes))
}
