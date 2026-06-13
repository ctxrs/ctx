use super::*;

pub(crate) trait StoreLeaseGuard: Send + Sync {}

impl<T> StoreLeaseGuard for T where T: Send + Sync {}

impl Store {
    pub(crate) fn with_lease_guard(&self, lease_guard: Arc<dyn StoreLeaseGuard>) -> Self {
        Self {
            pool: self.pool.clone(),
            sqlite_path: self.sqlite_path.clone(),
            event_log: self.event_log.clone(),
            active_head_projection: self.active_head_projection.clone(),
            write_gate: self.write_gate.clone(),
            _lease_guard: Some(lease_guard),
        }
    }
}
