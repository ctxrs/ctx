use super::*;
use ctx_daemon::daemon::{MobileRuntimeHandle, MobileSecureProxyHandle, MobileStoreHandle};
use ctx_mobile_access_service::route_contract::{
    CreateMobileConnectionProfileForRouteRequest, EnableMobileAccessRequest,
    MobileAccessRouteError, MobileAccessRouteErrorKind, MobileAccessStatusSnapshot,
    MobileConnectionProfileRouteParams, MobileSecureEnvelopeForRoute, PairMobileDeviceRequest,
    RegisterMobileDeviceForRouteRequest,
};

mod access_disable;
mod access_enable;
mod access_status;
mod body;
mod payloads;
mod profiles;
mod secure;
mod secure_pairing;

pub(in crate::api) use access_disable::disable_mobile_access;
pub(in crate::api) use access_enable::enable_mobile_access;
pub(in crate::api) use access_status::get_mobile_access_status;
use body::parse_json_body;
pub(in crate::api) use payloads::*;
pub(in crate::api) use profiles::{
    create_mobile_connection_profile, delete_mobile_connection_profile,
    list_mobile_connection_profiles, list_mobile_devices_for_profile, register_mobile_device,
};
pub(super) use secure::*;
pub(in crate::api) use secure_pairing::pair_mobile_device;

fn mobile_access_api_error(error: MobileAccessRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = mobile_access_status_code(&error);
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}

fn mobile_access_status_code(error: &MobileAccessRouteError) -> StatusCode {
    match error.kind() {
        MobileAccessRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        MobileAccessRouteErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        MobileAccessRouteErrorKind::Forbidden => StatusCode::FORBIDDEN,
        MobileAccessRouteErrorKind::Conflict => StatusCode::CONFLICT,
        MobileAccessRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        MobileAccessRouteErrorKind::BadGateway => StatusCode::BAD_GATEWAY,
        MobileAccessRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn mobile_access_status_from_snapshot(snapshot: MobileAccessStatusSnapshot) -> MobileAccessStatus {
    MobileAccessStatus {
        enabled: snapshot.enabled,
        tunnel_id: snapshot.tunnel_id,
        public_base_url: snapshot.public_base_url,
        relay_base_url: snapshot.relay_base_url,
        daemon_public_key: snapshot.daemon_public_key,
        tunnel_state: snapshot.tunnel_state,
        last_error: snapshot.last_error,
    }
}
