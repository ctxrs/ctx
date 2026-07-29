//! Provider-local source-backed projection for AstrBot `data_v4.db`.
//!
//! This module deliberately stops at the provider seam. Shared generation
//! lifecycle and registry wiring are owned by the source-backed assembler.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, CertifiedSource, CertifiedSourceInventory, ContentSourceResolver,
    EventHydrationRequest, EventIdentityInput, EventRole, EventType, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionHydrationRequest, SessionIdentityInput, SourceAnchor, SourceInventoryObservation,
    SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError, StableEntityId,
    TypedKey,
};
use ctx_history_index::{IndexError, LexicalDocument};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    common::io::ProviderSourceRoot,
    discover_provider_sources_for_provider_with_context,
    native_source::NativeSqliteValue,
    provider::normalization::{provider_json_text, provider_timestamp_millis, provider_value_text},
    provider::sqlite::sqlite_schema_fingerprint,
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    CaptureError, DiscoveryContext, ProviderSourceStatus, ASTRBOT_SQLITE_SOURCE_FORMAT,
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::super::{
    model::{
        checkpoint_id, item_id, item_is_output, item_role, item_text, provider_session_id,
        ConversationRow, PlatformMessageLink, PlatformMessageRow,
    },
    source::{
        fetch_candidate, hydrate_conversation, hydrate_platform_message, AstrBotSql, RowCandidate,
    },
    ASTRBOT_CAPTURE_REVISION, ASTRBOT_POLICY_REVISION,
};

const SOURCE_SCHEMA_VARIANT: &str = "astrbot-data-v4-logical-v0";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const SOURCE_REVISION_KIND: &str = "astrbot-sqlite-snapshot-v0";
const INVENTORY_AUTHORITY_NAMESPACE: &str = "astrbot.source-inventory";
const INVENTORY_AUTHORITY_KEY: &str = "winner-and-launcher-instances-v0";
const INVENTORY_REVISION_KIND: &str = "astrbot-bounded-discovery-v0";
const INVENTORY_DISCOVERY_REVISION: &str = "astrbot-winner-launcher-inventory-v0";
const PARSER_REVISION: &str = "astrbot-source-backed-v0";
const SELECTED_SOURCE_NAMESPACE: &str = "astrbot.selected-core";
const LAUNCHER_SOURCE_NAMESPACE: &str = "astrbot.launcher-instance";
const SESSION_NAMESPACE: &str = "astrbot.session";
const LOGICAL_SESSION_KIND: &str = "astrbot-session";
const LOGICAL_EVENT_KIND: &str = "astrbot-event";
const CONVERSATION_MESSAGE_RELATION: &str = "astrbot.conversation-message-v0";
const CONVERSATION_OUTPUT_RELATION: &str = "astrbot.conversation-output-v0";
const PLATFORM_MESSAGE_RELATION: &str = "astrbot.platform-message-v0";
const SQLITE_SOURCE_INVALID_REASON: &str =
    "AstrBot SQLite source must have an authorized parent and database leaf";

#[derive(Debug)]
struct SessionFact {
    provider_session_id: String,
    started_at: DateTime<Utc>,
}

#[derive(Debug)]
struct EventFact {
    source_record_ordinal: u64,
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: DateTime<Utc>,
}

#[derive(Debug)]
struct CoreUnit {
    session: SessionFact,
    event: Option<EventFact>,
}

#[derive(Debug, Error)]
pub(crate) enum AstrBotSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error("AstrBot source discovery is incomplete ({issues} bounded issue(s))")]
    IncompleteInventory { issues: usize },
    #[error("AstrBot source candidate {path:?} has non-admissible status {status}")]
    NonAdmissibleSource { path: PathBuf, status: &'static str },
    #[error("AstrBot discovery emitted more than one selected Core database")]
    DuplicateSelectedCore,
    #[error("AstrBot discovery emitted duplicate source identity {0}")]
    DuplicateSourceIdentity(String),
    #[error("AstrBot source-backed count overflow")]
    CountOverflow,
    #[error("AstrBot conversation parser emitted a message the exact resolver cannot reopen")]
    ExactConversationMismatch,
}

pub(crate) type AstrBotSourceBackedResultV0<T> = std::result::Result<T, AstrBotSourceBackedErrorV0>;

type PlatformUnitProjection = (Option<CoreUnit>, Option<String>, [u8; 32], Option<String>);

fn conversation_items(raw: &str) -> (Vec<Value>, bool) {
    match provider_json_text(raw) {
        Value::Array(items) => (items, true),
        value => (vec![value], false),
    }
}

fn conversation_session_fact(row: &ConversationRow) -> SessionFact {
    SessionFact {
        provider_session_id: provider_session_id(row),
        started_at: timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH),
    }
}

fn platform_session_fact(
    row: &PlatformMessageRow,
    link: Option<&PlatformMessageLink>,
) -> SessionFact {
    let provider_session_id = link
        .map(|link| link.provider_session_id.clone())
        .unwrap_or_else(|| {
            format!(
                "platform/{}/{}",
                row.platform_id.as_deref().unwrap_or("unknown"),
                row.user_id.as_deref().unwrap_or("unknown")
            )
        });
    let started_at = link
        .and_then(|link| link.parent_created_at)
        .map(|value| timestamp(Some(value), DateTime::<Utc>::UNIX_EPOCH))
        .unwrap_or_else(|| timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH));
    SessionFact {
        provider_session_id,
        started_at,
    }
}

fn source_backed_conversation_event(
    row: &ConversationRow,
    item: Option<&Value>,
    content_is_array: bool,
    native_ordinal: u64,
) -> Option<EventFact> {
    let item = item?;
    if checkpoint_id(item).is_some() {
        return None;
    }
    let text = if content_is_array {
        item_text(item)
    } else {
        provider_value_text(item)
    }?;
    if text.trim().is_empty() {
        return None;
    }
    let event_type = if item_is_output(item) {
        EventType::ToolOutput
    } else {
        EventType::Message
    };
    Some(EventFact {
        source_record_ordinal: native_ordinal,
        event_type,
        role: item_role(item),
        occurred_at: timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH),
    })
}

fn serialized_hash(
    value_domain: &[u8],
    value: &impl Serialize,
) -> std::result::Result<[u8; 32], CaptureError> {
    let encoded = serde_json::to_vec(value).map_err(CaptureError::from)?;
    let mut hash = Sha256::new();
    hash.update(value_domain);
    hash_field(&mut hash, &encoded);
    Ok(hash.finalize().into())
}

fn candidate_hash(domain: &[u8], candidate: RowCandidate) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(candidate.physical_rowid.to_le_bytes());
    hash.update(candidate.retained_bytes.to_le_bytes());
    hash.update(candidate.legacy_order.logical_id.to_le_bytes());
    hash.update(candidate.legacy_order.timestamp.to_le_bytes());
    hash.finalize().into()
}

fn chain_hash(prior: [u8; 32], row: [u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"ctx-astrbot-prefix-chain-v1\0");
    hash.update(prior);
    hash.update(row);
    hash.finalize().into()
}

fn hash_field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn timestamp(value: Option<i64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    provider_timestamp_millis(value, fallback)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AstrBotSourceIdentityV0 {
    SelectedCore,
    LauncherInstance(String),
}

#[derive(Debug, Clone)]
pub(crate) struct AstrBotSourceBackedSourceV0 {
    path: PathBuf,
    identity: AstrBotSourceIdentityV0,
    source_key: SourceKey,
}

impl AstrBotSourceBackedSourceV0 {
    pub(crate) fn source_key(&self) -> &SourceKey {
        &self.source_key
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AstrBotSourceBackedInventoryV0 {
    observation: SourceInventoryObservation,
    sources: Vec<AstrBotSourceBackedSourceV0>,
}

impl AstrBotSourceBackedInventoryV0 {
    pub(crate) fn discover(context: &DiscoveryContext) -> AstrBotSourceBackedResultV0<Self> {
        let report =
            discover_provider_sources_for_provider_with_context(context, CaptureProvider::AstrBot);
        if !report.issues.is_empty() {
            return Err(AstrBotSourceBackedErrorV0::IncompleteInventory {
                issues: report.issues.len(),
            });
        }
        let observation = inventory_observation(&report)?;
        let mut selected_core = false;
        let mut seen = BTreeSet::new();
        let mut sources = Vec::new();
        for candidate in &report.sources {
            match candidate.status {
                ProviderSourceStatus::Missing => continue,
                ProviderSourceStatus::Available => {}
                status => {
                    return Err(AstrBotSourceBackedErrorV0::NonAdmissibleSource {
                        path: candidate.path.clone(),
                        status: status.as_str(),
                    });
                }
            }
            if candidate.source_format != ASTRBOT_SQLITE_SOURCE_FORMAT {
                return Err(AstrBotSourceBackedErrorV0::NonAdmissibleSource {
                    path: candidate.path.clone(),
                    status: "unexpected_source_format",
                });
            }
            let identity = launcher_instance_identity(context.home(), &candidate.path)
                .map(AstrBotSourceIdentityV0::LauncherInstance)
                .unwrap_or(AstrBotSourceIdentityV0::SelectedCore);
            if identity == AstrBotSourceIdentityV0::SelectedCore {
                if selected_core {
                    return Err(AstrBotSourceBackedErrorV0::DuplicateSelectedCore);
                }
                selected_core = true;
            }
            let source_key = source_key(&identity)?;
            if !seen.insert(source_key.identity().digest()) {
                return Err(AstrBotSourceBackedErrorV0::DuplicateSourceIdentity(
                    source_key.identity().to_string(),
                ));
            }
            sources.push(AstrBotSourceBackedSourceV0 {
                path: candidate.path.clone(),
                identity,
                source_key,
            });
        }
        sources.sort_by(|left, right| left.identity.cmp(&right.identity));
        Ok(Self {
            observation,
            sources,
        })
    }

    pub(crate) fn sources(&self) -> &[AstrBotSourceBackedSourceV0] {
        &self.sources
    }

    pub(crate) fn certify(
        &self,
        closing: &Self,
    ) -> AstrBotSourceBackedResultV0<CertifiedSourceInventory> {
        Ok(CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            INVENTORY_DISCOVERY_REVISION,
            self.sources
                .iter()
                .map(|source| source.source_key.clone())
                .collect(),
        )?)
    }
}

pub(crate) trait AstrBotSourceBackedSinkV0 {
    fn emit(&mut self, document: LexicalDocument) -> AstrBotSourceBackedResultV0<()>;
}

impl<F> AstrBotSourceBackedSinkV0 for F
where
    F: FnMut(LexicalDocument) -> AstrBotSourceBackedResultV0<()>,
{
    fn emit(&mut self, document: LexicalDocument) -> AstrBotSourceBackedResultV0<()> {
        self(document)
    }
}

pub(crate) fn scan_astrbot_source_backed_v0(
    source: &AstrBotSourceBackedSourceV0,
    sink: &mut impl AstrBotSourceBackedSinkV0,
) -> AstrBotSourceBackedResultV0<CertifiedSource> {
    let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(&source.path)?;
    let opening_evidence = sqlite_snapshot.evidence().clone();
    let conn = sqlite_snapshot.connection()?;
    let sql = AstrBotSql::new(conn)?;
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(CaptureError::from)?;
    let schema_fingerprint = sqlite_schema_fingerprint(conn)?;
    let opening = source_observation(
        &source.source_key,
        &opening_evidence,
        user_version,
        &schema_fingerprint,
    )?;
    let source_revision_digest = revision_digest(&opening);
    let revision_scope = TypedKey::bytes(source_revision_digest.to_vec())?;
    let mut counts = ScannedSourceCounts::default();
    let mut content_chain = [0_u8; 32];
    let mut native_ordinal = 0_u64;
    let mut conversation_after = None;
    let mut pending_documents = Vec::new();
    let mut checkpoint_links = BTreeMap::new();

    loop {
        let Some(candidate) = fetch_candidate(
            conn,
            &sql.conversation_candidate_initial,
            &sql.conversation_candidate_after,
            conversation_after,
        )?
        else {
            break;
        };
        conversation_after = Some(candidate.physical_rowid);
        add_certified_bytes(&mut counts, candidate.observed_bytes()?)?;
        if candidate.observed_bytes()?
            > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
        {
            content_chain = chain_hash(
                content_chain,
                candidate_hash(
                    b"astrbot-source-backed-conversation-oversize-v0\0",
                    candidate,
                ),
            );
            add_complete(&mut counts)?;
            add_rejected(&mut counts)?;
            native_ordinal = native_ordinal
                .checked_add(1)
                .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
            continue;
        }

        let row =
            hydrate_conversation(conn, &sql.conversation_hydration, candidate.physical_rowid)?;
        let row_digest =
            logical_values_digest(&super::super::model::conversation_values(row.clone()));
        content_chain = chain_hash(content_chain, row_digest);
        let (items, content_is_array) = conversation_items(&row.content);
        let provider_session_id = provider_session_id(&row);
        for item in &items {
            if let Some(checkpoint) = checkpoint_id(item) {
                checkpoint_links.insert(
                    checkpoint,
                    PlatformMessageLink {
                        provider_session_id: provider_session_id.clone(),
                        parent_created_at: row.created_at,
                    },
                );
            }
        }
        let item_count = items.len().max(1);
        for item_index in 0..item_count {
            add_complete(&mut counts)?;
            let item = items.get(item_index);
            let event =
                source_backed_conversation_event(&row, item, content_is_array, native_ordinal);
            if let Some(event) = event {
                let complete_text = if content_is_array {
                    item.and_then(item_text)
                        .filter(|text| !text.trim().is_empty())
                        .ok_or(AstrBotSourceBackedErrorV0::ExactConversationMismatch)?
                } else {
                    item.and_then(provider_value_text)
                        .filter(|text| !text.trim().is_empty())
                        .ok_or(AstrBotSourceBackedErrorV0::ExactConversationMismatch)?
                };
                let session = conversation_session_fact(&row);
                let document = conversation_document(
                    source,
                    &source_revision_digest,
                    &revision_scope,
                    candidate.physical_rowid,
                    item_index,
                    row_digest,
                    item,
                    &session,
                    &event,
                    &complete_text,
                )?;
                pending_documents.push(document);
                add_retained(&mut counts)?;
            } else {
                add_ignored(&mut counts)?;
            }
            native_ordinal = native_ordinal
                .checked_add(1)
                .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
        }
    }

    if let (Some(initial), Some(after)) = (
        sql.platform_message_candidate_initial.as_deref(),
        sql.platform_message_candidate_after.as_deref(),
    ) {
        let mut platform_after = None;
        loop {
            let Some(candidate) = fetch_candidate(conn, initial, after, platform_after)? else {
                break;
            };
            platform_after = Some(candidate.physical_rowid);
            add_certified_bytes(&mut counts, candidate.observed_bytes()?)?;
            add_complete(&mut counts)?;
            if candidate.observed_bytes()?
                > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
            {
                content_chain = chain_hash(
                    content_chain,
                    candidate_hash(b"astrbot-source-backed-platform-oversize-v0\0", candidate),
                );
                add_rejected(&mut counts)?;
            } else {
                let (unit, rejection, row_digest, complete_text) = source_backed_platform_unit(
                    conn,
                    &sql,
                    candidate,
                    native_ordinal,
                    &checkpoint_links,
                )?;
                content_chain = chain_hash(content_chain, row_digest);
                if rejection.is_some() {
                    add_rejected(&mut counts)?;
                } else if let Some(unit) = unit {
                    if let Some(event) = unit.event {
                        let document = platform_document(
                            source,
                            &source_revision_digest,
                            candidate.physical_rowid,
                            candidate.legacy_order.logical_id,
                            row_digest,
                            &unit.session,
                            &event,
                            complete_text
                                .as_deref()
                                .ok_or(AstrBotSourceBackedErrorV0::ExactConversationMismatch)?,
                        )?;
                        pending_documents.push(document);
                        add_retained(&mut counts)?;
                    } else {
                        add_ignored(&mut counts)?;
                    }
                } else {
                    add_ignored(&mut counts)?;
                }
            }
            native_ordinal = native_ordinal
                .checked_add(1)
                .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
        }
    }

    let closing_evidence = sqlite_snapshot.finish()?;
    source_root.revalidate()?;
    let closing = source_observation(
        &source.source_key,
        &closing_evidence,
        user_version,
        &schema_fingerprint,
    )?;
    let mut digest = Sha256::new();
    digest.update(b"ctx-astrbot-source-backed-content-v0\0");
    digest.update(content_chain);
    digest.update(counts.complete_records.to_be_bytes());
    digest.update(counts.certified_bytes.to_be_bytes());
    let certificate = CertifiedSource::certify(
        opening,
        closing,
        PARSER_REVISION,
        digest.finalize().into(),
        counts,
    )?;
    for document in pending_documents {
        sink.emit(document)?;
    }
    Ok(certificate)
}

fn source_backed_platform_unit(
    conn: &rusqlite::Connection,
    sql: &AstrBotSql,
    candidate: RowCandidate,
    native_ordinal: u64,
    checkpoint_links: &BTreeMap<String, PlatformMessageLink>,
) -> AstrBotSourceBackedResultV0<PlatformUnitProjection> {
    let hydration =
        sql.platform_message_hydration
            .as_deref()
            .ok_or(CaptureError::SystemInvariant(
                "AstrBot platform-message hydration SQL is missing",
            ))?;
    let row = hydrate_platform_message(conn, hydration, candidate.physical_rowid)?;
    let row_sha256 = serialized_hash(b"astrbot-platform-row-v1\0", &row)?;
    let link = row
        .llm_checkpoint_id
        .as_ref()
        .and_then(|checkpoint| checkpoint_links.get(checkpoint));
    let Some(text) = row
        .content
        .as_deref()
        .map(provider_json_text)
        .as_ref()
        .and_then(provider_value_text)
        .filter(|text| !text.trim().is_empty())
    else {
        return Ok((None, None, row_sha256, None));
    };
    let session = platform_session_fact(&row, link);
    let role = if row.sender_id.as_deref() == row.user_id.as_deref() {
        Some(EventRole::User)
    } else {
        Some(EventRole::Assistant)
    };
    let event_type = EventType::Message;
    let occurred_at = timestamp(row.created_at, session.started_at);
    Ok((
        Some(CoreUnit {
            session,
            event: Some(EventFact {
                source_record_ordinal: native_ordinal,
                event_type,
                role,
                occurred_at,
            }),
        }),
        None,
        row_sha256,
        Some(text),
    ))
}

#[derive(Debug, Clone)]
pub(crate) struct AstrBotSourceBackedResolverV0 {
    sources: BTreeMap<[u8; 32], AstrBotSourceBackedSourceV0>,
}

impl AstrBotSourceBackedResolverV0 {
    pub(crate) fn from_inventory(
        inventory: &AstrBotSourceBackedInventoryV0,
    ) -> AstrBotSourceBackedResultV0<Self> {
        let sources = inventory
            .sources
            .iter()
            .cloned()
            .map(|source| (source.source_key.identity().digest(), source))
            .collect();
        Ok(Self { sources })
    }

    pub(crate) fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let mut coordinates = Vec::with_capacity(requests.len());
        for request in requests {
            coordinates.push(decode_locator(request.locator())?);
        }
        let source_key = requests[0].locator().source();
        let source = self
            .sources
            .get(&source_key.identity().digest())
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "AstrBot source is absent from the complete admitted inventory",
                )
            })?;
        if !source.source_key.exact_descriptor_eq(source_key) {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot source descriptor does not match the admitted source identity",
            ));
        }
        if requests
            .iter()
            .any(|request| !request.locator().source().exact_descriptor_eq(source_key))
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot hydration batch spans multiple source descriptors",
            ));
        }

        let (source_root, sqlite_snapshot) =
            open_root_authorized_snapshot(&source.path).map_err(map_source_hydration)?;
        let hydration = (|| {
            let conn = sqlite_snapshot.connection().map_err(map_sqlite_hydration)?;
            let sql = AstrBotSql::new(conn).map_err(map_parser_hydration)?;
            let user_version: i64 = conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .map_err(CaptureError::from)
                .map_err(map_capture_hydration)?;
            let schema_fingerprint =
                sqlite_schema_fingerprint(conn).map_err(map_capture_hydration)?;
            let observation = source_observation(
                &source.source_key,
                sqlite_snapshot.evidence(),
                user_version,
                &schema_fingerprint,
            )
            .map_err(|_| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    "AstrBot source observation is invalid",
                )
            })?;
            let observed_revision = revision_digest(&observation);
            if requests.iter().any(|request| {
                request.locator().certified_source_revision_digest() != Some(&observed_revision)
            }) {
                return Err(hydration_failure(
                    HydrationFailureKind::StaleSourceEvidence,
                    "AstrBot SQLite snapshot no longer matches the certified revision",
                ));
            }

            let checkpoint_links = coordinates
                .iter()
                .any(AstrBotCoordinate::is_platform)
                .then(|| load_checkpoint_links(conn, &sql))
                .transpose()?
                .unwrap_or_default();
            let revision_scope = TypedKey::bytes(observed_revision.to_vec()).map_err(|error| {
                hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
            })?;
            let mut hydrated = Vec::with_capacity(requests.len());
            let mut conversations = BTreeMap::new();
            for (request, coordinate) in requests.iter().zip(coordinates) {
                let provider_bytes = match coordinate {
                    AstrBotCoordinate::Conversation {
                        physical_rowid,
                        item_index,
                        row_digest,
                        content_kind,
                    } => {
                        if let std::collections::btree_map::Entry::Vacant(entry) =
                            conversations.entry(physical_rowid)
                        {
                            let row = hydrate_conversation(
                                conn,
                                &sql.conversation_hydration,
                                physical_rowid,
                            )
                            .map_err(map_capture_hydration)?;
                            let values = super::super::model::conversation_values(row);
                            entry.insert(values);
                        }
                        let values = conversations.get(&physical_rowid).ok_or_else(|| {
                            hydration_failure(
                                HydrationFailureKind::MissingRecord,
                                "AstrBot conversation row is missing",
                            )
                        })?;
                        if logical_values_digest(values) != row_digest {
                            return Err(hydration_failure(
                                HydrationFailureKind::StaleRecordEvidence,
                                "AstrBot conversation row version no longer matches",
                            ));
                        }
                        hydrate_conversation_coordinate(
                            &source.source_key,
                            request,
                            values,
                            physical_rowid,
                            item_index,
                            content_kind,
                            &revision_scope,
                        )?
                    }
                    AstrBotCoordinate::Platform {
                        physical_rowid,
                        logical_id,
                        row_digest,
                    } => hydrate_platform_coordinate(
                        &source.source_key,
                        request,
                        conn,
                        &sql,
                        physical_rowid,
                        logical_id,
                        row_digest,
                        &checkpoint_links,
                    )?,
                };
                hydrated.push(HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes,
                });
            }
            Ok(hydrated)
        })();
        let finished = sqlite_snapshot.finish().map_err(map_sqlite_hydration);
        let root_current = source_root.revalidate().map_err(map_capture_hydration);
        finished?;
        root_current?;
        hydration
    }

    pub(crate) fn hydrate_batch_request(
        &self,
        request: &BatchHydrationRequest,
    ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
        let result = BatchHydrationResult::new(self.hydrate_requests(request.events())?).map_err(
            |error| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    format!("invalid AstrBot batch hydration result: {error}"),
                )
            },
        )?;
        result.validate_for_request(request)?;
        Ok(result)
    }
}

fn open_root_authorized_snapshot(
    path: &Path,
) -> AstrBotSourceBackedResultV0<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook(path, || {})
}

fn open_root_authorized_snapshot_with_hook(
    path: &Path,
    after_authorize: impl FnOnce(),
) -> AstrBotSourceBackedResultV0<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let database_leaf =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: SQLITE_SOURCE_INVALID_REASON,
            })?;
    let source_root = ProviderSourceRoot::open(parent)?;
    let source_directory = source_root.directory()?;
    let parent_handle = source_directory
        .try_clone_authority_handle()
        .map_err(CaptureError::from)?;
    let sqlite_authority = retain_sqlite_source_directory_authority(&parent_handle, parent)?;
    let sqlite_snapshot =
        open_root_handle_sqlite_source_snapshot(&sqlite_authority, database_leaf)?;
    after_authorize();
    sqlite_snapshot.revalidate()?;
    source_directory.revalidate()?;
    source_root.revalidate()?;
    let connection = sqlite_snapshot.connection()?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| AstrBotSourceBackedErrorV0::CountOverflow)?;
    connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(CaptureError::from)?;
    Ok((source_root, sqlite_snapshot))
}

impl ContentSourceResolver for AstrBotSourceBackedResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate_requests(std::slice::from_ref(request))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    "AstrBot hydration returned no record",
                )
            })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
        self.hydrate_batch_request(request)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_requests(request.events())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConversationContentKind {
    Message,
    Output,
}

#[derive(Debug, Clone, Copy)]
enum AstrBotCoordinate {
    Conversation {
        physical_rowid: i64,
        item_index: u32,
        row_digest: [u8; 32],
        content_kind: ConversationContentKind,
    },
    Platform {
        physical_rowid: i64,
        logical_id: i64,
        row_digest: [u8; 32],
    },
}

impl AstrBotCoordinate {
    const fn is_platform(&self) -> bool {
        matches!(self, Self::Platform { .. })
    }
}

fn decode_locator(
    locator: &SourceRecordLocator,
) -> std::result::Result<AstrBotCoordinate, HydrationFailure> {
    if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot SQLite locator is not exact-revision scoped",
        ));
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot locator is not a provider SQLite coordinate",
        ));
    };
    let Some(TypedKey::Bytes(row_digest)) = row_version else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot SQLite locator has no typed row version",
        ));
    };
    let row_digest: [u8; 32] = row_digest.as_slice().try_into().map_err(|_| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot SQLite row version has an invalid length",
        )
    })?;
    let TypedKey::Composite(parts) = primary_key else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot SQLite key is not composite",
        ));
    };
    if logical_relation == PLATFORM_MESSAGE_RELATION {
        let [TypedKey::I64(physical_rowid), TypedKey::I64(logical_id)] = parts.as_slice() else {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot platform-message key has an invalid shape",
            ));
        };
        return Ok(AstrBotCoordinate::Platform {
            physical_rowid: *physical_rowid,
            logical_id: *logical_id,
            row_digest,
        });
    }
    let content_kind = match logical_relation.as_str() {
        CONVERSATION_MESSAGE_RELATION => ConversationContentKind::Message,
        CONVERSATION_OUTPUT_RELATION => ConversationContentKind::Output,
        _ => {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot logical relation is unsupported",
            ));
        }
    };
    let [TypedKey::I64(physical_rowid), TypedKey::U64(item_index)] = parts.as_slice() else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot conversation key has an invalid shape",
        ));
    };
    Ok(AstrBotCoordinate::Conversation {
        physical_rowid: *physical_rowid,
        item_index: u32::try_from(*item_index).map_err(|_| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot conversation item index exceeds u32",
            )
        })?,
        row_digest,
        content_kind,
    })
}

fn hydrate_conversation_coordinate(
    source: &SourceKey,
    request: &EventHydrationRequest,
    values: &[NativeSqliteValue],
    physical_rowid: i64,
    item_index: u32,
    content_kind: ConversationContentKind,
    revision_scope: &TypedKey,
) -> std::result::Result<Vec<u8>, HydrationFailure> {
    let row = super::super::model::decode_conversation(values).map_err(map_capture_hydration)?;
    let (items, content_is_array) = conversation_items(&row.content);
    let item = items
        .get(usize::try_from(item_index).unwrap_or(usize::MAX))
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "AstrBot conversation subrecord is missing",
            )
        })?;
    if checkpoint_id(item).is_some() {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot locator addresses a non-indexed checkpoint",
        ));
    }
    let is_output = item_is_output(item);
    if matches!(
        (content_kind, is_output),
        (ConversationContentKind::Message, true) | (ConversationContentKind::Output, false)
    ) {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot locator relation does not match the native subrecord kind",
        ));
    }
    let text = if content_is_array {
        item_text(item)
    } else {
        provider_value_text(item)
    }
    .filter(|text| !text.trim().is_empty())
    .ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::MissingRecord,
            "AstrBot conversation subrecord has no meaningful display text",
        )
    })?;
    let session_id =
        stable_session_id(source, &provider_session_id(&row)).map_err(invalid_locator)?;
    let native_item_key = conversation_native_item_key(
        physical_rowid,
        usize::try_from(item_index).unwrap_or(usize::MAX),
        Some(item),
        revision_scope,
    )
    .map_err(invalid_locator)?;
    let expected_event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(invalid_locator)?;
    if expected_event_id != request.event_id() {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot conversation native key does not derive the requested event identity",
        ));
    }
    verify_record_digest(request.locator(), text.as_bytes(), "conversation subrecord")?;
    Ok(text.into_bytes())
}

#[allow(clippy::too_many_arguments)]
fn hydrate_platform_coordinate(
    source: &SourceKey,
    request: &EventHydrationRequest,
    conn: &rusqlite::Connection,
    sql: &AstrBotSql,
    physical_rowid: i64,
    logical_id: i64,
    row_digest: [u8; 32],
    checkpoint_links: &BTreeMap<String, PlatformMessageLink>,
) -> std::result::Result<Vec<u8>, HydrationFailure> {
    let hydration = sql.platform_message_hydration.as_deref().ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "AstrBot source has no supported platform-message relation",
        )
    })?;
    let row =
        hydrate_platform_message(conn, hydration, physical_rowid).map_err(map_capture_hydration)?;
    if row.id != logical_id {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot platform-message logical key does not match the reopened row",
        ));
    }
    let observed_row_digest =
        serialized_hash(b"astrbot-platform-row-v1\0", &row).map_err(map_capture_hydration)?;
    if observed_row_digest != row_digest {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "AstrBot platform-message row version no longer matches",
        ));
    }
    let text = row
        .content
        .as_deref()
        .map(provider_json_text)
        .as_ref()
        .and_then(provider_value_text)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "AstrBot platform-message row has no meaningful display text",
            )
        })?;
    let link = row
        .llm_checkpoint_id
        .as_ref()
        .and_then(|checkpoint| checkpoint_links.get(checkpoint));
    let session = platform_session_fact(&row, link);
    let session_id =
        stable_session_id(source, &session.provider_session_id).map_err(invalid_locator)?;
    let native_item_key =
        NativeItemKey::native_id("astrbot.platform-message", TypedKey::I64(logical_id))
            .map_err(invalid_locator)?;
    let expected_event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(invalid_locator)?;
    if expected_event_id != request.event_id() {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot platform-message native key does not derive the requested event identity",
        ));
    }
    verify_record_digest(
        request.locator(),
        text.as_bytes(),
        "platform-message display text",
    )?;
    Ok(text.into_bytes())
}

fn load_checkpoint_links(
    conn: &rusqlite::Connection,
    sql: &AstrBotSql,
) -> std::result::Result<BTreeMap<String, PlatformMessageLink>, HydrationFailure> {
    let mut links = BTreeMap::new();
    let mut after = None;
    while let Some(candidate) = fetch_candidate(
        conn,
        &sql.conversation_candidate_initial,
        &sql.conversation_candidate_after,
        after,
    )
    .map_err(map_capture_hydration)?
    {
        after = Some(candidate.physical_rowid);
        if candidate.observed_bytes().map_err(map_capture_hydration)?
            > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
        {
            continue;
        }
        let row = hydrate_conversation(conn, &sql.conversation_hydration, candidate.physical_rowid)
            .map_err(map_capture_hydration)?;
        let provider_session_id = provider_session_id(&row);
        for item in conversation_items(&row.content).0 {
            if let Some(checkpoint) = checkpoint_id(&item) {
                links.insert(
                    checkpoint,
                    PlatformMessageLink {
                        provider_session_id: provider_session_id.clone(),
                        parent_created_at: row.created_at,
                    },
                );
            }
        }
    }
    Ok(links)
}

fn verify_record_digest(
    locator: &SourceRecordLocator,
    provider_bytes: &[u8],
    label: &str,
) -> std::result::Result<(), HydrationFailure> {
    let digest: [u8; 32] = Sha256::digest(provider_bytes).into();
    if &digest == locator.record_digest() {
        Ok(())
    } else {
        Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            format!("AstrBot {label} digest no longer matches"),
        ))
    }
}

fn invalid_locator(error: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
}

fn source_key(identity: &AstrBotSourceIdentityV0) -> AstrBotSourceBackedResultV0<SourceKey> {
    let (namespace, key) = match identity {
        AstrBotSourceIdentityV0::SelectedCore => {
            (SELECTED_SOURCE_NAMESPACE, TypedKey::utf8("selected-core")?)
        }
        AstrBotSourceIdentityV0::LauncherInstance(instance) => {
            (LAUNCHER_SOURCE_NAMESPACE, TypedKey::utf8(instance.clone())?)
        }
    };
    Ok(SourceKey::derive(
        CaptureProvider::AstrBot.as_str(),
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        SOURCE_IDENTITY_VERSION,
        SourceAnchor::provider_native(namespace, key)?,
    )?)
}

fn conversation_native_item_key(
    physical_rowid: i64,
    item_index: usize,
    item: Option<&Value>,
    revision_scope: &TypedKey,
) -> AstrBotSourceBackedResultV0<NativeItemKey> {
    if let Some(native_id) = item.and_then(item_id) {
        Ok(NativeItemKey::composite(
            "astrbot.conversation-item",
            vec![TypedKey::I64(physical_rowid), TypedKey::utf8(native_id)?],
        )?)
    } else {
        Ok(NativeItemKey::revision_scoped_position(
            "astrbot.conversation-position",
            TypedKey::composite(vec![
                TypedKey::I64(physical_rowid),
                TypedKey::U64(
                    u64::try_from(item_index)
                        .map_err(|_| AstrBotSourceBackedErrorV0::CountOverflow)?,
                ),
            ])?,
            revision_scope.clone(),
        )?)
    }
}

fn launcher_instance_identity(home: &Path, path: &Path) -> Option<String> {
    let root = home.join(".astrbot_launcher").join("instances");
    let relative = path.strip_prefix(root).ok()?;
    let components = relative.components().collect::<Vec<_>>();
    let [Component::Normal(instance), Component::Normal(core), Component::Normal(data), Component::Normal(database)] =
        components.as_slice()
    else {
        return None;
    };
    if core != &OsStr::new("core")
        || data != &OsStr::new("data")
        || database != &OsStr::new("data_v4.db")
    {
        return None;
    }
    Uuid::parse_str(instance.to_str()?)
        .ok()
        .map(|id| id.to_string())
}

fn inventory_observation(
    report: &crate::DiscoveryReport,
) -> AstrBotSourceBackedResultV0<SourceInventoryObservation> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-astrbot-source-inventory-observation-v0\0");
    digest.update((report.sources.len() as u64).to_be_bytes());
    for source in &report.sources {
        let path = source.path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        digest.update((source.source_format.len() as u64).to_be_bytes());
        digest.update(source.source_format.as_bytes());
        digest.update(source.status.as_str().as_bytes());
    }
    Ok(SourceInventoryObservation::new(
        CaptureProvider::AstrBot.as_str(),
        INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::utf8(INVENTORY_AUTHORITY_KEY)?,
        INVENTORY_REVISION_KIND,
        digest.finalize().to_vec(),
    )?)
}

fn source_observation(
    source: &SourceKey,
    evidence: &SqliteSourceEvidence,
    user_version: i64,
    schema_fingerprint: &str,
) -> AstrBotSourceBackedResultV0<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        SOURCE_REVISION_KIND,
        format!(
            "astrbot-sqlite-snapshot-v1:capture={ASTRBOT_CAPTURE_REVISION};policy={ASTRBOT_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint};identity={};length={};revision={}",
            hex(evidence.identity()),
            evidence.length(),
            hex(evidence.revision()),
        )
        .into_bytes(),
    )?)
}

fn revision_digest(observation: &SourceObservation) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-astrbot-source-revision-v0\0");
    digest.update(observation.revision_kind().as_bytes());
    digest.update(observation.revision());
    digest.finalize().into()
}

fn logical_values_digest(values: &[NativeSqliteValue]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-astrbot-source-backed-logical-row-v0\0");
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    digest.finalize().into()
}

#[allow(clippy::too_many_arguments)]
fn conversation_document(
    source: &AstrBotSourceBackedSourceV0,
    source_revision_digest: &[u8; 32],
    revision_scope: &TypedKey,
    physical_rowid: i64,
    item_index: usize,
    row_digest: [u8; 32],
    item: Option<&Value>,
    session: &SessionFact,
    event: &EventFact,
    complete_text: &str,
) -> AstrBotSourceBackedResultV0<LexicalDocument> {
    let session_id = stable_session_id(&source.source_key, &session.provider_session_id)?;
    let native_item_key =
        conversation_native_item_key(physical_rowid, item_index, item, revision_scope)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source.source_key,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let logical_relation = if event.event_type == EventType::Message {
        CONVERSATION_MESSAGE_RELATION
    } else {
        CONVERSATION_OUTPUT_RELATION
    };
    let locator = SourceRecordLocator::new(
        source.source_key.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: logical_relation.to_owned(),
            primary_key: TypedKey::composite(vec![
                TypedKey::I64(physical_rowid),
                TypedKey::U64(
                    u64::try_from(item_index)
                        .map_err(|_| AstrBotSourceBackedErrorV0::CountOverflow)?,
                ),
            ])?,
            row_version: Some(TypedKey::bytes(row_digest.to_vec())?),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(*source_revision_digest),
        Sha256::digest(complete_text.as_bytes()).into(),
    )?;
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.source_key.clone(),
        locator,
        provider_session_id: Some(session.provider_session_id.clone()),
        branch: None,
        source_path: Some(source.path.to_string_lossy().into_owned()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: event.source_record_ordinal,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body: complete_text.to_owned(),
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    })
}

fn platform_document(
    source: &AstrBotSourceBackedSourceV0,
    source_revision_digest: &[u8; 32],
    physical_rowid: i64,
    logical_id: i64,
    row_digest: [u8; 32],
    session: &SessionFact,
    event: &EventFact,
    complete_text: &str,
) -> AstrBotSourceBackedResultV0<LexicalDocument> {
    let session_id = stable_session_id(&source.source_key, &session.provider_session_id)?;
    let native_item_key =
        NativeItemKey::native_id("astrbot.platform-message", TypedKey::I64(logical_id))?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source.source_key,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = SourceRecordLocator::new(
        source.source_key.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: PLATFORM_MESSAGE_RELATION.to_owned(),
            primary_key: TypedKey::composite(vec![
                TypedKey::I64(physical_rowid),
                TypedKey::I64(logical_id),
            ])?,
            row_version: Some(TypedKey::bytes(row_digest.to_vec())?),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(*source_revision_digest),
        Sha256::digest(complete_text.as_bytes()).into(),
    )?;
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.source_key.clone(),
        locator,
        provider_session_id: Some(session.provider_session_id.clone()),
        branch: None,
        source_path: Some(source.path.to_string_lossy().into_owned()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: event.source_record_ordinal,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body: complete_text.to_owned(),
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    })
}

fn stable_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> AstrBotSourceBackedResultV0<StableEntityId> {
    let native_session_key =
        NativeSessionKey::native_id(SESSION_NAMESPACE, TypedKey::utf8(provider_session_id)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn add_complete(counts: &mut ScannedSourceCounts) -> AstrBotSourceBackedResultV0<()> {
    counts.complete_records = counts
        .complete_records
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn add_retained(counts: &mut ScannedSourceCounts) -> AstrBotSourceBackedResultV0<()> {
    counts.retained_records = counts
        .retained_records
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    counts.indexed_documents = counts
        .indexed_documents
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn add_rejected(counts: &mut ScannedSourceCounts) -> AstrBotSourceBackedResultV0<()> {
    counts.rejected_records = counts
        .rejected_records
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn add_ignored(counts: &mut ScannedSourceCounts) -> AstrBotSourceBackedResultV0<()> {
    counts.ignored_records = counts
        .ignored_records
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn add_certified_bytes(
    counts: &mut ScannedSourceCounts,
    bytes: u64,
) -> AstrBotSourceBackedResultV0<()> {
    counts.certified_bytes = counts
        .certified_bytes
        .checked_add(bytes)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn hydration_failure(kind: HydrationFailureKind, detail: impl Into<String>) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.into(),
    }
}

fn map_capture_hydration(error: CaptureError) -> HydrationFailure {
    match error {
        CaptureError::SourceChangedDuringCapture => hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "AstrBot source changed during reopening",
        ),
        CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => hydration_failure(
            HydrationFailureKind::MissingRecord,
            "AstrBot source record is missing",
        ),
        CaptureError::UnsupportedSchema(_) | CaptureError::UnsupportedSchemaVersion(_) => {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "AstrBot SQLite schema is unsupported",
            )
        }
        CaptureError::InvalidPayload(_)
        | CaptureError::Json(_)
        | CaptureError::Sqlite(
            rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::InvalidColumnType(..),
        ) => hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "AstrBot SQLite row is malformed for the certified parser",
        ),
        _ => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            "AstrBot source could not be reopened",
        ),
    }
}

fn map_parser_hydration(error: CaptureError) -> HydrationFailure {
    match error {
        CaptureError::InvalidPayload(_)
        | CaptureError::UnsupportedSchema(_)
        | CaptureError::UnsupportedSchemaVersion(_) => hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "AstrBot SQLite schema is unsupported",
        ),
        error => map_capture_hydration(error),
    }
}

fn map_sqlite_hydration(error: SqliteSourceAccessError) -> HydrationFailure {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "AstrBot SQLite source changed during reopening",
        ),
        SqliteSourceAccessError::Io { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "AstrBot database leaf is absent beneath the admitted source root",
            )
        }
        error => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            error.to_string(),
        ),
    }
}

fn map_source_hydration(error: AstrBotSourceBackedErrorV0) -> HydrationFailure {
    match error {
        AstrBotSourceBackedErrorV0::Capture(error) => map_capture_hydration(error),
        AstrBotSourceBackedErrorV0::SqliteSource(error) => map_sqlite_hydration(error),
        AstrBotSourceBackedErrorV0::Projection(error) => invalid_locator(error),
        AstrBotSourceBackedErrorV0::Resolver(error) => invalid_locator(error),
        AstrBotSourceBackedErrorV0::Index(error) => invalid_locator(error),
        error => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            error.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ctx_history_core::{ContentSourceResolver, EventHydrationRequest};
    use rusqlite::{params, Connection};
    use serde_json::json;

    use crate::{test_support_paths::tempdir, DiscoveryPlatform, DiscoveryPlatformDirs};

    use super::*;

    fn create_database(path: &Path, session: &str, text: &str) {
        fs::create_dir_all(path.parent().expect("database parent")).expect("create parent");
        let conn = Connection::open(path).expect("open AstrBot fixture");
        conn.execute_batch(
            "pragma user_version = 4;
             create table conversations (
                 id integer primary key,
                 inner_conversation_id text,
                 conversation_id text,
                 platform_id text,
                 user_id text,
                 content text not null,
                 title text,
                 persona_id text,
                 token_usage text,
                 created_at integer,
                 updated_at integer
             );
             create table platform_message_history (
                 id integer primary key,
                 platform_id text,
                 user_id text,
                 sender_id text,
                 sender_name text,
                 content text,
                 llm_checkpoint_id text,
                 created_at integer
             );",
        )
        .expect("AstrBot schema");
        conn.execute(
            "insert into conversations (
                 id, inner_conversation_id, conversation_id, platform_id, user_id,
                 content, title, persona_id, token_usage, created_at, updated_at
             ) values (1, ?1, ?2, 'webchat', 'user', ?3, 'title', 'persona',
                       '{\"prompt\":1,\"completion\":2}', 1780000000000, 1780000001000)",
            params![
                session,
                format!("conversation-{session}"),
                json!([{
                    "id": format!("message-{session}"),
                    "role": "user",
                    "content": text,
                }])
                .to_string(),
            ],
        )
        .expect("AstrBot conversation");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_backed_open_does_not_follow_leaf_swap_after_authorization() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("data_v4.db");
        let attacker = temp.path().join("attacker.db");
        let original = temp.path().join("original.db");
        create_database(&path, "expected", "expected");
        create_database(&attacker, "attacker", "attacker");

        let result = open_root_authorized_snapshot_with_hook(&path, || {
            fs::rename(&path, &original).unwrap();
            fs::rename(&attacker, &path).unwrap();
        });
        assert!(matches!(
            result,
            Err(AstrBotSourceBackedErrorV0::SqliteSource(
                SqliteSourceAccessError::SourceChanged,
            ))
        ));
    }

    #[test]
    fn active_wal_scan_reads_latest_rows_without_persistent_source_writes() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("data_v4.db");
        create_database(&path, "wal-session", "before WAL");
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "wal").unwrap();
        writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        writer
            .execute_batch("pragma wal_checkpoint(truncate)")
            .unwrap();
        writer
            .execute(
                "update conversations set content = ?1 where id = 1",
                [json!([{
                    "id": "message-wal-session",
                    "role": "user",
                    "content": "AstrBot active WAL sentinel",
                }])
                .to_string()],
            )
            .unwrap();
        let before = sqlite_persistent_bytes(&path);
        let source = selected_source(&path);
        let documents = scan_documents(&source);
        let document = documents
            .iter()
            .find(|document| document.body.contains("AstrBot active WAL sentinel"))
            .unwrap();
        let hydrated = resolver_for(&source)
            .hydrate_event(&event_request(document))
            .unwrap();
        assert_eq!(hydrated.provider_bytes, b"AstrBot active WAL sentinel");
        assert_eq!(sqlite_persistent_bytes(&path), before);
        drop(writer);
    }

    fn context(home: &Path, cwd: &Path) -> DiscoveryContext {
        DiscoveryContext::new(
            home,
            cwd,
            DiscoveryPlatform::Linux,
            DiscoveryPlatformDirs::default(),
        )
    }

    fn relation(document: &LexicalDocument) -> &str {
        match document.locator.coordinate() {
            NativeRecordCoordinate::ProviderSqlite {
                logical_relation, ..
            } => logical_relation,
            coordinate => panic!("unexpected AstrBot coordinate: {coordinate:?}"),
        }
    }

    fn sqlite_persistent_bytes(path: &Path) -> Vec<Vec<u8>> {
        // Stock WAL readers may update volatile SHM reader marks.
        ["", "-wal"]
            .into_iter()
            .map(|suffix| {
                let mut component = path.as_os_str().to_os_string();
                component.push(suffix);
                fs::read(PathBuf::from(component)).unwrap()
            })
            .collect()
    }

    fn selected_source(path: &Path) -> AstrBotSourceBackedSourceV0 {
        let identity = AstrBotSourceIdentityV0::SelectedCore;
        AstrBotSourceBackedSourceV0 {
            path: path.to_path_buf(),
            source_key: source_key(&identity).unwrap(),
            identity,
        }
    }

    fn resolver_for(source: &AstrBotSourceBackedSourceV0) -> AstrBotSourceBackedResolverV0 {
        AstrBotSourceBackedResolverV0 {
            sources: BTreeMap::from([(source.source_key.identity().digest(), source.clone())]),
        }
    }

    fn scan_documents(source: &AstrBotSourceBackedSourceV0) -> Vec<LexicalDocument> {
        let mut documents = Vec::new();
        scan_astrbot_source_backed_v0(source, &mut |document| {
            documents.push(document);
            Ok(())
        })
        .unwrap();
        documents
    }

    fn event_request(document: &LexicalDocument) -> EventHydrationRequest {
        EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
    }

    fn current_source_revision(source: &AstrBotSourceBackedSourceV0) -> [u8; 32] {
        let (source_root, snapshot) = open_root_authorized_snapshot(&source.path).unwrap();
        let digest = {
            let connection = snapshot.connection().unwrap();
            AstrBotSql::new(connection).unwrap();
            let user_version = connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap();
            let schema_fingerprint = sqlite_schema_fingerprint(connection).unwrap();
            revision_digest(
                &source_observation(
                    &source.source_key,
                    snapshot.evidence(),
                    user_version,
                    &schema_fingerprint,
                )
                .unwrap(),
            )
        };
        snapshot.finish().unwrap();
        source_root.revalidate().unwrap();
        digest
    }

    fn request_with_locator_evidence(
        document: &LexicalDocument,
        source_revision: [u8; 32],
        coordinate: NativeRecordCoordinate,
        record_digest: [u8; 32],
    ) -> EventHydrationRequest {
        let locator = SourceRecordLocator::new(
            document.source.clone(),
            coordinate,
            LocatorRevisionPolicy::ExactSourceRevision,
            Some(source_revision),
            record_digest,
        )
        .unwrap();
        EventHydrationRequest::new(document.event_id, locator).unwrap()
    }

    #[test]
    fn astrbot_source_backed_multi_instance_cold_scan_has_stable_ids_and_inventory() {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let cwd = temp.path().join("core");
        create_database(
            &cwd.join("data/data_v4.db"),
            "selected-session",
            "selected-core-prompt",
        );
        let instances = [
            (
                "123e4567-e89b-12d3-a456-426614174000",
                "launcher-one",
                "launcher-one-prompt",
            ),
            (
                "123e4567-e89b-12d3-a456-426614174001",
                "launcher-two",
                "launcher-two-prompt",
            ),
        ];
        for (instance, session, text) in instances {
            create_database(
                &home
                    .join(".astrbot_launcher/instances")
                    .join(instance)
                    .join("core/data/data_v4.db"),
                session,
                text,
            );
        }
        create_database(
            &home.join(".astrbot_launcher/instances/not-a-uuid/core/data/data_v4.db"),
            "ignored-instance",
            "ignored-instance-prompt",
        );

        let discovery = context(&home, &cwd);
        let opening =
            AstrBotSourceBackedInventoryV0::discover(&discovery).expect("opening inventory");
        assert_eq!(opening.sources().len(), 3);
        assert_eq!(
            &opening.sources()[0].identity,
            &AstrBotSourceIdentityV0::SelectedCore
        );
        assert!(opening.sources()[1..].iter().all(|source| matches!(
            &source.identity,
            AstrBotSourceIdentityV0::LauncherInstance(_)
        )));

        let mut first_ids = Vec::new();
        for source in opening.sources() {
            let mut documents = Vec::new();
            let certificate = scan_astrbot_source_backed_v0(source, &mut |document| {
                documents.push(document);
                Ok(())
            })
            .expect("cold source scan");
            assert_eq!(certificate.counts().complete_records, 1);
            assert_eq!(certificate.counts().retained_records, 1);
            assert_eq!(certificate.counts().indexed_documents, 1);
            assert_eq!(documents.len(), 1);
            first_ids.push((
                source.source_key().identity().digest(),
                documents[0].session_id.digest(),
                documents[0].event_id.digest(),
            ));
        }

        let closing =
            AstrBotSourceBackedInventoryV0::discover(&discovery).expect("closing inventory");
        let certified = opening.certify(&closing).expect("certified inventory");
        assert_eq!(certified.observed_sources(), 3);
        let mut second_ids = Vec::new();
        for source in closing.sources() {
            let mut documents = Vec::new();
            scan_astrbot_source_backed_v0(source, &mut |document| {
                documents.push(document);
                Ok(())
            })
            .expect("repeat source scan");
            second_ids.push((
                source.source_key().identity().digest(),
                documents[0].session_id.digest(),
                documents[0].event_id.digest(),
            ));
        }
        assert_eq!(first_ids, second_ids);
    }

    #[test]
    fn astrbot_source_backed_reopens_full_conversation_and_platform_text_exactly() {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let cwd = temp.path().join("core");
        let database = cwd.join("data/data_v4.db");
        let exact_text = format!(
            "astrbot-exact-content-{}-full-body-tail-sentinel",
            "x".repeat(4_096)
        );
        let exact_output = format!(
            "astrbot-tool-output-{}-output-tail-sentinel",
            "o".repeat(4_096)
        );
        let platform_text = format!(
            "astrbot-platform-content-{}-platform-tail-sentinel",
            "p".repeat(4_096)
        );
        create_database(&database, "exact-session", &exact_text);
        let conn = Connection::open(&database).expect("open platform fixture");
        conn.execute(
            "update conversations set content = ?1 where id = 1",
            [json!([
                {
                    "id": "message-exact-session",
                    "role": "user",
                    "content": exact_text,
                },
                {
                    "id": "tool-exact-session",
                    "role": "tool",
                    "content": exact_output,
                    "status": "success",
                }
            ])
            .to_string()],
        )
        .expect("conversation message and output");
        conn.execute(
            "insert into platform_message_history (
                 id, platform_id, user_id, sender_id, sender_name, content,
                 llm_checkpoint_id, created_at
             ) values (7, 'webchat', 'platform-user', 'platform-user', 'User', ?1,
                       null, 1780000002000)",
            [&platform_text],
        )
        .expect("platform message");
        drop(conn);

        let inventory =
            AstrBotSourceBackedInventoryV0::discover(&context(&home, &cwd)).expect("inventory");
        let source = inventory.sources().first().expect("selected source");
        let mut documents = Vec::new();
        scan_astrbot_source_backed_v0(source, &mut |document| {
            documents.push(document);
            Ok(())
        })
        .expect("source scan");
        assert_eq!(documents.len(), 3);

        let conversation = documents
            .iter()
            .find(|document| relation(document) == CONVERSATION_MESSAGE_RELATION)
            .expect("conversation document");
        assert_eq!(conversation.body, exact_text);
        assert!(conversation.body.ends_with("full-body-tail-sentinel"));
        assert_eq!(conversation.parent_session_id, None);
        assert_eq!(conversation.root_session_id, conversation.session_id);
        assert_eq!(
            conversation.provider_session_id.as_deref(),
            Some("exact-session")
        );
        assert_eq!(conversation.branch, None);
        assert_eq!(
            conversation.source_path.as_deref(),
            Some(database.to_string_lossy().as_ref())
        );
        assert_eq!(conversation.agent_type, AgentType::Primary.as_str());
        assert!(conversation.is_primary);
        assert_eq!(
            conversation.locator.revision_policy(),
            LocatorRevisionPolicy::ExactSourceRevision
        );
        assert!(conversation
            .locator
            .certified_source_revision_digest()
            .is_some());
        assert!(matches!(
            conversation.locator.coordinate(),
            NativeRecordCoordinate::ProviderSqlite {
                primary_key: TypedKey::Composite(parts),
                row_version: Some(TypedKey::Bytes(row_digest)),
                ..
            } if matches!(
                parts.as_slice(),
                [TypedKey::I64(1), TypedKey::U64(0)]
            ) && row_digest.len() == 32
        ));
        let resolver = AstrBotSourceBackedResolverV0::from_inventory(&inventory).expect("resolver");
        let request = event_request(conversation);
        let hydrated = resolver
            .hydrate_event(&request)
            .expect("exact conversation hydration");
        assert_eq!(hydrated.provider_bytes, exact_text.as_bytes());

        let output = documents
            .iter()
            .find(|document| relation(document) == CONVERSATION_OUTPUT_RELATION)
            .expect("conversation output document");
        assert_eq!(output.body, exact_output);
        assert!(output.body.ends_with("output-tail-sentinel"));
        let hydrated = resolver
            .hydrate_event(&event_request(output))
            .expect("exact conversation-output hydration");
        assert_eq!(hydrated.provider_bytes, exact_output.as_bytes());

        let platform = documents
            .iter()
            .find(|document| relation(document) == PLATFORM_MESSAGE_RELATION)
            .expect("platform document");
        assert_eq!(platform.body, platform_text);
        assert!(platform.body.ends_with("platform-tail-sentinel"));
        let request = event_request(platform);
        let hydrated = resolver
            .hydrate_event(&request)
            .expect("exact platform-message hydration");
        assert_eq!(hydrated.provider_bytes, platform_text.as_bytes());

        let requested = vec![
            event_request(platform),
            event_request(output),
            event_request(conversation),
        ];
        let batch = BatchHydrationRequest::new(requested.clone()).unwrap();
        let hydrated = resolver.hydrate_batch_request(&batch).unwrap();
        assert_eq!(
            hydrated
                .records()
                .iter()
                .map(|record| record.event_id)
                .collect::<Vec<_>>(),
            requested
                .iter()
                .map(EventHydrationRequest::event_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            hydrated.records()[0].provider_bytes,
            platform_text.as_bytes()
        );
        assert_eq!(
            hydrated.records()[1].provider_bytes,
            exact_output.as_bytes()
        );
        assert_eq!(hydrated.records()[2].provider_bytes, exact_text.as_bytes());
    }

    #[test]
    fn astrbot_hydration_types_stale_source_and_record_digest() {
        let temp = tempdir().unwrap();
        let stale_path = temp.path().join("stale.db");
        create_database(&stale_path, "stale-session", "original AstrBot body");
        let stale_source = selected_source(&stale_path);
        let documents = scan_documents(&stale_source);
        let document = &documents[0];
        Connection::open(&stale_path)
            .unwrap()
            .execute(
                "update conversations set content = ?1 where id = 1",
                [json!([{
                    "id": "message-stale-session",
                    "role": "user",
                    "content": "rewritten AstrBot body with a different length",
                }])
                .to_string()],
            )
            .unwrap();
        let failure = resolver_for(&stale_source)
            .hydrate_event(&event_request(document))
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);

        let digest_path = temp.path().join("digest.db");
        create_database(&digest_path, "digest-session", "digest AstrBot body");
        let digest_source = selected_source(&digest_path);
        let documents = scan_documents(&digest_source);
        let document = &documents[0];
        let NativeRecordCoordinate::ProviderSqlite {
            logical_relation,
            primary_key,
            ..
        } = document.locator.coordinate()
        else {
            panic!("expected provider SQLite locator");
        };
        let coordinate = NativeRecordCoordinate::ProviderSqlite {
            logical_relation: logical_relation.clone(),
            primary_key: primary_key.clone(),
            row_version: Some(TypedKey::bytes(vec![0x6b; 32]).unwrap()),
        };
        let request = request_with_locator_evidence(
            document,
            *document.locator.certified_source_revision_digest().unwrap(),
            coordinate,
            *document.locator.record_digest(),
        );
        let failure = resolver_for(&digest_source)
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);
        let request = request_with_locator_evidence(
            document,
            *document.locator.certified_source_revision_digest().unwrap(),
            document.locator.coordinate().clone(),
            [0xb6; 32],
        );
        let failure = resolver_for(&digest_source)
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);
    }

    #[test]
    fn astrbot_hydration_distinguishes_missing_row_deletion_and_unavailable_root() {
        let temp = tempdir().unwrap();
        let missing_path = temp.path().join("missing-row.db");
        create_database(&missing_path, "missing-session", "missing AstrBot body");
        let missing_source = selected_source(&missing_path);
        let documents = scan_documents(&missing_source);
        Connection::open(&missing_path)
            .unwrap()
            .execute("delete from conversations", [])
            .unwrap();
        let request = request_with_locator_evidence(
            &documents[0],
            current_source_revision(&missing_source),
            documents[0].locator.coordinate().clone(),
            *documents[0].locator.record_digest(),
        );
        let failure = resolver_for(&missing_source)
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::MissingRecord);

        let deleted_path = temp.path().join("deleted.db");
        create_database(&deleted_path, "deleted-session", "deleted AstrBot body");
        let deleted_source = selected_source(&deleted_path);
        let request = event_request(&scan_documents(&deleted_source)[0]);
        fs::remove_file(&deleted_path).unwrap();
        let failure = resolver_for(&deleted_source)
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::ConfirmedDeleted);

        let available_root = temp.path().join("available-root");
        let unavailable_path = available_root.join("data_v4.db");
        create_database(
            &unavailable_path,
            "unavailable-session",
            "unavailable AstrBot body",
        );
        let unavailable_source = selected_source(&unavailable_path);
        let request = event_request(&scan_documents(&unavailable_source)[0]);
        fs::rename(&available_root, temp.path().join("offline-root")).unwrap();
        let failure = resolver_for(&unavailable_source)
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::TemporarilyUnavailable);
    }

    #[test]
    fn astrbot_hydration_types_malformed_rows_schema_and_locator_without_fallbacks() {
        let temp = tempdir().unwrap();
        let malformed_path = temp.path().join("malformed.db");
        create_database(&malformed_path, "malformed-session", "valid AstrBot body");
        let malformed_source = selected_source(&malformed_path);
        let documents = scan_documents(&malformed_source);
        Connection::open(&malformed_path)
            .unwrap()
            .execute(
                "update conversations set content = cast(x'80' as text) where id = 1",
                [],
            )
            .unwrap();
        let request = request_with_locator_evidence(
            &documents[0],
            current_source_revision(&malformed_source),
            documents[0].locator.coordinate().clone(),
            *documents[0].locator.record_digest(),
        );
        let failure = resolver_for(&malformed_source)
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);

        let unsupported_path = temp.path().join("unsupported.db");
        create_database(
            &unsupported_path,
            "unsupported-session",
            "valid AstrBot body",
        );
        let unsupported_source = selected_source(&unsupported_path);
        let request = event_request(&scan_documents(&unsupported_source)[0]);
        Connection::open(&unsupported_path)
            .unwrap()
            .execute_batch("drop table conversations;")
            .unwrap();
        let failure = resolver_for(&unsupported_source)
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(
            failure.kind,
            HydrationFailureKind::UnsupportedParserRevision
        );

        let invalid_path = temp.path().join("invalid.db");
        create_database(&invalid_path, "invalid-session", "valid AstrBot body");
        let invalid_source = selected_source(&invalid_path);
        let documents = scan_documents(&invalid_source);
        let malformed_coordinate = NativeRecordCoordinate::ProviderSqlite {
            logical_relation: CONVERSATION_MESSAGE_RELATION.to_owned(),
            primary_key: TypedKey::I64(1),
            row_version: Some(TypedKey::bytes(vec![0; 32]).unwrap()),
        };
        let request = request_with_locator_evidence(
            &documents[0],
            *documents[0]
                .locator
                .certified_source_revision_digest()
                .unwrap(),
            malformed_coordinate,
            *documents[0].locator.record_digest(),
        );
        let failure = resolver_for(&invalid_source)
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);

        let provider_source = include_str!("source_backed.rs");
        for forbidden in [
            ["work", ".sqlite"].concat(),
            ["ctx_history_", "store"].concat(),
            ["MAX_BODY_", "PREVIEW_CHARS"].concat(),
            ["provider_local_", "preview"].concat(),
        ] {
            assert!(
                !provider_source.contains(&forbidden),
                "AstrBot source-backed path contains forbidden fallback {forbidden}"
            );
        }
        let route_source = include_str!("../../../source_backed.rs");
        let route = route_source
            .split_once("pub fn register_astrbot_source_backed_route")
            .unwrap()
            .1
            .split_once("/// Registers Shelley")
            .unwrap()
            .0;
        assert!(route.contains("with_batch_hydration"));
        assert!(route.contains("AstrBotSourceBackedResolverV0"));
        assert!(!route.contains(&["work", ".sqlite"].concat()));
        assert!(!route.contains(&["ctx_history_", "store"].concat()));
    }
}
