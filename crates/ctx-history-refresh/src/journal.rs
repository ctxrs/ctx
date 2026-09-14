use super::*;

/// Outcome of persisting the request journal at a durable boundary.
///
/// A retained outcome means replacement is visible or its durability is
/// indeterminate, so an admission keeps its stable request identity and retries
/// confirmation before acknowledgement. A failed admission may be rolled back.
/// At terminal completion both errors leave the exact terminal pending.
pub enum DurableAdmissionPersistence {
    Confirmed,
    Retained(anyhow::Error),
    Failed(anyhow::Error),
}

/// Durable queue storage supplied by the hosting process.
///
/// `store_before_ack` is the durability boundary for admissions and every
/// terminal-bearing replacement, including later overlays. Only Confirmed
/// permits acknowledgement or terminal completion. Mutable nonterminal status
/// uses `store`, preserving one identical journal document contract.
pub trait RefreshJournal: Send + Sync {
    fn load(&self, data_root: &Path) -> Result<Option<Value>>;

    fn store(&self, data_root: &Path, value: &Value) -> Result<()>;

    fn store_before_ack(&self, data_root: &Path, value: &Value) -> DurableAdmissionPersistence;
}
