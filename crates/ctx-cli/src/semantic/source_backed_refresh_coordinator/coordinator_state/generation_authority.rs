use super::*;

/// The only in-memory publication authority retained by the Core refresh
/// engine: one terminal receipt bound to its exact verified index pin.
pub(crate) struct PinnedCorePublication {
    receipt: SourceBackedRefreshReceipt,
    verified_index: Arc<VerifiedIndex>,
}

impl fmt::Debug for PinnedCorePublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PinnedCorePublication")
            .field("generation_id", &self.receipt.published_generation)
            .field("generation_changed", &self.receipt.generation_changed)
            .finish_non_exhaustive()
    }
}

impl PinnedCorePublication {
    pub(crate) fn generation_id(&self) -> &str {
        &self.receipt.published_generation
    }

    #[cfg(test)]
    pub(crate) fn receipt(&self) -> &SourceBackedRefreshReceipt {
        &self.receipt
    }

    pub(crate) fn verified_index(&self) -> Option<Arc<VerifiedIndex>> {
        Some(Arc::clone(&self.verified_index))
    }
}

impl CoreRefreshEngine {
    pub(in crate::semantic) fn pinned_core_publication(
        &self,
    ) -> Option<Arc<PinnedCorePublication>> {
        self.lock_state()
            .pinned_core_publication
            .as_ref()
            .map(Arc::clone)
    }

    pub(super) fn bind_core_publication(
        &self,
        receipt: SourceBackedRefreshReceipt,
        verified_index: Arc<VerifiedIndex>,
    ) -> Result<()> {
        if verified_index.generation_id() != receipt.published_generation {
            bail!(
                "cannot bind verified generation {} to Core publication receipt {}",
                verified_index.generation_id(),
                receipt.published_generation
            );
        }
        self.lock_state().pinned_core_publication = Some(Arc::new(PinnedCorePublication {
            receipt,
            verified_index,
        }));
        Ok(())
    }
}
