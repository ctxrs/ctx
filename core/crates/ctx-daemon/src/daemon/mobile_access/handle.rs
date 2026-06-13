use super::{lifecycle, profiles, runtime, MobileAccessStatusError, StartMobileTunnelRequest};
use crate::daemon::{MobileRuntimeHandle, MobileStoreHandle};
use chrono::{DateTime, Utc};
use ctx_core::ids::{ConnectionProfileId, MobileDeviceId, WorkspaceId};
use ctx_core::models::{MobileConnectionProfile, MobileDeviceRegistration};
use ctx_mobile_access_service::{
    route_contract::{
        CreateMobileConnectionProfileForRouteRequest, CreateMobileConnectionProfileForRouteResult,
        DisableMobileAccessError, EnableMobileAccessRequest, EnableMobileAccessResult,
        MobileAccessRouteError, MobileAccessRouteErrorKind, MobileAccessStatusSnapshot,
        MobileConnectionProfileRouteParams, MobileSecureEnvelope, MobileSecureEnvelopeForRoute,
        MobileSecureWorkspaceStreamRouteParams, PairMobileDeviceRequest,
        RegisterMobileDeviceForRouteRequest,
    },
    MobileAccessConfigSnapshot, MobileAccessConfigUpsert, MobileAuthContext,
    MobileAuthContextError, MobileDeviceRegistrationUpdate, MobileDeviceSequenceAdvance,
    MobileScope, MobileSecureProxyResponsePayload, MobileSecureResponseEncryption,
    MobileSecureStreamAccessError, MobileSecureWorkspaceStreamAdmission,
    OpenMobileSecureRequestResult,
};

impl MobileRuntimeHandle {
    pub async fn enable_mobile_access_for_route(
        &self,
        request: EnableMobileAccessRequest,
    ) -> Result<EnableMobileAccessResult, MobileAccessRouteError> {
        lifecycle::enable_mobile_access_for_route(
            self.store(),
            self.mobile_tunnel(),
            self.daemon_url(),
            self.auth_token_configured(),
            request,
        )
        .await
    }

    pub async fn disable_mobile_access_for_route(
        &self,
        request: EnableMobileAccessRequest,
    ) -> Result<(), DisableMobileAccessError> {
        lifecycle::disable_mobile_access_for_route(
            self.store(),
            self.mobile_tunnel(),
            request,
        )
        .await
    }
}

impl MobileStoreHandle {
    pub async fn create_mobile_connection_profile_for_route(
        &self,
        request: CreateMobileConnectionProfileForRouteRequest,
    ) -> Result<CreateMobileConnectionProfileForRouteResult, MobileAccessRouteError> {
        profiles::create_mobile_connection_profile_for_route(self.store(), request).await
    }

    pub async fn list_mobile_connection_profiles_for_route(
        &self,
    ) -> Result<Vec<MobileConnectionProfile>, MobileAccessRouteError> {
        profiles::list_mobile_connection_profiles_for_route(self.store()).await
    }

    pub async fn delete_mobile_connection_profile_for_route(
        &self,
        profile_id: ConnectionProfileId,
    ) -> Result<(), MobileAccessRouteError> {
        profiles::delete_mobile_connection_profile_for_route(self.store(), profile_id).await
    }

    pub async fn delete_mobile_connection_profile_for_route_params(
        &self,
        params: MobileConnectionProfileRouteParams,
    ) -> Result<(), MobileAccessRouteError> {
        profiles::delete_mobile_connection_profile_for_route_params(self.store(), params).await
    }

    pub async fn list_mobile_devices_for_profile_for_route(
        &self,
        profile_id: ConnectionProfileId,
    ) -> Result<Vec<MobileDeviceRegistration>, MobileAccessRouteError> {
        profiles::list_mobile_devices_for_profile_for_route(self.store(), profile_id).await
    }

    pub async fn list_mobile_devices_for_profile_for_route_params(
        &self,
        params: MobileConnectionProfileRouteParams,
    ) -> Result<Vec<MobileDeviceRegistration>, MobileAccessRouteError> {
        profiles::list_mobile_devices_for_profile_for_route_params(self.store(), params).await
    }

    pub async fn register_mobile_device_for_route(
        &self,
        auth: MobileAuthContext,
        request: RegisterMobileDeviceForRouteRequest,
    ) -> Result<MobileDeviceRegistration, MobileAccessRouteError> {
        profiles::register_mobile_device_for_route(self.store(), auth, request).await
    }

    pub async fn pair_mobile_device_for_route(
        &self,
        request: PairMobileDeviceRequest,
    ) -> Result<MobileSecureEnvelope, MobileAccessRouteError> {
        ctx_mobile_access_service::pair_mobile_device(self.store(), request)
            .await
            .map_err(Into::into)
    }

    pub async fn open_mobile_secure_request_for_route(
        &self,
        request: MobileSecureEnvelopeForRoute,
    ) -> Result<OpenMobileSecureRequestResult, MobileAccessRouteError> {
        ctx_mobile_access_service::open_mobile_secure_request(self.store(), request)
            .await
            .map_err(Into::into)
    }

    pub async fn encrypt_mobile_secure_response_for_route(
        &self,
        context: MobileSecureResponseEncryption,
        response: MobileSecureProxyResponsePayload,
    ) -> Result<MobileSecureEnvelope, MobileAccessRouteError> {
        ctx_mobile_access_service::encrypt_mobile_secure_response(context, response)
            .await
            .map_err(Into::into)
    }

    pub async fn create_mobile_connection_profile(
        &self,
        label: String,
        base_url: String,
        token_hash: String,
        token_prefix: String,
        scopes: Vec<String>,
    ) -> anyhow::Result<MobileConnectionProfile> {
        self.store()
            .create_mobile_connection_profile(label, base_url, token_hash, token_prefix, scopes)
            .await
    }

    pub async fn list_mobile_connection_profiles(
        &self,
    ) -> anyhow::Result<Vec<MobileConnectionProfile>> {
        self.store().list_mobile_connection_profiles().await
    }

    pub async fn get_mobile_connection_profile(
        &self,
        profile_id: ConnectionProfileId,
    ) -> anyhow::Result<Option<MobileConnectionProfile>> {
        self.store().get_mobile_connection_profile(profile_id).await
    }

    pub async fn update_mobile_connection_profile_scopes(
        &self,
        profile_id: ConnectionProfileId,
        scopes: Vec<String>,
    ) -> anyhow::Result<()> {
        self.store()
            .update_mobile_connection_profile_scopes(profile_id, scopes)
            .await
    }

    pub async fn delete_mobile_connection_profile(
        &self,
        profile_id: ConnectionProfileId,
    ) -> anyhow::Result<()> {
        self.store()
            .delete_mobile_connection_profile(profile_id)
            .await
    }

    pub async fn get_mobile_access_config(
        &self,
    ) -> anyhow::Result<Option<MobileAccessConfigSnapshot>> {
        self.store()
            .get_mobile_access_config()
            .await
            .map(|config| config.map(Into::into))
    }

    pub async fn upsert_mobile_access_config(
        &self,
        config: MobileAccessConfigUpsert,
    ) -> anyhow::Result<MobileAccessConfigSnapshot> {
        self.store()
            .upsert_mobile_access_config(config.into_store_config())
            .await
            .map(Into::into)
    }

    pub async fn insert_mobile_pairing_token(
        &self,
        token_id: &str,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.store()
            .insert_mobile_pairing_token(token_id, token_hash, expires_at)
            .await
    }

    pub async fn consume_mobile_pairing_token(&self, token_hash: &str) -> anyhow::Result<bool> {
        self.store().consume_mobile_pairing_token(token_hash).await
    }

    pub async fn list_mobile_devices(
        &self,
        profile_id: ConnectionProfileId,
    ) -> anyhow::Result<Vec<MobileDeviceRegistration>> {
        self.store().list_mobile_devices(profile_id).await
    }

    pub async fn get_mobile_device(
        &self,
        device_id: MobileDeviceId,
    ) -> anyhow::Result<Option<MobileDeviceRegistration>> {
        self.store().get_mobile_device(device_id).await
    }

    pub async fn upsert_mobile_device(
        &self,
        device_id: MobileDeviceId,
        profile_id: ConnectionProfileId,
        update: MobileDeviceRegistrationUpdate,
    ) -> anyhow::Result<MobileDeviceRegistration> {
        self.store()
            .upsert_mobile_device(device_id, profile_id, update.into())
            .await
    }

    pub async fn advance_mobile_device_seq(
        &self,
        device_id: MobileDeviceId,
        seq: i64,
    ) -> anyhow::Result<MobileDeviceSequenceAdvance> {
        self.store()
            .advance_mobile_device_seq(device_id, seq)
            .await
            .map(Into::into)
    }

    pub async fn load_mobile_auth_context_for_profile(
        &self,
        profile_id: ConnectionProfileId,
    ) -> Result<Option<MobileAuthContext>, MobileAuthContextError> {
        ctx_mobile_access_service::load_mobile_auth_context_for_profile(self.store(), profile_id)
            .await
    }

    pub async fn require_mobile_secure_stream_access(
        &self,
        workspace_id: WorkspaceId,
        device_id: &str,
        token: &str,
    ) -> Result<(), MobileSecureStreamAccessError> {
        ctx_mobile_access_service::require_mobile_secure_stream_access(
            self.store(),
            workspace_id,
            device_id,
            token,
        )
        .await
    }

    pub async fn admit_mobile_secure_workspace_stream_for_route(
        &self,
        params: MobileSecureWorkspaceStreamRouteParams,
    ) -> Result<MobileSecureWorkspaceStreamAdmission, MobileAccessRouteError> {
        ctx_mobile_access_service::admit_mobile_secure_workspace_stream(self.store(), params)
            .await
            .map_err(mobile_secure_stream_access_route_error)
    }
}

impl MobileRuntimeHandle {
    pub async fn mobile_access_status(
        &self,
    ) -> Result<MobileAccessStatusSnapshot, MobileAccessStatusError> {
        runtime::mobile_access_status(self.store(), self.mobile_tunnel()).await
    }

    pub async fn disable_mobile_access_runtime(&self) -> Result<(), DisableMobileAccessError> {
        runtime::disable_mobile_access_runtime(self.store(), self.mobile_tunnel()).await
    }

    pub async fn start_mobile_tunnel_best_effort(&self, request: StartMobileTunnelRequest) {
        runtime::start_mobile_tunnel_best_effort(self.mobile_tunnel(), self.daemon_url(), request)
            .await;
    }
}

fn mobile_secure_stream_access_route_error(
    error: MobileSecureStreamAccessError,
) -> MobileAccessRouteError {
    match error {
        MobileSecureStreamAccessError::BadDeviceId => {
            MobileAccessRouteError::bad_request("device_id must be a UUID")
        }
        MobileSecureStreamAccessError::BadWorkspaceId => {
            MobileAccessRouteError::bad_request("invalid workspace id")
        }
        MobileSecureStreamAccessError::Unauthorized => {
            MobileAccessRouteError::unauthorized(MobileScope::WorkspaceStream.missing_error())
        }
        MobileSecureStreamAccessError::NotFound => {
            MobileAccessRouteError::new(MobileAccessRouteErrorKind::NotFound, "workspace not found")
        }
        MobileSecureStreamAccessError::Store => {
            MobileAccessRouteError::internal("failed to authorize mobile stream")
        }
    }
}
