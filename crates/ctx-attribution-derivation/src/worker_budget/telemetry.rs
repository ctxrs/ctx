use super::*;

impl ProviderWorkerBudget {
    pub(crate) fn preparation_peak(&self) -> Result<u16, ProtocolError> {
        let activity = self.lock_activity()?;
        u16::try_from(activity.preparation_peak).map_err(|_| worker_activity_overflow())
    }
}
