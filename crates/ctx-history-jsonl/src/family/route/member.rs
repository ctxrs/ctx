use super::*;

#[derive(Debug, Clone)]
pub struct JsonlFamilyRejectedLeaf {
    pub(super) source_path: PathBuf,
    pub(super) authority_path: PathBuf,
    pub(super) observation: Option<JsonlFileObservation>,
    pub(super) proof: TypedKey,
    pub(super) rejected_records: u64,
    pub(super) logical_source_failure: Option<(SourceKey, String)>,
    pub(super) quarantined_source: Option<SourceKey>,
    pub(super) physical_encoding: JsonlPhysicalEncoding,
}

impl JsonlFamilyRejectedLeaf {
    pub fn bind_observed(
        source_path: PathBuf,
        authority_path: PathBuf,
        observation: JsonlFileObservation,
        proof: TypedKey,
        rejected_records: u64,
    ) -> Self {
        Self {
            source_path,
            authority_path,
            observation: Some(observation),
            proof,
            rejected_records,
            logical_source_failure: None,
            quarantined_source: None,
            physical_encoding: JsonlPhysicalEncoding::RawJsonl,
        }
    }

    /// Binds a selected physical member that could not be opened or observed.
    /// The retained root authority and terminal membership observation fence
    /// publication; no unread bytes are admitted as source evidence.
    pub fn bind_unobserved(
        source_path: PathBuf,
        authority_path: PathBuf,
        proof: TypedKey,
        rejected_records: u64,
    ) -> Self {
        Self {
            source_path,
            authority_path,
            observation: None,
            proof,
            rejected_records,
            logical_source_failure: None,
            quarantined_source: None,
            physical_encoding: JsonlPhysicalEncoding::RawJsonl,
        }
    }

    /// Records a file-local failure alongside a rejected membership leaf.
    ///
    /// The supplied key must identify only the physical rejected member, not
    /// an inferred provider session. This lets a family publish trustworthy
    /// peers while the member remains retryable on later discovery.
    pub fn with_logical_source_failure(
        mut self,
        source: SourceKey,
        detail: impl Into<String>,
    ) -> Self {
        self.logical_source_failure = Some((source, detail.into()));
        self
    }

    /// Binds the exact provider source claimed by this physical member.
    /// Core may also fill this from an exact prior certificate at the same
    /// path. It is retention/retry authority only and never admits rejected
    /// bytes to the certified inventory. When several conflicting physical
    /// members claim one source, every member carries this authority while one
    /// deterministic member carries the logical-source diagnostic.
    pub fn with_quarantined_source(mut self, source: SourceKey) -> Self {
        self.quarantined_source = Some(source);
        self
    }

    /// Records the physical encoding needed by the shared first-record probe.
    pub fn with_physical_encoding(mut self, physical_encoding: JsonlPhysicalEncoding) -> Self {
        self.physical_encoding = physical_encoding;
        self
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn source(&self) -> Option<&SourceKey> {
        self.quarantined_source.as_ref()
    }
}

/// Stable, route-independent identity for one physical JSONL member.
///
/// Automatic discovery and an explicit import may reach the same pathname
/// through different route selectors. This identity deliberately excludes
/// selector and route identity so the inventory still has one physical leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JsonlFamilyPhysicalSourceIdentity([u8; 32]);

impl JsonlFamilyPhysicalSourceIdentity {
    pub(super) fn derive(provider: CaptureProvider, source_path: &Path) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"ctx.jsonl-physical-source-v1\0");
        digest.update(provider.as_str().as_bytes());
        digest.update([0]);
        let path = source_path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        Self(digest.finalize().into())
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Explicit state of one member in the canonical physical inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyLeafDisposition {
    Accepted,
    Quarantined,
    Pending,
}

/// A physically present member that does not yet contain enough complete
/// input to admit or quarantine. Pending members remain in terminal membership
/// and are reconsidered on every changed observation.
#[derive(Debug, Clone)]
pub struct JsonlFamilyPendingLeaf {
    pub(super) source_path: PathBuf,
    pub(super) authority_path: PathBuf,
    pub(super) observation: JsonlFileObservation,
    pub(super) proof: TypedKey,
    pub(super) source: Option<SourceKey>,
}

impl JsonlFamilyPendingLeaf {
    pub fn bind_observed(
        source_path: PathBuf,
        authority_path: PathBuf,
        observation: JsonlFileObservation,
        proof: TypedKey,
        source: Option<SourceKey>,
    ) -> Self {
        Self {
            source_path,
            authority_path,
            observation,
            proof,
            source,
        }
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn source(&self) -> Option<&SourceKey> {
        self.source.as_ref()
    }
}

/// One authoritative production entry for a physical JSONL member.
///
/// The variant owns both physical identity and disposition-specific evidence;
/// no parallel accepted/rejected/pending catalogs can disagree with it.
#[derive(Debug)]
pub enum JsonlFamilyInventoryMember<E: JsonlFamilyError> {
    Accepted {
        identity: JsonlFamilyPhysicalSourceIdentity,
        leaf: JsonlFamilyLeaf<E>,
    },
    Quarantined {
        identity: JsonlFamilyPhysicalSourceIdentity,
        leaf: JsonlFamilyRejectedLeaf,
    },
    Pending {
        identity: JsonlFamilyPhysicalSourceIdentity,
        leaf: JsonlFamilyPendingLeaf,
    },
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyInventoryMember<E> {
    fn clone(&self) -> Self {
        match self {
            Self::Accepted { identity, leaf } => Self::Accepted {
                identity: *identity,
                leaf: leaf.clone(),
            },
            Self::Quarantined { identity, leaf } => Self::Quarantined {
                identity: *identity,
                leaf: leaf.clone(),
            },
            Self::Pending { identity, leaf } => Self::Pending {
                identity: *identity,
                leaf: leaf.clone(),
            },
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyInventoryMember<E> {
    pub const fn identity(&self) -> JsonlFamilyPhysicalSourceIdentity {
        match self {
            Self::Accepted { identity, .. }
            | Self::Quarantined { identity, .. }
            | Self::Pending { identity, .. } => *identity,
        }
    }

    pub fn source_path(&self) -> &Path {
        match self {
            Self::Accepted { leaf, .. } => leaf.source_path(),
            Self::Quarantined { leaf, .. } => &leaf.source_path,
            Self::Pending { leaf, .. } => &leaf.source_path,
        }
    }

    pub const fn disposition(&self) -> JsonlFamilyLeafDisposition {
        match self {
            Self::Accepted { .. } => JsonlFamilyLeafDisposition::Accepted,
            Self::Quarantined { .. } => JsonlFamilyLeafDisposition::Quarantined,
            Self::Pending { .. } => JsonlFamilyLeafDisposition::Pending,
        }
    }

    pub fn source(&self) -> Option<&SourceKey> {
        match self {
            Self::Accepted { leaf, .. } => Some(leaf.source()),
            Self::Quarantined { leaf, .. } => leaf.source(),
            Self::Pending { leaf, .. } => leaf.source(),
        }
    }
}
