//! Shared contracts for source-backed history projections.
//!
//! Provider adapters own discovery and native parsing. They do not own
//! projection identity, source certification, or publication state machines.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub const IDENTITY_VERSION: u16 = 1;

const MAX_PROVIDER_BYTES: usize = 128;
const MAX_SOURCE_FORMAT_BYTES: usize = 256;
const MAX_SCHEMA_VARIANT_BYTES: usize = 256;
const MAX_KEY_NAMESPACE_BYTES: usize = 256;
const MAX_TYPED_KEY_BYTES: usize = 64 * 1024;
const MAX_TYPED_KEY_COMPONENTS: usize = 256;
const MAX_LOGICAL_KIND_BYTES: usize = 256;
const MAX_LOCATOR_KIND_BYTES: usize = 256;
const MAX_LOCATOR_BYTES: usize = 64 * 1024;
const MAX_REVISION_KIND_BYTES: usize = 256;
const MAX_REVISION_BYTES: usize = 4 * 1024;
const MAX_PARSER_REVISION_BYTES: usize = 256;

const IDENTITY_DOMAIN: &[u8] = b"ctx.identity\0";
const ENTITY_SOURCE: u8 = 1;
const ENTITY_SESSION: u8 = 2;
const ENTITY_ITEM: u8 = 3;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProjectionContractError {
    #[error("{field} is empty")]
    EmptyField { field: &'static str },
    #[error("{field} is too large: {actual} bytes, maximum {maximum}")]
    FieldTooLarge {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("typed identity key has too many components: {actual}, maximum {maximum}")]
    TooManyKeyComponents { actual: usize, maximum: usize },
    #[error("source certification compared different sources")]
    SourceChanged,
    #[error("source descriptor changed within one scan or identity binding")]
    SourceDescriptorChanged,
    #[error("source revision changed while it was being scanned")]
    SourceRevisionChanged,
    #[error("source inventory authority changed while it was being scanned")]
    InventoryAuthorityChanged,
    #[error("source inventory revision changed while it was being scanned")]
    InventoryRevisionChanged,
    #[error("source inventory provider does not own the deleted source")]
    InventoryProviderMismatch,
    #[error("authoritative inventory still contains the source proposed for deletion")]
    InventoryContainsDeletedSource,
    #[error("authoritative inventory contains a duplicate source identity")]
    DuplicateInventorySource,
    #[error("scanned source counts do not reconcile")]
    CountMismatch,
    #[error("source frontier does not reconcile with the certified source")]
    FrontierMismatch,
    #[error("append proof does not match the committed source prefix")]
    AppendPrefixMismatch,
    #[error("append candidate regressed committed source counts")]
    AppendCountRegression,
    #[error("append candidate changed parser revision")]
    AppendParserChanged,
    #[error("revision-scoped positional identity requires an explicit revision scope")]
    RevisionScopeRequired,
    #[error("revision scope is only valid for revision-scoped positional identity")]
    UnexpectedRevisionScope,
    #[error("identity kind mismatch: expected {expected:?}, actual {actual:?}")]
    EntityKindMismatch {
        expected: StableEntityKind,
        actual: StableEntityKind,
    },
    #[error("serialized or supplied derived identity is invalid")]
    InvalidDerivedIdentity,
}

pub type ProjectionContractResult<T> = Result<T, ProjectionContractError>;

/// Exact provider-native key material.
///
/// Parsing determines the storage type. Identity encoding does not trim,
/// normalize, case-fold, stringify, or otherwise reinterpret values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypedKey {
    Null,
    Bytes(Vec<u8>),
    Utf8(String),
    I64(i64),
    U64(u64),
    F64Bits(u64),
    Bool(bool),
    Composite(Vec<TypedKey>),
}

impl TypedKey {
    pub fn bytes(value: Vec<u8>) -> ProjectionContractResult<Self> {
        validate_bytes("typed_key_bytes", &value, MAX_TYPED_KEY_BYTES)?;
        Ok(Self::Bytes(value))
    }

    pub fn utf8(value: impl Into<String>) -> ProjectionContractResult<Self> {
        let value = value.into();
        validate_text("typed_key_utf8", &value, MAX_TYPED_KEY_BYTES)?;
        Ok(Self::Utf8(value))
    }

    pub fn composite(values: Vec<Self>) -> ProjectionContractResult<Self> {
        if values.len() > MAX_TYPED_KEY_COMPONENTS {
            return Err(ProjectionContractError::TooManyKeyComponents {
                actual: values.len(),
                maximum: MAX_TYPED_KEY_COMPONENTS,
            });
        }
        let mut encoded = Vec::new();
        encode_typed_key(&mut encoded, &Self::Composite(values.clone()))?;
        validate_bytes("typed_composite_key", &encoded, MAX_TYPED_KEY_BYTES)?;
        Ok(Self::Composite(values))
    }

    pub fn from_f64(value: f64) -> Self {
        Self::F64Bits(value.to_bits())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceAnchor {
    ProviderNative { namespace: String, key: TypedKey },
    CatalogLineage([u8; 32]),
}

impl SourceAnchor {
    pub fn provider_native(
        namespace: impl Into<String>,
        key: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let namespace = namespace.into();
        validate_text(
            "source_anchor_namespace",
            &namespace,
            MAX_KEY_NAMESPACE_BYTES,
        )?;
        Ok(Self::ProviderNative { namespace, key })
    }
}

/// Persistent canonical source lineage.
///
/// Physical paths and mutable source fingerprints are not identity inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceKey {
    provider: String,
    source_format: String,
    schema_variant: String,
    provider_identity_version: u32,
    anchor: SourceAnchor,
    identity: StableEntityId,
}

impl SourceKey {
    pub fn derive(
        provider: impl Into<String>,
        source_format: impl Into<String>,
        schema_variant: impl Into<String>,
        provider_identity_version: u32,
        anchor: SourceAnchor,
    ) -> ProjectionContractResult<Self> {
        let provider = provider.into();
        let source_format = source_format.into();
        let schema_variant = schema_variant.into();
        validate_text("provider", &provider, MAX_PROVIDER_BYTES)?;
        validate_text("source_format", &source_format, MAX_SOURCE_FORMAT_BYTES)?;
        validate_text("schema_variant", &schema_variant, MAX_SCHEMA_VARIANT_BYTES)?;
        let identity = derive_source_identity(&provider, &anchor)?;
        Ok(Self {
            provider,
            source_format,
            schema_variant,
            provider_identity_version,
            anchor,
            identity,
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn source_format(&self) -> &str {
        &self.source_format
    }

    pub fn schema_variant(&self) -> &str {
        &self.schema_variant
    }

    pub fn provider_identity_version(&self) -> u32 {
        self.provider_identity_version
    }

    pub fn anchor(&self) -> &SourceAnchor {
        &self.anchor
    }

    pub fn identity(&self) -> StableEntityId {
        self.identity
    }

    /// Compares the complete provider descriptor, not only canonical lineage.
    ///
    /// `PartialEq`, `Eq`, and `Hash` intentionally compare lineage identity so
    /// replacement generations can change parser classification without
    /// creating a new source. Same-scan and append checks must use this method
    /// instead.
    pub fn exact_descriptor_eq(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.source_format == other.source_format
            && self.schema_variant == other.schema_variant
            && self.provider_identity_version == other.provider_identity_version
            && self.anchor == other.anchor
            && self.identity == other.identity
    }

    /// Requires both values to describe the exact same source generation
    /// contract while preserving a distinct error for different lineages.
    pub fn validate_exact_descriptor(&self, other: &Self) -> ProjectionContractResult<()> {
        if self.identity != other.identity {
            return Err(ProjectionContractError::SourceChanged);
        }
        if !self.exact_descriptor_eq(other) {
            return Err(ProjectionContractError::SourceDescriptorChanged);
        }
        Ok(())
    }

    pub fn exact_descriptor_digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"ctx.source-descriptor\0");
        digest.update(IDENTITY_VERSION.to_be_bytes());
        digest.update((self.provider.len() as u64).to_be_bytes());
        digest.update(self.provider.as_bytes());
        digest.update((self.source_format.len() as u64).to_be_bytes());
        digest.update(self.source_format.as_bytes());
        digest.update((self.schema_variant.len() as u64).to_be_bytes());
        digest.update(self.schema_variant.as_bytes());
        digest.update(self.provider_identity_version.to_be_bytes());
        digest.update(self.identity.digest);
        digest.finalize().into()
    }

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        validate_text("provider", &self.provider, MAX_PROVIDER_BYTES)?;
        validate_text(
            "source_format",
            &self.source_format,
            MAX_SOURCE_FORMAT_BYTES,
        )?;
        validate_text(
            "schema_variant",
            &self.schema_variant,
            MAX_SCHEMA_VARIANT_BYTES,
        )?;
        validate_source_anchor(&self.anchor)?;
        self.identity.validate_contract()?;
        if derive_source_identity(&self.provider, &self.anchor)? != self.identity {
            return Err(ProjectionContractError::InvalidDerivedIdentity);
        }
        Ok(())
    }
}

impl Ord for SourceKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.identity.digest.cmp(&other.identity.digest)
    }
}

impl PartialEq for SourceKey {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

impl Eq for SourceKey {}

impl Hash for SourceKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity.hash(state);
    }
}

impl PartialOrd for SourceKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Versioned source state observed at one instant.
///
/// The opaque revision is provider/storage specific but content-free. For a
/// regular file it should bind open-file identity, size, modification time,
/// change time, and available replacement markers. Provider databases should
/// use a read-snapshot identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceObservation {
    source: SourceKey,
    revision_kind: String,
    revision: Vec<u8>,
}

impl SourceObservation {
    pub fn new(
        source: SourceKey,
        revision_kind: impl Into<String>,
        revision: Vec<u8>,
    ) -> ProjectionContractResult<Self> {
        let observation = Self {
            source,
            revision_kind: revision_kind.into(),
            revision,
        };
        validate_text(
            "source_revision_kind",
            &observation.revision_kind,
            MAX_REVISION_KIND_BYTES,
        )?;
        validate_nonempty_bytes("source_revision", &observation.revision, MAX_REVISION_BYTES)?;
        Ok(observation)
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn revision_kind(&self) -> &str {
        &self.revision_kind
    }

    pub fn revision(&self) -> &[u8] {
        &self.revision
    }
}

/// One observation of an authoritative provider inventory.
///
/// An inventory may be a provider root, database catalog, or plugin source
/// registry. Its digest binds the complete discovered source set; it is not a
/// sampled directory mtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInventoryObservation {
    provider: String,
    authority_namespace: String,
    authority_key: TypedKey,
    revision_kind: String,
    revision: Vec<u8>,
}

impl SourceInventoryObservation {
    pub fn new(
        provider: impl Into<String>,
        authority_namespace: impl Into<String>,
        authority_key: TypedKey,
        revision_kind: impl Into<String>,
        revision: Vec<u8>,
    ) -> ProjectionContractResult<Self> {
        let observation = Self {
            provider: provider.into(),
            authority_namespace: authority_namespace.into(),
            authority_key,
            revision_kind: revision_kind.into(),
            revision,
        };
        validate_text(
            "inventory_provider",
            &observation.provider,
            MAX_PROVIDER_BYTES,
        )?;
        validate_text(
            "inventory_authority_namespace",
            &observation.authority_namespace,
            MAX_KEY_NAMESPACE_BYTES,
        )?;
        validate_text(
            "inventory_revision_kind",
            &observation.revision_kind,
            MAX_REVISION_KIND_BYTES,
        )?;
        validate_nonempty_bytes(
            "inventory_revision",
            &observation.revision,
            MAX_REVISION_BYTES,
        )?;
        Ok(observation)
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn authority_namespace(&self) -> &str {
        &self.authority_namespace
    }

    pub fn authority_key(&self) -> &TypedKey {
        &self.authority_key
    }

    pub fn revision_kind(&self) -> &str {
        &self.revision_kind
    }

    pub fn revision(&self) -> &[u8] {
        &self.revision
    }
}

/// A complete, internally digested provider inventory observed unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertifiedSourceInventory {
    observation: SourceInventoryObservation,
    discovery_revision: String,
    source_digests: Vec<[u8; 32]>,
    inventory_digest: [u8; 32],
}

impl CertifiedSourceInventory {
    pub fn certify(
        opening: SourceInventoryObservation,
        closing: SourceInventoryObservation,
        discovery_revision: impl Into<String>,
        mut sources: Vec<SourceKey>,
    ) -> ProjectionContractResult<Self> {
        if opening.provider != closing.provider
            || opening.authority_namespace != closing.authority_namespace
            || opening.authority_key != closing.authority_key
        {
            return Err(ProjectionContractError::InventoryAuthorityChanged);
        }
        if opening.revision_kind != closing.revision_kind || opening.revision != closing.revision {
            return Err(ProjectionContractError::InventoryRevisionChanged);
        }
        let discovery_revision = discovery_revision.into();
        validate_text(
            "discovery_revision",
            &discovery_revision,
            MAX_PARSER_REVISION_BYTES,
        )?;
        if sources
            .iter()
            .any(|source| source.provider != opening.provider)
        {
            return Err(ProjectionContractError::InventoryProviderMismatch);
        }
        sources.sort();
        let source_digests = sources
            .iter()
            .map(|source| source.identity.digest)
            .collect::<Vec<_>>();
        if source_digests.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ProjectionContractError::DuplicateInventorySource);
        }
        let mut digest = Sha256::new();
        digest.update(b"ctx.source-inventory\0");
        digest.update((source_digests.len() as u64).to_be_bytes());
        for source_digest in &source_digests {
            digest.update(source_digest);
        }
        Ok(Self {
            observation: opening,
            discovery_revision,
            source_digests,
            inventory_digest: digest.finalize().into(),
        })
    }

    pub fn observation(&self) -> &SourceInventoryObservation {
        &self.observation
    }

    pub fn discovery_revision(&self) -> &str {
        &self.discovery_revision
    }

    pub fn inventory_digest(&self) -> &[u8; 32] {
        &self.inventory_digest
    }

    pub fn observed_sources(&self) -> usize {
        self.source_digests.len()
    }

    pub fn contains(&self, source: &SourceKey) -> bool {
        self.source_digests
            .binary_search(&source.identity.digest)
            .is_ok()
    }
}

/// Proof that a complete authoritative inventory omitted one exact source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertifiedSourceDeletion {
    source: SourceKey,
    inventory: SourceInventoryObservation,
    discovery_revision: String,
    inventory_digest: [u8; 32],
    observed_sources: u64,
}

impl CertifiedSourceDeletion {
    pub fn from_inventory(
        source: SourceKey,
        inventory: &CertifiedSourceInventory,
    ) -> ProjectionContractResult<Self> {
        if inventory.observation.provider != source.provider {
            return Err(ProjectionContractError::InventoryProviderMismatch);
        }
        if inventory.contains(&source) {
            return Err(ProjectionContractError::InventoryContainsDeletedSource);
        }
        Ok(Self {
            source,
            inventory: inventory.observation.clone(),
            discovery_revision: inventory.discovery_revision.clone(),
            inventory_digest: inventory.inventory_digest,
            observed_sources: inventory.observed_sources() as u64,
        })
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn inventory(&self) -> &SourceInventoryObservation {
        &self.inventory
    }

    pub fn discovery_revision(&self) -> &str {
        &self.discovery_revision
    }

    pub fn inventory_digest(&self) -> &[u8; 32] {
        &self.inventory_digest
    }

    pub fn observed_sources(&self) -> u64 {
        self.observed_sources
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedSourceCounts {
    pub complete_records: u64,
    pub retained_records: u64,
    pub rejected_records: u64,
    pub ignored_records: u64,
    pub indexed_documents: u64,
    pub certified_bytes: u64,
}

impl ScannedSourceCounts {
    fn validate(self) -> ProjectionContractResult<()> {
        let classified = self
            .retained_records
            .checked_add(self.rejected_records)
            .and_then(|value| value.checked_add(self.ignored_records))
            .ok_or(ProjectionContractError::CountMismatch)?;
        if classified != self.complete_records || self.indexed_documents > self.retained_records {
            return Err(ProjectionContractError::CountMismatch);
        }
        Ok(())
    }
}

/// A scan that observed one unchanged provider snapshot from open to close.
///
/// `content_digest` binds exactly the first `counts.certified_bytes` bytes (or
/// the provider-equivalent canonical snapshot bytes). It is computed during a
/// required parser/hash pass and is not used as the source key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertifiedSource {
    observation: SourceObservation,
    parser_revision: String,
    content_digest: [u8; 32],
    counts: ScannedSourceCounts,
    frontier: Option<SourceFrontier>,
}

impl CertifiedSource {
    pub fn certify(
        opening: SourceObservation,
        closing: SourceObservation,
        parser_revision: impl Into<String>,
        content_digest: [u8; 32],
        counts: ScannedSourceCounts,
    ) -> ProjectionContractResult<Self> {
        Self::certify_with_frontier(
            opening,
            closing,
            parser_revision,
            content_digest,
            counts,
            None,
        )
    }

    pub fn certify_with_frontier(
        opening: SourceObservation,
        closing: SourceObservation,
        parser_revision: impl Into<String>,
        content_digest: [u8; 32],
        counts: ScannedSourceCounts,
        frontier: Option<SourceFrontier>,
    ) -> ProjectionContractResult<Self> {
        opening.source.validate_exact_descriptor(&closing.source)?;
        if opening.revision_kind != closing.revision_kind || opening.revision != closing.revision {
            return Err(ProjectionContractError::SourceRevisionChanged);
        }
        counts.validate()?;
        let parser_revision = parser_revision.into();
        validate_text(
            "parser_revision",
            &parser_revision,
            MAX_PARSER_REVISION_BYTES,
        )?;
        if let Some(frontier) = &frontier {
            if frontier.certified_prefix_bytes != counts.certified_bytes
                || frontier.certified_prefix_digest != content_digest
            {
                return Err(ProjectionContractError::FrontierMismatch);
            }
        }
        Ok(Self {
            observation: opening,
            parser_revision,
            content_digest,
            counts,
            frontier,
        })
    }

    pub fn observation(&self) -> &SourceObservation {
        &self.observation
    }

    pub fn parser_revision(&self) -> &str {
        &self.parser_revision
    }

    pub fn content_digest(&self) -> &[u8; 32] {
        &self.content_digest
    }

    pub fn counts(&self) -> ScannedSourceCounts {
        self.counts
    }

    pub fn frontier(&self) -> Option<&SourceFrontier> {
        self.frontier.as_ref()
    }

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        self.observation.source.validate_contract()?;
        validate_text(
            "source_revision_kind",
            &self.observation.revision_kind,
            MAX_REVISION_KIND_BYTES,
        )?;
        validate_nonempty_bytes(
            "source_revision",
            &self.observation.revision,
            MAX_REVISION_BYTES,
        )?;
        validate_text(
            "parser_revision",
            &self.parser_revision,
            MAX_PARSER_REVISION_BYTES,
        )?;
        self.counts.validate()?;
        if let Some(frontier) = &self.frontier {
            frontier.validate_contract()?;
            if frontier.certified_prefix_bytes != self.counts.certified_bytes
                || frontier.certified_prefix_digest != self.content_digest
            {
                return Err(ProjectionContractError::FrontierMismatch);
            }
        }
        Ok(())
    }
}

/// A safe provider checkpoint at an exactly hashed source prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFrontier {
    checkpoint_kind: String,
    checkpoint: TypedKey,
    certified_prefix_bytes: u64,
    certified_prefix_digest: [u8; 32],
}

impl SourceFrontier {
    pub fn new(
        checkpoint_kind: impl Into<String>,
        checkpoint: TypedKey,
        certified_prefix_bytes: u64,
        certified_prefix_digest: [u8; 32],
    ) -> ProjectionContractResult<Self> {
        let checkpoint_kind = checkpoint_kind.into();
        validate_text(
            "source_checkpoint_kind",
            &checkpoint_kind,
            MAX_KEY_NAMESPACE_BYTES,
        )?;
        let mut encoded = Vec::new();
        encode_typed_key(&mut encoded, &checkpoint)?;
        validate_bytes("source_checkpoint", &encoded, MAX_TYPED_KEY_BYTES)?;
        Ok(Self {
            checkpoint_kind,
            checkpoint,
            certified_prefix_bytes,
            certified_prefix_digest,
        })
    }

    pub fn checkpoint_kind(&self) -> &str {
        &self.checkpoint_kind
    }

    pub fn checkpoint(&self) -> &TypedKey {
        &self.checkpoint
    }

    pub fn certified_prefix_bytes(&self) -> u64 {
        self.certified_prefix_bytes
    }

    pub fn certified_prefix_digest(&self) -> &[u8; 32] {
        &self.certified_prefix_digest
    }

    fn validate_contract(&self) -> ProjectionContractResult<()> {
        validate_text(
            "source_checkpoint_kind",
            &self.checkpoint_kind,
            MAX_KEY_NAMESPACE_BYTES,
        )?;
        let mut encoded = Vec::new();
        encode_typed_key(&mut encoded, &self.checkpoint)?;
        validate_bytes("source_checkpoint", &encoded, MAX_TYPED_KEY_BYTES)
    }
}

/// Exact proof that a candidate extends, rather than replaces, one committed
/// source prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertifiedSourceAppend {
    base: CertifiedSource,
    current: CertifiedSource,
}

impl CertifiedSourceAppend {
    pub fn certify(
        base: &CertifiedSource,
        current: CertifiedSource,
        verified_prefix_bytes: u64,
        verified_prefix_digest: [u8; 32],
    ) -> ProjectionContractResult<Self> {
        let Some(base_frontier) = base.frontier() else {
            return Err(ProjectionContractError::AppendPrefixMismatch);
        };
        base.observation
            .source
            .validate_exact_descriptor(&current.observation.source)?;
        if verified_prefix_bytes != base_frontier.certified_prefix_bytes
            || verified_prefix_digest != base_frontier.certified_prefix_digest
        {
            return Err(ProjectionContractError::AppendPrefixMismatch);
        }
        if base.parser_revision != current.parser_revision {
            return Err(ProjectionContractError::AppendParserChanged);
        }
        let base_counts = base.counts;
        let current_counts = current.counts;
        if current_counts.complete_records < base_counts.complete_records
            || current_counts.retained_records < base_counts.retained_records
            || current_counts.rejected_records < base_counts.rejected_records
            || current_counts.ignored_records < base_counts.ignored_records
            || current_counts.indexed_documents < base_counts.indexed_documents
            || current_counts.certified_bytes < base_counts.certified_bytes
        {
            return Err(ProjectionContractError::AppendCountRegression);
        }
        Ok(Self {
            base: base.clone(),
            current,
        })
    }

    pub fn base(&self) -> &CertifiedSource {
        &self.base
    }

    pub fn current(&self) -> &CertifiedSource {
        &self.current
    }

    pub fn into_current(self) -> CertifiedSource {
        self.current
    }
}

/// Hydration/citation evidence. A locator is intentionally not identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeLocator {
    kind: String,
    value: Vec<u8>,
}

impl NativeLocator {
    pub fn new(kind: impl Into<String>, value: Vec<u8>) -> ProjectionContractResult<Self> {
        let locator = Self {
            kind: kind.into(),
            value,
        };
        validate_text("native_locator_kind", &locator.kind, MAX_LOCATOR_KIND_BYTES)?;
        validate_nonempty_bytes("native_locator", &locator.value, MAX_LOCATOR_BYTES)?;
        Ok(locator)
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PositionStability {
    AppendStable,
    StableSlot,
    RevisionScoped,
}

/// Provider-native identity for one logical session.
///
/// Native IDs and composites are durable across source revisions. A positional
/// key must instead declare the provider guarantee that makes its coordinate
/// safe to reuse.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NativeSessionKey {
    NativeId {
        namespace: String,
        value: TypedKey,
    },
    Composite {
        namespace: String,
        parts: Vec<TypedKey>,
    },
    CertifiedPosition {
        kind: String,
        coordinate: TypedKey,
        stability: PositionStability,
        revision_scope: Option<TypedKey>,
    },
}

impl NativeSessionKey {
    pub fn native_id(
        namespace: impl Into<String>,
        value: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let key = Self::NativeId {
            namespace: namespace.into(),
            value,
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn composite(
        namespace: impl Into<String>,
        parts: Vec<TypedKey>,
    ) -> ProjectionContractResult<Self> {
        let key = Self::Composite {
            namespace: namespace.into(),
            parts,
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn certified_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        stability: PositionStability,
    ) -> ProjectionContractResult<Self> {
        if stability == PositionStability::RevisionScoped {
            return Err(ProjectionContractError::RevisionScopeRequired);
        }
        let key = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability,
            revision_scope: None,
        };
        key.validate_contract()?;
        Ok(key)
    }

    /// A session position that is stable only within one provider/source
    /// revision.
    pub fn revision_scoped_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        revision_scope: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let key = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability: PositionStability::RevisionScoped,
            revision_scope: Some(revision_scope),
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_native_session_key(&mut encoded, self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NativeItemKey {
    NativeId {
        namespace: String,
        value: TypedKey,
    },
    Composite {
        namespace: String,
        parts: Vec<TypedKey>,
    },
    CertifiedPosition {
        kind: String,
        coordinate: TypedKey,
        stability: PositionStability,
        revision_scope: Option<TypedKey>,
    },
}

impl NativeItemKey {
    pub fn native_id(
        namespace: impl Into<String>,
        value: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let key = Self::NativeId {
            namespace: namespace.into(),
            value,
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn composite(
        namespace: impl Into<String>,
        parts: Vec<TypedKey>,
    ) -> ProjectionContractResult<Self> {
        let key = Self::Composite {
            namespace: namespace.into(),
            parts,
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn certified_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        stability: PositionStability,
    ) -> ProjectionContractResult<Self> {
        if stability == PositionStability::RevisionScoped {
            return Err(ProjectionContractError::RevisionScopeRequired);
        }
        let key = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability,
            revision_scope: None,
        };
        key.validate_contract()?;
        Ok(key)
    }

    /// A position that is stable only within one provider/source revision.
    ///
    /// The scope must be a provider-native snapshot/generation key known before
    /// projection. It is explicit so a rewrite cannot accidentally reuse an
    /// ordinal from an earlier snapshot.
    pub fn revision_scoped_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        revision_scope: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let key = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability: PositionStability::RevisionScoped,
            revision_scope: Some(revision_scope),
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_native_item_key(&mut encoded, self)
    }
}

/// Provider-native selector for one logical subrecord within a native item.
///
/// Absence means the event represents the whole native item. A present
/// positional selector must declare why its coordinate is stable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubrecordSelector {
    NativeId {
        namespace: String,
        value: TypedKey,
    },
    Composite {
        namespace: String,
        parts: Vec<TypedKey>,
    },
    CertifiedPosition {
        kind: String,
        coordinate: TypedKey,
        stability: PositionStability,
        revision_scope: Option<TypedKey>,
    },
}

impl SubrecordSelector {
    pub fn native_id(
        namespace: impl Into<String>,
        value: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let selector = Self::NativeId {
            namespace: namespace.into(),
            value,
        };
        selector.validate_contract()?;
        Ok(selector)
    }

    pub fn composite(
        namespace: impl Into<String>,
        parts: Vec<TypedKey>,
    ) -> ProjectionContractResult<Self> {
        let selector = Self::Composite {
            namespace: namespace.into(),
            parts,
        };
        selector.validate_contract()?;
        Ok(selector)
    }

    pub fn certified_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        stability: PositionStability,
    ) -> ProjectionContractResult<Self> {
        if stability == PositionStability::RevisionScoped {
            return Err(ProjectionContractError::RevisionScopeRequired);
        }
        let selector = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability,
            revision_scope: None,
        };
        selector.validate_contract()?;
        Ok(selector)
    }

    /// A subrecord position that is stable only within one provider/source
    /// revision.
    pub fn revision_scoped_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        revision_scope: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let selector = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability: PositionStability::RevisionScoped,
            revision_scope: Some(revision_scope),
        };
        selector.validate_contract()?;
        Ok(selector)
    }

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_subrecord_selector(&mut encoded, self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum StableEntityKind {
    Source = ENTITY_SOURCE,
    Session = ENTITY_SESSION,
    Event = ENTITY_ITEM,
}

/// Full identity equality uses the complete SHA-256 digest.
///
/// The UUIDv8 is a public compact representation. A registry must fail closed
/// if an existing UUID is ever observed with a different full digest.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StableEntityId {
    contract_version: u16,
    entity_kind: StableEntityKind,
    digest: [u8; 32],
    source_digest: [u8; 32],
    source_descriptor_digest: [u8; 32],
    uuid: Uuid,
}

impl StableEntityId {
    pub fn contract_version(self) -> u16 {
        self.contract_version
    }

    pub fn entity_kind(self) -> StableEntityKind {
        self.entity_kind
    }

    pub fn digest(self) -> [u8; 32] {
        self.digest
    }

    pub fn source_digest(self) -> [u8; 32] {
        self.source_digest
    }

    pub fn source_descriptor_digest(self) -> [u8; 32] {
        self.source_descriptor_digest
    }

    pub fn as_uuid(self) -> Uuid {
        self.uuid
    }

    pub fn validate_contract(self) -> ProjectionContractResult<()> {
        if self.contract_version != IDENTITY_VERSION {
            return Err(ProjectionContractError::InvalidDerivedIdentity);
        }
        let mut uuid_bytes = [0_u8; 16];
        uuid_bytes.copy_from_slice(&self.digest[..16]);
        uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
        uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
        if Uuid::from_bytes(uuid_bytes) != self.uuid
            || (self.entity_kind == StableEntityKind::Source
                && (self.source_digest != self.digest || self.source_descriptor_digest != [0; 32]))
        {
            return Err(ProjectionContractError::InvalidDerivedIdentity);
        }
        Ok(())
    }
}

impl PartialEq for StableEntityId {
    fn eq(&self, other: &Self) -> bool {
        self.contract_version == other.contract_version
            && self.entity_kind == other.entity_kind
            && self.digest == other.digest
    }
}

impl Eq for StableEntityId {}

impl Hash for StableEntityId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.contract_version.hash(state);
        self.entity_kind.hash(state);
        self.digest.hash(state);
    }
}

impl std::fmt::Display for StableEntityId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.uuid.fmt(formatter)
    }
}

pub struct SessionIdentityInput<'a> {
    pub source: &'a SourceKey,
    pub logical_session_kind: &'a str,
    pub native_session_key: &'a NativeSessionKey,
}

pub struct EventIdentityInput<'a> {
    pub source: &'a SourceKey,
    pub session_id: StableEntityId,
    pub logical_item_kind: &'a str,
    pub native_item_key: &'a NativeItemKey,
    pub subrecord_selector: Option<&'a SubrecordSelector>,
}

pub fn derive_session_id(
    input: SessionIdentityInput<'_>,
) -> ProjectionContractResult<StableEntityId> {
    input.source.validate_contract()?;
    validate_text(
        "logical_session_kind",
        input.logical_session_kind,
        MAX_LOGICAL_KIND_BYTES,
    )?;
    let mut fields = IdentityFields::new();
    fields.bytes(1, &input.source.identity.digest);
    fields.utf8(2, input.logical_session_kind);
    fields.native_session_key(3, input.native_session_key)?;
    derive_identity(StableEntityKind::Session, fields, Some(input.source))
}

pub fn derive_event_id(input: EventIdentityInput<'_>) -> ProjectionContractResult<StableEntityId> {
    input.source.validate_contract()?;
    validate_text(
        "logical_item_kind",
        input.logical_item_kind,
        MAX_LOGICAL_KIND_BYTES,
    )?;
    if input.session_id.entity_kind != StableEntityKind::Session {
        return Err(ProjectionContractError::EntityKindMismatch {
            expected: StableEntityKind::Session,
            actual: input.session_id.entity_kind,
        });
    }
    input.session_id.validate_contract()?;
    if input.session_id.source_digest != input.source.identity.digest {
        return Err(ProjectionContractError::SourceChanged);
    }
    if input.session_id.source_descriptor_digest != input.source.exact_descriptor_digest() {
        return Err(ProjectionContractError::SourceDescriptorChanged);
    }
    let mut fields = IdentityFields::new();
    fields.bytes(1, &input.source.identity.digest);
    fields.bytes(2, &input.session_id.digest);
    fields.utf8(3, input.logical_item_kind);
    fields.native_item_key(4, input.native_item_key)?;
    if let Some(subrecord) = input.subrecord_selector {
        fields.subrecord_selector(5, subrecord)?;
    }
    derive_identity(StableEntityKind::Event, fields, Some(input.source))
}

fn derive_source_identity(
    provider: &str,
    anchor: &SourceAnchor,
) -> ProjectionContractResult<StableEntityId> {
    let mut fields = IdentityFields::new();
    fields.utf8(1, provider);
    fields.source_anchor(2, anchor)?;
    derive_identity(StableEntityKind::Source, fields, None)
}

fn derive_identity(
    entity_kind: StableEntityKind,
    fields: IdentityFields,
    source: Option<&SourceKey>,
) -> ProjectionContractResult<StableEntityId> {
    let field_count =
        u16::try_from(fields.values.len()).map_err(|_| ProjectionContractError::FieldTooLarge {
            field: "identity_field_count",
            actual: fields.values.len(),
            maximum: u16::MAX as usize,
        })?;
    let mut digest = Sha256::new();
    digest.update(IDENTITY_DOMAIN);
    digest.update(IDENTITY_VERSION.to_be_bytes());
    digest.update([entity_kind as u8]);
    digest.update(field_count.to_be_bytes());
    for (tag, value) in fields.values {
        digest.update(tag.to_be_bytes());
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    let digest: [u8; 32] = digest.finalize().into();
    let mut uuid_bytes = [0_u8; 16];
    uuid_bytes.copy_from_slice(&digest[..16]);
    uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
    uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
    let source_digest = source
        .map(|source| source.identity.digest)
        .unwrap_or(digest);
    let source_descriptor_digest = source
        .map(SourceKey::exact_descriptor_digest)
        .unwrap_or([0; 32]);
    Ok(StableEntityId {
        contract_version: IDENTITY_VERSION,
        entity_kind,
        digest,
        source_digest,
        source_descriptor_digest,
        uuid: Uuid::from_bytes(uuid_bytes),
    })
}

struct IdentityFields {
    values: Vec<(u16, Vec<u8>)>,
}

impl IdentityFields {
    fn new() -> Self {
        Self { values: Vec::new() }
    }

    fn push(&mut self, tag: u16, value: Vec<u8>) {
        debug_assert!(self.values.last().is_none_or(|(prior, _)| *prior < tag));
        self.values.push((tag, value));
    }

    fn bytes(&mut self, tag: u16, value: &[u8]) {
        self.push(tag, value.to_vec());
    }

    fn utf8(&mut self, tag: u16, value: &str) {
        self.push(tag, value.as_bytes().to_vec());
    }

    fn native_session_key(
        &mut self,
        tag: u16,
        key: &NativeSessionKey,
    ) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_native_session_key(&mut encoded, key)?;
        self.push(tag, encoded);
        Ok(())
    }

    fn source_anchor(&mut self, tag: u16, anchor: &SourceAnchor) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        match anchor {
            SourceAnchor::ProviderNative { namespace, key } => {
                encoded.push(1);
                encode_length_prefixed(&mut encoded, namespace.as_bytes());
                encode_typed_key(&mut encoded, key)?;
            }
            SourceAnchor::CatalogLineage(lineage) => {
                encoded.push(2);
                encoded.extend_from_slice(lineage);
            }
        }
        self.push(tag, encoded);
        Ok(())
    }

    fn native_item_key(&mut self, tag: u16, key: &NativeItemKey) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_native_item_key(&mut encoded, key)?;
        self.push(tag, encoded);
        Ok(())
    }

    fn subrecord_selector(
        &mut self,
        tag: u16,
        selector: &SubrecordSelector,
    ) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_subrecord_selector(&mut encoded, selector)?;
        self.push(tag, encoded);
        Ok(())
    }
}

enum IdentityKeyRef<'a> {
    NativeId {
        namespace: &'a str,
        value: &'a TypedKey,
    },
    Composite {
        namespace: &'a str,
        parts: &'a [TypedKey],
    },
    CertifiedPosition {
        kind: &'a str,
        coordinate: &'a TypedKey,
        stability: PositionStability,
        revision_scope: Option<&'a TypedKey>,
    },
}

fn encode_native_session_key(
    target: &mut Vec<u8>,
    key: &NativeSessionKey,
) -> ProjectionContractResult<()> {
    let key = match key {
        NativeSessionKey::NativeId { namespace, value } => {
            IdentityKeyRef::NativeId { namespace, value }
        }
        NativeSessionKey::Composite { namespace, parts } => {
            IdentityKeyRef::Composite { namespace, parts }
        }
        NativeSessionKey::CertifiedPosition {
            kind,
            coordinate,
            stability,
            revision_scope,
        } => IdentityKeyRef::CertifiedPosition {
            kind,
            coordinate,
            stability: *stability,
            revision_scope: revision_scope.as_ref(),
        },
    };
    encode_identity_key(
        target,
        key,
        "native_session_namespace",
        "native_session_position_kind",
    )
}

fn encode_native_item_key(
    target: &mut Vec<u8>,
    key: &NativeItemKey,
) -> ProjectionContractResult<()> {
    let key = match key {
        NativeItemKey::NativeId { namespace, value } => {
            IdentityKeyRef::NativeId { namespace, value }
        }
        NativeItemKey::Composite { namespace, parts } => {
            IdentityKeyRef::Composite { namespace, parts }
        }
        NativeItemKey::CertifiedPosition {
            kind,
            coordinate,
            stability,
            revision_scope,
        } => IdentityKeyRef::CertifiedPosition {
            kind,
            coordinate,
            stability: *stability,
            revision_scope: revision_scope.as_ref(),
        },
    };
    encode_identity_key(target, key, "native_item_namespace", "native_position_kind")
}

fn encode_subrecord_selector(
    target: &mut Vec<u8>,
    selector: &SubrecordSelector,
) -> ProjectionContractResult<()> {
    let selector = match selector {
        SubrecordSelector::NativeId { namespace, value } => {
            IdentityKeyRef::NativeId { namespace, value }
        }
        SubrecordSelector::Composite { namespace, parts } => {
            IdentityKeyRef::Composite { namespace, parts }
        }
        SubrecordSelector::CertifiedPosition {
            kind,
            coordinate,
            stability,
            revision_scope,
        } => IdentityKeyRef::CertifiedPosition {
            kind,
            coordinate,
            stability: *stability,
            revision_scope: revision_scope.as_ref(),
        },
    };
    encode_identity_key(
        target,
        selector,
        "subrecord_namespace",
        "subrecord_position_kind",
    )
}

fn encode_identity_key(
    target: &mut Vec<u8>,
    key: IdentityKeyRef<'_>,
    namespace_field: &'static str,
    position_kind_field: &'static str,
) -> ProjectionContractResult<()> {
    match key {
        IdentityKeyRef::NativeId { namespace, value } => {
            validate_text(namespace_field, namespace, MAX_KEY_NAMESPACE_BYTES)?;
            target.push(1);
            encode_length_prefixed(target, namespace.as_bytes());
            encode_typed_key(target, value)?;
        }
        IdentityKeyRef::Composite { namespace, parts } => {
            validate_text(namespace_field, namespace, MAX_KEY_NAMESPACE_BYTES)?;
            if parts.len() > MAX_TYPED_KEY_COMPONENTS {
                return Err(ProjectionContractError::TooManyKeyComponents {
                    actual: parts.len(),
                    maximum: MAX_TYPED_KEY_COMPONENTS,
                });
            }
            target.push(2);
            encode_length_prefixed(target, namespace.as_bytes());
            target.extend_from_slice(&(parts.len() as u32).to_be_bytes());
            for part in parts {
                encode_typed_key(target, part)?;
            }
        }
        IdentityKeyRef::CertifiedPosition {
            kind,
            coordinate,
            stability,
            revision_scope,
        } => {
            validate_text(position_kind_field, kind, MAX_KEY_NAMESPACE_BYTES)?;
            match (stability, revision_scope) {
                (PositionStability::RevisionScoped, None) => {
                    return Err(ProjectionContractError::RevisionScopeRequired);
                }
                (PositionStability::AppendStable | PositionStability::StableSlot, Some(_)) => {
                    return Err(ProjectionContractError::UnexpectedRevisionScope)
                }
                _ => {}
            }
            target.push(3);
            encode_length_prefixed(target, kind.as_bytes());
            target.push(match stability {
                PositionStability::AppendStable => 1,
                PositionStability::StableSlot => 2,
                PositionStability::RevisionScoped => 3,
            });
            encode_typed_key(target, coordinate)?;
            if let Some(scope) = revision_scope {
                encode_typed_key(target, scope)?;
            }
        }
    }
    Ok(())
}

fn encode_typed_key(target: &mut Vec<u8>, key: &TypedKey) -> ProjectionContractResult<()> {
    match key {
        TypedKey::Null => target.push(0),
        TypedKey::Bytes(value) => {
            validate_bytes("typed_key_bytes", value, MAX_TYPED_KEY_BYTES)?;
            target.push(1);
            encode_length_prefixed(target, value);
        }
        TypedKey::Utf8(value) => {
            validate_text("typed_key_utf8", value, MAX_TYPED_KEY_BYTES)?;
            target.push(2);
            encode_length_prefixed(target, value.as_bytes());
        }
        TypedKey::I64(value) => {
            target.push(3);
            target.extend_from_slice(&value.to_be_bytes());
        }
        TypedKey::U64(value) => {
            target.push(4);
            target.extend_from_slice(&value.to_be_bytes());
        }
        TypedKey::F64Bits(value) => {
            target.push(5);
            target.extend_from_slice(&value.to_be_bytes());
        }
        TypedKey::Bool(value) => {
            target.push(6);
            target.push(u8::from(*value));
        }
        TypedKey::Composite(values) => {
            if values.len() > MAX_TYPED_KEY_COMPONENTS {
                return Err(ProjectionContractError::TooManyKeyComponents {
                    actual: values.len(),
                    maximum: MAX_TYPED_KEY_COMPONENTS,
                });
            }
            target.push(7);
            target.extend_from_slice(&(values.len() as u32).to_be_bytes());
            for value in values {
                encode_typed_key(target, value)?;
            }
        }
    }
    Ok(())
}

fn validate_source_anchor(anchor: &SourceAnchor) -> ProjectionContractResult<()> {
    match anchor {
        SourceAnchor::ProviderNative { namespace, key } => {
            validate_text(
                "source_anchor_namespace",
                namespace,
                MAX_KEY_NAMESPACE_BYTES,
            )?;
            let mut encoded = Vec::new();
            encode_typed_key(&mut encoded, key)?;
            validate_bytes("source_anchor_key", &encoded, MAX_TYPED_KEY_BYTES)
        }
        SourceAnchor::CatalogLineage(_) => Ok(()),
    }
}

fn encode_length_prefixed(target: &mut Vec<u8>, value: &[u8]) {
    target.extend_from_slice(&(value.len() as u64).to_be_bytes());
    target.extend_from_slice(value);
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> ProjectionContractResult<()> {
    validate_nonempty_bytes(field, value.as_bytes(), maximum)
}

fn validate_nonempty_bytes(
    field: &'static str,
    value: &[u8],
    maximum: usize,
) -> ProjectionContractResult<()> {
    if value.is_empty() {
        return Err(ProjectionContractError::EmptyField { field });
    }
    validate_bytes(field, value, maximum)
}

fn validate_bytes(
    field: &'static str,
    value: &[u8],
    maximum: usize,
) -> ProjectionContractResult<()> {
    if value.len() > maximum {
        return Err(ProjectionContractError::FieldTooLarge {
            field,
            actual: value.len(),
            maximum,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn source(lineage: u8) -> SourceKey {
        SourceKey::derive(
            "codex",
            "codex_session_jsonl",
            "session",
            1,
            SourceAnchor::CatalogLineage([lineage; 32]),
        )
        .unwrap()
    }

    fn native_id(value: &str) -> NativeItemKey {
        NativeItemKey::native_id("message", TypedKey::utf8(value).unwrap()).unwrap()
    }

    fn native_session_id(value: &str) -> NativeSessionKey {
        NativeSessionKey::native_id("session", TypedKey::utf8(value).unwrap()).unwrap()
    }

    #[test]
    fn source_lineage_disambiguates_equal_provider_session_ids() {
        let first = source(1);
        let second = source(2);
        let session_key = native_session_id("provider-thread-123");
        let first_id = derive_session_id(SessionIdentityInput {
            source: &first,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let second_id = derive_session_id(SessionIdentityInput {
            source: &second,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        assert_ne!(first_id, second_id);
    }

    #[test]
    fn exact_catalog_lineage_survives_source_relocation() {
        let before_move = source(1);
        let after_move = source(1);
        let session_key = native_session_id("provider-thread-123");
        assert_eq!(
            derive_session_id(SessionIdentityInput {
                source: &before_move,
                logical_session_kind: "thread",
                native_session_key: &session_key,
            })
            .unwrap(),
            derive_session_id(SessionIdentityInput {
                source: &after_move,
                logical_session_kind: "thread",
                native_session_key: &session_key,
            })
            .unwrap()
        );
    }

    #[test]
    fn source_format_and_parser_classification_do_not_rotate_lineage_identity() {
        let anchor = SourceAnchor::CatalogLineage([7; 32]);
        let before = SourceKey::derive(
            "codex",
            "codex_session_jsonl",
            "session-v1",
            1,
            anchor.clone(),
        )
        .unwrap();
        let after = SourceKey::derive(
            "codex",
            "codex_session_jsonl_tree_leaf",
            "session-v2",
            2,
            anchor,
        )
        .unwrap();
        assert_eq!(before.identity(), after.identity());
        assert_eq!(before, after);
        assert!(!before.exact_descriptor_eq(&after));
        assert_eq!(
            before.validate_exact_descriptor(&after).unwrap_err(),
            ProjectionContractError::SourceDescriptorChanged
        );

        let session_key = native_session_id("provider-thread-123");
        let before_session = derive_session_id(SessionIdentityInput {
            source: &before,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let after_session = derive_session_id(SessionIdentityInput {
            source: &after,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        assert_eq!(before_session, after_session);
        assert_ne!(
            before_session.source_descriptor_digest(),
            after_session.source_descriptor_digest()
        );
    }

    #[test]
    fn scans_and_identity_inputs_reject_same_lineage_with_a_different_descriptor() {
        let anchor = SourceAnchor::CatalogLineage([7; 32]);
        let opening_source = SourceKey::derive(
            "codex",
            "codex_session_jsonl",
            "session-v1",
            1,
            anchor.clone(),
        )
        .unwrap();
        let closing_source = SourceKey::derive(
            "codex",
            "codex_session_jsonl_tree_leaf",
            "session-v2",
            2,
            anchor,
        )
        .unwrap();
        let opening =
            SourceObservation::new(opening_source.clone(), "regular-file-v1", vec![1]).unwrap();
        let closing =
            SourceObservation::new(closing_source.clone(), "regular-file-v1", vec![1]).unwrap();
        assert_eq!(
            CertifiedSource::certify(
                opening,
                closing,
                "parser-v1",
                [3; 32],
                ScannedSourceCounts::default(),
            )
            .unwrap_err(),
            ProjectionContractError::SourceDescriptorChanged
        );

        let session_key = native_session_id("provider-thread-123");
        let session_id = derive_session_id(SessionIdentityInput {
            source: &opening_source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let item = native_id("event-1");
        assert_eq!(
            derive_event_id(EventIdentityInput {
                source: &closing_source,
                session_id,
                logical_item_kind: "message",
                native_item_key: &item,
                subrecord_selector: None,
            })
            .unwrap_err(),
            ProjectionContractError::SourceDescriptorChanged
        );
    }

    #[test]
    fn stable_native_item_identity_excludes_mutable_content_and_locator() {
        let source = source(1);
        let session_key = native_session_id("session");
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let key = native_id("event-1");
        let first = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &key,
            subrecord_selector: None,
        })
        .unwrap();
        let second = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &key,
            subrecord_selector: None,
        })
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.as_uuid().get_version_num(), 8);
        assert_ne!(first.digest(), [0; 32]);
    }

    #[test]
    fn typed_native_keys_do_not_collapse_storage_classes() {
        let source = source(1);
        let session_key = native_session_id("session");
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let keys = [
            NativeItemKey::native_id("sqlite-key", TypedKey::I64(1)).unwrap(),
            NativeItemKey::native_id("sqlite-key", TypedKey::utf8("1").unwrap()).unwrap(),
            NativeItemKey::native_id("sqlite-key", TypedKey::bytes(vec![0x31]).unwrap()).unwrap(),
            NativeItemKey::native_id("sqlite-key", TypedKey::from_f64(1.0)).unwrap(),
        ];
        let ids = keys
            .iter()
            .map(|key| {
                derive_event_id(EventIdentityInput {
                    source: &source,
                    session_id,
                    logical_item_kind: "message",
                    native_item_key: key,
                    subrecord_selector: None,
                })
                .unwrap()
            })
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), keys.len());
    }

    #[test]
    fn framed_identity_fields_do_not_have_concatenation_collisions() {
        let first = SourceKey::derive(
            "ab",
            "c",
            "schema",
            1,
            SourceAnchor::CatalogLineage([1; 32]),
        )
        .unwrap();
        let second = SourceKey::derive(
            "a",
            "bc",
            "schema",
            1,
            SourceAnchor::CatalogLineage([1; 32]),
        )
        .unwrap();
        assert_ne!(first.identity(), second.identity());
    }

    #[test]
    fn positional_stability_is_an_explicit_identity_input() {
        let source = source(1);
        let session_key = native_session_id("session");
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let append = NativeItemKey::certified_position(
            "jsonl-record",
            TypedKey::U64(4),
            PositionStability::AppendStable,
        )
        .unwrap();
        let missing_scope = NativeItemKey::certified_position(
            "jsonl-record",
            TypedKey::U64(4),
            PositionStability::RevisionScoped,
        )
        .unwrap_err();
        assert_eq!(
            missing_scope,
            ProjectionContractError::RevisionScopeRequired
        );
        let revision_one = NativeItemKey::revision_scoped_position(
            "jsonl-record",
            TypedKey::U64(4),
            TypedKey::bytes(vec![1; 32]).unwrap(),
        )
        .unwrap();
        let revision_two = NativeItemKey::revision_scoped_position(
            "jsonl-record",
            TypedKey::U64(4),
            TypedKey::bytes(vec![2; 32]).unwrap(),
        )
        .unwrap();
        let derive = |key: &NativeItemKey| {
            derive_event_id(EventIdentityInput {
                source: &source,
                session_id,
                logical_item_kind: "message",
                native_item_key: key,
                subrecord_selector: None,
            })
            .unwrap()
        };
        assert_ne!(derive(&append), derive(&revision_one));
        assert_ne!(derive(&revision_one), derive(&revision_two));
    }

    #[test]
    fn session_positions_require_an_explicit_stability_contract() {
        let source = source(1);
        let append = NativeSessionKey::certified_position(
            "session-array-index",
            TypedKey::U64(4),
            PositionStability::AppendStable,
        )
        .unwrap();
        let stable_slot = NativeSessionKey::certified_position(
            "session-array-index",
            TypedKey::U64(4),
            PositionStability::StableSlot,
        )
        .unwrap();
        assert_eq!(
            NativeSessionKey::certified_position(
                "session-array-index",
                TypedKey::U64(4),
                PositionStability::RevisionScoped,
            )
            .unwrap_err(),
            ProjectionContractError::RevisionScopeRequired
        );
        let revision_one = NativeSessionKey::revision_scoped_position(
            "session-array-index",
            TypedKey::U64(4),
            TypedKey::bytes(vec![1; 32]).unwrap(),
        )
        .unwrap();
        let revision_two = NativeSessionKey::revision_scoped_position(
            "session-array-index",
            TypedKey::U64(4),
            TypedKey::bytes(vec![2; 32]).unwrap(),
        )
        .unwrap();
        let derive = |key: &NativeSessionKey| {
            derive_session_id(SessionIdentityInput {
                source: &source,
                logical_session_kind: "thread",
                native_session_key: key,
            })
            .unwrap()
        };
        let ids = [&append, &stable_slot, &revision_one, &revision_two]
            .into_iter()
            .map(derive)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), 4);

        let unscoped_revision = NativeSessionKey::CertifiedPosition {
            kind: "session-array-index".to_owned(),
            coordinate: TypedKey::U64(4),
            stability: PositionStability::RevisionScoped,
            revision_scope: None,
        };
        assert_eq!(
            derive_session_id(SessionIdentityInput {
                source: &source,
                logical_session_kind: "thread",
                native_session_key: &unscoped_revision,
            })
            .unwrap_err(),
            ProjectionContractError::RevisionScopeRequired
        );
    }

    #[test]
    fn subrecord_positions_require_an_explicit_stability_contract() {
        let source = source(1);
        let session_key = native_session_id("session");
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let item = native_id("event-1");
        let append = SubrecordSelector::certified_position(
            "content-block",
            TypedKey::U64(2),
            PositionStability::AppendStable,
        )
        .unwrap();
        let stable_slot = SubrecordSelector::certified_position(
            "content-block",
            TypedKey::U64(2),
            PositionStability::StableSlot,
        )
        .unwrap();
        assert_eq!(
            SubrecordSelector::certified_position(
                "content-block",
                TypedKey::U64(2),
                PositionStability::RevisionScoped,
            )
            .unwrap_err(),
            ProjectionContractError::RevisionScopeRequired
        );
        let revision_one = SubrecordSelector::revision_scoped_position(
            "content-block",
            TypedKey::U64(2),
            TypedKey::bytes(vec![1; 32]).unwrap(),
        )
        .unwrap();
        let revision_two = SubrecordSelector::revision_scoped_position(
            "content-block",
            TypedKey::U64(2),
            TypedKey::bytes(vec![2; 32]).unwrap(),
        )
        .unwrap();
        let derive = |selector: &SubrecordSelector| {
            derive_event_id(EventIdentityInput {
                source: &source,
                session_id,
                logical_item_kind: "message",
                native_item_key: &item,
                subrecord_selector: Some(selector),
            })
            .unwrap()
        };
        let ids = [&append, &stable_slot, &revision_one, &revision_two]
            .into_iter()
            .map(derive)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), 4);

        let unexpectedly_scoped = SubrecordSelector::CertifiedPosition {
            kind: "content-block".to_owned(),
            coordinate: TypedKey::U64(2),
            stability: PositionStability::AppendStable,
            revision_scope: Some(TypedKey::U64(9)),
        };
        assert_eq!(
            unexpectedly_scoped.validate_contract().unwrap_err(),
            ProjectionContractError::UnexpectedRevisionScope
        );
    }

    #[test]
    fn stable_identity_key_variants_keep_native_item_framing() {
        let item_native = NativeItemKey::native_id("id", TypedKey::U64(7)).unwrap();
        let session_native = NativeSessionKey::native_id("id", TypedKey::U64(7)).unwrap();
        let subrecord_native = SubrecordSelector::native_id("id", TypedKey::U64(7)).unwrap();
        let item_composite =
            NativeItemKey::composite("id", vec![TypedKey::U64(7), TypedKey::Bool(true)]).unwrap();
        let session_composite =
            NativeSessionKey::composite("id", vec![TypedKey::U64(7), TypedKey::Bool(true)])
                .unwrap();
        let subrecord_composite =
            SubrecordSelector::composite("id", vec![TypedKey::U64(7), TypedKey::Bool(true)])
                .unwrap();

        let mut expected_native = vec![1];
        expected_native.extend_from_slice(&2_u64.to_be_bytes());
        expected_native.extend_from_slice(b"id");
        expected_native.push(4);
        expected_native.extend_from_slice(&7_u64.to_be_bytes());

        let mut expected_composite = vec![2];
        expected_composite.extend_from_slice(&2_u64.to_be_bytes());
        expected_composite.extend_from_slice(b"id");
        expected_composite.extend_from_slice(&2_u32.to_be_bytes());
        expected_composite.push(4);
        expected_composite.extend_from_slice(&7_u64.to_be_bytes());
        expected_composite.extend_from_slice(&[6, 1]);

        let mut item_native_encoded = Vec::new();
        let mut session_native_encoded = Vec::new();
        let mut subrecord_native_encoded = Vec::new();
        encode_native_item_key(&mut item_native_encoded, &item_native).unwrap();
        encode_native_session_key(&mut session_native_encoded, &session_native).unwrap();
        encode_subrecord_selector(&mut subrecord_native_encoded, &subrecord_native).unwrap();
        assert_eq!(item_native_encoded, expected_native);
        assert_eq!(session_native_encoded, expected_native);
        assert_eq!(subrecord_native_encoded, expected_native);

        let mut item_composite_encoded = Vec::new();
        let mut session_composite_encoded = Vec::new();
        let mut subrecord_composite_encoded = Vec::new();
        encode_native_item_key(&mut item_composite_encoded, &item_composite).unwrap();
        encode_native_session_key(&mut session_composite_encoded, &session_composite).unwrap();
        encode_subrecord_selector(&mut subrecord_composite_encoded, &subrecord_composite).unwrap();
        assert_eq!(item_composite_encoded, expected_composite);
        assert_eq!(session_composite_encoded, expected_composite);
        assert_eq!(subrecord_composite_encoded, expected_composite);
    }

    #[test]
    fn event_identity_rejects_a_non_session_parent() {
        let source = source(1);
        let key = native_id("event-1");
        let error = derive_event_id(EventIdentityInput {
            source: &source,
            session_id: source.identity(),
            logical_item_kind: "message",
            native_item_key: &key,
            subrecord_selector: None,
        })
        .unwrap_err();
        assert_eq!(
            error,
            ProjectionContractError::EntityKindMismatch {
                expected: StableEntityKind::Session,
                actual: StableEntityKind::Source,
            }
        );
    }

    #[test]
    fn event_identity_rejects_a_session_from_another_source() {
        let first = source(1);
        let second = source(2);
        let session_key = native_session_id("session");
        let first_session = derive_session_id(SessionIdentityInput {
            source: &first,
            logical_session_kind: "thread",
            native_session_key: &session_key,
        })
        .unwrap();
        let item = native_id("event-1");
        let error = derive_event_id(EventIdentityInput {
            source: &second,
            session_id: first_session,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })
        .unwrap_err();
        assert_eq!(error, ProjectionContractError::SourceChanged);
    }

    #[test]
    fn certification_rejects_a_source_mutation() {
        let source = source(1);
        let opening =
            SourceObservation::new(source.clone(), "regular-file-v1", vec![1, 2, 3]).unwrap();
        let closing = SourceObservation::new(source, "regular-file-v1", vec![1, 2, 4]).unwrap();
        let error = CertifiedSource::certify(
            opening,
            closing,
            "codex-parser-v1",
            [9; 32],
            ScannedSourceCounts {
                complete_records: 1,
                retained_records: 1,
                indexed_documents: 1,
                certified_bytes: 10,
                ..ScannedSourceCounts::default()
            },
        )
        .unwrap_err();
        assert_eq!(error, ProjectionContractError::SourceRevisionChanged);
    }

    #[test]
    fn certification_reconciles_record_counts() {
        let source = source(1);
        let observation = SourceObservation::new(source, "regular-file-v1", vec![1, 2, 3]).unwrap();
        let error = CertifiedSource::certify(
            observation.clone(),
            observation,
            "codex-parser-v1",
            [9; 32],
            ScannedSourceCounts {
                complete_records: 3,
                retained_records: 1,
                rejected_records: 1,
                ignored_records: 0,
                indexed_documents: 1,
                certified_bytes: 10,
            },
        )
        .unwrap_err();
        assert_eq!(error, ProjectionContractError::CountMismatch);
    }

    #[test]
    fn deletion_requires_one_unchanged_authoritative_inventory() {
        let opening = SourceInventoryObservation::new(
            "codex",
            "sessions-root",
            TypedKey::utf8("root-lineage").unwrap(),
            "tree-inventory-v1",
            vec![1],
        )
        .unwrap();
        let closing = SourceInventoryObservation::new(
            "codex",
            "sessions-root",
            TypedKey::utf8("root-lineage").unwrap(),
            "tree-inventory-v1",
            vec![2],
        )
        .unwrap();
        let error =
            CertifiedSourceInventory::certify(opening, closing, "codex-discovery-v1", vec![])
                .unwrap_err();
        assert_eq!(error, ProjectionContractError::InventoryRevisionChanged);
    }

    #[test]
    fn deletion_inventory_must_own_the_source_provider() {
        let observation = SourceInventoryObservation::new(
            "claude_code",
            "projects-root",
            TypedKey::utf8("root-lineage").unwrap(),
            "tree-inventory-v1",
            vec![1],
        )
        .unwrap();
        let inventory = CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            "claude-discovery-v1",
            vec![],
        )
        .unwrap();
        let error = CertifiedSourceDeletion::from_inventory(source(1), &inventory).unwrap_err();
        assert_eq!(error, ProjectionContractError::InventoryProviderMismatch);
    }

    #[test]
    fn deletion_inventory_must_prove_the_source_is_absent() {
        let source = source(1);
        let observation = SourceInventoryObservation::new(
            "codex",
            "sessions-root",
            TypedKey::utf8("root-lineage").unwrap(),
            "tree-inventory-v1",
            vec![1],
        )
        .unwrap();
        let inventory = CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            "codex-discovery-v1",
            vec![source.clone()],
        )
        .unwrap();
        let error = CertifiedSourceDeletion::from_inventory(source, &inventory).unwrap_err();
        assert_eq!(
            error,
            ProjectionContractError::InventoryContainsDeletedSource
        );
    }

    #[test]
    fn append_requires_an_exact_committed_prefix() {
        let source = source(1);
        let base_observation =
            SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
        let base = CertifiedSource::certify_with_frontier(
            base_observation.clone(),
            base_observation,
            "parser-v1",
            [3; 32],
            ScannedSourceCounts {
                complete_records: 2,
                retained_records: 2,
                indexed_documents: 2,
                certified_bytes: 100,
                ..ScannedSourceCounts::default()
            },
            Some(
                SourceFrontier::new("jsonl-byte-offset", TypedKey::U64(100), 100, [3; 32]).unwrap(),
            ),
        )
        .unwrap();
        let current_observation =
            SourceObservation::new(source, "regular-file-v1", vec![2]).unwrap();
        let current = CertifiedSource::certify_with_frontier(
            current_observation.clone(),
            current_observation,
            "parser-v1",
            [4; 32],
            ScannedSourceCounts {
                complete_records: 3,
                retained_records: 3,
                indexed_documents: 3,
                certified_bytes: 150,
                ..ScannedSourceCounts::default()
            },
            Some(
                SourceFrontier::new("jsonl-byte-offset", TypedKey::U64(150), 150, [4; 32]).unwrap(),
            ),
        )
        .unwrap();
        let error =
            CertifiedSourceAppend::certify(&base, current.clone(), 100, [9; 32]).unwrap_err();
        assert_eq!(error, ProjectionContractError::AppendPrefixMismatch);
        assert!(CertifiedSourceAppend::certify(&base, current, 100, [3; 32]).is_ok());
    }

    #[test]
    fn frontier_must_bind_the_certified_byte_prefix() {
        let source = source(1);
        let observation = SourceObservation::new(source, "regular-file-v1", vec![1]).unwrap();
        let error = CertifiedSource::certify_with_frontier(
            observation.clone(),
            observation,
            "parser-v1",
            [3; 32],
            ScannedSourceCounts {
                certified_bytes: 100,
                ..ScannedSourceCounts::default()
            },
            Some(SourceFrontier::new("jsonl-byte-offset", TypedKey::U64(99), 99, [3; 32]).unwrap()),
        )
        .unwrap_err();
        assert_eq!(error, ProjectionContractError::FrontierMismatch);
    }
}
