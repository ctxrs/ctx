use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Seek, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, CertifiedSource, EventIdentityInput, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError,
    StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    complete_content::sqlite::{
        configure_complete_content_sqlite_connection, CompleteContentSqliteBoundError,
        CompleteContentSqliteQueryBudget,
    },
    native_source::{NativeLocator, NativeSourceError},
    provider::{
        providers::nanoclaw::{
            position::{decode_nanoclaw_message_locator, nanoclaw_message_locator},
            project::{
                NanoClawProjectOpenError, NanoClawSelectedProject, NanoClawSourceBackedProject,
            },
            projection::nanoclaw_core_event,
            rows::{
                nanoclaw_hydrate_native_messages, nanoclaw_hydrate_native_sessions,
                nanoclaw_logical_record_digest_bytes, nanoclaw_message_digest_values,
                nanoclaw_session_columns, NanoClawMessageRow, NanoClawSessionRow,
                NANOCLAW_NATIVE_SET_READ_MAX_ROWS,
            },
            source::{NanoClawNativeScanner, NanoClawNativeUnit, NanoClawPreparedUnit},
            NANOCLAW_MESSAGE_LOCATOR_KIND,
        },
        source_backed::{
            family::document::{
                ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
                DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
            },
            hydration_failure, SourceBackedRouteError, SourceBackedRouteErrorKind,
            SourceBackedRouteResult,
        },
        sqlite::{
            ensure_sqlite_table_columns, sqlite_schema_fingerprint, sqlite_table_columns,
            sqlite_table_exists,
        },
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
    NanoClawWorkCounters,
};

#[derive(Debug, Error)]
pub(crate) enum NanoClawSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    NativeSource(#[from] NativeSourceError),
    #[error("NanoClaw source-backed scan counters overflowed")]
    CountOverflow,
    #[error("NanoClaw source-backed scanner emitted inconsistent counts")]
    CountMismatch,
    #[error("NanoClaw source-backed locator does not name this compound project")]
    InvalidProjectMessageLocator,
    #[error("NanoClaw project message no longer exists")]
    MissingProjectMessage,
    #[error("NanoClaw project message digest no longer matches the certified locator")]
    StaleProjectMessageEvidence,
    #[error("NanoClaw exact project-message query exceeded its bound")]
    ExactQueryBoundExceeded,
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
    work: Arc<Mutex<NanoClawWorkCounters>>,
    replay_frontier: Arc<Mutex<Option<NanoClawReplayFrontier>>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct NanoClawHydrationWork {
    central_snapshot_opens: u64,
    component_snapshot_opens: u64,
    central_set_reads: u64,
    component_set_reads: u64,
}

struct NanoClawPreparedProjection {
    spool: File,
    source_path: String,
    logical_fingerprint: [u8; 32],
    observation: SourceObservation,
    content_digest: [u8; 32],
    counts: ScannedSourceCounts,
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_SOURCE_BACKED_FINISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct NanoClawSourceBackedFinishHook;

#[cfg(test)]
impl Drop for NanoClawSourceBackedFinishHook {
    fn drop(&mut self) {
        BEFORE_SOURCE_BACKED_FINISH.with(|installed| {
            installed.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(crate) fn set_before_source_backed_finish_hook(
    hook: impl FnOnce() + 'static,
) -> NanoClawSourceBackedFinishHook {
    BEFORE_SOURCE_BACKED_FINISH.with(|installed| {
        *installed.borrow_mut() = Some(Box::new(hook));
    });
    NanoClawSourceBackedFinishHook
}

#[cfg(test)]
fn run_before_source_backed_finish_hook() {
    BEFORE_SOURCE_BACKED_FINISH.with(|installed| {
        if let Some(hook) = installed.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_source_backed_finish_hook() {}

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
            self.record_revision_precheck(&frontier)?;
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
        {
            let mut work = self
                .work
                .lock()
                .map_err(|_| nanoclaw_internal("NanoClaw work counter lock was poisoned"))?;
            work.projection_passes = work.projection_passes.saturating_add(1);
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
        run_before_source_backed_finish_hook();
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

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let (records, hydration) =
            hydrate_nanoclaw_group(&self.data_root, &self.path, &self.source, request)
                .map_err(nanoclaw_hydration_failure)?;
        {
            let mut work = self.work.lock().map_err(|_| HydrationFailure {
                kind: HydrationFailureKind::TemporarilyUnavailable,
                detail: "NanoClaw work counter lock was poisoned".to_owned(),
            })?;
            work.hydration_central_snapshot_opens = work
                .hydration_central_snapshot_opens
                .saturating_add(hydration.central_snapshot_opens);
            work.hydration_component_snapshot_opens = work
                .hydration_component_snapshot_opens
                .saturating_add(hydration.component_snapshot_opens);
            work.hydration_central_set_reads = work
                .hydration_central_set_reads
                .saturating_add(hydration.central_set_reads);
            work.hydration_component_set_reads = work
                .hydration_component_set_reads
                .saturating_add(hydration.component_set_reads);
        }
        BatchHydrationResult::new(records)
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))
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
    let source_path = project.root_path().display().to_string();

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
                    locator,
                    ..
                } => {
                    let _ = (ordinal, message_source, session, message, locator);
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
    run_before_source_backed_finish_hook();
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
        source_path,
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
            session_rowid,
            source: message_source,
            message_rowid,
            session,
            message,
        } = unit
        {
            let document = nanoclaw_lexical_document(
                source,
                ordinal,
                message_source.label(),
                &prepared.source_path,
                &session,
                &message.into_native(message_source),
                nanoclaw_message_locator(session_rowid, message_source, message_rowid)
                    .map_err(nanoclaw_route_capture_error)?,
            )
            .map_err(nanoclaw_route_error)?;
            sink.emit_document(document)?;
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

fn hydrate_nanoclaw_group(
    data_root: &Path,
    path: &Path,
    source: &SourceKey,
    request: &BatchHydrationRequest,
) -> NanoClawSourceBackedResult<(Vec<HydratedProviderRecord>, NanoClawHydrationWork)> {
    if request.events().iter().any(|event| {
        event.locator().validate_contract().is_err()
            || !source.exact_descriptor_eq(event.locator().source())
    }) {
        return Err(NanoClawSourceBackedError::InvalidProjectMessageLocator);
    }
    let coordinates = request
        .events()
        .iter()
        .map(|event| {
            let locator = project_message_locator(source, event.locator())?;
            decode_nanoclaw_message_locator(&locator)
                .map_err(|_| NanoClawSourceBackedError::InvalidProjectMessageLocator)
        })
        .collect::<NanoClawSourceBackedResult<Vec<_>>>()?;
    let mut work = NanoClawHydrationWork {
        central_snapshot_opens: 1,
        ..NanoClawHydrationWork::default()
    };
    let mut project = NanoClawSelectedProject::open(data_root, path)?;
    let resolution = (|| {
        let central = project.connection()?;
        configure_complete_content_sqlite_connection(
            central,
            CompleteContentSqliteQueryBudget::new(),
        )
        .map_err(map_exact_route_error)?;
        let session_columns = nanoclaw_session_columns(central)?;
        let session_rowids = coordinates
            .iter()
            .map(|coordinate| coordinate.session_rowid)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut sessions = BTreeMap::new();
        for rowids in session_rowids.chunks(NANOCLAW_NATIVE_SET_READ_MAX_ROWS) {
            work.central_set_reads = checked_add(work.central_set_reads, 1)?;
            sessions.extend(nanoclaw_hydrate_native_sessions(
                central,
                &session_columns,
                rowids,
            )?);
        }

        let mut components = BTreeMap::<_, Vec<(usize, i64)>>::new();
        for (index, coordinate) in coordinates.iter().enumerate() {
            if !sessions.contains_key(&coordinate.session_rowid) {
                return Err(NanoClawSourceBackedError::MissingProjectMessage);
            }
            components
                .entry((coordinate.session_rowid, coordinate.source))
                .or_default()
                .push((index, coordinate.message_rowid));
        }

        let mut records = std::iter::repeat_with(|| None)
            .take(request.events().len())
            .collect::<Vec<Option<HydratedProviderRecord>>>();
        for ((session_rowid, message_source), addressed) in components {
            let session = sessions
                .get(&session_rowid)
                .ok_or(NanoClawSourceBackedError::MissingProjectMessage)?;
            let Some(component) = project.open_component(
                data_root,
                &session.agent_group_id,
                &session.id,
                message_source,
            )?
            else {
                return Err(NanoClawSourceBackedError::MissingProjectMessage);
            };
            work.component_snapshot_opens = checked_add(work.component_snapshot_opens, 1)?;
            let component_result = (|| {
                let connection = component.connection()?;
                configure_complete_content_sqlite_connection(
                    connection,
                    CompleteContentSqliteQueryBudget::new(),
                )
                .map_err(map_exact_route_error)?;
                let table = message_source.table();
                if !sqlite_table_exists(connection, table)? {
                    return Err(CaptureError::InvalidPayload(format!(
                        "NanoClaw {table} component is missing its message table"
                    ))
                    .into());
                }
                let columns = sqlite_table_columns(connection, table)?;
                ensure_sqlite_table_columns(&columns, table, &["id"])?;
                let message_rowids = addressed
                    .iter()
                    .map(|(_, rowid)| *rowid)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                let mut messages = BTreeMap::new();
                for rowids in message_rowids.chunks(NANOCLAW_NATIVE_SET_READ_MAX_ROWS) {
                    work.component_set_reads = checked_add(work.component_set_reads, 1)?;
                    messages.extend(nanoclaw_hydrate_native_messages(
                        connection,
                        &columns,
                        message_source,
                        rowids,
                    )?);
                }
                for (index, message_rowid) in addressed {
                    let message = messages
                        .get(&message_rowid)
                        .ok_or(NanoClawSourceBackedError::MissingProjectMessage)?;
                    let requested = &request.events()[index];
                    if nanoclaw_logical_record_digest_bytes(&nanoclaw_message_digest_values(
                        message,
                    )) != *requested.locator().record_digest()
                        || nanoclaw_event_identity(source, message_source, session, message)?
                            != requested.event_id()
                    {
                        return Err(NanoClawSourceBackedError::StaleProjectMessageEvidence);
                    }
                    let seq = message
                        .seq
                        .map(|value| {
                            u64::try_from(value).map_err(|_| {
                                NanoClawSourceBackedError::Capture(CaptureError::InvalidPayload(
                                    "NanoClaw complete-content message seq must be nonnegative"
                                        .to_owned(),
                                ))
                            })
                        })
                        .transpose()?;
                    let (_, text) =
                        nanoclaw_core_event(session, message, seq, chrono::DateTime::UNIX_EPOCH);
                    records[index] = Some(HydratedProviderRecord {
                        event_id: requested.event_id(),
                        provider_bytes: text.into_bytes(),
                    });
                }
                Ok(())
            })();
            let finish_result = component.finish();
            match (component_result, finish_result) {
                (_, Err(error)) => return Err(error.into()),
                (Err(error), Ok(())) => return Err(error),
                (Ok(()), Ok(())) => {}
            }
        }
        records
            .into_iter()
            .map(|record| record.ok_or(NanoClawSourceBackedError::MissingProjectMessage))
            .collect()
    })();
    let finish_result = project.finish();
    let records = match (resolution, finish_result) {
        (_, Err(error)) => return Err(error.into()),
        (Err(error), Ok(())) => return Err(error),
        (Ok(records), Ok(())) => records,
    };
    Ok((records, work))
}

fn nanoclaw_event_identity(
    source: &SourceKey,
    message_source: super::super::position::NanoClawMessageSource,
    session: &NanoClawSessionRow,
    message: &NanoClawMessageRow,
) -> NanoClawSourceBackedResult<StableEntityId> {
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
    let native_item_key = NativeItemKey::composite(
        NANOCLAW_NATIVE_EVENT_NAMESPACE,
        vec![
            TypedKey::utf8(message_source.label())?,
            TypedKey::utf8(&message.id)?,
        ],
    )?;
    Ok(derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: NANOCLAW_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?)
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

fn nanoclaw_hydration_failure(error: NanoClawSourceBackedError) -> HydrationFailure {
    let kind = match &error {
        NanoClawSourceBackedError::InvalidProjectMessageLocator
        | NanoClawSourceBackedError::Resolver(_)
        | NanoClawSourceBackedError::NativeSource(_)
        | NanoClawSourceBackedError::ExactQueryBoundExceeded
        | NanoClawSourceBackedError::InvalidReplayCheckpoint(_) => {
            HydrationFailureKind::InvalidLocator
        }
        NanoClawSourceBackedError::Capture(CaptureError::SourceChangedDuringCapture)
        | NanoClawSourceBackedError::Capture(CaptureError::InvalidProviderTranscriptPath {
            ..
        }) => HydrationFailureKind::StaleSourceEvidence,
        NanoClawSourceBackedError::MissingProjectMessage => HydrationFailureKind::MissingRecord,
        NanoClawSourceBackedError::StaleProjectMessageEvidence => {
            HydrationFailureKind::StaleRecordEvidence
        }
        NanoClawSourceBackedError::Capture(CaptureError::Io(_))
        | NanoClawSourceBackedError::Capture(CaptureError::ProviderSource { .. }) => {
            HydrationFailureKind::TemporarilyUnavailable
        }
        NanoClawSourceBackedError::Projection(_)
        | NanoClawSourceBackedError::CountOverflow
        | NanoClawSourceBackedError::CountMismatch
        | NanoClawSourceBackedError::Capture(_) => HydrationFailureKind::StaleSourceEvidence,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
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

#[allow(clippy::too_many_arguments)]
fn nanoclaw_lexical_document(
    source: &SourceKey,
    ordinal: u64,
    message_source: &str,
    source_path: &str,
    session: &super::super::rows::NanoClawSessionRow,
    message: &super::super::rows::NanoClawMessageRow,
    native_locator: NativeLocator,
) -> NanoClawSourceBackedResult<LexicalDocument> {
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
    let native_item_key = NativeItemKey::composite(
        NANOCLAW_NATIVE_EVENT_NAMESPACE,
        vec![
            TypedKey::utf8(message_source)?,
            TypedKey::utf8(&message.id)?,
        ],
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: NANOCLAW_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let seq = message
        .seq
        .map(|value| {
            u64::try_from(value).map_err(|_| {
                NanoClawSourceBackedError::Capture(CaptureError::InvalidPayload(
                    "NanoClaw source-backed message seq must be nonnegative".to_owned(),
                ))
            })
        })
        .transpose()?;
    let (event, exact_text) =
        nanoclaw_core_event(session, message, seq, chrono::DateTime::UNIX_EPOCH);
    let mut body = exact_text;
    if body.is_empty() {
        body = format!("NanoClaw {message_source} message");
    }
    let record_digest =
        nanoclaw_logical_record_digest_bytes(&nanoclaw_message_digest_values(message));
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderNative {
            namespace: NANOCLAW_MESSAGE_LOCATOR_KIND.to_owned(),
            coordinate: TypedKey::bytes(native_locator.value().to_vec())?,
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record_digest,
    )?;
    let provider_session_id = session
        .thread_id
        .clone()
        .filter(|thread| !thread.is_empty())
        .unwrap_or_else(|| session.id.clone());
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(provider_session_id),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: ordinal,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: session.agent_group_folder.clone(),
        cwd: session.agent_group_folder.clone(),
        touched_files: Vec::new(),
    })
}

fn project_message_locator(
    source: &SourceKey,
    locator: &SourceRecordLocator,
) -> NanoClawSourceBackedResult<NativeLocator> {
    locator.validate_contract()?;
    if !source.exact_descriptor_eq(locator.source())
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(NanoClawSourceBackedError::InvalidProjectMessageLocator);
    }
    let NativeRecordCoordinate::ProviderNative {
        namespace,
        coordinate,
    } = locator.coordinate()
    else {
        return Err(NanoClawSourceBackedError::InvalidProjectMessageLocator);
    };
    let TypedKey::Bytes(value) = coordinate else {
        return Err(NanoClawSourceBackedError::InvalidProjectMessageLocator);
    };
    if namespace != NANOCLAW_MESSAGE_LOCATOR_KIND {
        return Err(NanoClawSourceBackedError::InvalidProjectMessageLocator);
    }
    let native_locator = NativeLocator::new(namespace.clone(), value.clone())?;
    decode_nanoclaw_message_locator(&native_locator)
        .map_err(|_| NanoClawSourceBackedError::InvalidProjectMessageLocator)?;
    Ok(native_locator)
}

fn checked_add(value: u64, increment: u64) -> NanoClawSourceBackedResult<u64> {
    value
        .checked_add(increment)
        .ok_or(NanoClawSourceBackedError::CountOverflow)
}

fn map_exact_route_error(error: CompleteContentSqliteBoundError) -> NanoClawSourceBackedError {
    match error {
        CompleteContentSqliteBoundError::Capture(error) => error.into(),
        CompleteContentSqliteBoundError::ContentTooLarge => {
            NanoClawSourceBackedError::ExactQueryBoundExceeded
        }
    }
}
