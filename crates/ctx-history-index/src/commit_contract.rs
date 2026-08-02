use ctx_history_core::{CertifiedSource, CertifiedSourceDeletion};

use crate::contracts::{GenerationManifest, Result};

#[derive(Debug, Clone)]
pub struct CommitReceipt {
    pub generation_id: String,
    pub opstamp: u64,
    pub indexed_documents: u64,
    pub certified_sources: usize,
    pub certified_source_bytes: u64,
    manifest: GenerationManifest,
}

impl CommitReceipt {
    pub(crate) fn from_manifest(opstamp: u64, manifest: GenerationManifest) -> Result<Self> {
        Ok(Self {
            generation_id: manifest.generation_id()?,
            opstamp,
            indexed_documents: manifest.indexed_documents,
            certified_sources: manifest.sources.len(),
            certified_source_bytes: manifest.certified_source_bytes,
            manifest,
        })
    }

    /// Returns the exact immutable manifest snapshot published by this commit.
    pub fn manifest(&self) -> &GenerationManifest {
        &self.manifest
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}
