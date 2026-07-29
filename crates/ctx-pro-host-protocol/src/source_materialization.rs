use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ctx_history_core::{
    CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory, EventHydrationRequest,
    SessionHydrationRequest, SourceFrontier, SourceKey, SourceRecordLocator, StableEntityId,
    StableEntityKind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ErrorClass, ProtocolError};

pub const SOURCE_MATERIALIZATION_CONTRACT_VERSION: u16 = 1;
pub const MAX_SOURCE_MANIFEST_SOURCES: usize = 100_000;
pub const MAX_SOURCE_MANIFEST_REMOVALS: usize = 100_000;
pub const MAX_SOURCE_INVENTORY_SOURCES: usize = 100_000;
pub const MAX_SOURCE_PROGRESS_SOURCES: usize = 100_000;
pub const MAX_SOURCE_RECORDS_PER_PAGE: usize = 4_096;
pub const MAX_SOURCE_FACTS_PER_RECORD: usize = 256;
pub const MAX_SOURCE_TOUCHED_FILES_PER_RECORD: usize = 4_096;
pub const MAX_SOURCE_CONTENT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SOURCE_CONTENT_BYTES_PER_PAGE: usize = MAX_SOURCE_CONTENT_BYTES;
pub const MAX_SOURCE_MANIFEST_WIRE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SOURCE_MANIFEST_PAGE_ITEMS: usize = 64;
pub const MAX_SOURCE_MANIFEST_PAGE_WIRE_BYTES: usize = MAX_SOURCE_MANIFEST_WIRE_BYTES;
pub const MAX_SOURCE_CONTROL_WIRE_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_SOURCE_PAGE_WIRE_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_SOURCE_IDENTITY_BYTES: usize = 8 * 1024;
pub const MAX_SOURCE_PATH_BYTES: usize = 64 * 1024;

const MAX_SOURCE_ENCODED_CONTENT_BYTES: usize = MAX_SOURCE_CONTENT_BYTES.div_ceil(3) * 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRemoval {
    pub deletion: CertifiedSourceDeletion,
    pub inventory: CertifiedSourceInventory,
}

impl SourceRemoval {
    pub fn new(
        deletion: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> Result<Self, ProtocolError> {
        let removal = Self {
            deletion,
            inventory,
        };
        removal.validate()?;
        Ok(removal)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.inventory.observed_sources() > MAX_SOURCE_INVENTORY_SOURCES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source inventory witness exceeds its source count bound",
            ));
        }
        self.inventory
            .validate_contract()
            .map_err(|error| invalid_contract("source inventory witness", error))?;
        self.deletion
            .validate_contract()
            .map_err(|error| invalid_contract("source deletion witness", error))?;
        if !self.deletion.verifies(&self.inventory) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source deletion does not verify against its complete inventory witness",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    pub contract_version: u16,
    pub core_generation_id: String,
    pub sources: Vec<CertifiedSource>,
    pub removals: Vec<SourceRemoval>,
}

impl SourceManifest {
    pub fn new(
        core_generation_id: impl Into<String>,
        mut sources: Vec<CertifiedSource>,
        mut removals: Vec<SourceRemoval>,
    ) -> Result<Self, ProtocolError> {
        sources.sort_by_key(source_identity_digest);
        removals.sort_by_key(|removal| removal.deletion.source().identity().digest());
        let manifest = Self {
            contract_version: SOURCE_MATERIALIZATION_CONTRACT_VERSION,
            core_generation_id: core_generation_id.into(),
            sources,
            removals,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.contract_version != SOURCE_MATERIALIZATION_CONTRACT_VERSION {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "source manifest does not match the materialization contract",
            ));
        }
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        if self.sources.len() > MAX_SOURCE_MANIFEST_SOURCES
            || self.removals.len() > MAX_SOURCE_MANIFEST_REMOVALS
            || self.sources.len().saturating_add(self.removals.len()) > MAX_SOURCE_MANIFEST_SOURCES
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest exceeds its source or removal count bound",
            ));
        }
        let mut prior_source = None;
        for source in &self.sources {
            source
                .validate_contract()
                .map_err(|error| invalid_contract("certified source", error))?;
            let current = source_identity_digest(source);
            if prior_source.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source manifest sources must be sorted and unique by stable lineage",
                ));
            }
            prior_source = Some(current);
        }
        let retained = self
            .sources
            .iter()
            .map(source_identity_digest)
            .collect::<BTreeSet<_>>();
        let mut prior_removal = None;
        for removal in &self.removals {
            removal.validate()?;
            let current = removal.deletion.source().identity().digest();
            if retained.contains(&current) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source manifest cannot retain and delete the same stable lineage",
                ));
            }
            if prior_removal.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source manifest removals must be sorted and unique by stable lineage",
                ));
            }
            prior_removal = Some(current);
        }
        Ok(())
    }

    fn validate_legacy_wire(&self) -> Result<(), ProtocolError> {
        validate_encoded_bound(
            self,
            MAX_SOURCE_MANIFEST_WIRE_BYTES,
            "source manifest exceeds its encoded byte bound",
        )
    }
}

/// Exact metadata-only identity admitted before source records are reread.
///
/// `core_generation_id` is the digest of the immutable Core generation
/// manifest. The explicit contract fields fail closed before any page is
/// accepted, while `aggregate_sha256` binds them to every ordered source and
/// removal entry without requiring a whole-manifest wire transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestHeader {
    pub contract_version: u16,
    pub core_generation_id: String,
    pub generation_manifest_version: u32,
    pub identity_version: u16,
    pub lexical_schema_version: u32,
    pub lexical_analyzer_version: u32,
    pub policy_schema_hash: String,
    pub source_count: u32,
    pub removal_count: u32,
    pub aggregate_sha256: String,
}

impl SourceManifestHeader {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core_generation_id: impl Into<String>,
        generation_manifest_version: u32,
        identity_version: u16,
        lexical_schema_version: u32,
        lexical_analyzer_version: u32,
        policy_schema_hash: impl Into<String>,
        sources: &[CertifiedSource],
        removals: &[SourceRemoval],
    ) -> Result<Self, ProtocolError> {
        validate_manifest_entries(sources, removals)?;
        let source_count = u32::try_from(sources.len()).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest source count overflowed",
            )
        })?;
        let removal_count = u32::try_from(removals.len()).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest removal count overflowed",
            )
        })?;
        let mut header = Self {
            contract_version: SOURCE_MATERIALIZATION_CONTRACT_VERSION,
            core_generation_id: core_generation_id.into(),
            generation_manifest_version,
            identity_version,
            lexical_schema_version,
            lexical_analyzer_version,
            policy_schema_hash: policy_schema_hash.into(),
            source_count,
            removal_count,
            aggregate_sha256: String::new(),
        };
        header.aggregate_sha256 = source_manifest_aggregate_sha256(&header, sources, removals)?;
        header.validate()?;
        Ok(header)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.contract_version != SOURCE_MATERIALIZATION_CONTRACT_VERSION {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "source manifest header does not match the materialization contract",
            ));
        }
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_sha256(&self.policy_schema_hash, "source generation policy")?;
        validate_sha256(&self.aggregate_sha256, "source manifest aggregate")?;
        let sources = usize::try_from(self.source_count).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest source count overflowed",
            )
        })?;
        let removals = usize::try_from(self.removal_count).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest removal count overflowed",
            )
        })?;
        if sources > MAX_SOURCE_MANIFEST_SOURCES
            || removals > MAX_SOURCE_MANIFEST_REMOVALS
            || sources.saturating_add(removals) > MAX_SOURCE_MANIFEST_SOURCES
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest header exceeds its source or removal count bound",
            ));
        }
        Ok(())
    }

    pub fn validate_contents(
        &self,
        sources: &[CertifiedSource],
        removals: &[SourceRemoval],
    ) -> Result<(), ProtocolError> {
        self.validate()?;
        validate_manifest_entries(sources, removals)?;
        if usize::try_from(self.source_count).ok() != Some(sources.len())
            || usize::try_from(self.removal_count).ok() != Some(removals.len())
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source manifest aggregate counts do not match admitted entries",
            ));
        }
        if source_manifest_aggregate_sha256(self, sources, removals)? != self.aggregate_sha256 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source manifest aggregate digest does not match admitted entries",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "entries",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SourceManifestPageEntries {
    Sources(Vec<CertifiedSource>),
    Removals(Vec<SourceRemoval>),
}

impl SourceManifestPageEntries {
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Sources(entries) => entries.len(),
            Self::Removals(entries) => entries.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One deterministic metadata-only page in a source-manifest admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestPage {
    pub contract_version: u16,
    pub core_generation_id: String,
    pub aggregate_sha256: String,
    pub page_index: u32,
    pub item_index: u32,
    pub entries: SourceManifestPageEntries,
    pub page_sha256: String,
}

impl SourceManifestPage {
    pub fn new(
        header: &SourceManifestHeader,
        page_index: u32,
        item_index: u32,
        entries: SourceManifestPageEntries,
    ) -> Result<Self, ProtocolError> {
        header.validate()?;
        let mut page = Self {
            contract_version: SOURCE_MATERIALIZATION_CONTRACT_VERSION,
            core_generation_id: header.core_generation_id.clone(),
            aggregate_sha256: header.aggregate_sha256.clone(),
            page_index,
            item_index,
            entries,
            page_sha256: String::new(),
        };
        page.page_sha256 = source_manifest_page_sha256(&page)?;
        page.validate()?;
        Ok(page)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.contract_version != SOURCE_MATERIALIZATION_CONTRACT_VERSION {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "source manifest page does not match the materialization contract",
            ));
        }
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_sha256(&self.aggregate_sha256, "source manifest aggregate")?;
        validate_sha256(&self.page_sha256, "source manifest page")?;
        if self.entries.is_empty() || self.entries.len() > MAX_SOURCE_MANIFEST_PAGE_ITEMS {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest page exceeds its item count bound",
            ));
        }
        match &self.entries {
            SourceManifestPageEntries::Sources(sources) => {
                validate_manifest_entries(sources, &[])?;
            }
            SourceManifestPageEntries::Removals(removals) => {
                validate_manifest_entries(&[], removals)?;
            }
        }
        if source_manifest_page_sha256(self)? != self.page_sha256 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source manifest page digest does not match its entries",
            ));
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_MANIFEST_PAGE_WIRE_BYTES,
            "source manifest page exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestAdmissionCursor {
    pub core_generation_id: String,
    pub aggregate_sha256: String,
    pub next_page_index: u32,
    pub next_source_index: u32,
    pub next_removal_index: u32,
}

impl SourceManifestAdmissionCursor {
    #[must_use]
    pub fn initial(header: &SourceManifestHeader) -> Self {
        Self {
            core_generation_id: header.core_generation_id.clone(),
            aggregate_sha256: header.aggregate_sha256.clone(),
            next_page_index: 0,
            next_source_index: 0,
            next_removal_index: 0,
        }
    }

    pub fn validate_for(&self, header: &SourceManifestHeader) -> Result<(), ProtocolError> {
        header.validate()?;
        if self.core_generation_id != header.core_generation_id
            || self.aggregate_sha256 != header.aggregate_sha256
            || self.next_source_index > header.source_count
            || self.next_removal_index > header.removal_count
            || (self.next_source_index < header.source_count && self.next_removal_index != 0)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source manifest admission cursor is outside its exact manifest",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn is_complete_for(&self, header: &SourceManifestHeader) -> bool {
        self.validate_for(header).is_ok()
            && self.next_source_index == header.source_count
            && self.next_removal_index == header.removal_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestAdmissionReceipt {
    pub header: SourceManifestHeader,
    pub page_count: u32,
}

impl SourceManifestAdmissionReceipt {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.header.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceProgress {
    pub source: SourceKey,
    pub source_epoch: u64,
    pub certified_revision_sha256: String,
    pub frontier: Option<SourceFrontier>,
    pub materializer_revision: String,
    pub terminal: bool,
}

impl SourceProgress {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("source progress identity", error))?;
        if self.source_epoch == 0 {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source progress epoch must be positive",
            ));
        }
        validate_sha256(&self.certified_revision_sha256, "certified source revision")?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        if let Some(frontier) = &self.frontier {
            frontier
                .validate_contract()
                .map_err(|error| invalid_contract("source progress frontier", error))?;
        }
        Ok(())
    }

    #[must_use]
    pub fn exact_eq(&self, other: &Self) -> bool {
        self.source.exact_descriptor_eq(&other.source)
            && self.source_epoch == other.source_epoch
            && self.certified_revision_sha256 == other.certified_revision_sha256
            && self.frontier == other.frontier
            && self.materializer_revision == other.materializer_revision
            && self.terminal == other.terminal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceDisposition {
    NewSource,
    Resume,
    Rewrite,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransientSourceContent(String);

impl TransientSourceContent {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        (bytes.len() <= MAX_SOURCE_CONTENT_BYTES).then(|| Self(STANDARD.encode(bytes)))
    }

    pub fn decode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.0.len() > MAX_SOURCE_ENCODED_CONTENT_BYTES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "transient source content exceeds its encoded byte bound",
            ));
        }
        let decoded = STANDARD.decode(&self.0).map_err(|_| {
            ProtocolError::new(
                ErrorClass::InvalidRequest,
                "transient source content is not canonical base64",
            )
        })?;
        if decoded.len() > MAX_SOURCE_CONTENT_BYTES || STANDARD.encode(&decoded) != self.0 {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "transient source content exceeds its decoded bound or is not canonical base64",
            ));
        }
        Ok(decoded)
    }

    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for TransientSourceContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransientSourceContent")
            .field("encoded_bytes", &self.0.len())
            .field("content", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMessageFact {
    pub content: TransientSourceContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCommandFact {
    pub call_id: Option<String>,
    pub tool_name: Option<String>,
    pub command: TransientSourceContent,
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceOutcome {
    Success,
    Failure,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceResultFact {
    pub call_id: Option<String>,
    pub outcome: SourceOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub content: TransientSourceContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TransientSourceFact {
    Message(SourceMessageFact),
    Command(SourceCommandFact),
    Result(SourceResultFact),
}

impl TransientSourceFact {
    fn validate_and_count_bytes(&self) -> Result<usize, ProtocolError> {
        match self {
            Self::Message(fact) => fact.content.decode().map(|content| content.len()),
            Self::Command(fact) => {
                validate_optional_identity(fact.call_id.as_deref(), "source command call ID")?;
                validate_optional_identity(fact.tool_name.as_deref(), "source command tool name")?;
                validate_optional_path(
                    fact.working_directory.as_deref(),
                    "source command working directory",
                )?;
                fact.command.decode().map(|content| content.len())
            }
            Self::Result(fact) => {
                validate_optional_identity(fact.call_id.as_deref(), "source result call ID")?;
                fact.content.decode().map(|content| content.len())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSessionRelationships {
    pub direct_session_id: StableEntityId,
    pub root_session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub provider_session_id: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRepositoryContext {
    pub repository_id: String,
    pub checkout_id: Option<String>,
    pub worktree_id: Option<String>,
    pub object_format: Option<String>,
}

impl SourceRepositoryContext {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_identity(&self.repository_id, "source repository ID")?;
        validate_optional_identity(self.checkout_id.as_deref(), "source checkout ID")?;
        validate_optional_identity(self.worktree_id.as_deref(), "source worktree ID")?;
        validate_optional_identity(self.object_format.as_deref(), "source object format")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecordMetadata {
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub touched_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub locator: SourceRecordLocator,
    pub relationships: SourceSessionRelationships,
    pub repository: Option<SourceRepositoryContext>,
    pub metadata: SourceRecordMetadata,
    pub facts: Vec<TransientSourceFact>,
}

impl SourceRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_id: StableEntityId,
        session_id: StableEntityId,
        locator: SourceRecordLocator,
        relationships: SourceSessionRelationships,
        repository: Option<SourceRepositoryContext>,
        metadata: SourceRecordMetadata,
        facts: Vec<TransientSourceFact>,
    ) -> Result<Self, ProtocolError> {
        let record = Self {
            event_id,
            session_id,
            locator,
            relationships,
            repository,
            metadata,
            facts,
        };
        record.validate_and_count_bytes()?;
        Ok(record)
    }

    fn validate_and_count_bytes(&self) -> Result<usize, ProtocolError> {
        let event = EventHydrationRequest::new(self.event_id, self.locator.clone())
            .map_err(|error| invalid_contract("source record event locator", error))?;
        SessionHydrationRequest::new(self.session_id, vec![event])
            .map_err(|error| invalid_contract("source record session locator", error))?;
        validate_session_id_for_locator(
            self.relationships.direct_session_id,
            &self.locator,
            "direct session",
        )?;
        if self.relationships.direct_session_id != self.session_id {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source record direct session must equal its session ID",
            ));
        }
        validate_session_id(self.relationships.root_session_id, "root session")?;
        if let Some(parent) = self.relationships.parent_session_id {
            validate_session_id(parent, "parent session")?;
        }
        validate_optional_identity(
            self.relationships.provider_session_id.as_deref(),
            "provider session ID",
        )?;
        validate_optional_identity(self.relationships.agent_id.as_deref(), "source agent ID")?;
        if let Some(repository) = &self.repository {
            repository.validate()?;
        }
        validate_identity(&self.metadata.event_type, "source event type")?;
        validate_optional_identity(self.metadata.role.as_deref(), "source event role")?;
        validate_optional_path(self.metadata.workspace.as_deref(), "source workspace")?;
        validate_optional_path(self.metadata.cwd.as_deref(), "source working directory")?;
        if self.metadata.touched_files.len() > MAX_SOURCE_TOUCHED_FILES_PER_RECORD {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source record exceeds its touched-file count bound",
            ));
        }
        for path in &self.metadata.touched_files {
            validate_path(path, "source touched-file path")?;
        }
        if self.facts.len() > MAX_SOURCE_FACTS_PER_RECORD {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source record exceeds its detector-fact count bound",
            ));
        }
        self.facts.iter().try_fold(0_usize, |total, fact| {
            total
                .checked_add(fact.validate_and_count_bytes()?)
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Bounds,
                        "source record transient-content byte total overflowed",
                    )
                })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginSourceManifestRequest {
    pub manifest: SourceManifest,
}

impl BeginSourceManifestRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.manifest.validate()?;
        self.manifest.validate_legacy_wire()?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "begin source manifest request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestBegan {
    pub core_generation_id: String,
    pub materializer_revision: String,
    pub progress: Vec<SourceProgress>,
    pub replayed: bool,
}

impl SourceManifestBegan {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        validate_progress_set(
            &self.progress,
            None,
            false,
            "source manifest begin progress",
        )?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source manifest begin response exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginSourceManifestAdmissionRequest {
    pub header: SourceManifestHeader,
}

impl BeginSourceManifestAdmissionRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.header.validate()?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "begin source manifest admission request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestAdmissionBegan {
    pub cursor: SourceManifestAdmissionCursor,
    pub replayed: bool,
}

impl SourceManifestAdmissionBegan {
    pub fn validate_for(&self, header: &SourceManifestHeader) -> Result<(), ProtocolError> {
        self.cursor.validate_for(header)?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source manifest admission begin response exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmitSourceManifestPageRequest {
    pub page: SourceManifestPage,
}

impl AdmitSourceManifestPageRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.page.validate()?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_MANIFEST_PAGE_WIRE_BYTES,
            "source manifest page admission request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestPageAdmitted {
    pub cursor: SourceManifestAdmissionCursor,
    pub replayed: bool,
}

impl SourceManifestPageAdmitted {
    pub fn validate_for(&self, header: &SourceManifestHeader) -> Result<(), ProtocolError> {
        self.cursor.validate_for(header)?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source manifest page admission response exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishSourceManifestAdmissionRequest {
    pub header: SourceManifestHeader,
}

impl FinishSourceManifestAdmissionRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.header.validate()?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "finish source manifest admission request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestAdmitted {
    pub receipt: SourceManifestAdmissionReceipt,
    pub materializer_revision: String,
    pub progress: Vec<SourceProgress>,
    pub replayed: bool,
}

impl SourceManifestAdmitted {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.receipt.validate()?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        validate_progress_set(
            &self.progress,
            None,
            false,
            "admitted source manifest progress",
        )?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source manifest admission response exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareSourceRequest {
    pub core_generation_id: String,
    pub source: SourceKey,
    pub certified_revision_sha256: String,
    pub materializer_revision: String,
    pub disposition: SourceDisposition,
    pub expected_prior: Option<SourceProgress>,
}

impl PrepareSourceRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("prepared source identity", error))?;
        validate_sha256(&self.certified_revision_sha256, "certified source revision")?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        if let Some(prior) = &self.expected_prior {
            prior.validate()?;
            if prior.source.identity() != self.source.identity() {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "source preparation prior progress belongs to another lineage",
                ));
            }
        }
        match (self.disposition, self.expected_prior.as_ref()) {
            (SourceDisposition::NewSource, None) => Ok(()),
            (SourceDisposition::Resume, Some(prior))
                if prior.source.exact_descriptor_eq(&self.source)
                    && prior.certified_revision_sha256 == self.certified_revision_sha256
                    && prior.materializer_revision == self.materializer_revision =>
            {
                Ok(())
            }
            (SourceDisposition::Rewrite, Some(prior)) if prior.source_epoch < u64::MAX => Ok(()),
            (SourceDisposition::Rewrite, Some(_)) => Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source rewrite cannot advance an exhausted source epoch",
            )),
            _ => Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source disposition and expected prior progress disagree",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePrepared {
    pub core_generation_id: String,
    pub progress: SourceProgress,
    pub replayed: bool,
}

impl SourcePrepared {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.progress.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializeSourcePageRequest {
    pub core_generation_id: String,
    pub expected_prior: SourceProgress,
    pub next_frontier: Option<SourceFrontier>,
    pub terminal: bool,
    pub records: Vec<SourceRecord>,
}

impl MaterializeSourcePageRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.expected_prior.validate()?;
        if self.expected_prior.terminal {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "terminal source progress cannot accept another materialization page",
            ));
        }
        if let Some(frontier) = &self.next_frontier {
            frontier
                .validate_contract()
                .map_err(|error| invalid_contract("next source frontier", error))?;
        }
        if !self.terminal
            && (self.records.is_empty()
                || self.next_frontier.is_none()
                || self.next_frontier == self.expected_prior.frontier)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "nonterminal source page must contain records and advance its frontier",
            ));
        }
        if self.records.len() > MAX_SOURCE_RECORDS_PER_PAGE {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source page exceeds its record count bound",
            ));
        }
        let mut prior_order = None;
        let mut event_ids = BTreeSet::new();
        let mut content_bytes = 0_usize;
        for record in &self.records {
            if !record
                .locator
                .source()
                .exact_descriptor_eq(&self.expected_prior.source)
            {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source page record belongs to another source descriptor",
                ));
            }
            let current = (record.metadata.event_sequence, record.event_id.digest());
            if prior_order.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "source page records must be in strict stable event order",
                ));
            }
            if !event_ids.insert(record.event_id.digest()) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source page contains a duplicate stable event ID",
                ));
            }
            content_bytes = content_bytes
                .checked_add(record.validate_and_count_bytes()?)
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Bounds,
                        "source page transient-content byte total overflowed",
                    )
                })?;
            if content_bytes > MAX_SOURCE_CONTENT_BYTES_PER_PAGE {
                return Err(ProtocolError::new(
                    ErrorClass::Bounds,
                    "source page exceeds its transient-content byte bound",
                ));
            }
            prior_order = Some(current);
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_PAGE_WIRE_BYTES,
            "source page exceeds its encoded byte bound",
        )
    }

    #[must_use]
    pub fn next_progress(&self) -> SourceProgress {
        SourceProgress {
            source: self.expected_prior.source.clone(),
            source_epoch: self.expected_prior.source_epoch,
            certified_revision_sha256: self.expected_prior.certified_revision_sha256.clone(),
            frontier: self.next_frontier.clone(),
            materializer_revision: self.expected_prior.materializer_revision.clone(),
            terminal: self.terminal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePageMaterialized {
    pub core_generation_id: String,
    pub progress: SourceProgress,
    pub accepted_records: u32,
    pub materialized_facts: u32,
    pub replayed: bool,
}

impl SourcePageMaterialized {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.progress.validate()?;
        if self.accepted_records as usize > MAX_SOURCE_RECORDS_PER_PAGE {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source page acknowledgement exceeds its record count bound",
            ));
        }
        if self.materialized_facts as usize
            > MAX_SOURCE_RECORDS_PER_PAGE.saturating_mul(MAX_SOURCE_FACTS_PER_RECORD)
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source page acknowledgement exceeds its detector-fact count bound",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteSourceRequest {
    pub core_generation_id: String,
    pub removal: SourceRemoval,
    pub expected_prior: SourceProgress,
}

impl DeleteSourceRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.removal.validate()?;
        self.expected_prior.validate()?;
        if !self
            .removal
            .deletion
            .source()
            .exact_descriptor_eq(&self.expected_prior.source)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source deletion witness does not match the expected progress descriptor",
            ));
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "delete source request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDeleted {
    pub core_generation_id: String,
    pub source: SourceKey,
    pub removed_source_epoch: u64,
    pub replayed: bool,
}

impl SourceDeleted {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("deleted source identity", error))?;
        if self.removed_source_epoch == 0 {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "deleted source epoch must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishSourceManifestRequest {
    pub manifest: SourceManifest,
    pub expected_progress: Vec<SourceProgress>,
}

impl FinishSourceManifestRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.manifest.validate()?;
        self.manifest.validate_legacy_wire()?;
        validate_progress_set(
            &self.expected_progress,
            None,
            true,
            "finished source progress",
        )?;
        if self.expected_progress.len() != self.manifest.sources.len() {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "finish source manifest progress does not cover every retained source",
            ));
        }
        let mut materializer_revision = None;
        for (progress, source) in self.expected_progress.iter().zip(&self.manifest.sources) {
            if !progress
                .source
                .exact_descriptor_eq(source.observation().source())
                || progress.certified_revision_sha256 != certified_source_revision_sha256(source)?
                || progress.frontier.as_ref() != source.frontier()
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "finish source manifest progress does not match its certified source",
                ));
            }
            if materializer_revision
                .is_some_and(|revision: &str| revision != progress.materializer_revision)
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "finish source manifest progress mixes materializer revisions",
                ));
            }
            materializer_revision = Some(progress.materializer_revision.as_str());
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "finish source manifest request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishAdmittedSourceManifestRequest {
    pub admission: SourceManifestAdmissionReceipt,
    pub expected_progress: Vec<SourceProgress>,
}

impl FinishAdmittedSourceManifestRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.admission.validate()?;
        validate_progress_set(
            &self.expected_progress,
            None,
            true,
            "finished admitted source progress",
        )?;
        if self.expected_progress.len()
            != usize::try_from(self.admission.header.source_count).map_err(|_| {
                ProtocolError::new(
                    ErrorClass::Bounds,
                    "admitted source manifest source count overflowed",
                )
            })?
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "finish admitted source manifest progress does not cover every retained source",
            ));
        }
        let mut materializer_revision = None;
        for progress in &self.expected_progress {
            if materializer_revision
                .is_some_and(|revision: &str| revision != progress.materializer_revision)
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "finish admitted source manifest progress mixes materializer revisions",
                ));
            }
            materializer_revision = Some(progress.materializer_revision.as_str());
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "finish admitted source manifest request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestReceipt {
    pub core_generation_id: String,
    pub manifest_aggregate_sha256: String,
    pub materializer_revision: String,
    pub progress: Vec<SourceProgress>,
}

impl SourceManifestReceipt {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_sha256(&self.manifest_aggregate_sha256, "source manifest aggregate")?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        validate_progress_set(
            &self.progress,
            Some(&self.materializer_revision),
            true,
            "source manifest receipt progress",
        )?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source manifest receipt exceeds its encoded byte bound",
        )
    }
}

/// Compact, exact identity of one completed source-materialization receipt.
///
/// Status returns the complete receipt so the public host can validate it.
/// Queries carry this identity instead of repeating the receipt's potentially
/// large per-source progress set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestReceiptIdentity {
    pub core_generation_id: String,
    pub materializer_revision: String,
    pub receipt_sha256: String,
}

impl SourceManifestReceiptIdentity {
    pub fn from_receipt(receipt: &SourceManifestReceipt) -> Result<Self, ProtocolError> {
        receipt.validate()?;
        Ok(Self {
            core_generation_id: receipt.core_generation_id.clone(),
            materializer_revision: receipt.materializer_revision.clone(),
            receipt_sha256: source_manifest_receipt_sha256(receipt)?,
        })
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        validate_sha256(&self.receipt_sha256, "source manifest receipt")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestFinished {
    pub receipt: SourceManifestReceipt,
    pub replayed: bool,
}

impl SourceManifestFinished {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.receipt.validate()
    }
}

pub fn certified_source_revision_sha256(source: &CertifiedSource) -> Result<String, ProtocolError> {
    source
        .validate_contract()
        .map_err(|error| invalid_contract("certified source", error))?;
    let bytes = serde_json::to_vec(source).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "certified source revision encoding failed",
        )
    })?;
    Ok(sha256_hex(&bytes))
}

pub fn legacy_source_manifest_sha256(manifest: &SourceManifest) -> Result<String, ProtocolError> {
    manifest.validate()?;
    manifest.validate_legacy_wire()?;
    let bytes = serde_json::to_vec(manifest).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "legacy source manifest encoding failed",
        )
    })?;
    Ok(sha256_hex(&bytes))
}

pub fn source_manifest_receipt_sha256(
    receipt: &SourceManifestReceipt,
) -> Result<String, ProtocolError> {
    receipt.validate()?;
    let bytes = serde_json::to_vec(receipt).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "source manifest receipt encoding failed",
        )
    })?;
    Ok(sha256_hex(&bytes))
}

fn validate_manifest_entries(
    sources: &[CertifiedSource],
    removals: &[SourceRemoval],
) -> Result<(), ProtocolError> {
    if sources.len() > MAX_SOURCE_MANIFEST_SOURCES
        || removals.len() > MAX_SOURCE_MANIFEST_REMOVALS
        || sources.len().saturating_add(removals.len()) > MAX_SOURCE_MANIFEST_SOURCES
    {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "source manifest exceeds its source or removal count bound",
        ));
    }
    let mut prior_source = None;
    for source in sources {
        source
            .validate_contract()
            .map_err(|error| invalid_contract("certified source", error))?;
        let current = source_identity_digest(source);
        if prior_source.is_some_and(|prior| prior >= current) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source manifest sources must be sorted and unique by stable lineage",
            ));
        }
        prior_source = Some(current);
    }
    let retained = sources
        .iter()
        .map(source_identity_digest)
        .collect::<BTreeSet<_>>();
    let mut prior_removal = None;
    for removal in removals {
        removal.validate()?;
        let current = removal.deletion.source().identity().digest();
        if retained.contains(&current) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source manifest cannot retain and delete the same stable lineage",
            ));
        }
        if prior_removal.is_some_and(|prior| prior >= current) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source manifest removals must be sorted and unique by stable lineage",
            ));
        }
        prior_removal = Some(current);
    }
    Ok(())
}

fn source_manifest_aggregate_sha256(
    header: &SourceManifestHeader,
    sources: &[CertifiedSource],
    removals: &[SourceRemoval],
) -> Result<String, ProtocolError> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pro-source-manifest-admission-v1\0");
    digest.update(header.contract_version.to_be_bytes());
    digest_field(&mut digest, header.core_generation_id.as_bytes());
    digest.update(header.generation_manifest_version.to_be_bytes());
    digest.update(header.identity_version.to_be_bytes());
    digest.update(header.lexical_schema_version.to_be_bytes());
    digest.update(header.lexical_analyzer_version.to_be_bytes());
    digest_field(&mut digest, header.policy_schema_hash.as_bytes());
    digest.update(header.source_count.to_be_bytes());
    digest.update(header.removal_count.to_be_bytes());
    for source in sources {
        digest.update(b"s");
        digest_json(&mut digest, source)?;
    }
    for removal in removals {
        digest.update(b"r");
        digest_json(&mut digest, removal)?;
    }
    Ok(hex_digest(digest.finalize()))
}

fn source_manifest_page_sha256(page: &SourceManifestPage) -> Result<String, ProtocolError> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pro-source-manifest-page-v1\0");
    digest.update(page.contract_version.to_be_bytes());
    digest_field(&mut digest, page.core_generation_id.as_bytes());
    digest_field(&mut digest, page.aggregate_sha256.as_bytes());
    digest.update(page.page_index.to_be_bytes());
    digest.update(page.item_index.to_be_bytes());
    digest_json(&mut digest, &page.entries)?;
    Ok(hex_digest(digest.finalize()))
}

fn digest_json<T: Serialize>(digest: &mut Sha256, value: &T) -> Result<(), ProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "source manifest digest encoding failed",
        )
    })?;
    digest_field(digest, &bytes);
    Ok(())
}

fn digest_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn validate_progress_set(
    progress: &[SourceProgress],
    required_materializer_revision: Option<&str>,
    require_terminal: bool,
    name: &str,
) -> Result<(), ProtocolError> {
    if progress.len() > MAX_SOURCE_PROGRESS_SOURCES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{name} exceeds its source count bound"),
        ));
    }
    let mut prior = None;
    for value in progress {
        value.validate()?;
        if require_terminal && !value.terminal {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                format!("{name} contains nonterminal progress"),
            ));
        }
        if required_materializer_revision
            .is_some_and(|revision| revision != value.materializer_revision)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                format!("{name} contains a mismatched materializer revision"),
            ));
        }
        let current = value.source.identity().digest();
        if prior.is_some_and(|prior| prior >= current) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                format!("{name} must be sorted and unique by stable source lineage"),
            ));
        }
        prior = Some(current);
    }
    Ok(())
}

fn validate_session_id(session_id: StableEntityId, name: &str) -> Result<(), ProtocolError> {
    session_id
        .validate_contract()
        .map_err(|error| invalid_contract(name, error))?;
    if session_id.entity_kind() != StableEntityKind::Session {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("source record {name} is not a stable session identity"),
        ));
    }
    Ok(())
}

fn validate_session_id_for_locator(
    session_id: StableEntityId,
    locator: &SourceRecordLocator,
    name: &str,
) -> Result<(), ProtocolError> {
    validate_session_id(session_id, name)?;
    if session_id.source_digest() != locator.source().identity().digest()
        || session_id.source_descriptor_digest() != locator.source().exact_descriptor_digest()
    {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("source record {name} does not belong to its locator source"),
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str, name: &str) -> Result<(), ProtocolError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("{name} must be lowercase SHA-256"),
        ));
    }
    Ok(())
}

fn validate_identity(value: &str, name: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty()
        || value.len() > MAX_SOURCE_IDENTITY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{name} is empty, unsafe, or exceeds its byte bound"),
        ));
    }
    Ok(())
}

fn validate_optional_identity(value: Option<&str>, name: &str) -> Result<(), ProtocolError> {
    value.map_or(Ok(()), |value| validate_identity(value, name))
}

fn validate_path(value: &str, name: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty() || value.len() > MAX_SOURCE_PATH_BYTES || value.contains('\0') {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{name} is empty, unsafe, or exceeds its byte bound"),
        ));
    }
    Ok(())
}

fn validate_optional_path(value: Option<&str>, name: &str) -> Result<(), ProtocolError> {
    value.map_or(Ok(()), |value| validate_path(value, name))
}

fn source_identity_digest(source: &CertifiedSource) -> [u8; 32] {
    source.observation().source().identity().digest()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn invalid_contract(name: &str, error: impl fmt::Display) -> ProtocolError {
    ProtocolError::new(
        ErrorClass::InvalidRequest,
        format!("invalid {name}: {error}"),
    )
}

fn validate_encoded_bound<T: Serialize>(
    value: &T,
    maximum: usize,
    message: &'static str,
) -> Result<(), ProtocolError> {
    let mut counter = SerializedByteCounter { bytes: 0 };
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| ProtocolError::new(ErrorClass::Internal, "encoded-size validation failed"))?;
    if counter.bytes > maximum {
        return Err(ProtocolError::new(ErrorClass::Bounds, message));
    }
    Ok(())
}

struct SerializedByteCounter {
    bytes: usize,
}

impl Write for SerializedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "source_materialization/tests.rs"]
mod tests;
