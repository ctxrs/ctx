use super::*;

/// Outcome of persisting the admission journal at the pre-ack boundary.
///
/// A retained outcome means replacement is visible or its durability is
/// indeterminate, so the stable request identity must remain admitted and be
/// acknowledged. A failed outcome is known to precede replacement and may be
/// rolled back.
pub enum DurableAdmissionPersistence {
    Confirmed,
    Retained(anyhow::Error),
    Failed(anyhow::Error),
}

/// Durable queue storage supplied by the hosting process.
///
/// `store_before_ack` is the sole admission durability boundary. Implementors
/// may perform stronger directory durability there than for later mutable
/// status updates, while preserving one identical journal document contract.
pub trait RefreshJournal: Send + Sync {
    fn load(&self, data_root: &Path) -> Result<Option<Value>>;

    fn store(&self, data_root: &Path, value: &Value) -> Result<()>;

    fn store_before_ack(&self, data_root: &Path, value: &Value) -> DurableAdmissionPersistence;
}
