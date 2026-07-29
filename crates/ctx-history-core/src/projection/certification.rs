use serde::{Deserialize, Serialize};

use super::errors::{
    validate_bytes, validate_nonempty_bytes, validate_text, ProjectionContractError,
    ProjectionContractResult, MAX_KEY_NAMESPACE_BYTES, MAX_PARSER_REVISION_BYTES,
    MAX_REVISION_BYTES, MAX_REVISION_KIND_BYTES, MAX_TYPED_KEY_BYTES,
};
use super::native::{encode_typed_key, TypedKey};
use super::source::SourceObservation;

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

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
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
