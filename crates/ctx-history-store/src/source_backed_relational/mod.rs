//! Independent relational compatibility projection for source-backed Core.
//!
//! This database is a disposable consumer of a committed Core generation. It
//! stores stable identities, relational metadata, bounded previews, and native
//! locator/evidence envelopes; it never stores complete provider bodies and it
//! never participates in Core lexical publication.
//!
//! Integration sequence:
//!
//! 1. Commit and reopen the source-backed lexical generation.
//! 2. Serialize the verified generation manifest and pair it with the exact
//!    Core commit receipt in [`CommittedCoreGeneration`].
//! 3. Stream source-grouped [`RelationalProjectionRecord`] values into
//!    [`SourceBackedRelationalProjection::catch_up`]. Use
//!    [`SourceBackedRelationalProjection::rebuild`] for first install, repair,
//!    or a consumer-contract change.
//! 4. Treat the returned frontier as SQL-owned state. A projection error leaves
//!    the prior SQL generation queryable and marks only this consumer behind.
//!
//! For the schema-v4 lexical seam, one source-backed event supplies event and
//! session identities, parent/root lineage, provider-session ID, branch,
//! source path, agent scope, workspace/cwd, event ordering/type/role, touched
//! paths, bounded preview, and locator evidence. The integration host emits one
//! deduplicated session record before its events and supplies deterministic
//! file-relation IDs plus any richer old-path/change metadata retained by the
//! provider projector. Rebuild obtains the same records by rereading certified
//! provider sources; it does not enumerate or hydrate complete bodies from
//! SQLite.
//!
//! A normal catch-up stream contains only sources whose certificates changed.
//! A rebuild stream contains every source in the manifest. Confirmed deletion
//! is represented by omission from the new certified manifest, so no provider
//! body archive or relational tombstone payload is required.

mod model;
mod schema;

pub use model::*;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    CertifiedSource, EventRole, FileChangeKind, ProjectionContractError, SourceKey,
    SourceResolverContractError, StableEntityId, StableEntityKind, IDENTITY_VERSION,
};
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    connection::{configure_read_only_connection, BUSY_TIMEOUT},
    object_store::{restrict_private_dir, restrict_private_file},
    raw_sql::{raw_sql_query_connection, RawSqlOptions, RawSqlResult},
};

const GENERATION_MANIFEST_VERSION: u32 = 1;
const REQUIRED_LEXICAL_SCHEMA_VERSION: u32 = 4;
const REQUIRED_LEXICAL_ANALYZER_VERSION: u32 = 1;
const MAX_GENERATION_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_METADATA_TEXT_BYTES: usize = 64 * 1024;
const MAX_PATH_BYTES: usize = 64 * 1024;
const MAX_FAILURE_DETAIL_CHARS: usize = 2_048;

pub struct SourceBackedRelationalProjection {
    path: PathBuf,
    conn: Connection,
    read_only: bool,
}

impl SourceBackedRelationalProjection {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            restrict_private_dir(parent)?;
        }
        let conn = Connection::open(&path)?;
        restrict_private_file(&path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        schema::initialize(&conn)?;
        Ok(Self {
            path,
            conn,
            read_only: false,
        })
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_read_only_connection(&conn, BUSY_TIMEOUT)?;
        schema::verify(&conn)?;
        Ok(Self {
            path,
            conn,
            read_only: true,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn metadata(&self) -> Result<RelationalProjectionMetadata> {
        let row = self.conn.query_row(
            "SELECT build_generation, active_generation_id, target_generation_id, status,
                    source_count, session_count, event_count, file_touch_count, last_error
             FROM source_backed_relational_state WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )?;
        let status = match row.3.as_str() {
            "empty" => RelationalProjectionStatus::Empty,
            "ready" => RelationalProjectionStatus::Ready,
            "behind" => RelationalProjectionStatus::Behind,
            other => {
                return Err(RelationalProjectionError::InvalidRecord(format!(
                    "stored projection status {other} is invalid"
                )))
            }
        };
        Ok(RelationalProjectionMetadata {
            build_generation: sqlite_u64(row.0, "build_generation")?,
            active_core_generation_id: row.1,
            target_core_generation_id: row.2,
            status,
            source_count: sqlite_u64(row.4, "source_count")?,
            session_count: sqlite_u64(row.5, "session_count")?,
            event_count: sqlite_u64(row.6, "event_count")?,
            file_touch_count: sqlite_u64(row.7, "file_touch_count")?,
            last_error: row.8,
        })
    }

    pub fn raw_sql_query(&self, sql: &str, options: RawSqlOptions) -> Result<RawSqlResult> {
        raw_sql_query_connection(&self.conn, sql, options).map_err(RelationalProjectionError::from)
    }

    /// Replaces the complete relational projection from one Core generation.
    pub fn rebuild<I>(
        &mut self,
        generation: &CommittedCoreGeneration,
        records: I,
    ) -> Result<RelationalProjectionReceipt>
    where
        I: IntoIterator<Item = RelationalProjectionRecord>,
    {
        self.apply_generation(BuildMode::Rebuild, generation, records)
    }

    /// Advances only changed sources and retires sources omitted by the new
    /// certified manifest.
    pub fn catch_up<I>(
        &mut self,
        generation: &CommittedCoreGeneration,
        records: I,
    ) -> Result<RelationalProjectionReceipt>
    where
        I: IntoIterator<Item = RelationalProjectionRecord>,
    {
        self.apply_generation(BuildMode::CatchUp, generation, records)
    }

    fn apply_generation<I>(
        &mut self,
        mode: BuildMode,
        generation: &CommittedCoreGeneration,
        records: I,
    ) -> Result<RelationalProjectionReceipt>
    where
        I: IntoIterator<Item = RelationalProjectionRecord>,
    {
        if self.read_only {
            return Err(RelationalProjectionError::InvalidStreamOrder(
                "a read-only SQL projection cannot publish a generation".to_owned(),
            ));
        }
        let manifest = ValidatedManifest::from_commit(generation)?;
        let result = apply_transaction(
            &mut self.conn,
            mode,
            generation,
            &manifest,
            records.into_iter(),
        );
        if let Err(error) = &result {
            note_failed_target(&self.conn, &generation.generation_id, error);
        }
        result
    }
}

#[derive(Debug, Clone, Copy)]
enum BuildMode {
    Rebuild,
    CatchUp,
}

#[derive(Debug, Serialize, Deserialize)]
struct GenerationManifest {
    manifest_version: u32,
    identity_version: u16,
    lexical_schema_version: u32,
    lexical_analyzer_version: u32,
    indexed_documents: u64,
    certified_source_bytes: u64,
    sources: Vec<CertifiedSource>,
}

struct ValidatedManifest {
    digest: [u8; 32],
    sources: BTreeMap<String, ManifestSource>,
    indexed_documents: u64,
}

struct ManifestSource {
    certificate: CertifiedSource,
    certificate_json: Vec<u8>,
    certificate_digest: [u8; 32],
}

impl ValidatedManifest {
    fn from_commit(commit: &CommittedCoreGeneration) -> Result<Self> {
        if commit.manifest_json.len() > MAX_GENERATION_MANIFEST_BYTES {
            return invalid_generation("manifest exceeds the relational projection limit");
        }
        let manifest: GenerationManifest = serde_json::from_slice(&commit.manifest_json)?;
        if serde_json::to_vec(&manifest)? != commit.manifest_json {
            return invalid_generation("manifest is not in canonical ctx JSON encoding");
        }
        if manifest.manifest_version != GENERATION_MANIFEST_VERSION
            || manifest.identity_version != IDENTITY_VERSION
            || manifest.lexical_schema_version != REQUIRED_LEXICAL_SCHEMA_VERSION
            || manifest.lexical_analyzer_version != REQUIRED_LEXICAL_ANALYZER_VERSION
        {
            return invalid_generation(
                "manifest, identity, or schema-v4 lexical lineage contract is unsupported",
            );
        }
        let digest: [u8; 32] = Sha256::digest(&commit.manifest_json).into();
        if commit.generation_id != hex(&digest) {
            return invalid_generation("generation ID does not match the manifest digest");
        }
        if manifest.indexed_documents != commit.indexed_documents
            || manifest.certified_source_bytes != commit.certified_source_bytes
            || manifest.sources.len() != commit.certified_sources
        {
            return invalid_generation("commit receipt counts do not match the manifest");
        }
        let mut expected_events = 0_u64;
        let mut expected_bytes = 0_u64;
        let mut prior_digest = None;
        let mut sources = BTreeMap::new();
        for certificate in manifest.sources {
            certificate
                .validate_contract()
                .map_err(contract_generation_error)?;
            let source = certificate.observation().source();
            let source_digest = source.identity().digest();
            if prior_digest.is_some_and(|prior| prior >= source_digest) {
                return invalid_generation("manifest sources are not strictly sorted");
            }
            prior_digest = Some(source_digest);
            expected_events = expected_events
                .checked_add(certificate.counts().indexed_documents)
                .ok_or(RelationalProjectionError::CountOverflow(
                    "manifest indexed documents",
                ))?;
            expected_bytes = expected_bytes
                .checked_add(certificate.counts().certified_bytes)
                .ok_or(RelationalProjectionError::CountOverflow(
                    "manifest certified bytes",
                ))?;
            let certificate_json = serde_json::to_vec(&certificate)?;
            let certificate_digest = Sha256::digest(&certificate_json).into();
            let source_id = source.identity().as_uuid().to_string();
            sources.insert(
                source_id,
                ManifestSource {
                    certificate,
                    certificate_json,
                    certificate_digest,
                },
            );
        }
        if expected_events != manifest.indexed_documents
            || expected_bytes != manifest.certified_source_bytes
        {
            return invalid_generation("manifest totals do not reconcile");
        }
        Ok(Self {
            digest,
            sources,
            indexed_documents: manifest.indexed_documents,
        })
    }
}

fn apply_transaction<I>(
    conn: &mut Connection,
    mode: BuildMode,
    generation: &CommittedCoreGeneration,
    manifest: &ValidatedManifest,
    records: I,
) -> Result<RelationalProjectionReceipt>
where
    I: Iterator<Item = RelationalProjectionRecord>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior = stored_certificate_digests(&tx)?;
    let manifest_ids = manifest.sources.keys().cloned().collect::<BTreeSet<_>>();
    let expected = match mode {
        BuildMode::Rebuild => {
            tx.execute("DELETE FROM source_backed_sources", [])?;
            manifest_ids.clone()
        }
        BuildMode::CatchUp => {
            let changed = manifest
                .sources
                .iter()
                .filter_map(|(source_id, source)| {
                    (prior.get(source_id) != Some(&source.certificate_digest))
                        .then(|| source_id.clone())
                })
                .collect::<BTreeSet<_>>();
            for source_id in prior.keys().filter(|id| !manifest_ids.contains(*id)) {
                tx.execute(
                    "DELETE FROM source_backed_sources WHERE source_id = ?1",
                    [source_id],
                )?;
            }
            for source_id in &changed {
                tx.execute(
                    "DELETE FROM source_backed_sources WHERE source_id = ?1",
                    [source_id],
                )?;
            }
            changed
        }
    };

    let mut current: Option<OpenSource> = None;
    let mut received = BTreeSet::new();
    for record in records {
        match record {
            RelationalProjectionRecord::BeginSource(metadata) => {
                if current.is_some() {
                    return stream_order("a source began before the prior source ended");
                }
                let source_id = metadata.source.identity().as_uuid().to_string();
                if !expected.contains(&source_id) {
                    return stream_order(format!(
                        "source {source_id} is not required by this projection update"
                    ));
                }
                if !received.insert(source_id.clone()) {
                    return stream_order(format!("source {source_id} appeared more than once"));
                }
                let source = manifest.sources.get(&source_id).ok_or_else(|| {
                    RelationalProjectionError::InvalidRecord(format!(
                        "source {source_id} is absent from the manifest"
                    ))
                })?;
                metadata
                    .source
                    .validate_exact_descriptor(source.certificate.observation().source())
                    .map_err(contract_record_error)?;
                validate_source_metadata(&metadata)?;
                insert_source(&tx, &metadata, source)?;
                current = Some(OpenSource {
                    source_id,
                    source: metadata.source,
                    expected_events: source.certificate.counts().indexed_documents,
                    received_events: 0,
                });
            }
            RelationalProjectionRecord::Session(session) => {
                let open = current_source(&current)?;
                validate_session(&session, &open.source)?;
                insert_session(&tx, &open.source_id, &session)?;
            }
            RelationalProjectionRecord::Event(event) => {
                let open = current_source_mut(&mut current)?;
                validate_event(&event, &open.source)?;
                insert_event(&tx, &open.source_id, &event)?;
                open.received_events = open.received_events.checked_add(1).ok_or(
                    RelationalProjectionError::CountOverflow("source event count"),
                )?;
            }
            RelationalProjectionRecord::FileTouch(file) => {
                let open = current_source(&current)?;
                validate_file_touch(&file, &open.source)?;
                insert_file_touch(&tx, &open.source_id, &file)?;
            }
            RelationalProjectionRecord::EndSource { source_id } => {
                let open = current.take().ok_or_else(|| {
                    RelationalProjectionError::InvalidStreamOrder(
                        "a source ended while no source was active".to_owned(),
                    )
                })?;
                if open.source.identity().as_uuid() != source_id {
                    return stream_order(
                        "the end-source identity does not match the active source",
                    );
                }
                if open.received_events != open.expected_events {
                    return Err(RelationalProjectionError::SourceEventCountMismatch {
                        source_id: open.source_id,
                        expected: open.expected_events,
                        received: open.received_events,
                    });
                }
            }
        }
    }
    if current.is_some() {
        return stream_order("the final source did not emit EndSource");
    }
    if received != expected {
        return Err(RelationalProjectionError::SourceSetMismatch {
            expected: expected.into_iter().collect(),
            received: received.into_iter().collect(),
        });
    }
    validate_projected_generation(&tx, manifest)?;
    let counts = projection_counts(&tx)?;
    let prior_build: i64 = tx.query_row(
        "SELECT build_generation FROM source_backed_relational_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    let build_generation =
        prior_build
            .checked_add(1)
            .ok_or(RelationalProjectionError::CountOverflow(
                "projection build generation",
            ))?;
    tx.execute(
        "UPDATE source_backed_relational_state
         SET build_generation = ?1,
             active_generation_id = ?2,
             active_manifest_digest = ?3,
             target_generation_id = NULL,
             status = 'ready',
             source_count = ?4,
             session_count = ?5,
             event_count = ?6,
             file_touch_count = ?7,
             last_error = NULL
         WHERE singleton = 1",
        params![
            build_generation,
            generation.generation_id,
            manifest.digest.as_slice(),
            counts.sources,
            counts.sessions,
            counts.events,
            counts.file_touches,
        ],
    )?;
    tx.commit()?;
    Ok(RelationalProjectionReceipt {
        core_generation_id: generation.generation_id.clone(),
        build_generation: sqlite_u64(build_generation, "build_generation")?,
        source_count: sqlite_u64(counts.sources, "source_count")?,
        session_count: sqlite_u64(counts.sessions, "session_count")?,
        event_count: sqlite_u64(counts.events, "event_count")?,
        file_touch_count: sqlite_u64(counts.file_touches, "file_touch_count")?,
    })
}

struct OpenSource {
    source_id: String,
    source: SourceKey,
    expected_events: u64,
    received_events: u64,
}

#[derive(Debug, Serialize)]
struct CompatibilityPayload<'a> {
    content_authority: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_preview: Option<&'a str>,
}

struct ProjectionCounts {
    sources: i64,
    sessions: i64,
    events: i64,
    file_touches: i64,
}

fn insert_source(
    conn: &Connection,
    metadata: &RelationalSourceMetadata,
    manifest: &ManifestSource,
) -> Result<()> {
    let certificate = &manifest.certificate;
    let source = certificate.observation().source();
    conn.execute(
        "INSERT INTO source_backed_sources (
            source_id, source_identity, source_descriptor_json, certificate_json,
            certificate_digest, provider, source_format, source_root, source_path, cwd,
            revision_kind, parser_revision, certified_bytes, content_digest_hex,
            indexed_event_count
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
         )",
        params![
            source.identity().as_uuid().to_string(),
            source
                .identity()
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            serde_json::to_vec(source)?,
            manifest.certificate_json,
            manifest.certificate_digest.as_slice(),
            source.provider(),
            source.source_format(),
            metadata.source_root,
            metadata.source_path,
            metadata.cwd,
            certificate.observation().revision_kind(),
            certificate.parser_revision(),
            sqlite_i64(certificate.counts().certified_bytes, "certified bytes")?,
            hex(certificate.content_digest()),
            sqlite_i64(
                certificate.counts().indexed_documents,
                "source indexed documents"
            )?,
        ],
    )?;
    Ok(())
}

fn insert_session(
    conn: &Connection,
    source_id: &str,
    session: &RelationalSessionMetadata,
) -> Result<()> {
    conn.execute(
        "INSERT INTO source_backed_sessions (
            ctx_session_id, session_identity, source_id, parent_ctx_session_id,
            parent_session_identity, root_ctx_session_id, root_session_identity,
            provider_session_id, external_agent_id, agent_type, role_hint, is_primary,
            branch, workspace, cwd, source_path, status, fidelity, started_at_ms,
            ended_at_ms
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19, ?20
         )",
        params![
            session.session_id.as_uuid().to_string(),
            session
                .session_id
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            source_id,
            session.parent_session_id.map(|id| id.as_uuid().to_string()),
            session
                .parent_session_id
                .map(StableEntityId::encode_canonical)
                .transpose()
                .map_err(contract_record_error)?
                .map(|identity| identity.to_vec()),
            session.root_session_id.as_uuid().to_string(),
            session
                .root_session_id
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            session.provider_session_id,
            session.external_agent_id,
            session.agent_type.as_str(),
            session.role_hint,
            i64::from(session.is_primary),
            session.branch,
            session.workspace,
            session.cwd,
            session.source_path,
            session.status.as_str(),
            session.fidelity.as_str(),
            session.started_at_unix_ms,
            session.ended_at_unix_ms,
        ],
    )?;
    Ok(())
}

fn insert_event(conn: &Connection, source_id: &str, event: &RelationalEventMetadata) -> Result<()> {
    let payload = serde_json::to_string(&CompatibilityPayload {
        content_authority: "provider_source",
        body_preview: event.bounded_preview.as_deref(),
    })?;
    conn.execute(
        "INSERT INTO source_backed_events (
            ctx_event_id, event_identity, source_id, ctx_session_id, session_identity, event_seq,
            event_type, role, occurred_at_ms, payload_json, fidelity,
            native_locator_json, record_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            event.event_id.as_uuid().to_string(),
            event
                .event_id
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            source_id,
            event.session_id.as_uuid().to_string(),
            event
                .session_id
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            sqlite_i64(event.event_sequence, "event sequence")?,
            event.event_type.as_str(),
            event.role.map(EventRole::as_str),
            event.occurred_at_unix_ms,
            payload,
            event.fidelity.as_str(),
            serde_json::to_vec(&event.locator)?,
            event.locator.record_digest().as_slice(),
        ],
    )?;
    Ok(())
}

fn insert_file_touch(
    conn: &Connection,
    source_id: &str,
    file: &RelationalFileTouchMetadata,
) -> Result<()> {
    conn.execute(
        "INSERT INTO source_backed_files_touched (
            ctx_file_touch_id, source_id, ctx_event_id, event_identity, ctx_session_id,
            session_identity, path, old_path, change_kind, line_count_delta,
            confidence, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            file.file_touch_id.to_string(),
            source_id,
            file.event_id.map(|id| id.as_uuid().to_string()),
            file.event_id
                .map(StableEntityId::encode_canonical)
                .transpose()
                .map_err(contract_record_error)?
                .map(|identity| identity.to_vec()),
            file.session_id.map(|id| id.as_uuid().to_string()),
            file.session_id
                .map(StableEntityId::encode_canonical)
                .transpose()
                .map_err(contract_record_error)?
                .map(|identity| identity.to_vec()),
            file.path,
            file.old_path,
            file.change_kind.map(FileChangeKind::as_str),
            file.line_count_delta,
            file.confidence.as_str(),
            file.created_at_unix_ms,
            file.updated_at_unix_ms,
        ],
    )?;
    Ok(())
}

fn validate_source_metadata(metadata: &RelationalSourceMetadata) -> Result<()> {
    metadata
        .source
        .validate_contract()
        .map_err(contract_record_error)?;
    validate_optional_text(
        "source_root",
        metadata.source_root.as_deref(),
        MAX_PATH_BYTES,
    )?;
    validate_optional_text(
        "source_path",
        metadata.source_path.as_deref(),
        MAX_PATH_BYTES,
    )?;
    validate_optional_text("cwd", metadata.cwd.as_deref(), MAX_PATH_BYTES)
}

fn validate_session(session: &RelationalSessionMetadata, source: &SourceKey) -> Result<()> {
    validate_entity(session.session_id, StableEntityKind::Session, source)?;
    for relation in [session.parent_session_id, Some(session.root_session_id)]
        .into_iter()
        .flatten()
    {
        relation
            .validate_contract()
            .map_err(contract_record_error)?;
        if relation.entity_kind() != StableEntityKind::Session {
            return invalid_record("session relationship has a non-session identity");
        }
    }
    validate_optional_text(
        "provider_session_id",
        session.provider_session_id.as_deref(),
        MAX_METADATA_TEXT_BYTES,
    )?;
    validate_optional_text(
        "external_agent_id",
        session.external_agent_id.as_deref(),
        MAX_METADATA_TEXT_BYTES,
    )?;
    validate_optional_text(
        "role_hint",
        session.role_hint.as_deref(),
        MAX_METADATA_TEXT_BYTES,
    )?;
    validate_optional_text("branch", session.branch.as_deref(), MAX_METADATA_TEXT_BYTES)?;
    validate_optional_text(
        "workspace",
        session.workspace.as_deref(),
        MAX_METADATA_TEXT_BYTES,
    )?;
    validate_optional_text("cwd", session.cwd.as_deref(), MAX_PATH_BYTES)?;
    validate_optional_text(
        "source_path",
        session.source_path.as_deref(),
        MAX_PATH_BYTES,
    )
}

fn validate_event(event: &RelationalEventMetadata, source: &SourceKey) -> Result<()> {
    validate_entity(event.event_id, StableEntityKind::Event, source)?;
    validate_entity(event.session_id, StableEntityKind::Session, source)?;
    event
        .locator
        .validate_contract()
        .map_err(resolver_record_error)?;
    if !event.locator.source().exact_descriptor_eq(source) {
        return invalid_record("event locator does not match the active source");
    }
    if let Some(preview) = &event.bounded_preview {
        if preview.chars().count() > RELATIONAL_EVENT_PREVIEW_MAX_CHARS {
            return invalid_record("event preview exceeds 2,048 characters");
        }
    }
    Ok(())
}

fn validate_file_touch(file: &RelationalFileTouchMetadata, source: &SourceKey) -> Result<()> {
    validate_text("path", &file.path, MAX_PATH_BYTES)?;
    validate_optional_text("old_path", file.old_path.as_deref(), MAX_PATH_BYTES)?;
    if let Some(event_id) = file.event_id {
        validate_entity(event_id, StableEntityKind::Event, source)?;
    }
    if let Some(session_id) = file.session_id {
        validate_entity(session_id, StableEntityKind::Session, source)?;
    }
    Ok(())
}

fn validate_entity(id: StableEntityId, kind: StableEntityKind, source: &SourceKey) -> Result<()> {
    id.validate_contract().map_err(contract_record_error)?;
    if id.entity_kind() != kind {
        return invalid_record("stable identity has the wrong entity kind");
    }
    if id.source_digest() != source.identity().digest()
        || id.source_descriptor_digest() != source.exact_descriptor_digest()
    {
        return invalid_record("stable identity does not belong to the active source");
    }
    Ok(())
}

fn validate_projected_generation(conn: &Connection, manifest: &ValidatedManifest) -> Result<()> {
    let counts = projection_counts(conn)?;
    let projected_events = sqlite_u64(counts.events, "event_count")?;
    if projected_events != manifest.indexed_documents {
        return Err(RelationalProjectionError::GenerationEventCountMismatch {
            expected: manifest.indexed_documents,
            projected: projected_events,
        });
    }
    let source_count = sqlite_u64(counts.sources, "source_count")?;
    if source_count != manifest.sources.len() as u64 {
        return invalid_record("projected source count does not match the manifest");
    }
    let dangling_relationships: i64 = conn.query_row(
        "SELECT COUNT(*) FROM source_backed_sessions child
         WHERE (child.parent_ctx_session_id IS NOT NULL
                AND NOT EXISTS (
                    SELECT 1 FROM source_backed_sessions parent
                    WHERE parent.ctx_session_id = child.parent_ctx_session_id
                      AND parent.session_identity = child.parent_session_identity
                ))
            OR (child.root_ctx_session_id IS NOT NULL
                AND NOT EXISTS (
                    SELECT 1 FROM source_backed_sessions root
                    WHERE root.ctx_session_id = child.root_ctx_session_id
                      AND root.session_identity = child.root_session_identity
                ))",
        [],
        |row| row.get(0),
    )?;
    if dangling_relationships != 0 {
        return invalid_record("session relationships reference absent sessions");
    }
    let dangling_event_or_file_relations: i64 = conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM source_backed_events event
             WHERE NOT EXISTS (
                 SELECT 1 FROM source_backed_sessions session
                 WHERE session.ctx_session_id = event.ctx_session_id
                   AND session.session_identity = event.session_identity
             ))
          + (SELECT COUNT(*) FROM source_backed_files_touched file
             WHERE file.ctx_event_id IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM source_backed_events event
                   WHERE event.ctx_event_id = file.ctx_event_id
                     AND event.event_identity = file.event_identity
               ))
          + (SELECT COUNT(*) FROM source_backed_files_touched file
             WHERE file.ctx_session_id IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM source_backed_sessions session
                   WHERE session.ctx_session_id = file.ctx_session_id
                     AND session.session_identity = file.session_identity
               ))",
        [],
        |row| row.get(0),
    )?;
    if dangling_event_or_file_relations != 0 {
        return invalid_record("event or file relationships have mismatched stable identities");
    }
    Ok(())
}

fn projection_counts(conn: &Connection) -> Result<ProjectionCounts> {
    conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM source_backed_sources),
            (SELECT COUNT(*) FROM source_backed_sessions),
            (SELECT COUNT(*) FROM source_backed_events),
            (SELECT COUNT(*) FROM source_backed_files_touched)",
        [],
        |row| {
            Ok(ProjectionCounts {
                sources: row.get(0)?,
                sessions: row.get(1)?,
                events: row.get(2)?,
                file_touches: row.get(3)?,
            })
        },
    )
    .map_err(RelationalProjectionError::from)
}

fn stored_certificate_digests(conn: &Connection) -> Result<BTreeMap<String, [u8; 32]>> {
    let mut stmt =
        conn.prepare("SELECT source_id, certificate_digest FROM source_backed_sources")?;
    let mut rows = stmt.query([])?;
    let mut output = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let source_id: String = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        let digest: [u8; 32] = bytes.try_into().map_err(|_| {
            RelationalProjectionError::InvalidRecord(
                "stored source certificate digest is malformed".to_owned(),
            )
        })?;
        output.insert(source_id, digest);
    }
    Ok(output)
}

fn note_failed_target(conn: &Connection, generation_id: &str, error: &RelationalProjectionError) {
    let detail = error
        .to_string()
        .chars()
        .take(MAX_FAILURE_DETAIL_CHARS)
        .collect::<String>();
    let _ = conn.execute(
        "UPDATE source_backed_relational_state
         SET target_generation_id = ?1, status = 'behind', last_error = ?2
         WHERE singleton = 1",
        params![generation_id, detail],
    );
}

fn current_source(current: &Option<OpenSource>) -> Result<&OpenSource> {
    current.as_ref().ok_or_else(|| {
        RelationalProjectionError::InvalidStreamOrder(
            "a relational record appeared outside a source scope".to_owned(),
        )
    })
}

fn current_source_mut(current: &mut Option<OpenSource>) -> Result<&mut OpenSource> {
    current.as_mut().ok_or_else(|| {
        RelationalProjectionError::InvalidStreamOrder(
            "a relational record appeared outside a source scope".to_owned(),
        )
    })
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<()> {
    if value.is_empty() || value.len() > maximum {
        return invalid_record(format!("{field} is empty or exceeds {maximum} bytes"));
    }
    Ok(())
}

fn validate_optional_text(field: &'static str, value: Option<&str>, maximum: usize) -> Result<()> {
    if let Some(value) = value {
        validate_text(field, value, maximum)?;
    }
    Ok(())
}

fn sqlite_i64(value: u64, field: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| RelationalProjectionError::CountOverflow(field))
}

fn sqlite_u64(value: i64, field: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| RelationalProjectionError::CountOverflow(field))
}

fn contract_generation_error(error: ProjectionContractError) -> RelationalProjectionError {
    RelationalProjectionError::InvalidCoreGeneration(error.to_string())
}

fn contract_record_error(error: ProjectionContractError) -> RelationalProjectionError {
    RelationalProjectionError::InvalidRecord(error.to_string())
}

fn resolver_record_error(error: SourceResolverContractError) -> RelationalProjectionError {
    RelationalProjectionError::InvalidRecord(error.to_string())
}

fn invalid_generation<T>(detail: impl Into<String>) -> Result<T> {
    Err(RelationalProjectionError::InvalidCoreGeneration(
        detail.into(),
    ))
}

fn invalid_record<T>(detail: impl Into<String>) -> Result<T> {
    Err(RelationalProjectionError::InvalidRecord(detail.into()))
}

fn stream_order<T>(detail: impl Into<String>) -> Result<T> {
    Err(RelationalProjectionError::InvalidStreamOrder(detail.into()))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests;
