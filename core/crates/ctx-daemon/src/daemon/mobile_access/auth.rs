use std::sync::Arc;

use ctx_core::ids::ConnectionProfileId;
use ctx_core::models::MobileConnectionProfile;
pub use ctx_mobile_access_service::{
    default_mobile_profile_scopes, mobile_scope_set_from_strings, MobileAuthContext,
    MobileAuthContextError,
};

use crate::daemon::DaemonState;

pub async fn resolve_mobile_auth_context(
    state: &Arc<DaemonState>,
    profile: MobileConnectionProfile,
) -> Result<Option<MobileAuthContext>, MobileAuthContextError> {
    ctx_mobile_access_service::resolve_mobile_auth_context(state.global_store(), profile).await
}

pub async fn load_mobile_auth_context_for_profile(
    state: &Arc<DaemonState>,
    profile_id: ConnectionProfileId,
) -> Result<Option<MobileAuthContext>, MobileAuthContextError> {
    ctx_mobile_access_service::load_mobile_auth_context_for_profile(
        state.global_store(),
        profile_id,
    )
    .await
}

pub async fn verify_mobile_api_token_hash(
    state: &Arc<DaemonState>,
    hash: &str,
) -> Result<Option<MobileAuthContext>, MobileAuthContextError> {
    ctx_mobile_access_service::verify_mobile_api_token_hash(state.global_store(), hash).await
}
