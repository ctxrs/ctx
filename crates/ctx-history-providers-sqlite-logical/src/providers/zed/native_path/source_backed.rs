use std::{collections::BTreeSet, path::Path};

use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope, CaptureProvider, CoreActivity, CoreRecord, CoreRecordError,
    EventIdentityInput, LiteralFactKind, NativeItemKey, NativeSessionKey, PositionStability,
    ProjectionContractError, ProviderDeclaredFact, ProviderNativeSessionRelationship,
    SessionIdentityInput, SourceAnchor, SourceAnchorScope, SourceObservation, StableEntityId,
    TypedKey, CORE_ACTIVITY_REVISION,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) use super::query::scan_zed_native_snapshot;
#[cfg(test)]
use super::record_zed_projected_core_record;
use super::{
    acquire_immutable_snapshot,
    dto::{
        ZedNativeEvent, ZedNativeMessageIdentity, ZedNativePage, ZedNativeSession, ZedNativeSink,
    },
    ZedNativePathError, ZedNativeResult, ZedSnapshotAcquisition,
};
use crate::{CaptureError, ZED_THREADS_SQLITE_SOURCE_FORMAT};

const ZED_SOURCE_ANCHOR_NAMESPACE: &str = "zed.selected-threads-database";
const ZED_SOURCE_ANCHOR_KEY: &str = "threads";
const ZED_NATIVE_SESSION_NAMESPACE: &str = "zed.thread";
const ZED_NATIVE_EVENT_NAMESPACE: &str = "zed.thread-message";
const ZED_NATIVE_EVENT_POSITION_KIND: &str = "zed.thread-message-ordinal";
const ZED_LOGICAL_SESSION_KIND: &str = "zed-thread";
const ZED_LOGICAL_EVENT_KIND: &str = "zed-thread-event";
const ZED_SOURCE_SCHEMA_VARIANT: &str = "zed-nativepath-sqlite-v0";
const ZED_SOURCE_REVISION_KIND: &str = "zed-logical-rows-v1";
pub(crate) const ZED_PARSER_REVISION: &str =
    "zed-nativepath-source-backed-v5-neutral-core-agent-scope-optional-admission";

#[derive(Debug, Error)]
pub(crate) enum ZedSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Native(#[from] ZedNativePathError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("Zed immutable SQLite snapshot could not be acquired")]
    SnapshotAcquisitionRace,
    #[error("Zed source-backed count overflow")]
    CountOverflow,
    #[error("Zed event {0:?} was emitted without its bounded session context")]
    MissingSessionContext(String),
    #[error("Zed source-backed parser emitted an empty normalized body")]
    MissingNormalizedBody,
    #[error("Zed source-backed parser emitted an invalid SHA-256 digest")]
    InvalidDigest,
}

pub(crate) type ZedSourceBackedResultV0<T> = Result<T, ZedSourceBackedErrorV0>;

pub(crate) struct ZedSourceBackedSinkV0<'writer> {
    emit_core_record: Box<dyn FnMut(CoreRecord) -> ZedSourceBackedResultV0<()> + 'writer>,
    source: ctx_history_core::SourceKey,
    known_thread_ids: BTreeSet<String>,
    last_session: Option<ZedSessionProjectionContextV0>,
    staged_core_records: u64,
    failure: Option<ZedSourceBackedErrorV0>,
}

#[derive(Clone)]
struct ZedSessionProjectionContextV0 {
    session: ZedNativeSession,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
}

impl<'writer> ZedSourceBackedSinkV0<'writer> {
    pub(crate) fn with_emitter(
        connection: &rusqlite::Connection,
        source: ctx_history_core::SourceKey,
        emit_core_record: impl FnMut(CoreRecord) -> ZedSourceBackedResultV0<()> + 'writer,
    ) -> ZedSourceBackedResultV0<Self> {
        let mut statement = connection.prepare("SELECT id FROM threads")?;
        let known_thread_ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(Self {
            emit_core_record: Box::new(emit_core_record),
            source,
            known_thread_ids,
            last_session: None,
            staged_core_records: 0,
            failure: None,
        })
    }

    pub(crate) fn take_failure(&mut self) -> Option<ZedSourceBackedErrorV0> {
        self.failure.take()
    }

    pub(crate) fn staged_core_records(&self) -> u64 {
        self.staged_core_records
    }

    fn project_session(
        &mut self,
        session: ZedNativeSession,
    ) -> ZedSourceBackedResultV0<ZedSessionProjectionContextV0> {
        Ok(ZedSessionProjectionContextV0 {
            session_id: zed_session_identity(&self.source, &session.thread_id)?,
            parent_session_id: session
                .parent_thread_id
                .as_deref()
                .filter(|thread_id| self.known_thread_ids.contains(*thread_id))
                .map(|thread_id| zed_session_identity(&self.source, thread_id))
                .transpose()?,
            session,
        })
    }

    fn push_page_inner(&mut self, page: ZedNativePage) -> ZedSourceBackedResultV0<()> {
        let sessions = page
            .sessions
            .into_iter()
            .map(|session| self.project_session(session))
            .collect::<ZedSourceBackedResultV0<Vec<_>>>()?;
        for event in page.events {
            let session = sessions
                .iter()
                .find(|context| context.session.thread_id == event.identity.thread_id)
                .or_else(|| {
                    self.last_session
                        .as_ref()
                        .filter(|context| context.session.thread_id == event.identity.thread_id)
                })
                .ok_or_else(|| {
                    ZedSourceBackedErrorV0::MissingSessionContext(event.identity.thread_id.clone())
                })?;
            let record = zed_core_record(&self.source, session, event)?;
            (self.emit_core_record)(record)?;
            #[cfg(test)]
            record_zed_projected_core_record();
            self.staged_core_records = self
                .staged_core_records
                .checked_add(1)
                .ok_or(ZedSourceBackedErrorV0::CountOverflow)?;
        }
        if let Some(session) = sessions.last() {
            self.last_session = Some(session.clone());
        }
        Ok(())
    }
}

impl ZedNativeSink for ZedSourceBackedSinkV0<'_> {
    fn push_page(&mut self, page: ZedNativePage) -> ZedNativeResult<()> {
        if let Err(error) = self.push_page_inner(page) {
            self.failure = Some(error);
            return Err(ZedNativePathError::UnsupportedSchema(
                "Zed source-backed Core sink rejected a bounded page".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn zed_source_key() -> ZedSourceBackedResultV0<ctx_history_core::SourceKey> {
    zed_source_key_scoped(SourceAnchorScope::Unqualified)
}

pub(crate) fn zed_source_key_scoped(
    source_scope: SourceAnchorScope,
) -> ZedSourceBackedResultV0<ctx_history_core::SourceKey> {
    let anchor = SourceAnchor::provider_native(
        ZED_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(ZED_SOURCE_ANCHOR_KEY)?,
    )?;
    Ok(ctx_history_core::SourceKey::derive_scoped(
        CaptureProvider::Zed.as_str(),
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        ZED_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
        source_scope,
    )?)
}

fn zed_session_identity(
    source: &ctx_history_core::SourceKey,
    thread_id: &str,
) -> ZedSourceBackedResultV0<StableEntityId> {
    let native_session_key =
        NativeSessionKey::native_id(ZED_NATIVE_SESSION_NAMESPACE, TypedKey::utf8(thread_id)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: ZED_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn zed_core_record(
    source: &ctx_history_core::SourceKey,
    context: &ZedSessionProjectionContextV0,
    event: ZedNativeEvent,
) -> ZedSourceBackedResultV0<CoreRecord> {
    if event.normalized_body.is_empty() {
        return Err(ZedSourceBackedErrorV0::MissingNormalizedBody);
    }
    let session = &context.session;
    let session_id = context.session_id;
    let native_item_key = native_event_key(&event)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: ZED_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let event_sequence = event
        .native_order
        .message_ordinal
        .checked_mul(2)
        .and_then(|value| value.checked_add(u64::from(event.native_order.sub_ordinal)))
        .ok_or(ZedSourceBackedErrorV0::CountOverflow)?;
    let native_event_id = native_event_typed_key(&event)?;
    let structured_content = json!({
        "native_message": {
            "sqlite_rowid": event.sqlite_rowid,
            "identity": &event.identity.message,
            "thread_id": &event.identity.thread_id,
            "thread_ordinal": event.native_order.thread_ordinal,
            "message_ordinal": event.native_order.message_ordinal,
            "sub_ordinal": event.native_order.sub_ordinal,
            "record_digest": event.record_digest.as_str(),
            "kind": &event.kind,
            "call_ids": &event.call_ids,
            "content": &event.native_content,
        },
        "native_session": {
            "sqlite_rowid": session.sqlite_rowid,
            "thread_id": &session.thread_id,
            "parent_thread_id": &session.parent_thread_id,
            "title": &session.title,
            "payload_title": &session.payload_title,
            "summary": &session.summary,
            "created_at_unix_ms": session.created_at.timestamp_millis(),
            "updated_at_unix_ms": session.updated_at.timestamp_millis(),
            "native_created_at": &session.native_created_at,
            "native_updated_at": &session.native_updated_at,
            "folder_paths": &session.folder_paths,
            "native_folder_paths": &session.native_folder_paths,
            "native_folder_paths_order": &session.native_folder_paths_order,
            "native_data_type": &session.native_data_type,
            "encoding": &session.encoding,
        },
    });
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        ZED_PARSER_REVISION,
        event.normalized_body.clone(),
    )?;
    record.agent_scope = Some(if context.session.parent_thread_id.is_some() {
        AgentScope::Subagent
    } else {
        AgentScope::Primary
    });
    if let Some(parent_session_id) = context.parent_session_id {
        record.parent_session_id = Some(parent_session_id);
        record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    }
    record.provider_session_id = Some(session.thread_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = Some(event.role.as_str().to_owned());
    record.content.structured_content = Some(structured_content);
    let mut facts = Vec::new();
    if let Some(cwd) = session.cwd.clone() {
        if let Some(fact) =
            admit_provider_declared_fact(LiteralFactKind::SessionCwd, cwd, facts.len())
        {
            facts.push(fact);
        }
    }
    for folder_path in &session.folder_paths {
        if let Some(fact) = admit_provider_declared_fact(
            LiteralFactKind::Workspace,
            folder_path.clone(),
            facts.len(),
        ) {
            facts.push(fact);
        }
    }
    collect_zed_facts(&event.native_content, &mut facts);
    let (provider_call_id, invocation, result) =
        zed_activity(&event, event.occurred_at.timestamp_millis())?;
    if invocation.is_some() || result.is_some() || !facts.is_empty() {
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result,
            facts,
        });
    }
    record
        .content
        .omit_provider_declared_facts_if_aggregate_exceeds_limit()?;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(record)
}

fn collect_zed_facts(value: &serde_json::Value, facts: &mut Vec<ProviderDeclaredFact>) {
    const MAX_EXPLICIT_TOOL_FACTS: usize = 64;
    let maximum = facts.len().saturating_add(MAX_EXPLICIT_TOOL_FACTS);
    let Some(content) = value.get("content").and_then(serde_json::Value::as_array) else {
        return;
    };
    for tool_use in content
        .iter()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("tool_use"))
    {
        for (kind, pointers) in [
            (
                LiteralFactKind::File,
                &["/input/path", "/input/file", "/input/file_path"][..],
            ),
            (
                LiteralFactKind::ToolWorkdir,
                &["/input/cwd", "/input/workdir", "/input/working_directory"][..],
            ),
            (
                LiteralFactKind::Command,
                &["/input/command", "/input/cmd"][..],
            ),
            (LiteralFactKind::Url, &["/input/url", "/input/uri"][..]),
        ] {
            for pointer in pointers {
                if let Some(value) = tool_use.pointer(pointer) {
                    collect_zed_fact_values(kind, value, facts, maximum);
                    if facts.len() >= maximum {
                        return;
                    }
                }
            }
        }
    }
}

fn collect_zed_fact_values(
    kind: LiteralFactKind,
    value: &serde_json::Value,
    facts: &mut Vec<ProviderDeclaredFact>,
    maximum: usize,
) {
    if facts.len() >= maximum {
        return;
    }
    match value {
        serde_json::Value::String(value) => {
            if let Some(fact) = admit_provider_declared_fact(kind, value.clone(), facts.len()) {
                facts.push(fact);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_zed_fact_values(kind, value, facts, maximum);
            }
        }
        _ => {}
    }
}

fn zed_activity(
    event: &ZedNativeEvent,
    occurred_at_unix_ms: i64,
) -> ZedSourceBackedResultV0<(
    Option<TypedKey>,
    Option<ActivityInvocation>,
    Option<ActivityResult>,
)> {
    let Some(first_call_id) = event.call_ids.first() else {
        return Ok((None, None, None));
    };
    if event
        .call_ids
        .iter()
        .any(|call_id| call_id != first_call_id)
    {
        return Ok((None, None, None));
    }
    let Some(provider_call_id) = admit_optional_provider_call_id(Some(first_call_id.clone()))
    else {
        return Ok((None, None, None));
    };
    let invocation = event
        .native_content
        .get("content")
        .and_then(serde_json::Value::as_array)
        .and_then(|content| {
            let mut matching = content.iter().filter(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")
                    && item.get("id").and_then(serde_json::Value::as_str)
                        == Some(first_call_id.as_str())
            });
            let selected = matching.next()?;
            matching.next().is_none().then_some(selected)
        })
        .and_then(|tool_use| {
            let tool = admit_optional_metadata_text(
                tool_use
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            )?;
            let arguments = match (tool_use.get("input"), tool_use.get("raw_input")) {
                (Some(input), Some(raw_input)) if !input.is_null() && !raw_input.is_null() => {
                    ActivityJsonCapture::Unavailable
                }
                (Some(value), _) if !value.is_null() => ActivityJsonCapture::Present {
                    value: value.clone(),
                },
                (_, Some(value)) if !value.is_null() => ActivityJsonCapture::Present {
                    value: value.clone(),
                },
                _ => ActivityJsonCapture::Absent,
            };
            Some(ActivityInvocation {
                protocol: None,
                server: None,
                tool,
                arguments,
                started_at_unix_ms: Some(occurred_at_unix_ms),
            })
        });
    let result_value = event
        .native_content
        .get("tool_results")
        .and_then(serde_json::Value::as_object)
        .and_then(|results| results.get(first_call_id));
    let result = result_value.map(|value| ActivityResult {
        status: admit_optional_metadata_text(unique_zed_string(
            value,
            &["/status", "/output/status"],
        )),
        completed_at_unix_ms: Some(occurred_at_unix_ms),
        duration_ns: None,
        text: value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(|value| ActivityTextCapture::Present {
                value: value.to_owned(),
            })
            .unwrap_or(ActivityTextCapture::Absent),
        structured_content: ActivityJsonCapture::Present {
            value: value.clone(),
        },
    });
    if invocation.is_none() && result.is_none() {
        return Ok((None, None, None));
    }
    Ok((Some(provider_call_id), invocation, result))
}

fn unique_zed_string(value: &serde_json::Value, pointers: &[&str]) -> Option<String> {
    let mut selected = None::<&str>;
    for pointer in pointers {
        let Some(candidate) = value.pointer(pointer).and_then(serde_json::Value::as_str) else {
            continue;
        };
        if selected.is_some_and(|selected| selected != candidate) {
            return None;
        }
        selected = Some(candidate);
    }
    selected.map(str::to_owned)
}

fn native_event_key(event: &ZedNativeEvent) -> ZedSourceBackedResultV0<NativeItemKey> {
    let message_ordinal = event.native_order.message_ordinal;
    let sub_ordinal = u64::from(event.native_order.sub_ordinal);
    match &event.identity.message {
        ZedNativeMessageIdentity::ProviderId { value, .. } => Ok(NativeItemKey::composite(
            ZED_NATIVE_EVENT_NAMESPACE,
            vec![
                TypedKey::utf8(&event.identity.thread_id)?,
                TypedKey::utf8(value)?,
                TypedKey::U64(sub_ordinal),
            ],
        )?),
        ZedNativeMessageIdentity::MessageOrdinal(_) => Ok(NativeItemKey::certified_position(
            ZED_NATIVE_EVENT_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::utf8(&event.identity.thread_id)?,
                TypedKey::U64(message_ordinal),
                TypedKey::U64(sub_ordinal),
            ])?,
            PositionStability::AppendStable,
        )?),
    }
}

fn native_event_typed_key(event: &ZedNativeEvent) -> ZedSourceBackedResultV0<TypedKey> {
    let sub_ordinal = TypedKey::U64(u64::from(event.native_order.sub_ordinal));
    Ok(match &event.identity.message {
        ZedNativeMessageIdentity::ProviderId { value, .. } => TypedKey::composite(vec![
            TypedKey::utf8(&event.identity.thread_id)?,
            TypedKey::utf8(value)?,
            sub_ordinal,
        ])?,
        ZedNativeMessageIdentity::MessageOrdinal(message_ordinal) => TypedKey::composite(vec![
            TypedKey::utf8(&event.identity.thread_id)?,
            TypedKey::U64(*message_ordinal),
            sub_ordinal,
        ])?,
    })
}

pub(crate) fn source_observation(
    source: &ctx_history_core::SourceKey,
    snapshot_revision: &str,
) -> ZedSourceBackedResultV0<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        ZED_SOURCE_REVISION_KIND,
        snapshot_revision.as_bytes().to_vec(),
    )?)
}

pub(crate) fn snapshot_revision_digest(snapshot_revision: &str) -> [u8; 32] {
    Sha256::digest(snapshot_revision.as_bytes()).into()
}

pub(crate) fn acquire_snapshot(
    data_root: &Path,
    path: &Path,
) -> ZedSourceBackedResultV0<super::ZedImmutableSqliteSnapshot> {
    match acquire_immutable_snapshot(data_root, path)? {
        ZedSnapshotAcquisition::Acquired(snapshot) => Ok(*snapshot),
        ZedSnapshotAcquisition::Incomplete => Err(ZedSourceBackedErrorV0::SnapshotAcquisitionRace),
    }
}

pub(crate) fn decode_sha256_hex(value: &str) -> ZedSourceBackedResultV0<[u8; 32]> {
    if value.len() != 64 {
        return Err(ZedSourceBackedErrorV0::InvalidDigest);
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0]).ok_or(ZedSourceBackedErrorV0::InvalidDigest)?;
        let low = hex_nibble(pair[1]).ok_or(ZedSourceBackedErrorV0::InvalidDigest)?;
        digest[index] = (high << 4) | low;
    }
    Ok(digest)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "source_backed/semantic_tests.rs"]
mod semantic_tests;

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        fs,
        io::{Read, Seek, SeekFrom, Write},
        rc::Rc,
    };

    use rusqlite::{params, Connection};
    use serde_json::json;

    use super::*;
    use crate::providers::zed::native_path::dto::ZedNativeRejectionKind;

    #[test]
    fn source_backed_zed_direct_core_and_replacement_preserve_stable_ids() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let database = source.join("threads.db");
        create_database(&database, "cold exact sentinel");

        let event = project_root_record(&database);
        let cold_event_id = event.event_id;
        let cold_session_id = event.session_id;
        let cold_native_event_id = event.native_event_id.clone();
        assert_eq!(event.parent_session_id, None);
        assert_eq!(event.root_session_id, None);
        assert_eq!(event.provider_session_id.as_deref(), Some("thread-1"));
        assert!(event.native_event_id.is_some());
        assert_eq!(
            event.event_type,
            ctx_history_core::EventType::Message.as_str()
        );
        assert_eq!(
            event.role.as_deref(),
            Some(ctx_history_core::EventRole::User.as_str())
        );
        assert_eq!(
            event.occurred_at_unix_ms,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-07-28T12:00:10Z")
                    .unwrap()
                    .timestamp_millis()
            )
        );
        assert_eq!(event.content.meaningful_text(), "cold exact sentinel");
        let structured = event.content.structured_content.as_ref().unwrap();
        assert_eq!(
            structured
                .pointer("/native_message/content/content/0/type")
                .and_then(serde_json::Value::as_str),
            Some("text")
        );
        assert_eq!(
            structured
                .pointer("/native_session/title")
                .and_then(serde_json::Value::as_str),
            Some("Source-backed Zed thread")
        );
        assert_eq!(
            structured
                .pointer("/native_session/native_updated_at")
                .and_then(serde_json::Value::as_str),
            Some("2026-07-28T12:00:10Z")
        );
        assert_eq!(
            structured
                .pointer("/native_session/native_data_type")
                .and_then(serde_json::Value::as_str),
            Some("json")
        );
        let encoded = String::from_utf8(event.encode_stored().unwrap()).unwrap();
        assert!(!encoded.contains("\"locator\""));
        assert!(!encoded.contains("\"source_path\""));

        replace_thread(&database, "replacement exact sentinel");
        let replacement_event = project_root_record(&database);
        assert_eq!(replacement_event.event_id, cold_event_id);
        assert_eq!(replacement_event.session_id, cold_session_id);
        assert_eq!(replacement_event.native_event_id, cold_native_event_id);
        assert_eq!(
            replacement_event.content.meaningful_text(),
            "replacement exact sentinel"
        );
    }

    #[test]
    fn source_backed_zed_resolves_native_thread_lineage() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("threads.db");
        create_database(&database, "root lineage sentinel");
        insert_child_thread(&database, "child lineage sentinel");

        let records = project_records_through_sink(&database);
        let child = records
            .iter()
            .find(|record| record.provider_session_id.as_deref() == Some("a-child"))
            .unwrap();
        let root = records
            .iter()
            .find(|record| record.provider_session_id.as_deref() == Some("thread-1"))
            .unwrap();
        assert_eq!(child.parent_session_id, Some(root.session_id));
        assert_eq!(child.agent_scope, Some(AgentScope::Subagent));
        assert_eq!(root.agent_scope, Some(AgentScope::Primary));
        assert_eq!(
            child.session_relationship,
            Some(ProviderNativeSessionRelationship::Delegated)
        );
    }

    #[test]
    fn provider_p1_lineage_rejects_a_missing_referenced_parent() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("threads.db");
        create_database(&database, "missing parent lineage sentinel");
        Connection::open(&database)
            .unwrap()
            .execute(
                "update threads set parent_id = 'missing-parent' where id = 'thread-1'",
                [],
            )
            .unwrap();

        let records = project_records_through_sink(&database);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].parent_session_id, None);
        assert_eq!(records[0].session_relationship, None);
        assert_eq!(records[0].agent_scope, Some(AgentScope::Subagent));
    }

    #[test]
    fn unsupported_version_and_malformed_payload_are_rejected_without_hiding_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let database = source.join("threads.db");
        create_database(&database, "healthy sibling");
        let connection = Connection::open(&database).unwrap();
        insert_raw_thread(
            &connection,
            "unsupported-version",
            serde_json::to_vec(&json!({
                "version": "9.9.9",
                "messages": []
            }))
            .unwrap(),
        );
        insert_raw_thread(&connection, "malformed-json", b"{not-json".to_vec());
        drop(connection);

        let mut snapshot =
            acquire_snapshot(crate::test_provider_sqlite_data_root(), &database).unwrap();
        let revision = snapshot.snapshot_revision.clone();
        let mut sink = CollectingSink::default();
        let scan =
            scan_zed_native_snapshot(snapshot.connection().unwrap(), &revision, &mut sink).unwrap();
        snapshot.finish().unwrap();

        assert_eq!(scan.counters.native_thread_rows, 3);
        assert_eq!(scan.counters.sessions_retained, 1);
        assert_eq!(scan.counters.rejected_threads, 2);
        let rejections = sink
            .pages
            .iter()
            .flat_map(|page| page.rejections.iter())
            .map(|rejection| rejection.kind)
            .collect::<Vec<_>>();
        assert!(rejections.contains(&ZedNativeRejectionKind::UnsupportedThreadVersion));
        assert!(rejections.contains(&ZedNativeRejectionKind::MalformedJson));
    }

    #[test]
    fn physical_mutation_during_pinned_snapshot_fails_terminal_authority() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("threads.db");
        create_database(&database, "terminal authority sentinel");
        let mut snapshot = acquire_snapshot(&temp.path().join("data-root"), &database).unwrap();

        rewrite_same_database_bytes(&database);
        let error = snapshot.finish().unwrap_err();
        assert!(matches!(
            error,
            ZedNativePathError::Capture(crate::CaptureError::SourceChangedDuringCapture)
        ));
    }

    #[derive(Default)]
    struct CollectingSink {
        pages: Vec<ZedNativePage>,
    }

    impl ZedNativeSink for CollectingSink {
        fn push_page(&mut self, page: ZedNativePage) -> ZedNativeResult<()> {
            self.pages.push(page);
            Ok(())
        }
    }

    pub(super) fn project_root_record(path: &Path) -> CoreRecord {
        let mut snapshot = acquire_snapshot(crate::test_provider_sqlite_data_root(), path).unwrap();
        let revision = snapshot.snapshot_revision.clone();
        let mut sink = CollectingSink::default();
        let scan =
            scan_zed_native_snapshot(snapshot.connection().unwrap(), &revision, &mut sink).unwrap();
        assert_eq!(scan.counters.native_thread_rows, 1);
        assert_eq!(scan.counters.sessions_retained, 1);
        assert_eq!(scan.counters.retained_events, 1);
        assert_eq!(scan.counters.rejected_threads, 0);
        assert!(scan.counters.certified_logical_bytes > 0);
        snapshot.finish().unwrap();

        let mut sessions = sink
            .pages
            .iter_mut()
            .flat_map(|page| page.sessions.drain(..));
        let session = sessions.next().unwrap();
        assert!(sessions.next().is_none());
        drop(sessions);
        let source = zed_source_key().unwrap();
        let session_id = zed_session_identity(&source, &session.thread_id).unwrap();
        let context = ZedSessionProjectionContextV0 {
            session,
            session_id,
            parent_session_id: None,
        };
        let mut events = sink.pages.into_iter().flat_map(|page| page.events);
        let event = events.next().unwrap();
        assert!(events.next().is_none());
        zed_core_record(&source, &context, event).unwrap()
    }

    fn project_records_through_sink(path: &Path) -> Vec<CoreRecord> {
        let mut snapshot = acquire_snapshot(crate::test_provider_sqlite_data_root(), path).unwrap();
        let revision = snapshot.snapshot_revision.clone();
        let records = Rc::new(RefCell::new(Vec::new()));
        {
            let emitted = Rc::clone(&records);
            let connection = snapshot.connection().unwrap();
            let mut sink = ZedSourceBackedSinkV0::with_emitter(
                connection,
                zed_source_key().unwrap(),
                move |record| {
                    emitted.borrow_mut().push(record);
                    Ok(())
                },
            )
            .unwrap();
            scan_zed_native_snapshot(connection, &revision, &mut sink).unwrap();
            assert!(sink.take_failure().is_none());
        }
        snapshot.finish().unwrap();
        Rc::try_unwrap(records).unwrap().into_inner()
    }

    pub(super) fn create_database(path: &Path, text: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "PRAGMA user_version = 3;
                 CREATE TABLE threads (
                     id TEXT PRIMARY KEY,
                     summary TEXT NOT NULL,
                     updated_at TEXT NOT NULL,
                     data_type TEXT NOT NULL,
                     data BLOB NOT NULL,
                     parent_id TEXT,
                     folder_paths TEXT,
                     folder_paths_order TEXT,
                     created_at TEXT
                 );",
            )
            .unwrap();
        insert_thread(&connection, text);
    }

    fn insert_thread(connection: &Connection, text: &str) {
        let payload = serde_json::to_vec(&json!({
            "version": "0.3.0",
            "title": "Source-backed Zed thread",
            "updated_at": "2026-07-28T12:00:10Z",
            "messages": [{
                "User": {
                    "id": "message-1",
                    "content": [{"Text": text}]
                }
            }]
        }))
        .unwrap();
        connection
            .execute(
                "INSERT INTO threads (
                     id, summary, updated_at, data_type, data, parent_id,
                     folder_paths, folder_paths_order, created_at
                 ) VALUES (
                     'thread-1', 'source-backed fixture', '2026-07-28T12:00:10Z',
                     'json', ?1, NULL, '/workspace/zed', '0',
                     '2026-07-28T12:00:00Z'
                 )",
                params![payload],
            )
            .unwrap();
    }

    fn insert_raw_thread(connection: &Connection, id: &str, payload: Vec<u8>) {
        connection
            .execute(
                "INSERT INTO threads (
                     id, summary, updated_at, data_type, data, parent_id,
                     folder_paths, folder_paths_order, created_at
                 ) VALUES (
                     ?1, 'source-backed rejection fixture', '2026-07-28T12:00:10Z',
                     'json', ?2, NULL, '/workspace/zed', '0',
                     '2026-07-28T12:00:00Z'
                 )",
                params![id, payload],
            )
            .unwrap();
    }

    fn insert_child_thread(path: &Path, text: &str) {
        let connection = Connection::open(path).unwrap();
        let payload = serde_json::to_vec(&json!({
            "version": "0.3.0",
            "title": "Source-backed Zed child thread",
            "updated_at": "2026-07-28T12:00:12Z",
            "messages": [{
                "User": {
                    "id": "message-child",
                    "content": [{"Text": text}]
                }
            }]
        }))
        .unwrap();
        connection
            .execute(
                "INSERT INTO threads (
                     id, summary, updated_at, data_type, data, parent_id,
                     folder_paths, folder_paths_order, created_at
                 ) VALUES (
                     'a-child', 'source-backed child fixture', '2026-07-28T12:00:12Z',
                     'json', ?1, 'thread-1', '/workspace/zed', '0',
                     '2026-07-28T12:00:01Z'
                 )",
                params![payload],
            )
            .unwrap();
    }

    fn rewrite_same_database_bytes(path: &Path) {
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let mut byte = [0_u8; 1];
        file.seek(SeekFrom::Start(64)).unwrap();
        file.read_exact(&mut byte).unwrap();
        file.seek(SeekFrom::Start(64)).unwrap();
        file.write_all(&[byte[0] ^ 1]).unwrap();
        file.seek(SeekFrom::Start(64)).unwrap();
        file.write_all(&byte).unwrap();
        file.sync_all().unwrap();
    }

    pub(super) fn replace_thread(path: &Path, text: &str) {
        let connection = Connection::open(path).unwrap();
        let payload = serde_json::to_vec(&json!({
            "version": "0.3.0",
            "title": "Source-backed Zed thread",
            "updated_at": "2026-07-28T12:00:11Z",
            "messages": [{
                "User": {
                    "id": "message-1",
                    "content": [{"Text": text}]
                }
            }]
        }))
        .unwrap();
        connection
            .execute(
                "UPDATE threads
                 SET data = ?1, updated_at = '2026-07-28T12:00:11Z'
                 WHERE id = 'thread-1'",
                params![payload],
            )
            .unwrap();
    }
}
