use anyhow::{bail, Result};
use ctx_pro_host_protocol::{Capability, PROTOCOL_FINGERPRINT, PROTOCOL_VERSION};

use super::{
    artifact_delivery::SetupArtifactBundle, client::HelperSmoke, lifecycle::SetupInstallation,
};

pub(super) fn setup_artifact(
    installation: &SetupInstallation,
    artifact: Option<SetupArtifactBundle>,
) -> Result<Option<SetupArtifactBundle>> {
    if artifact.is_none() && !matches!(installation, SetupInstallation::Current(_)) {
        bail!("invalid_response: Pro setup returned no helper artifact for an install or repair");
    }
    Ok(artifact)
}

pub(super) fn validate_staged_helper(smoke: &HelperSmoke) -> Result<()> {
    if smoke.protocol_version != PROTOCOL_VERSION
        || smoke.protocol_fingerprint != PROTOCOL_FINGERPRINT
        || smoke.helper_version.is_empty()
        || smoke.helper_version.len() > 128
        || !smoke
            .capabilities
            .contains(&Capability::EntitlementAuthorization)
        || !smoke.capabilities.contains(&Capability::Status)
    {
        bail!("protocol_mismatch: staged Pro helper failed the activation smoke contract");
    }
    Ok(())
}

pub(super) fn validate_account_state(value: &str) -> Result<()> {
    if !matches!(value, "trial" | "active" | "canceling_paid") {
        bail!("invalid_response: Pro setup returned an unknown account state");
    }
    Ok(())
}
