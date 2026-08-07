use super::*;

/// The only in-memory publication authority retained by the Core refresh
/// engine: one terminal receipt bound to its exact verified index pin.
pub struct PinnedCorePublication {
    receipt: SourceBackedRefreshReceipt,
    verified_index: Arc<VerifiedIndex>,
    query_ready: bool,
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
        let query_ready = verified_generation_is_query_ready(&verified_index)?;
        Ok(Arc::new(Self {
            receipt,
            verified_index,
            query_ready,
        }))
    }

    pub fn generation_id(&self) -> &str {
        &self.receipt.published_generation
    }

    #[cfg(test)]
    pub(crate) fn receipt(&self) -> &SourceBackedRefreshReceipt {
        &self.receipt
    }

    pub fn verified_index_ref(&self) -> &VerifiedIndex {
        self.verified_index.as_ref()
    }

    pub fn is_query_ready(&self) -> bool {
        self.query_ready
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
    #[cfg(any(test, feature = "test-support"))]
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

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn state_only(receipt: SourceBackedRefreshReceipt) -> Self {
        Self::StateOnly(Box::new(receipt))
    }

    pub(super) fn publication_receipt(&self) -> Option<&SourceBackedRefreshReceipt> {
        match self {
            Self::Verified(authority) => Some(&authority.receipt),
            #[cfg(any(test, feature = "test-support"))]
            Self::StateOnly(_) => None,
        }
    }

    pub(super) fn request_source_count(&self, receipt: &SourceBackedRefreshReceipt) -> usize {
        match self {
            Self::Verified(authority) => receipt.source_count(authority.verified_index_ref()),
            #[cfg(any(test, feature = "test-support"))]
            Self::StateOnly(_) => receipt.state_only_source_count(),
        }
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
            #[cfg(any(test, feature = "test-support"))]
            Self::StateOnly(receipt) => *receipt,
        }
    }
}

impl CoreRefreshEngine {
    pub fn pinned_core_publication(&self) -> Option<Arc<PinnedCorePublication>> {
        self.lock_state()
            .pinned_core_publication
            .as_ref()
            .filter(|authority| authority.is_query_ready())
            .map(Arc::clone)
    }
}
