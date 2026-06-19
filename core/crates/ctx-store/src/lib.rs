pub mod manager;
pub mod store;

pub use manager::{StoreManager, StoreManagerConfig, StoreManagerStats};
pub use store::{
    is_unique_constraint_violation, AgentWorkImportBatchResult, Store, StoreStats,
    WorktreeBootstrapResultUpdate,
};

#[cfg(feature = "fault_injection")]
pub mod fault_injection;

#[cfg(not(feature = "fault_injection"))]
pub mod fault_injection {
    pub fn clear_failpoints() {}
    pub fn set_failpoint(_point: &'static str, _times: u32) {}
    pub fn maybe_fail(_point: &'static str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
