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
    fn new(
        receipt: SourceBackedRefreshReceipt,
        verified_index: Arc<VerifiedIndex>,
    ) -> Result<Arc<Self>> {
        if verified_index.generation_id() != receipt.published_generation {
            bail!(
                "cannot bind verified generation {} to Core publication receipt {}",
                verified_index.generation_id(),
                receipt.published_generation
            );
        }
        Ok(Arc::new(Self {
            receipt,
            verified_index,
        }))
    }

    pub(crate) fn generation_id(&self) -> &str {
        &self.receipt.published_generation
    }

    #[cfg(test)]
    pub(crate) fn receipt(&self) -> &SourceBackedRefreshReceipt {
        &self.receipt
    }

    pub(in crate::semantic) fn verified_index_ref(&self) -> &VerifiedIndex {
        self.verified_index.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn verified_index(&self) -> Option<Arc<VerifiedIndex>> {
        Some(Arc::clone(&self.verified_index))
    }
}

/// The terminal success admitted by the coordinator state machine.
///
/// In production this has exactly one representation: a truthful receipt and
/// its generation-matching retained `VerifiedIndex` authority. The state-only
/// representation exists solely for unit tests of queue/status transitions
/// that use synthetic generation labels instead of on-disk indexes.
pub(super) enum CoreRefreshTerminalSuccess {
    Verified(Arc<PinnedCorePublication>),
    #[cfg(test)]
    StateOnly(Box<SourceBackedRefreshReceipt>),
}

impl CoreRefreshTerminalSuccess {
    pub(super) fn bind(
        receipt: SourceBackedRefreshReceipt,
        verified_index: Arc<VerifiedIndex>,
    ) -> Result<Self> {
        Ok(Self::Verified(PinnedCorePublication::new(
            receipt,
            verified_index,
        )?))
    }

    #[cfg(test)]
    pub(super) fn state_only(receipt: SourceBackedRefreshReceipt) -> Self {
        Self::StateOnly(Box::new(receipt))
    }

    /// Installs retained authority before returning the receipt that may be
    /// exposed as Published/current under the same state lock.
    pub(super) fn install(self, state: &mut CoreRefreshEngineState) -> SourceBackedRefreshReceipt {
        match self {
            Self::Verified(authority) => {
                let receipt = authority.receipt.clone();
                state.pinned_core_publication = Some(authority);
                receipt
            }
            #[cfg(test)]
            Self::StateOnly(receipt) => *receipt,
        }
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
}
