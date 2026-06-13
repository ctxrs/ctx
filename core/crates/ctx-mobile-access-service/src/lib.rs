use std::collections::BTreeSet;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use ctx_core::ids::{ConnectionProfileId, MobileDeviceId, WorkspaceId};
use ctx_core::models::{MobileConnectionProfile, MobileDeviceRegistration};
use ctx_store::store::{MobileAccessConfig, MobileDeviceSeqAdvance, MobileDeviceUpsert};
use ctx_store::Store;
use ctx_transport_runtime::mobile_e2ee::E2eeKey;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Digest;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MobileScope {
    RegisterDevice,
    SecureProxy,
    WorkspaceStream,
}

impl MobileScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RegisterDevice => "register_device",
            Self::SecureProxy => "secure_proxy",
            Self::WorkspaceStream => "workspace_stream",
        }
    }

    pub fn missing_error(self) -> &'static str {
        match self {
            Self::RegisterDevice => "mobile token is missing register_device scope",
            Self::SecureProxy => "mobile token is missing secure_proxy scope",
            Self::WorkspaceStream => "mobile token is missing workspace_stream scope",
        }
    }
}

pub fn default_mobile_profile_scopes() -> Vec<String> {
    [
        MobileScope::RegisterDevice,
        MobileScope::SecureProxy,
        MobileScope::WorkspaceStream,
    ]
    .into_iter()
    .map(|scope| scope.as_str().to_string())
    .collect()
}

pub fn mobile_scope_set_from_strings(scopes: &[String]) -> BTreeSet<MobileScope> {
    scopes
        .iter()
        .filter_map(|scope| match scope.trim() {
            "register_device" => Some(MobileScope::RegisterDevice),
            "secure_proxy" => Some(MobileScope::SecureProxy),
            "workspace_stream" => Some(MobileScope::WorkspaceStream),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct MobileAuthContext {
    pub profile: MobileConnectionProfile,
    pub scopes: BTreeSet<MobileScope>,
}

impl MobileAuthContext {
    pub fn has_scope(&self, scope: MobileScope) -> bool {
        self.scopes.contains(&scope)
    }
}

#[derive(Debug, Error)]
pub enum MobileAuthContextError {
    #[error("mobile auth store error")]
    Store,
}

#[derive(Debug, Clone)]
pub struct CreateMobileConnectionProfileRequest {
    pub label: String,
    pub base_url: String,
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct CreateMobileConnectionProfileResult {
    pub profile: MobileConnectionProfile,
    pub token: String,
    pub qr_payload: serde_json::Value,
}

pub async fn create_mobile_connection_profile(
    store: &Store,
    request: CreateMobileConnectionProfileRequest,
) -> Result<CreateMobileConnectionProfileResult> {
    let token = format!("ctxm_{}", uuid::Uuid::new_v4().simple());
    let token_hash = hash_api_token(&token);
    let token_prefix = token.chars().take(12).collect::<String>();
    let scopes = request.scopes.unwrap_or_else(default_mobile_profile_scopes);
    let profile = store
        .create_mobile_connection_profile(
            request.label,
            request.base_url,
            token_hash,
            token_prefix,
            scopes.clone(),
        )
        .await?;
    let qr_payload = json!({
        "type": "ctx_mobile_connection_profile",
        "version": 1,
        "profile_id": profile.id,
        "base_url": profile.base_url,
        "token": token,
        "scopes": scopes,
    });
    Ok(CreateMobileConnectionProfileResult {
        profile,
        token,
        qr_payload,
    })
}

pub async fn list_mobile_connection_profiles(
    store: &Store,
) -> Result<Vec<MobileConnectionProfile>> {
    store.list_mobile_connection_profiles().await
}

pub async fn delete_mobile_connection_profile(
    store: &Store,
    profile_id: ConnectionProfileId,
) -> Result<()> {
    store.delete_mobile_connection_profile(profile_id).await
}

pub async fn list_mobile_devices_for_profile(
    store: &Store,
    profile_id: ConnectionProfileId,
) -> Result<Vec<MobileDeviceRegistration>> {
    store.list_mobile_devices(profile_id).await
}

#[derive(Debug, Clone)]
pub struct MobileDeviceRegistrationUpdate {
    pub device_label: Option<String>,
    pub platform: Option<String>,
    pub push_token: Option<String>,
    pub push_provider: Option<String>,
    pub public_key: Option<String>,
    pub app_version: Option<String>,
}

impl From<MobileDeviceRegistrationUpdate> for MobileDeviceUpsert {
    fn from(value: MobileDeviceRegistrationUpdate) -> Self {
        Self {
            device_label: value.device_label,
            platform: value.platform,
            push_token: value.push_token,
            push_provider: value.push_provider,
            public_key: value.public_key,
            app_version: value.app_version,
        }
    }
}

pub async fn register_mobile_device(
    store: &Store,
    auth: MobileAuthContext,
    update: RegisterMobileDeviceRequest,
) -> Result<MobileDeviceRegistration> {
    let device_id = MobileDeviceId(uuid::Uuid::parse_str(update.device_id.trim())?);
    store
        .upsert_mobile_device(device_id, auth.profile.id, update.update.into())
        .await
}

pub async fn resolve_mobile_auth_context(
    _store: &Store,
    profile: MobileConnectionProfile,
) -> Result<Option<MobileAuthContext>, MobileAuthContextError> {
    Ok(Some(MobileAuthContext {
        scopes: mobile_scope_set_from_strings(&profile.scopes),
        profile,
    }))
}

pub async fn load_mobile_auth_context_for_profile(
    store: &Store,
    profile_id: ConnectionProfileId,
) -> Result<Option<MobileAuthContext>, MobileAuthContextError> {
    let profile = store
        .get_mobile_connection_profile(profile_id)
        .await
        .map_err(|_| MobileAuthContextError::Store)?;
    match profile {
        Some(profile) => resolve_mobile_auth_context(store, profile).await,
        None => Ok(None),
    }
}

pub async fn verify_mobile_api_token_hash(
    store: &Store,
    hash: &str,
) -> Result<Option<MobileAuthContext>, MobileAuthContextError> {
    let profile = store
        .get_mobile_connection_profile_by_token_hash(hash)
        .await
        .map_err(|_| MobileAuthContextError::Store)?;
    let Some(profile) = profile else {
        return Ok(None);
    };
    store
        .mark_mobile_connection_profile_used(profile.id)
        .await
        .map_err(|_| MobileAuthContextError::Store)?;
    resolve_mobile_auth_context(store, profile).await
}

#[derive(Debug, Clone)]
pub struct MobileAccessConfigSnapshot {
    pub id: String,
    pub profile_id: ConnectionProfileId,
    pub tunnel_id: String,
    pub public_base_url: String,
    pub relay_base_url: String,
    pub tunnel_secret: String,
    pub daemon_public_key: String,
    pub daemon_private_key: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<MobileAccessConfig> for MobileAccessConfigSnapshot {
    fn from(value: MobileAccessConfig) -> Self {
        Self {
            id: value.id,
            profile_id: value.profile_id,
            tunnel_id: value.tunnel_id,
            public_base_url: value.public_base_url,
            relay_base_url: value.relay_base_url,
            tunnel_secret: value.tunnel_secret,
            daemon_public_key: value.daemon_public_key,
            daemon_private_key: value.daemon_private_key,
            enabled: value.enabled,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MobileAccessConfigUpsert {
    pub id: String,
    pub profile_id: ConnectionProfileId,
    pub tunnel_id: String,
    pub public_base_url: String,
    pub relay_base_url: String,
    pub tunnel_secret: String,
    pub daemon_public_key: String,
    pub daemon_private_key: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MobileAccessConfigUpsert {
    pub fn into_store_config(self) -> MobileAccessConfig {
        MobileAccessConfig {
            id: self.id,
            profile_id: self.profile_id,
            tunnel_id: self.tunnel_id,
            public_base_url: self.public_base_url,
            relay_base_url: self.relay_base_url,
            tunnel_secret: self.tunnel_secret,
            daemon_public_key: self.daemon_public_key,
            daemon_private_key: self.daemon_private_key,
            enabled: self.enabled,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PersistMobileAccessEnableBootstrapRequest {
    pub public_base_url: String,
    pub relay_base_url: String,
    pub tunnel_id: String,
    pub tunnel_secret: String,
    pub daemon_public_key: String,
    pub daemon_private_key: String,
    pub now: DateTime<Utc>,
    pub pairing_token_ttl_seconds: i64,
}

#[derive(Debug, Clone)]
pub struct PersistMobileAccessEnableBootstrapResult {
    pub config: MobileAccessConfigSnapshot,
    pub pairing_token: String,
    pub pairing_expires_at: DateTime<Utc>,
}

pub async fn persist_mobile_access_enable_bootstrap(
    store: &Store,
    request: PersistMobileAccessEnableBootstrapRequest,
) -> Result<PersistMobileAccessEnableBootstrapResult> {
    let pairing_token = uuid::Uuid::new_v4().simple().to_string();
    let pairing_expires_at = request.now + Duration::seconds(request.pairing_token_ttl_seconds);
    let profile = store
        .create_mobile_connection_profile(
            "Mobile access".to_string(),
            request.public_base_url.clone(),
            hash_api_token(&pairing_token),
            pairing_token.chars().take(8).collect(),
            default_mobile_profile_scopes(),
        )
        .await?;
    let config = MobileAccessConfig {
        id: "default".to_string(),
        profile_id: profile.id,
        tunnel_id: request.tunnel_id,
        public_base_url: request.public_base_url,
        relay_base_url: request.relay_base_url,
        tunnel_secret: request.tunnel_secret,
        daemon_public_key: request.daemon_public_key,
        daemon_private_key: request.daemon_private_key,
        enabled: true,
        created_at: request.now,
        updated_at: request.now,
    };
    let config = store.upsert_mobile_access_config(config).await?.into();
    store
        .insert_mobile_pairing_token(
            &uuid::Uuid::new_v4().to_string(),
            &hash_api_token(&pairing_token),
            pairing_expires_at,
        )
        .await?;
    Ok(PersistMobileAccessEnableBootstrapResult {
        config,
        pairing_token,
        pairing_expires_at,
    })
}

#[derive(Debug, Clone)]
pub struct DisabledMobileAccessState {
    pub profile_id: Option<ConnectionProfileId>,
}

pub async fn persist_mobile_access_disabled_state(
    store: &Store,
) -> Result<DisabledMobileAccessState, route_contract::DisableMobileAccessError> {
    let cfg = store
        .get_mobile_access_config()
        .await
        .map_err(|_| route_contract::DisableMobileAccessError::ReadConfig)?;
    store
        .set_mobile_access_enabled(false)
        .await
        .map_err(|_| route_contract::DisableMobileAccessError::DisableConfig)?;
    store
        .clear_mobile_pairing_tokens()
        .await
        .map_err(|_| route_contract::DisableMobileAccessError::ClearPairingTokens)?;
    Ok(DisabledMobileAccessState {
        profile_id: cfg.map(|cfg| cfg.profile_id),
    })
}

pub async fn finish_mobile_access_disable_cleanup(
    store: &Store,
    disabled: DisabledMobileAccessState,
) -> Result<(), route_contract::DisableMobileAccessError> {
    store
        .delete_mobile_access_config()
        .await
        .map_err(|_| route_contract::DisableMobileAccessError::DeleteConfig)?;
    if let Some(profile_id) = disabled.profile_id {
        store
            .delete_mobile_connection_profile(profile_id)
            .await
            .map_err(|_| route_contract::DisableMobileAccessError::DeleteConnectionProfile)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub enum MobileDeviceSequenceAdvance {
    Advanced,
    Stale { current: i64 },
    Missing,
}

impl From<MobileDeviceSeqAdvance> for MobileDeviceSequenceAdvance {
    fn from(value: MobileDeviceSeqAdvance) -> Self {
        match value {
            MobileDeviceSeqAdvance::Advanced => Self::Advanced,
            MobileDeviceSeqAdvance::Stale { current } => Self::Stale { current },
            MobileDeviceSeqAdvance::Missing => Self::Missing,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegisterMobileDeviceRequest {
    pub device_id: String,
    pub update: MobileDeviceRegistrationUpdate,
}

#[derive(Debug, Clone)]
pub struct MobileSecureProxyPayload {
    pub uri: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MobileSecureProxyResponsePayload {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_b64: String,
}

#[derive(Debug, Clone)]
pub struct MobileSecureResponseEncryption {
    pub device_id: String,
    pub key: E2eeKey,
    pub seq: i64,
}

#[derive(Debug, Clone)]
pub struct OpenMobileSecureRequestResult {
    pub mobile_auth: Option<MobileAuthContext>,
    pub payload: MobileSecureProxyPayload,
    pub response_encryption: MobileSecureResponseEncryption,
}

#[derive(Debug, Clone)]
pub struct MobileSecureProxyAdmitted {
    pub uri: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct MobileSecureProxyDenied {
    message: String,
}

impl MobileSecureProxyDenied {
    pub fn message(&self) -> &str {
        &self.message
    }
}

pub enum MobileSecureProxyAdmission {
    Admitted(MobileSecureProxyAdmitted),
    Denied(MobileSecureProxyDenied),
}

pub fn prepare_mobile_secure_proxy_request(
    mobile_auth: Option<MobileAuthContext>,
    payload: MobileSecureProxyPayload,
) -> Result<MobileSecureProxyAdmission, MobileSecureStreamAccessError> {
    let Some(auth) = mobile_auth else {
        return Ok(MobileSecureProxyAdmission::Denied(
            MobileSecureProxyDenied {
                message: "mobile auth required".to_string(),
            },
        ));
    };
    if !auth.has_scope(MobileScope::SecureProxy) {
        return Ok(MobileSecureProxyAdmission::Denied(
            MobileSecureProxyDenied {
                message: MobileScope::SecureProxy.missing_error().to_string(),
            },
        ));
    }
    Ok(MobileSecureProxyAdmission::Admitted(
        MobileSecureProxyAdmitted {
            uri: payload.uri,
            headers: payload.headers,
        },
    ))
}

pub async fn pair_mobile_device(
    _store: &Store,
    _request: route_contract::PairMobileDeviceRequest,
) -> Result<route_contract::MobileSecureEnvelope, MobileSecureStreamAccessError> {
    Err(MobileSecureStreamAccessError::Unauthorized)
}

pub async fn open_mobile_secure_request(
    _store: &Store,
    _request: route_contract::MobileSecureEnvelopeForRoute,
) -> Result<OpenMobileSecureRequestResult, MobileSecureStreamAccessError> {
    Err(MobileSecureStreamAccessError::Unauthorized)
}

pub async fn encrypt_mobile_secure_response(
    context: MobileSecureResponseEncryption,
    response: MobileSecureProxyResponsePayload,
) -> Result<route_contract::MobileSecureEnvelope, MobileSecureStreamAccessError> {
    let payload =
        serde_json::to_vec(&response).map_err(|_| MobileSecureStreamAccessError::Store)?;
    let envelope = ctx_transport_runtime::mobile_e2ee::encrypt(
        &context.key,
        &context.device_id,
        context.seq + 1,
        &payload,
    )
    .map_err(|_| MobileSecureStreamAccessError::Store)?;
    Ok(route_contract::MobileSecureEnvelope {
        device_id: envelope.device_id,
        seq: envelope.seq,
        nonce: envelope.nonce_b64,
        ciphertext: envelope.ciphertext_b64,
    })
}

#[derive(Debug, Error)]
pub enum MobileSecureStreamAccessError {
    #[error("device_id must be a UUID")]
    BadDeviceId,
    #[error("invalid workspace id")]
    BadWorkspaceId,
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("store error")]
    Store,
}

pub async fn require_mobile_secure_stream_access(
    _store: &Store,
    workspace_id: WorkspaceId,
    device_id: &str,
    _token: &str,
) -> Result<(), MobileSecureStreamAccessError> {
    let _ = workspace_id;
    uuid::Uuid::parse_str(device_id).map_err(|_| MobileSecureStreamAccessError::BadDeviceId)?;
    Err(MobileSecureStreamAccessError::Unauthorized)
}

#[derive(Debug, Clone)]
pub struct MobileSecureWorkspaceStreamAdmission {
    pub workspace_id: WorkspaceId,
    pub context: route_contract::MobileSecureStreamContext,
}

pub async fn admit_mobile_secure_workspace_stream(
    _store: &Store,
    params: route_contract::MobileSecureWorkspaceStreamRouteParams,
) -> Result<MobileSecureWorkspaceStreamAdmission, MobileSecureStreamAccessError> {
    let _workspace_id = params.workspace_id()?;
    let _device_id = params.device_id()?;
    Err(MobileSecureStreamAccessError::Unauthorized)
}

pub mod route_contract {
    use super::*;
    use ctx_transport_runtime::mobile_tunnel::MobileTunnelState;

    #[derive(Debug, Clone, Deserialize)]
    pub struct CreateMobileConnectionProfileForRouteRequest {
        pub label: String,
        pub base_url: String,
        #[serde(default)]
        pub scopes: Vec<String>,
    }

    impl From<CreateMobileConnectionProfileForRouteRequest> for CreateMobileConnectionProfileRequest {
        fn from(value: CreateMobileConnectionProfileForRouteRequest) -> Self {
            Self {
                label: value.label,
                base_url: value.base_url,
                scopes: (!value.scopes.is_empty()).then_some(value.scopes),
            }
        }
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct CreateMobileConnectionProfileForRouteResult {
        pub profile: MobileConnectionProfile,
        pub token: String,
        pub qr_payload: serde_json::Value,
    }

    impl From<CreateMobileConnectionProfileResult> for CreateMobileConnectionProfileForRouteResult {
        fn from(value: CreateMobileConnectionProfileResult) -> Self {
            Self {
                profile: value.profile,
                token: value.token,
                qr_payload: value.qr_payload,
            }
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct EnableMobileAccessRequest {}

    #[derive(Debug, Clone, Serialize)]
    pub struct EnableMobileAccessResult {
        pub status: MobileAccessStatusSnapshot,
        pub qr_payload: serde_json::Value,
        pub pairing_expires_at: DateTime<Utc>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct MobileAccessStatusSnapshot {
        pub enabled: bool,
        pub tunnel_id: Option<String>,
        pub public_base_url: Option<String>,
        pub relay_base_url: Option<String>,
        pub daemon_public_key: Option<String>,
        pub tunnel_state: MobileTunnelState,
        pub last_error: Option<String>,
    }

    #[derive(Debug, Clone)]
    pub struct MobileConnectionProfileRouteParams {
        id: String,
    }

    impl MobileConnectionProfileRouteParams {
        pub fn new(id: String) -> Self {
            Self { id }
        }

        pub fn into_profile_id(self) -> Result<ConnectionProfileId, MobileAccessRouteError> {
            uuid::Uuid::parse_str(self.id.trim())
                .map(ConnectionProfileId)
                .map_err(|_| {
                    MobileAccessRouteError::bad_request("connection profile id must be a UUID")
                })
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct RegisterMobileDeviceForRouteRequest {
        pub device_id: String,
        pub device_label: Option<String>,
        pub platform: Option<String>,
        pub push_token: Option<String>,
        pub push_provider: Option<String>,
        pub public_key: Option<String>,
        pub app_version: Option<String>,
    }

    impl From<RegisterMobileDeviceForRouteRequest> for RegisterMobileDeviceRequest {
        fn from(value: RegisterMobileDeviceForRouteRequest) -> Self {
            Self {
                device_id: value.device_id,
                update: MobileDeviceRegistrationUpdate {
                    device_label: value.device_label,
                    platform: value.platform,
                    push_token: value.push_token,
                    push_provider: value.push_provider,
                    public_key: value.public_key,
                    app_version: value.app_version,
                },
            }
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct PairMobileDeviceRequest {
        pub device_id: String,
        pub public_key: String,
        pub seq: i64,
        pub nonce: String,
        pub ciphertext: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct MobileSecureEnvelope {
        pub device_id: String,
        pub seq: i64,
        pub nonce: String,
        pub ciphertext: String,
    }

    #[derive(Debug, Clone)]
    pub struct MobileSecureEnvelopeForRoute {
        pub device_id: String,
        pub seq: i64,
        pub nonce: String,
        pub ciphertext: String,
    }

    #[derive(Debug, Clone)]
    pub struct MobileSecureWorkspaceStreamRouteParams {
        workspace_id: String,
        device_id: String,
        token: String,
    }

    impl MobileSecureWorkspaceStreamRouteParams {
        pub fn new(workspace_id: String, device_id: String, token: String) -> Self {
            Self {
                workspace_id,
                device_id,
                token,
            }
        }

        pub fn workspace_id(&self) -> Result<WorkspaceId, MobileSecureStreamAccessError> {
            uuid::Uuid::parse_str(self.workspace_id.trim())
                .map(WorkspaceId)
                .map_err(|_| MobileSecureStreamAccessError::BadWorkspaceId)
        }

        pub fn device_id(&self) -> Result<MobileDeviceId, MobileSecureStreamAccessError> {
            uuid::Uuid::parse_str(self.device_id.trim())
                .map(MobileDeviceId)
                .map_err(|_| MobileSecureStreamAccessError::BadDeviceId)
        }

        pub fn token(&self) -> &str {
            &self.token
        }
    }

    #[derive(Debug, Clone)]
    pub struct MobileSecureStreamContext {
        pub device_id: String,
        pub key: E2eeKey,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MobileAccessRouteErrorKind {
        BadRequest,
        Unauthorized,
        Forbidden,
        Conflict,
        NotFound,
        BadGateway,
        Internal,
    }

    #[derive(Debug, Clone)]
    pub struct MobileAccessRouteError {
        kind: MobileAccessRouteErrorKind,
        message: String,
    }

    impl MobileAccessRouteError {
        pub fn new(kind: MobileAccessRouteErrorKind, message: impl Into<String>) -> Self {
            Self {
                kind,
                message: message.into(),
            }
        }

        pub fn bad_request(message: impl Into<String>) -> Self {
            Self::new(MobileAccessRouteErrorKind::BadRequest, message)
        }

        pub fn unauthorized(message: impl Into<String>) -> Self {
            Self::new(MobileAccessRouteErrorKind::Unauthorized, message)
        }

        pub fn internal(message: impl Into<String>) -> Self {
            Self::new(MobileAccessRouteErrorKind::Internal, message)
        }

        pub fn kind(&self) -> MobileAccessRouteErrorKind {
            self.kind
        }

        pub fn message(&self) -> &str {
            &self.message
        }
    }

    impl std::fmt::Display for MobileAccessRouteError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.message)
        }
    }

    impl std::error::Error for MobileAccessRouteError {}

    impl From<anyhow::Error> for MobileAccessRouteError {
        fn from(error: anyhow::Error) -> Self {
            Self::internal(format!("{error:#}"))
        }
    }

    impl From<MobileSecureStreamAccessError> for MobileAccessRouteError {
        fn from(error: MobileSecureStreamAccessError) -> Self {
            match error {
                MobileSecureStreamAccessError::BadDeviceId => {
                    Self::bad_request("device_id must be a UUID")
                }
                MobileSecureStreamAccessError::BadWorkspaceId => {
                    Self::bad_request("invalid workspace id")
                }
                MobileSecureStreamAccessError::Unauthorized => {
                    Self::unauthorized("mobile secure access unauthorized")
                }
                MobileSecureStreamAccessError::NotFound => Self::new(
                    MobileAccessRouteErrorKind::NotFound,
                    "mobile secure target not found",
                ),
                MobileSecureStreamAccessError::Store => {
                    Self::internal("mobile secure access failed")
                }
            }
        }
    }

    #[derive(Debug, Error)]
    pub enum DisableMobileAccessError {
        #[error("failed to read mobile access config")]
        ReadConfig,
        #[error("failed to disable mobile access config")]
        DisableConfig,
        #[error("failed to clear pairing tokens")]
        ClearPairingTokens,
        #[error("failed to delete mobile access config")]
        DeleteConfig,
        #[error("failed to delete mobile connection profile")]
        DeleteConnectionProfile,
    }

    impl From<anyhow::Error> for DisableMobileAccessError {
        fn from(_: anyhow::Error) -> Self {
            Self::DisableConfig
        }
    }
}

fn hash_api_token(token: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}
