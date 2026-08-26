use std::{
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope, CaptureProvider, CertifiedSource, CoreActivity,
    CoreRecord, CoreRecordError, EventIdentityInput, LiteralFactKind, NativeItemKey,
    NativeSessionKey, ProjectionContractError, ProviderDeclaredFact, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceAnchorScope, SourceKey, SourceObservation,
    StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
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
            combine_primary_and_cleanup_route_errors,
            family::document::{
                ChangedDocumentSink, CompleteDocumentTree, DocumentLeafExecutionPolicy,
                DocumentLeafFingerprint, DocumentSourceTerminal, ObservedDocumentLeaf,
                ReplacementDocumentTree,
            },
            SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
        },
        sqlite::sqlite_schema_fingerprint,
    },
    CaptureError, NANOCLAW_SOURCE_FORMAT,
};

const NANOCLAW_SOURCE_SCHEMA_VARIANT: &str = "nanoclaw-compound-project-v1";
const NANOCLAW_SOURCE_REVISION_KIND: &str = "nanoclaw-compound-project-snapshot-v1";
const NANOCLAW_SOURCE_BACKED_PARSER_REVISION: &str = "nanoclaw-source-backed-v4-neutral-core";
const NANOCLAW_LOGICAL_SESSION_KIND: &str = "nanoclaw-session";
const NANOCLAW_NATIVE_SESSION_NAMESPACE: &str = "nanoclaw.project-session";
const NANOCLAW_LOGICAL_EVENT_KIND: &str = "nanoclaw-message";
const NANOCLAW_NATIVE_EVENT_NAMESPACE: &str = "nanoclaw.project-message";

mod replay;

use replay::{
    NanoClawCertifiedReplayCheckpoint, NanoClawPreparedAuthority, NanoClawReplayFrontier,
};

#[derive(Debug, Error)]
pub enum NanoClawSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    SqliteStaging(#[from] crate::provider_sources::SqliteSourceAccessError),
    #[error("NanoClaw private SQLite staging data is invalid: {0}")]
    StagingData(#[source] serde_json::Error),
    #[error("NanoClaw source-backed scan counters overflowed")]
    CountOverflow,
    #[error("NanoClaw source-backed scanner emitted inconsistent counts")]
    CountMismatch,
    #[error("NanoClaw certified replay checkpoint is invalid: {0}")]
    InvalidReplayCheckpoint(
        #[source] ctx_history_capture_runtime::DocumentFullSnapshotCheckpointError,
    ),
    #[error("multiple current NanoClaw certificates name the same compound source")]
    DuplicateReplayCheckpoint,
}

pub type NanoClawSourceBackedResult<T> = Result<T, NanoClawSourceBackedError>;

#[derive(Debug, Clone)]
pub struct NanoClawDocumentLeaf {
    source: SourceKey,
}

pub struct NanoClawDocumentTreeAuthority {
    prepared: Mutex<NanoClawPreparedAuthority>,
}

type NanoClawDocumentTree =
    CompleteDocumentTree<NanoClawDocumentLeaf, NanoClawDocumentTreeAuthority>;

#[derive(Clone)]
pub struct NanoClawDocumentTreeAdapter<B = ()> {
    data_root: PathBuf,
    path: PathBuf,
    source: SourceKey,
    certified_checkpoint: Option<NanoClawCertifiedReplayCheckpoint>,
    replay_frontier: Arc<Mutex<Option<NanoClawReplayFrontier>>>,
    _binding: PhantomData<fn() -> B>,
}

struct NanoClawPreparedProjection {
    spool: crate::provider_sources::SqliteSourceStagingFile,
    logical_fingerprint: [u8; 32],
    observation: SourceObservation,
    content_digest: [u8; 32],
    counts: ScannedSourceCounts,
}

impl<B> ReplacementDocumentTree for NanoClawDocumentTreeAdapter<B>
where
    B: crate::ProviderRuntimeBinding,
{
    type Lifecycle = B::CaptureLifecycleSink;
    type Spool = B::DocumentRecordSpool;
    type RouteControl = crate::ProviderRouteControlExpectation;
    type Leaf = NanoClawDocumentLeaf;
    type TreeAuthority = NanoClawDocumentTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        NANOCLAW_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        self.source.exact_descriptor_eq(source)
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        DocumentLeafExecutionPolicy::Serial
    }

    fn durable_replay_source(
        &self,
        _authority: &Self::TreeAuthority,
        _leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<Option<SourceKey>> {
        Ok(Some(self.source.clone()))
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
        sink: &mut ChangedDocumentSink<'_, '_, B>,
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
        project_nanoclaw_prepared::<B>(projection, &leaf.source, sink)
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
    let primary = (|| {
        let central = project.connection().map_err(nanoclaw_route_capture_error)?;
        let user_version: i64 = central
            .query_row("pragma user_version", [], |row| row.get(0))
            .map_err(CaptureError::from)
            .map_err(nanoclaw_route_capture_error)?;
        let schema_fingerprint =
            sqlite_schema_fingerprint(central).map_err(nanoclaw_route_capture_error)?;
        let mut scanner = NanoClawNativeScanner::new(central, project.snapshot())
            .map_err(nanoclaw_route_capture_error)?;
        let mut spool = crate::provider_sources::open_private_sqlite_staging_file(data_root)
            .map_err(nanoclaw_route_staging_error)?;
        let scan = (|| {
            let mut complete_records = 0_u64;
            let mut retained_records = 0_u64;
            let mut rejected_records = 0_u64;
            let mut ignored_records = 0_u64;
            let mut indexed_documents = 0_u64;
            loop {
                let page = scanner.next_page().map_err(nanoclaw_route_capture_error)?;
                let terminal = page.terminal;
                for unit in page.units {
                    complete_records =
                        checked_add(complete_records, 1).map_err(nanoclaw_route_error)?;
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
                    let mut encoded = serde_json::to_vec(&NanoClawPreparedUnit::from_native(unit))
                        .map_err(nanoclaw_route_staging_data_error)?;
                    encoded.push(b'\n');
                    spool
                        .write_all(&encoded)
                        .map_err(nanoclaw_route_staging_error)?;
                }
                if terminal {
                    break;
                }
            }
            let prefix_digest = scanner.prefix_digest_bytes();
            let certified_bytes = scanner.prefix_bytes();
            spool.flush().map_err(nanoclaw_route_staging_error)?;
            Ok((
                complete_records,
                retained_records,
                rejected_records,
                ignored_records,
                indexed_documents,
                prefix_digest,
                certified_bytes,
            ))
        })();
        let scan = combine_nanoclaw_finalization(
            scan,
            scanner.finish().map_err(nanoclaw_route_capture_error),
        )?;
        let (
            complete_records,
            retained_records,
            rejected_records,
            ignored_records,
            indexed_documents,
            prefix_digest,
            certified_bytes,
        ) = scan;
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
        rewind_nanoclaw_staging(&mut spool)?;
        Ok(NanoClawPreparedProjection {
            spool,
            logical_fingerprint,
            observation,
            content_digest: logical_fingerprint,
            counts,
        })
    })();
    combine_nanoclaw_finalization(
        primary,
        project.finish().map_err(nanoclaw_route_capture_error),
    )
}

fn combine_nanoclaw_finalization<T>(
    primary: SourceBackedRouteResult<T>,
    finalization: SourceBackedRouteResult<()>,
) -> SourceBackedRouteResult<T> {
    match (primary, finalization) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(_), Err(finalization)) => Err(finalization),
        (Err(primary), Err(finalization)) => Err(combine_primary_and_cleanup_route_errors(
            primary,
            finalization,
        )),
    }
}

fn project_nanoclaw_prepared<B>(
    prepared: &mut NanoClawPreparedProjection,
    source: &SourceKey,
    sink: &mut ChangedDocumentSink<'_, '_, B>,
) -> SourceBackedRouteResult<DocumentSourceTerminal>
where
    B: crate::ProviderRuntimeBinding,
{
    rewind_nanoclaw_staging(&mut prepared.spool)?;
    let mut reader = prepared.spool.reader();
    let mut line = String::new();
    let mut unit = read_nanoclaw_staging_unit(&mut reader, &mut line)?;
    sink.begin_source(source.clone())?;
    while let Some(current) = unit {
        if let NanoClawPreparedUnit::Message {
            ordinal,
            source: message_source,
            session,
            message,
            ..
        } = current
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
        unit = read_nanoclaw_staging_unit(&mut reader, &mut line)?;
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

fn rewind_nanoclaw_staging(
    spool: &mut crate::provider_sources::SqliteSourceStagingFile,
) -> SourceBackedRouteResult<()> {
    spool.rewind().map_err(nanoclaw_route_staging_error)
}

fn read_nanoclaw_staging_unit(
    reader: &mut crate::provider_sources::SqliteSourceStagingReader<'_>,
    line: &mut String,
) -> SourceBackedRouteResult<Option<NanoClawPreparedUnit>> {
    line.clear();
    if reader
        .read_line(line)
        .map_err(nanoclaw_route_staging_error)?
        == 0
    {
        return Ok(None);
    }
    serde_json::from_str(line)
        .map(Some)
        .map_err(nanoclaw_route_staging_data_error)
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
        NanoClawSourceBackedError::SqliteStaging(error) if error.is_snapshot_capacity_failure() => {
            SourceBackedRouteErrorKind::Unavailable
        }
        NanoClawSourceBackedError::SqliteStaging(error) if error.is_systemic_resource_failure() => {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        NanoClawSourceBackedError::SqliteStaging(_) | NanoClawSourceBackedError::StagingData(_) => {
            SourceBackedRouteErrorKind::Internal
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn nanoclaw_route_capture_error(error: CaptureError) -> SourceBackedRouteError {
    nanoclaw_route_error(error.into())
}

fn nanoclaw_route_staging_error(
    error: crate::provider_sources::SqliteSourceAccessError,
) -> SourceBackedRouteError {
    nanoclaw_route_error(error.into())
}

fn nanoclaw_route_staging_data_error(error: serde_json::Error) -> SourceBackedRouteError {
    nanoclaw_route_error(NanoClawSourceBackedError::StagingData(error))
}

fn nanoclaw_route_project_open_error(error: NanoClawProjectOpenError) -> SourceBackedRouteError {
    match error {
        NanoClawProjectOpenError::Capture(error) => nanoclaw_route_capture_error(error),
        NanoClawProjectOpenError::Finalization {
            primary,
            finalization,
        } => combine_primary_and_cleanup_route_errors(
            nanoclaw_route_project_open_error(*primary),
            nanoclaw_route_capture_error(finalization),
        ),
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

#[cfg(test)]
pub(crate) fn nanoclaw_source_key(
    catalog_lineage: [u8; 32],
) -> NanoClawSourceBackedResult<SourceKey> {
    nanoclaw_source_key_scoped(catalog_lineage, SourceAnchorScope::Unqualified)
}

pub(crate) fn nanoclaw_source_key_scoped(
    catalog_lineage: [u8; 32],
    source_anchor_scope: SourceAnchorScope,
) -> NanoClawSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive_scoped(
        CaptureProvider::NanoClaw.as_str(),
        NANOCLAW_SOURCE_FORMAT,
        NANOCLAW_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::CatalogLineage(catalog_lineage),
        source_anchor_scope,
    )?)
}

fn nanoclaw_core_record(
    source: &SourceKey,
    ordinal: u64,
    message_source: &str,
    session: &super::super::rows::NanoClawSessionRow,
    message: &super::super::rows::NanoClawMessageRow,
) -> NanoClawSourceBackedResult<CoreRecord> {
    let session_id = nanoclaw_session_id(source, &session.agent_group_id, &session.id)?;
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
        source.clone(),
        ordinal,
        event.event_type.as_str(),
        NANOCLAW_SOURCE_BACKED_PARSER_REVISION,
        body,
    )?;
    record.agent_scope = Some(AgentScope::Primary);
    record.provider_session_id = Some(provider_session_id);
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = event.role.map(|role| role.as_str().to_owned());
    record.content.activity = session
        .agent_group_folder
        .as_ref()
        .map(|project| CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: None,
            invocation: None,
            result: None,
            facts: vec![ProviderDeclaredFact {
                kind: LiteralFactKind::Project,
                value: project.clone(),
            }],
        });
    record.validate_contract()?;
    Ok(record)
}

fn nanoclaw_session_id(
    source: &SourceKey,
    agent_group_id: &str,
    native_session_id: &str,
) -> NanoClawSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::composite(
        NANOCLAW_NATIVE_SESSION_NAMESPACE,
        vec![
            TypedKey::utf8(agent_group_id)?,
            TypedKey::utf8(native_session_id)?,
        ],
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: NANOCLAW_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn checked_add(value: u64, increment: u64) -> NanoClawSourceBackedResult<u64> {
    value
        .checked_add(increment)
        .ok_or(NanoClawSourceBackedError::CountOverflow)
}

#[cfg(test)]
mod staging_error_tests {
    use super::*;
    use rusqlite::Connection;
    use std::{fs, io};

    fn write_minimal_project(project: &Path) {
        fs::create_dir_all(project.join("data/v2-sessions/ag-1/session-1")).unwrap();
        let connection = Connection::open(project.join("data/v2.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    agent_group_id TEXT NOT NULL
                );
                INSERT INTO sessions VALUES ('session-1', 'ag-1');",
            )
            .unwrap();
    }

    fn adapter_fixture() -> (tempfile::TempDir, NanoClawDocumentTreeAdapter<()>) {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let project = temp.path().join("project");
        let data_root = temp.path().join("ctx-data");
        fs::create_dir(&data_root).unwrap();
        write_minimal_project(&project);
        let adapter = NanoClawDocumentTreeAdapter::<()>::new_with_base_sources(
            &data_root,
            project,
            [0x91; 32],
            &[],
        )
        .unwrap();
        (temp, adapter)
    }

    #[test]
    fn root_scope_composes_with_catalog_lineage_and_unqualified_is_unchanged() {
        let catalog_lineage = [0x42; 32];
        let legacy = nanoclaw_source_key(catalog_lineage).unwrap();
        let unqualified =
            nanoclaw_source_key_scoped(catalog_lineage, SourceAnchorScope::Unqualified).unwrap();
        let first =
            nanoclaw_source_key_scoped(catalog_lineage, SourceAnchorScope::Lineage([1; 32]))
                .unwrap();
        let second =
            nanoclaw_source_key_scoped(catalog_lineage, SourceAnchorScope::Lineage([2; 32]))
                .unwrap();

        assert!(legacy.exact_descriptor_eq(&unqualified));
        assert_ne!(first.identity(), second.identity());
        assert_ne!(
            nanoclaw_session_id(&first, "same-agent-group", "same-session").unwrap(),
            nanoclaw_session_id(&second, "same-agent-group", "same-session").unwrap()
        );
    }

    fn assert_route_fatal_without_source_carry(
        error: &SourceBackedRouteError,
        expected: SourceBackedRouteErrorKind,
    ) {
        assert_eq!(error.kind, expected);
        assert_eq!(error.kind.source_failure_class(), None);
        assert!(!error.kind.is_logical_source_failure());
        assert_ne!(error.kind, SourceBackedRouteErrorKind::InvalidSource);
        assert_ne!(error.kind, SourceBackedRouteErrorKind::Unavailable);
    }

    #[test]
    fn systemic_staging_open_fails_before_tree_or_replay_carry() {
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::OutOfMemory,
        ] {
            let (_temp, adapter) = adapter_fixture();
            crate::provider_sources::fail_next_private_sqlite_staging_operation_for_test(
                crate::provider_sources::SqliteSourceStagingOperationForTest::Open,
                kind,
            );

            let error = match adapter.prepare_authority() {
                Ok(_) => panic!("injected {kind:?} staging failure unexpectedly prepared a tree"),
                Err(error) => error,
            };

            assert_route_fatal_without_source_carry(
                &error,
                SourceBackedRouteErrorKind::ResourceUnavailable,
            );
            assert!(error
                .detail
                .contains("private provider SQLite staging file"));
            assert!(adapter.replay_frontier.lock().unwrap().is_none());
        }
    }

    #[test]
    fn provider_source_not_found_remains_a_logical_source_failure() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let data_root = temp.path().join("ctx-data");
        fs::create_dir(&data_root).unwrap();
        let adapter = NanoClawDocumentTreeAdapter::<()>::new_with_base_sources(
            &data_root,
            temp.path().join("missing-provider-project"),
            [0x92; 32],
            &[],
        )
        .unwrap();

        let error = match adapter.prepare_authority() {
            Ok(_) => panic!("missing provider project unexpectedly prepared a tree"),
            Err(error) => error,
        };

        assert_eq!(error.kind, SourceBackedRouteErrorKind::Unavailable);
        assert!(error.kind.is_logical_source_failure());
        assert!(error.kind.source_failure_class().is_some());
        assert!(adapter.replay_frontier.lock().unwrap().is_none());
    }

    #[test]
    fn every_post_open_staging_io_failure_is_a_route_fatal_resource() {
        use crate::provider_sources::SqliteSourceStagingOperationForTest as Operation;

        for operation in [Operation::Write, Operation::Flush, Operation::Rewind] {
            let (_temp, adapter) = adapter_fixture();
            crate::provider_sources::fail_next_private_sqlite_staging_operation_for_test(
                operation,
                io::ErrorKind::PermissionDenied,
            );
            let error = match adapter.prepare_authority() {
                Ok(_) => panic!("injected {operation:?} failure unexpectedly prepared a tree"),
                Err(error) => error,
            };
            assert_route_fatal_without_source_carry(
                &error,
                SourceBackedRouteErrorKind::ResourceUnavailable,
            );
            assert!(error
                .detail
                .contains("private provider SQLite staging file"));
            assert!(adapter.replay_frontier.lock().unwrap().is_none());
        }

        let (_temp, adapter) = adapter_fixture();
        let mut authority = adapter.prepare_authority().unwrap();
        crate::provider_sources::fail_next_private_sqlite_staging_operation_for_test(
            Operation::Read,
            io::ErrorKind::PermissionDenied,
        );
        let projection = authority.projection.as_mut().unwrap();
        let mut reader = projection.spool.reader();
        let error = read_nanoclaw_staging_unit(&mut reader, &mut String::new()).unwrap_err();
        assert_route_fatal_without_source_carry(
            &error,
            SourceBackedRouteErrorKind::ResourceUnavailable,
        );
        assert!(error
            .detail
            .contains("reading a private provider SQLite staging file"));
        assert!(adapter.replay_frontier.lock().unwrap().is_none());
    }

    #[test]
    fn residual_staging_decode_failure_is_internal_before_publication_or_carry() {
        let (_temp, adapter) = adapter_fixture();
        let mut authority = adapter.prepare_authority().unwrap();
        let projection = authority.projection.as_mut().unwrap();
        projection.spool.write_all(b"not-json\n").unwrap();
        projection.spool.flush().unwrap();
        rewind_nanoclaw_staging(&mut projection.spool).unwrap();
        let mut reader = projection.spool.reader();
        let error = read_nanoclaw_staging_unit(&mut reader, &mut String::new()).unwrap_err();

        assert_route_fatal_without_source_carry(&error, SourceBackedRouteErrorKind::Internal);
        assert!(error
            .detail
            .contains("private SQLite staging data is invalid"));
        assert!(adapter.replay_frontier.lock().unwrap().is_none());
    }
}
