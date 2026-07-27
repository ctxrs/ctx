use ctx_history_store::{ProviderSourceLocatorObservation, Store};

use crate::{CaptureError, Result};

use super::super::source_relocation::CanonicalProviderSourceOverride;
use super::admission::CapturedSourceAdmission;

impl CapturedSourceAdmission {
    pub(super) fn reconcile_provider_locator(
        &self,
        store: &Store,
        observed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        if self.locator_resolution.borrow().is_some() {
            return Ok(());
        }
        let proposed_source_identity =
            self.scope
                .stable_source_identity()
                .ok_or(CaptureError::SystemInvariant(
                    "captured provider source has no stable identity",
                ))?;
        let resolution =
            store.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: self.source.provider(),
                source_format: self.source.source_format().to_owned(),
                machine_id: self.scope.machine_id.clone(),
                locator_identity: self.source.source_identity().to_owned(),
                cursor_stream: self.source.cursor_stream().to_owned(),
                proposed_source_identity,
                raw_source_path: self.scope.raw_source_path.clone(),
                source_revision: self.source.source_revision().to_owned(),
                observed_at_ms: observed_at.timestamp_millis(),
            })?;
        self.locator_resolution.replace(Some(resolution));
        Ok(())
    }

    pub(super) fn stable_source_identity(&self) -> Option<String> {
        self.locator_resolution
            .borrow()
            .as_ref()
            .map(|resolution| resolution.canonical_source_identity.clone())
            .or_else(|| self.scope.stable_source_identity())
    }

    fn stable_session_identity(&self) -> Option<String> {
        self.scope
            .stable_session_identity()
            .or_else(|| self.stable_source_identity())
    }

    fn source_was_relocated(&self) -> bool {
        self.locator_resolution
            .borrow()
            .as_ref()
            .is_some_and(|resolution| resolution.relocated)
    }

    pub(super) fn canonical_source_override(&self) -> Option<CanonicalProviderSourceOverride> {
        Some(CanonicalProviderSourceOverride {
            stable_source_identity: self.stable_source_identity()?,
            stable_session_identity: self.stable_session_identity()?,
            machine_id: self.scope.machine_id.clone(),
            uses_relocation_alias: self.source_was_relocated(),
        })
    }
}
