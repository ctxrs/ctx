use ctx_history_core::{CertifiedSource, CertifiedSourceDeletion};
use std::sync::Arc;

use crate::{GenerationManifest, IndexError, Result, VerifiedIndex};

/// Bounded publication transitions observable without assigning unlike work
/// one fabricated shared denominator.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PublicationStage {
    Merging,
    Syncing,
    PhysicalVerification,
    LogicalVerification,
    Activation,
}

impl PublicationStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Merging => "merging",
            Self::Syncing => "syncing",
            Self::PhysicalVerification => "physical_verification",
            Self::LogicalVerification => "logical_verification",
            Self::Activation => "activation",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommitReceipt {
    pub generation_id: String,
    pub opstamp: u64,
    pub indexed_documents: u64,
    pub certified_sources: usize,
    pub certified_source_bytes: u64,
    manifest: Arc<GenerationManifest>,
}

impl CommitReceipt {
    pub(crate) fn from_verified_manifest(
        opstamp: u64,
        generation_id: String,
        manifest: Arc<GenerationManifest>,
    ) -> Self {
        Self {
            generation_id,
            opstamp,
            indexed_documents: manifest.indexed_documents,
            certified_sources: manifest.sources.len(),
            certified_source_bytes: manifest.certified_source_bytes,
            manifest,
        }
    }

    /// Returns the exact immutable logical manifest snapshot published by this
    /// commit.
    ///
    /// [`Self::generation_id`] names the persisted manifest descriptor. A
    /// compact descriptor may materialize to this full logical snapshot, so
    /// recomputing [`GenerationManifest::generation_id`] from the returned
    /// value need not reproduce the descriptor ID.
    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }

    /// Moves the exact commit facts and shared immutable manifest out without
    /// cloning the generation ID or incrementing the manifest reference count.
    pub fn into_parts(self) -> (String, u64, u64, usize, u64, Arc<GenerationManifest>) {
        let Self {
            generation_id,
            opstamp,
            indexed_documents,
            certified_sources,
            certified_source_bytes,
            manifest,
        } = self;
        (
            generation_id,
            opstamp,
            indexed_documents,
            certified_sources,
            certified_source_bytes,
            manifest,
        )
    }
}

/// Whether a commit call advanced the durable generation pointer or reused the
/// exact active generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationDisposition {
    Published,
    Reused,
}

/// Final logical publication facts available to an opaque metadata factory.
///
/// The logical manifest and persisted descriptor generation ID are complete
/// and deterministic. A compact descriptor's ID need not equal the canonical
/// full-manifest ID obtained from [`GenerationManifest::generation_id`]. Core
/// invokes the factory inside the publication fence, then terminally
/// revalidates every source and inventory before binding the returned bytes to
/// a pointer-advancing commit.
#[derive(Debug, Clone, Copy)]
pub struct PublicationMetadataContext<'a> {
    generation_id: &'a str,
    manifest: &'a GenerationManifest,
}

impl<'a> PublicationMetadataContext<'a> {
    pub(crate) fn new(generation_id: &'a str, manifest: &'a GenerationManifest) -> Self {
        Self {
            generation_id,
            manifest,
        }
    }

    pub fn generation_id(self) -> &'a str {
        self.generation_id
    }

    pub fn manifest(self) -> &'a GenerationManifest {
        self.manifest
    }
}

/// One exact committed or reused generation and its already-open verified pin.
pub struct PublishedGeneration {
    receipt: CommitReceipt,
    disposition: PublicationDisposition,
    verified_index: VerifiedIndex,
}

impl std::fmt::Debug for PublishedGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublishedGeneration")
            .field("generation_id", &self.receipt.generation_id)
            .field("disposition", &self.disposition)
            .finish_non_exhaustive()
    }
}

impl PublishedGeneration {
    pub(crate) fn new(
        receipt: CommitReceipt,
        disposition: PublicationDisposition,
        verified_index: VerifiedIndex,
    ) -> Result<Self> {
        if receipt.generation_id != verified_index.generation_id() {
            return Err(IndexError::WriterInvariant(
                "published receipt and verified index name different generations",
            ));
        }
        Ok(Self {
            receipt,
            disposition,
            verified_index,
        })
    }

    pub fn receipt(&self) -> &CommitReceipt {
        &self.receipt
    }

    pub fn disposition(&self) -> PublicationDisposition {
        self.disposition
    }

    pub fn verified_index(&self) -> &VerifiedIndex {
        &self.verified_index
    }

    pub fn into_parts(self) -> (CommitReceipt, PublicationDisposition, VerifiedIndex) {
        (self.receipt, self.disposition, self.verified_index)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}
