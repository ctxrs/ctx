use super::*;

/// The terminal success admitted by the coordinator state machine.
///
/// In production this has exactly one representation: a truthful receipt and
/// its generation-matching retained `VerifiedIndex` authority. The state-only
/// representation exists solely for unit tests of queue/status transitions
/// that use synthetic generation labels instead of on-disk indexes.
pub(super) enum CoreRefreshTerminalSuccess {
    Verified(Arc<VerifiedCorePublication>),
    #[cfg(any(test, feature = "test-support"))]
    StateOnly(Box<SourceBackedRefreshReceipt>),
}

impl CoreRefreshTerminalSuccess {
    pub(super) fn verified(publication: Arc<VerifiedCorePublication>) -> Self {
        Self::Verified(publication)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn state_only(receipt: SourceBackedRefreshReceipt) -> Self {
        Self::StateOnly(Box::new(receipt))
    }

    pub(super) fn publication_receipt(&self) -> Option<&SourceBackedRefreshReceipt> {
        match self {
            Self::Verified(authority) => Some(authority.receipt()),
            #[cfg(any(test, feature = "test-support"))]
            Self::StateOnly(_) => None,
        }
    }

    pub(super) fn request_source_count(&self, receipt: &SourceBackedRefreshReceipt) -> usize {
        match self {
            Self::Verified(authority) => receipt.source_count(authority.verified_index()),
            #[cfg(any(test, feature = "test-support"))]
            Self::StateOnly(_) => receipt.state_only_source_count(),
        }
    }

    /// Installs retained authority before returning the receipt that may be
    /// exposed as Published/current under the same state lock.
    pub(super) fn install(self, state: &mut CoreRefreshEngineState) -> SourceBackedRefreshReceipt {
        match self {
            Self::Verified(authority) => {
                let receipt = authority.receipt().clone();
                state.pinned_core_publication = Some(authority);
                receipt
            }
            #[cfg(any(test, feature = "test-support"))]
            Self::StateOnly(receipt) => *receipt,
        }
    }
}

impl CoreRefreshEngine {
    pub fn pinned_core_publication(&self) -> Option<Arc<VerifiedCorePublication>> {
        self.lock_state()
            .pinned_core_publication
            .as_ref()
            .filter(|authority| authority.is_query_ready())
            .map(Arc::clone)
    }
}
