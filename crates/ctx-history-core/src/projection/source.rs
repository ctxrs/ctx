use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::errors::{
    validate_bytes, validate_nonempty_bytes, validate_text, ProjectionContractError,
    ProjectionContractResult, MAX_KEY_NAMESPACE_BYTES, MAX_PARSER_REVISION_BYTES,
    MAX_PROVIDER_BYTES, MAX_REVISION_BYTES, MAX_REVISION_KIND_BYTES, MAX_SCHEMA_VARIANT_BYTES,
    MAX_SOURCE_FORMAT_BYTES, MAX_TYPED_KEY_BYTES,
};
use super::identity::{derive_source_identity, StableEntityId, IDENTITY_VERSION};
use super::native::{encode_typed_key, TypedKey};

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
    pub(super) provider: String,
    pub(super) source_format: String,
    pub(super) schema_variant: String,
    pub(super) provider_identity_version: u32,
    pub(super) anchor: SourceAnchor,
    pub(super) identity: StableEntityId,
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
    pub(super) source: SourceKey,
    pub(super) revision_kind: String,
    pub(super) revision: Vec<u8>,
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
    pub(super) provider: String,
    pub(super) authority_namespace: String,
    pub(super) authority_key: TypedKey,
    pub(super) revision_kind: String,
    pub(super) revision: Vec<u8>,
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

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        validate_text("inventory_provider", &self.provider, MAX_PROVIDER_BYTES)?;
        validate_text(
            "inventory_authority_namespace",
            &self.authority_namespace,
            MAX_KEY_NAMESPACE_BYTES,
        )?;
        let mut encoded = Vec::new();
        encode_typed_key(&mut encoded, &self.authority_key)?;
        validate_bytes("inventory_authority_key", &encoded, MAX_TYPED_KEY_BYTES)?;
        validate_text(
            "inventory_revision_kind",
            &self.revision_kind,
            MAX_REVISION_KIND_BYTES,
        )?;
        validate_nonempty_bytes("inventory_revision", &self.revision, MAX_REVISION_BYTES)
    }
}

/// A complete, internally digested provider inventory observed unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertifiedSourceInventory {
    pub(super) observation: SourceInventoryObservation,
    pub(super) discovery_revision: String,
    pub(super) source_digests: Vec<[u8; 32]>,
    pub(super) inventory_digest: [u8; 32],
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

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        self.observation.validate_contract()?;
        validate_text(
            "discovery_revision",
            &self.discovery_revision,
            MAX_PARSER_REVISION_BYTES,
        )?;
        if self
            .source_digests
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(ProjectionContractError::DuplicateInventorySource);
        }
        let mut digest = Sha256::new();
        digest.update(b"ctx.source-inventory\0");
        digest.update((self.source_digests.len() as u64).to_be_bytes());
        for source_digest in &self.source_digests {
            digest.update(source_digest);
        }
        if self.inventory_digest != <[u8; 32]>::from(digest.finalize()) {
            return Err(ProjectionContractError::InventoryRevisionChanged);
        }
        Ok(())
    }
}

/// Proof that a complete authoritative inventory omitted one exact source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertifiedSourceDeletion {
    pub(super) source: SourceKey,
    pub(super) inventory: SourceInventoryObservation,
    pub(super) discovery_revision: String,
    pub(super) inventory_digest: [u8; 32],
    pub(super) observed_sources: u64,
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

    /// Verifies this deletion against the exact complete inventory that
    /// certified it.
    pub fn verifies(&self, inventory: &CertifiedSourceInventory) -> bool {
        self.source.provider == inventory.observation.provider
            && &self.inventory == inventory.observation()
            && self.discovery_revision == inventory.discovery_revision()
            && self.inventory_digest == *inventory.inventory_digest()
            && self.observed_sources == inventory.observed_sources() as u64
            && !inventory.contains(&self.source)
    }

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        self.source.validate_contract()?;
        self.inventory.validate_contract()?;
        validate_text(
            "discovery_revision",
            &self.discovery_revision,
            MAX_PARSER_REVISION_BYTES,
        )?;
        if self.source.provider != self.inventory.provider {
            return Err(ProjectionContractError::InventoryProviderMismatch);
        }
        Ok(())
    }
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
