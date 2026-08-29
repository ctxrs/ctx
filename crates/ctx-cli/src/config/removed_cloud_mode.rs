#[derive(Debug, thiserror::Error)]
#[error("unknown config key `cloud.mode`: cloud history configuration is no longer supported")]
pub(super) struct RemovedCloudModeConfigError;

pub(crate) fn is_removed_cloud_mode_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RemovedCloudModeConfigError>()
        .is_some()
}
