use std::{
    fs::File,
    io::{BufRead, BufReader, BufWriter, Seek, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
    TypedKey,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    provider::{
        providers::nanoclaw::{
            project::{NanoClawProjectOpenError, NanoClawSourceBackedProject},
            projection::nanoclaw_core_event,
            source::{NanoClawNativeScanner, NanoClawNativeUnit, NanoClawPreparedUnit},
        },
        source_backed::{
            family::document::{
                ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
                DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
            },
            SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
        },
        sqlite::sqlite_schema_fingerprint,
    },
    CaptureError, NANOCLAW_SOURCE_FORMAT,
};

const NANOCLAW_SOURCE_SCHEMA_VARIANT: &str = "nanoclaw-compound-project-v1";
const NANOCLAW_SOURCE_REVISION_KIND: &str = "nanoclaw-compound-project-snapshot-v1";
const NANOCLAW_SOURCE_BACKED_PARSER_REVISION: &str = "nanoclaw-source-backed-v3";
const NANOCLAW_LOGICAL_SESSION_KIND: &str = "nanoclaw-session";
const NANOCLAW_NATIVE_SESSION_NAMESPACE: &str = "nanoclaw.project-session";
const NANOCLAW_LOGICAL_EVENT_KIND: &str = "nanoclaw-message";
const NANOCLAW_NATIVE_EVENT_NAMESPACE: &str = "nanoclaw.project-message";

mod replay;

use replay::{
    NanoClawCertifiedReplayCheckpoint, NanoClawPreparedAuthority, NanoClawReplayFrontier,
};

#[derive(Debug, Error)]
pub(crate) enum NanoClawSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error("NanoClaw source-backed scan counters overflowed")]
    CountOverflow,
    #[error("NanoClaw source-backed scanner emitted inconsistent counts")]
    CountMismatch,
    #[error("NanoClaw certified replay checkpoint is invalid: {0}")]
    InvalidReplayCheckpoint(&'static str),
}

pub(crate) type NanoClawSourceBackedResult<T> = Result<T, NanoClawSourceBackedError>;

#[derive(Debug, Clone)]
pub(crate) struct NanoClawDocumentLeaf {
    source: SourceKey,
}

pub(crate) struct NanoClawDocumentTreeAuthority {
    prepared: Mutex<NanoClawPreparedAuthority>,
}

type NanoClawDocumentTree =
    CompleteDocumentTree<NanoClawDocumentLeaf, NanoClawDocumentTreeAuthority>;

#[derive(Clone)]
pub(crate) struct NanoClawDocumentTreeAdapter {
    data_root: PathBuf,
    path: PathBuf,
    source: SourceKey,
    certified_checkpoint: Option<NanoClawCertifiedReplayCheckpoint>,
    replay_frontier: Arc<Mutex<Option<NanoClawReplayFrontier>>>,
}

struct NanoClawPreparedProjection {
    spool: File,
    logical_fingerprint: [u8; 32],
    observation: SourceObservation,
    content_digest: [u8; 32],
    counts: ScannedSourceCounts,
}

impl ReplacementDocumentTree for NanoClawDocumentTreeAdapter {
    type Leaf = NanoClawDocumentLeaf;
    type TreeAuthority = NanoClawDocumentTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        NANOCLAW_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        self.source.exact_descriptor_eq(source)
    }

    fn discover_complete(&self) -> SourceBackedRouteResult<NanoClawDocumentTree> {
        let replay = self
            .replay_frontier
            .lock()
            .map_err(|_| nanoclaw_internal("NanoClaw replay frontier lock was poisoned"))?
            .clone();
        let authority = if let Some(frontier) = replay {
            if frontier
                .snapshot
                .revalidate()
                .map_err(nanoclaw_route_capture_error)?
            {
                NanoClawPreparedAuthority {
                    frontier,
                    projection: None,
                }
            } else {
                self.prepare_authority()?
            }
        } else if let Some(checkpoint) = self.certified_checkpoint {
            self.prepare_certified_checkpoint(checkpoint)?
        } else {
            self.prepare_authority()?
        };
        let tree_fingerprint =
            nanoclaw_tree_fingerprint(authority.frontier.logical_fingerprint, &self.source);
        Ok(CompleteDocumentTree::new(
            tree_fingerprint,
            vec![ObservedDocumentLeaf::new(
                DocumentLeafFingerprint::new(authority.frontier.physical_fingerprint),
                NanoClawDocumentLeaf {
                    source: self.source.clone(),
                },
            )],
            NanoClawDocumentTreeAuthority {
                prepared: Mutex::new(authority),
            },
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        if !leaf.source.exact_descriptor_eq(&self.source) {
            return Err(nanoclaw_changed(
                "NanoClaw document leaf changed catalog lineage",
            ));
        }
        let mut prepared = authority
            .prepared
            .lock()
            .map_err(|_| nanoclaw_internal("NanoClaw document authority lock was poisoned"))?;
        if prepared.projection.is_none() {
            let expected = prepared.frontier.clone();
            let current = self.prepare_authority()?;
            if current.frontier.physical_fingerprint != expected.physical_fingerprint
                || current.frontier.logical_fingerprint != expected.logical_fingerprint
            {
                return Err(nanoclaw_changed(
                    "NanoClaw compound project changed after revision precheck",
                ));
            }
            *prepared = current;
        }
        let projection = prepared
            .projection
            .as_mut()
            .ok_or_else(|| nanoclaw_internal("NanoClaw projection was not prepared"))?;
        project_nanoclaw_prepared(projection, &leaf.source, sink)
    }

    fn revalidate_complete(
        &self,
        tree: &NanoClawDocumentTree,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let prepared = tree
            .authority
            .prepared
            .lock()
            .map_err(|_| nanoclaw_internal("NanoClaw document authority lock was poisoned"))?;
        if !prepared
            .frontier
            .snapshot
            .revalidate()
            .map_err(nanoclaw_route_capture_error)?
        {
            return Err(nanoclaw_changed(
                "NanoClaw compound project changed before publication",
            ));
        }
        let terminal =
            nanoclaw_tree_fingerprint(prepared.frontier.logical_fingerprint, &self.source);
        if terminal != tree.tree_fingerprint {
            return Err(nanoclaw_changed(
                "NanoClaw logical project revision changed before publication",
            ));
        }
        let frontier = prepared.frontier.clone();
        drop(prepared);
        *self
            .replay_frontier
            .lock()
            .map_err(|_| nanoclaw_internal("NanoClaw replay frontier lock was poisoned"))? =
            Some(frontier);
        Ok(terminal)
    }
}

fn prepare_nanoclaw_project(
    data_root: &Path,
    project: &mut NanoClawSourceBackedProject,
    source: &SourceKey,
) -> SourceBackedRouteResult<NanoClawPreparedProjection> {
    let central = project.connection().map_err(nanoclaw_route_capture_error)?;
    let user_version: i64 = central
        .query_row("pragma user_version", [], |row| row.get(0))
        .map_err(CaptureError::from)
        .map_err(nanoclaw_route_capture_error)?;
    let schema_fingerprint =
        sqlite_schema_fingerprint(central).map_err(nanoclaw_route_capture_error)?;
    let mut scanner = NanoClawNativeScanner::new(central, project.snapshot())
        .map_err(nanoclaw_route_capture_error)?;
    let mut spool = tempfile::tempfile_in(data_root)
        .map_err(CaptureError::from)
        .map_err(nanoclaw_route_capture_error)?;
    let mut spool_writer = BufWriter::new(&mut spool);
    let mut complete_records = 0_u64;
    let mut retained_records = 0_u64;
    let mut rejected_records = 0_u64;
    let mut ignored_records = 0_u64;
    let mut indexed_documents = 0_u64;
    loop {
        let page = scanner.next_page().map_err(nanoclaw_route_capture_error)?;
        let terminal = page.terminal;
        for unit in page.units {
            complete_records = checked_add(complete_records, 1).map_err(nanoclaw_route_error)?;
            match &unit {
                NanoClawNativeUnit::Session { .. } => {
                    ignored_records =
                        checked_add(ignored_records, 1).map_err(nanoclaw_route_error)?;
                }
                NanoClawNativeUnit::Message {
                    ordinal,
                    source: message_source,
                    session,
                    message,
                    ..
                } => {
                    let _ = (ordinal, message_source, session, message);
                    retained_records =
                        checked_add(retained_records, 1).map_err(nanoclaw_route_error)?;
                    indexed_documents =
                        checked_add(indexed_documents, 1).map_err(nanoclaw_route_error)?;
                }
                NanoClawNativeUnit::Rejection { .. } => {
                    rejected_records =
                        checked_add(rejected_records, 1).map_err(nanoclaw_route_error)?;
                }
            }
            serde_json::to_writer(&mut spool_writer, &NanoClawPreparedUnit::from_native(unit))
                .map_err(CaptureError::from)
                .map_err(nanoclaw_route_capture_error)?;
            spool_writer
                .write_all(b"\n")
                .map_err(CaptureError::from)
                .map_err(nanoclaw_route_capture_error)?;
        }
        if terminal {
            break;
        }
    }

    let prefix_digest = scanner.prefix_digest_bytes();
    let certified_bytes = scanner.prefix_bytes();
    scanner.finish().map_err(nanoclaw_route_capture_error)?;
    spool_writer
        .flush()
        .map_err(CaptureError::from)
        .map_err(nanoclaw_route_capture_error)?;
    drop(spool_writer);
    project.finish().map_err(nanoclaw_route_capture_error)?;
    let classified = retained_records
        .checked_add(rejected_records)
        .and_then(|value| value.checked_add(ignored_records))
        .ok_or_else(|| nanoclaw_route_error(NanoClawSourceBackedError::CountOverflow))?;
    if classified != complete_records || indexed_documents != retained_records {
        return Err(nanoclaw_route_error(
            NanoClawSourceBackedError::CountMismatch,
        ));
    }
    let counts = ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents,
        certified_bytes,
    };
    let mut logical = Sha256::new();
    logical.update(b"ctx-nanoclaw-compound-logical-snapshot-v1\0");
    logical.update(project.snapshot().logical_authority_fingerprint());
    logical.update(user_version.to_be_bytes());
    logical.update((schema_fingerprint.len() as u64).to_be_bytes());
    logical.update(schema_fingerprint.as_bytes());
    logical.update(prefix_digest);
    for count in [
        counts.complete_records,
        counts.retained_records,
        counts.rejected_records,
        counts.ignored_records,
        counts.indexed_documents,
        counts.certified_bytes,
    ] {
        logical.update(count.to_be_bytes());
    }
    let logical_fingerprint: [u8; 32] = logical.finalize().into();
    let revision = project
        .snapshot()
        .source_backed_revision_evidence(
            user_version,
            &schema_fingerprint,
            logical_fingerprint,
            counts,
        )
        .map_err(nanoclaw_route_capture_error)?;
    let observation =
        SourceObservation::new(source.clone(), NANOCLAW_SOURCE_REVISION_KIND, revision)
            .map_err(nanoclaw_route_contract_error)?;
    spool
        .rewind()
        .map_err(CaptureError::from)
        .map_err(nanoclaw_route_capture_error)?;
    Ok(NanoClawPreparedProjection {
        spool,
        logical_fingerprint,
        observation,
        content_digest: logical_fingerprint,
        counts,
    })
}

fn project_nanoclaw_prepared(
    prepared: &mut NanoClawPreparedProjection,
    source: &SourceKey,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> SourceBackedRouteResult<DocumentSourceTerminal> {
    prepared
        .spool
        .rewind()
        .map_err(CaptureError::from)
        .map_err(nanoclaw_route_capture_error)?;
    sink.begin_source(source.clone())?;
    let mut reader = BufReader::new(&mut prepared.spool);
    let mut line = String::new();
    loop {
        line.clear();
        if reader
            .read_line(&mut line)
            .map_err(CaptureError::from)
            .map_err(nanoclaw_route_capture_error)?
            == 0
        {
            break;
        }
        let unit: NanoClawPreparedUnit = serde_json::from_str(&line)
            .map_err(CaptureError::from)
            .map_err(nanoclaw_route_capture_error)?;
        if let NanoClawPreparedUnit::Message {
            ordinal,
            source: message_source,
            session,
            message,
            ..
        } = unit
        {
            let document = nanoclaw_core_record(
                source,
                ordinal,
                message_source.label(),
                &session,
                &message.into_native(message_source),
            )
            .map_err(nanoclaw_route_error)?;
            sink.emit_core_record(document)?;
        }
    }
    Ok(DocumentSourceTerminal {
        source: source.clone(),
        opening: prepared.observation.clone(),
        closing: prepared.observation.clone(),
        parser_revision: NANOCLAW_SOURCE_BACKED_PARSER_REVISION,
        content_digest: prepared.content_digest,
        counts: prepared.counts,
    })
}

fn nanoclaw_tree_fingerprint(logical: [u8; 32], source: &SourceKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-nanoclaw-document-tree-v1\0");
    digest.update(logical);
    digest.update(source.identity().digest());
    digest.finalize().into()
}

fn nanoclaw_route_error(error: NanoClawSourceBackedError) -> SourceBackedRouteError {
    let kind = match &error {
        NanoClawSourceBackedError::Capture(CaptureError::SourceChangedDuringCapture) => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        NanoClawSourceBackedError::Capture(CaptureError::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            SourceBackedRouteErrorKind::Unavailable
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn nanoclaw_route_capture_error(error: CaptureError) -> SourceBackedRouteError {
    nanoclaw_route_error(error.into())
}

fn nanoclaw_route_project_open_error(error: NanoClawProjectOpenError) -> SourceBackedRouteError {
    match error {
        NanoClawProjectOpenError::Capture(error) => nanoclaw_route_capture_error(error),
        limit @ NanoClawProjectOpenError::SessionSnapshotLimitExceeded { .. } => {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::InvalidSource,
                limit.to_string(),
            )
        }
    }
}

fn nanoclaw_route_contract_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

fn nanoclaw_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn nanoclaw_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

pub(crate) fn nanoclaw_source_key(
    catalog_lineage: [u8; 32],
) -> NanoClawSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive(
        CaptureProvider::NanoClaw.as_str(),
        NANOCLAW_SOURCE_FORMAT,
        NANOCLAW_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::CatalogLineage(catalog_lineage),
    )?)
}

fn nanoclaw_core_record(
    source: &SourceKey,
    ordinal: u64,
    message_source: &str,
    session: &super::super::rows::NanoClawSessionRow,
    message: &super::super::rows::NanoClawMessageRow,
) -> NanoClawSourceBackedResult<CoreRecord> {
    let native_session_key = NativeSessionKey::composite(
        NANOCLAW_NATIVE_SESSION_NAMESPACE,
        vec![
            TypedKey::utf8(&session.agent_group_id)?,
            TypedKey::utf8(&session.id)?,
        ],
    )?;
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: NANOCLAW_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?;
    let native_event_parts = vec![
        TypedKey::utf8(message_source)?,
        TypedKey::utf8(&message.id)?,
    ];
    let native_event_id = TypedKey::composite(native_event_parts.clone())?;
    let native_item_key =
        NativeItemKey::composite(NANOCLAW_NATIVE_EVENT_NAMESPACE, native_event_parts)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: NANOCLAW_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let (event, exact_text) = nanoclaw_core_event(message, chrono::DateTime::UNIX_EPOCH);
    let mut body = exact_text;
    if body.is_empty() {
        body = format!("NanoClaw {message_source} message");
    }
    let provider_session_id = session
        .thread_id
        .clone()
        .filter(|thread| !thread.is_empty())
        .unwrap_or_else(|| session.id.clone());
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        ordinal,
        event.event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
        NANOCLAW_SOURCE_BACKED_PARSER_REVISION,
        body,
    )?;
    record.provider_session_id = Some(provider_session_id);
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = event.role.map(|role| role.as_str().to_owned());
    record.workspace = session.agent_group_folder.clone();
    record.cwd = session.agent_group_folder.clone();
    record.validate_contract()?;
    Ok(record)
}

fn checked_add(value: u64, increment: u64) -> NanoClawSourceBackedResult<u64> {
    value
        .checked_add(increment)
        .ok_or(NanoClawSourceBackedError::CountOverflow)
}
