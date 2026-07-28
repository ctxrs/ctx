//! Provider-local source-backed projection for AstrBot `data_v4.db`.
//!
//! This module deliberately stops at the provider seam. Shared generation
//! lifecycle and registry wiring are owned by the source-backed assembler.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceInventory,
    ContentSourceResolver, EventHydrationRequest, EventIdentityInput, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionHydrationRequest, SessionIdentityInput, SourceAnchor, SourceInventoryObservation,
    SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError, StableEntityId,
    TypedKey,
};
use ctx_history_index::{IndexError, LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    discover_provider_sources_for_provider_with_context,
    provider::sqlite::{open_provider_sqlite_readonly, sqlite_schema_fingerprint},
    DiscoveryContext, ProviderSourceStatus,
};

use super::*;

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

#[derive(Debug, Error)]
pub(super) enum AstrBotSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
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
    #[error("AstrBot source-backed sink rejected a document")]
    SinkRejected,
}

pub(super) type AstrBotSourceBackedResultV0<T> = std::result::Result<T, AstrBotSourceBackedErrorV0>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum AstrBotSourceIdentityV0 {
    SelectedCore,
    LauncherInstance(String),
}

#[derive(Debug, Clone)]
pub(super) struct AstrBotSourceBackedSourceV0 {
    path: PathBuf,
    identity: AstrBotSourceIdentityV0,
    source_key: SourceKey,
}

impl AstrBotSourceBackedSourceV0 {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn identity(&self) -> &AstrBotSourceIdentityV0 {
        &self.identity
    }

    pub(super) fn source_key(&self) -> &SourceKey {
        &self.source_key
    }
}

#[derive(Debug, Clone)]
pub(super) struct AstrBotSourceBackedInventoryV0 {
    observation: SourceInventoryObservation,
    sources: Vec<AstrBotSourceBackedSourceV0>,
}

impl AstrBotSourceBackedInventoryV0 {
    pub(super) fn discover(context: &DiscoveryContext) -> AstrBotSourceBackedResultV0<Self> {
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

    pub(super) fn sources(&self) -> &[AstrBotSourceBackedSourceV0] {
        &self.sources
    }

    pub(super) fn certify(
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

pub(super) trait AstrBotSourceBackedSinkV0 {
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

pub(super) fn scan_astrbot_source_backed_v0(
    source: &AstrBotSourceBackedSourceV0,
    sink: &mut impl AstrBotSourceBackedSinkV0,
) -> AstrBotSourceBackedResultV0<CertifiedSource> {
    let opening_snapshot = astrbot_source_snapshot(&source.path)?;
    let conn = open_provider_sqlite_readonly(&source.path)?;
    let sql = AstrBotSql::new(&conn)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let opening = source_observation(
        &source.source_key,
        &opening_snapshot,
        user_version,
        &schema_fingerprint,
    )?;
    let source_revision_digest = revision_digest(&opening);
    let revision_scope = TypedKey::bytes(source_revision_digest.to_vec())?;
    let mut counts = ScannedSourceCounts::default();
    let mut content_chain = [0_u8; 32];
    let mut native_ordinal = 0_u64;
    let mut conversation_after = None;

    loop {
        let Some(candidate) = fetch_candidate(
            &conn,
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
            hydrate_conversation(&conn, &sql.conversation_hydration, candidate.physical_rowid)?;
        let row_digest =
            logical_values_digest(&super::super::model::conversation_values(row.clone()));
        content_chain = chain_hash(content_chain, row_digest);
        let (items, content_is_array) = conversation_items(&row.content);
        let item_count = items.len().max(1);
        let values = super::super::model::conversation_values(row.clone());
        for item_index in 0..item_count {
            add_complete(&mut counts)?;
            let item = items.get(item_index);
            let (event, _, rejection) = conversation_event(
                &row,
                candidate.physical_rowid,
                item_index,
                item,
                content_is_array,
                native_ordinal,
                false,
            )?;
            if rejection.is_some() {
                add_rejected(&mut counts)?;
            } else if let Some(event) = event {
                let complete_text = if event.event_type == EventType::Message {
                    super::super::astrbot_complete_conversation_message(
                        &values,
                        u32::try_from(item_index)
                            .map_err(|_| AstrBotSourceBackedErrorV0::CountOverflow)?,
                    )?
                    .ok_or(AstrBotSourceBackedErrorV0::ExactConversationMismatch)?
                    .text
                } else {
                    item.and_then(item_text)
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
                sink.emit(document)?;
                add_retained(&mut counts)?;
            } else {
                add_ignored(&mut counts)?;
            }
            native_ordinal = native_ordinal
                .checked_add(1)
                .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
        }
    }

    prepare_relationship_projection(&conn, &sql)?;
    if let (Some(initial), Some(after)) = (
        sql.platform_message_candidate_initial.as_deref(),
        sql.platform_message_candidate_after.as_deref(),
    ) {
        let mut platform_after = None;
        let platform_reader =
            AstrBotReader::new(&conn, AstrBotSql::new(&conn)?, AstrBotFrontier::initial());
        loop {
            let Some(candidate) = fetch_candidate(&conn, initial, after, platform_after)? else {
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
                let (unit, rejection, row_digest) =
                    platform_reader.platform_unit(candidate, native_ordinal)?;
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
                        )?;
                        sink.emit(document)?;
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

    let closing_snapshot = astrbot_source_snapshot(&source.path)?;
    let closing = source_observation(
        &source.source_key,
        &closing_snapshot,
        user_version,
        &schema_fingerprint,
    )?;
    let mut digest = Sha256::new();
    digest.update(b"ctx-astrbot-source-backed-content-v0\0");
    digest.update(content_chain);
    digest.update(counts.complete_records.to_be_bytes());
    digest.update(counts.certified_bytes.to_be_bytes());
    Ok(CertifiedSource::certify(
        opening,
        closing,
        PARSER_REVISION,
        digest.finalize().into(),
        counts,
    )?)
}

#[derive(Debug, Clone)]
pub(super) struct AstrBotSourceBackedResolverV0 {
    sources: BTreeMap<[u8; 32], AstrBotSourceBackedSourceV0>,
}

impl AstrBotSourceBackedResolverV0 {
    pub(super) fn from_inventory(
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

    fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let mut coordinates = Vec::with_capacity(requests.len());
        for request in requests {
            coordinates.push(decode_conversation_locator(request.locator())?);
        }
        let source_key = requests[0].locator().source();
        let source = self
            .sources
            .get(&source_key.identity().digest())
            .filter(|source| source.source_key.exact_descriptor_eq(source_key))
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::TemporarilyUnavailable,
                    "AstrBot source is not present in the admitted inventory",
                )
            })?;
        if requests
            .iter()
            .any(|request| !request.locator().source().exact_descriptor_eq(source_key))
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot hydration batch spans multiple source descriptors",
            ));
        }

        let snapshot = astrbot_source_snapshot(&source.path).map_err(map_capture_hydration)?;
        let conn = open_provider_sqlite_readonly(&source.path).map_err(map_capture_hydration)?;
        let user_version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(CaptureError::from)
            .map_err(map_capture_hydration)?;
        let schema_fingerprint = sqlite_schema_fingerprint(&conn).map_err(map_capture_hydration)?;
        let observation = source_observation(
            &source.source_key,
            &snapshot,
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

        let mut hydrated = Vec::with_capacity(requests.len());
        for (request, coordinate) in requests.iter().zip(coordinates) {
            let values = super::super::astrbot_complete_conversation_values(
                &conn,
                coordinate.physical_rowid,
            )
            .map_err(map_capture_hydration)?
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    "AstrBot conversation row is missing",
                )
            })?;
            if logical_values_digest(&values) != coordinate.row_digest {
                return Err(hydration_failure(
                    HydrationFailureKind::StaleRecordEvidence,
                    "AstrBot conversation row version no longer matches",
                ));
            }
            let message =
                super::super::astrbot_complete_conversation_message(&values, coordinate.item_index)
                    .map_err(map_capture_hydration)?
                    .ok_or_else(|| {
                        hydration_failure(
                            HydrationFailureKind::MissingRecord,
                            "AstrBot conversation message is missing",
                        )
                    })?;
            let provider_bytes = message.text.into_bytes();
            if Sha256::digest(&provider_bytes).as_slice() != request.locator().record_digest() {
                return Err(hydration_failure(
                    HydrationFailureKind::StaleRecordEvidence,
                    "AstrBot conversation message digest no longer matches",
                ));
            }
            hydrated.push(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes,
            });
        }
        if !snapshot
            .revalidate(&source.path)
            .map_err(map_capture_hydration)?
        {
            return Err(hydration_failure(
                HydrationFailureKind::StaleSourceEvidence,
                "AstrBot SQLite snapshot changed during reopening",
            ));
        }
        Ok(hydrated)
    }
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

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_requests(request.events())
    }
}

#[derive(Debug, Clone, Copy)]
struct ConversationCoordinate {
    physical_rowid: i64,
    item_index: u32,
    row_digest: [u8; 32],
}

fn decode_conversation_locator(
    locator: &SourceRecordLocator,
) -> std::result::Result<ConversationCoordinate, HydrationFailure> {
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
    if matches!(
        logical_relation.as_str(),
        PLATFORM_MESSAGE_RELATION | CONVERSATION_OUTPUT_RELATION
    ) {
        return Err(hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            if logical_relation == PLATFORM_MESSAGE_RELATION {
                "AstrBot platform-message rows have no verified exact-content resolver"
            } else {
                "AstrBot conversation output rows have no verified exact-content resolver"
            },
        ));
    }
    if logical_relation != CONVERSATION_MESSAGE_RELATION {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot logical relation is unsupported",
        ));
    }
    let TypedKey::Composite(parts) = primary_key else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot conversation key is not composite",
        ));
    };
    let [TypedKey::I64(physical_rowid), TypedKey::U64(item_index)] = parts.as_slice() else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot conversation key has an invalid shape",
        ));
    };
    let Some(TypedKey::Bytes(row_digest)) = row_version else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot conversation locator has no row version",
        ));
    };
    let row_digest: [u8; 32] = row_digest.as_slice().try_into().map_err(|_| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot conversation row version has an invalid length",
        )
    })?;
    Ok(ConversationCoordinate {
        physical_rowid: *physical_rowid,
        item_index: u32::try_from(*item_index).map_err(|_| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot conversation item index exceeds u32",
            )
        })?,
        row_digest,
    })
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
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_fingerprint: &str,
) -> AstrBotSourceBackedResultV0<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        SOURCE_REVISION_KIND,
        astrbot_source_revision(snapshot, user_version, schema_fingerprint).into_bytes(),
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
    let native_item_key = if let Some(native_id) = item.and_then(item_id) {
        NativeItemKey::composite(
            "astrbot.conversation-item",
            vec![TypedKey::I64(physical_rowid), TypedKey::utf8(native_id)?],
        )?
    } else {
        NativeItemKey::revision_scoped_position(
            "astrbot.conversation-position",
            TypedKey::composite(vec![
                TypedKey::I64(physical_rowid),
                TypedKey::U64(
                    u64::try_from(item_index)
                        .map_err(|_| AstrBotSourceBackedErrorV0::CountOverflow)?,
                ),
            ])?,
            revision_scope.clone(),
        )?
    };
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
        body: provider_local_preview(complete_text, MAX_BODY_PREVIEW_CHARS).0,
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
        row_digest,
    )?;
    let text = event
        .payload
        .get("text")
        .and_then(Value::as_str)
        .ok_or(AstrBotSourceBackedErrorV0::ExactConversationMismatch)?;
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
        body: provider_local_preview(text, MAX_BODY_PREVIEW_CHARS).0,
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
        CaptureError::Io(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "AstrBot source is missing",
            )
        }
        CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => hydration_failure(
            HydrationFailureKind::MissingRecord,
            "AstrBot source record is missing",
        ),
        _ => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            "AstrBot source could not be reopened",
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
            opening.sources()[0].identity(),
            &AstrBotSourceIdentityV0::SelectedCore
        );
        assert!(opening.sources()[1..].iter().all(|source| matches!(
            source.identity(),
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
    fn astrbot_source_backed_reopens_conversation_exactly_and_typed_fails_platform_rows() {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let cwd = temp.path().join("core");
        let database = cwd.join("data/data_v4.db");
        let exact_text = format!(
            "astrbot-exact-content-{}",
            "x".repeat(MAX_BODY_PREVIEW_CHARS + 64)
        );
        create_database(&database, "exact-session", &exact_text);
        let conn = Connection::open(&database).expect("open platform fixture");
        conn.execute(
            "insert into platform_message_history (
                 id, platform_id, user_id, sender_id, sender_name, content,
                 llm_checkpoint_id, created_at
             ) values (7, 'webchat', 'platform-user', 'platform-user', 'User',
                       'platform-searchable-content', null, 1780000002000)",
            [],
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
        assert_eq!(documents.len(), 2);

        let conversation = documents
            .iter()
            .find(|document| relation(document) == CONVERSATION_MESSAGE_RELATION)
            .expect("conversation document");
        assert_eq!(conversation.body.chars().count(), MAX_BODY_PREVIEW_CHARS);
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
        let resolver = AstrBotSourceBackedResolverV0::from_inventory(&inventory).expect("resolver");
        let request =
            EventHydrationRequest::new(conversation.event_id, conversation.locator.clone())
                .expect("conversation request");
        let hydrated = resolver
            .hydrate_event(&request)
            .expect("exact conversation hydration");
        assert_eq!(hydrated.provider_bytes, exact_text.as_bytes());

        let platform = documents
            .iter()
            .find(|document| relation(document) == PLATFORM_MESSAGE_RELATION)
            .expect("platform document");
        assert!(platform.body.contains("platform-searchable-content"));
        let request = EventHydrationRequest::new(platform.event_id, platform.locator.clone())
            .expect("platform request");
        let failure = resolver
            .hydrate_event(&request)
            .expect_err("platform exact reopening must fail");
        assert_eq!(
            failure.kind,
            HydrationFailureKind::UnsupportedParserRevision
        );
        assert!(failure
            .detail
            .contains("no verified exact-content resolver"));
    }
}
