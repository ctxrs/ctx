use ctx_history_core::{CertifiedSource, CertifiedSourceDeletion};
use std::sync::Arc;

use crate::contracts::{GenerationManifest, IndexError, Result};
use crate::VerifiedIndex;

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
    pub(crate) fn from_manifest(opstamp: u64, manifest: GenerationManifest) -> Result<Self> {
        Self::from_shared_manifest(opstamp, Arc::new(manifest))
    }

    pub(crate) fn from_shared_manifest(
        opstamp: u64,
        manifest: Arc<GenerationManifest>,
    ) -> Result<Self> {
        let generation_id = manifest.generation_id()?;
        Ok(Self::from_verified_manifest(
            opstamp,
            generation_id,
            manifest,
        ))
    }

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

    /// Returns the exact immutable manifest snapshot published by this commit.
    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }

    pub(crate) fn shared_manifest(&self) -> Arc<GenerationManifest> {
        Arc::clone(&self.manifest)
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
/// The manifest and its generation ID are complete, deterministic, and already
/// terminally revalidated. The factory owns the meaning and encoding of the
/// bytes it returns; Core only binds those bytes to a pointer-advancing commit.
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
