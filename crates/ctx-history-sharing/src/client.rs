use std::{io::Read, time::Duration};

use ctx_history_server::{
    AccessListRequest, CancelPublishRequest, CancelPublishResponse, CollectionStatus,
    ConnectionIdentity, CredentialPage, EnrollRequest, EnrollmentFile, GrantRequest, HostedEvent,
    InviteRequest, PrincipalPage, PublicationListRequest, PublicationPage, PublicationState,
    PublishRequest, Receipt, SearchResponse, SessionPage, TokenFile, UploadSpec, UploadStatus,
    WithdrawRequest,
};
use serde::{de::DeserializeOwned, Serialize};
use url::Url;

use crate::{Connection, Credentials, Endpoint, Error, Result, SharingStore};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
pub const UPLOAD_CHUNK_BYTES: usize = 1024 * 1024;
// This bounds a single response/page, not retained history or a whole session.
// The server allows a Core page up to its record byte contract, then adds
// original-source provenance and citations. Reserve a second Core budget for
// duplicated source descriptors plus bounded per-result envelope overhead.
const RESPONSE_BYTES: u64 =
    2 * ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES as u64 + 4 * 1024 * 1024;

/// Authenticated remote reads and publication. Has no local index, daemon,
/// provider, implicit endpoint, cookie jar, redirect, or external model owner.
pub struct RemoteClient {
    connection: Connection,
    credentials: Credentials,
    agent: ureq::Agent,
}

impl std::fmt::Debug for RemoteClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteClient")
            .field("connection", &self.connection)
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}

impl RemoteClient {
    /// Constructing a client never performs a request.
    pub fn new(connection: Connection, credentials: Credentials) -> Result<Self> {
        credentials.validate()?;
        if connection.collection.is_empty() {
            return Err(Error::InvalidConfig);
        }
        Ok(Self {
            connection,
            credentials,
            agent: agent(),
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Explicit one-use enrollment exchange. It neither saves credentials nor
    /// enables a sharing policy. The returned secret is for protected storage.
    pub fn enroll(endpoint: &Endpoint, enrollment: &str) -> Result<TokenFile> {
        let request = EnrollRequest {
            enrollment: enrollment.to_owned(),
        };
        let bytes = serde_json::to_vec(&request).map_err(|_| Error::Credentials)?;
        let url = endpoint.route(&["v1", "enroll"])?;
        decode(
            agent()
                .post(url.as_str())
                .set("Content-Type", "application/json")
                .send_bytes(&bytes),
        )
    }

    pub fn invite(&self, request: &InviteRequest) -> Result<EnrollmentFile> {
        self.admin_post(self.url(&["invite"])?, request)
    }

    /// Authenticate the credential used for publishing, or the read credential
    /// on a reader-only connection. The server owns identity and scope checks.
    pub fn whoami(&self) -> Result<ConnectionIdentity> {
        self.get(self.url(&["whoami"])?, self.credentials.publish().is_ok())
    }

    pub fn list_principals(&self, request: &AccessListRequest) -> Result<PrincipalPage> {
        let url = self.connection.endpoint.route(&["v1", "principals"])?;
        self.access_page(url, request)
    }

    pub fn list_credentials(
        &self,
        principal: &str,
        request: &AccessListRequest,
    ) -> Result<CredentialPage> {
        let url =
            self.connection
                .endpoint
                .route(&["v1", "principals", principal, "credentials"])?;
        self.access_page(url, request)
    }

    pub fn admin_revoke_principal(&self, principal: &str) -> Result<()> {
        let url = self
            .connection
            .endpoint
            .route(&["v1", "principals", principal, "revoke"])?;
        let _: serde_json::Value = self.admin_post(url, &serde_json::json!({}))?;
        Ok(())
    }

    pub fn admin_revoke_credential(&self, credential_id: &str) -> Result<()> {
        let url =
            self.connection
                .endpoint
                .route(&["v1", "credentials", credential_id, "revoke"])?;
        let _: serde_json::Value = self.admin_post(url, &serde_json::json!({}))?;
        Ok(())
    }

    fn access_page<T: DeserializeOwned>(
        &self,
        mut url: Url,
        request: &AccessListRequest,
    ) -> Result<T> {
        if !(1..=100).contains(&request.limit) {
            return Err(Error::InvalidConfig);
        }
        url.query_pairs_mut()
            .append_pair("limit", &request.limit.to_string());
        if let Some(after) = &request.after {
            url.query_pairs_mut().append_pair("after", after);
        }
        self.get(url, self.credentials.publish().is_ok())
    }

    pub fn grant(&self, request: &GrantRequest) -> Result<()> {
        let _: serde_json::Value = self.admin_post(self.url(&["grants"])?, request)?;
        Ok(())
    }

    pub fn revoke_member(&self, principal: &str) -> Result<()> {
        let _: serde_json::Value = self.admin_post(
            self.url(&["members", principal, "revoke"])?,
            &serde_json::json!({}),
        )?;
        Ok(())
    }

    pub fn publication(&self, publication: &str) -> Result<PublicationState> {
        self.get(
            self.url(&["publications", publication])?,
            self.credentials.publish().is_ok(),
        )
    }

    pub fn list_publications(&self, request: &PublicationListRequest) -> Result<PublicationPage> {
        if !(1..=100).contains(&request.limit) {
            return Err(Error::InvalidConfig);
        }
        let mut url = self.url(&["publications"])?;
        url.query_pairs_mut()
            .append_pair("limit", &request.limit.to_string());
        if let Some(after) = &request.after {
            url.query_pairs_mut().append_pair("after", after);
        }
        self.get(url, self.credentials.publish().is_ok())
    }

    pub fn withdraw(&self, request: &WithdrawRequest) -> Result<Receipt> {
        self.post(self.url(&["withdraw"])?, request)
    }

    pub fn remove(&self, request: &WithdrawRequest) -> Result<Receipt> {
        self.admin_post(self.url(&["remove"])?, request)
    }

    pub fn status(&self) -> Result<CollectionStatus> {
        // The server exposes status to either current read or publish rights.
        self.get(self.url(&["status"])?, self.credentials.read().is_err())
    }

    /// Authenticate the publishing identity even when a separate read token exists.
    pub(crate) fn publisher_status(&self) -> Result<CollectionStatus> {
        self.get(self.url(&["status"])?, true)
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<SearchResponse> {
        if !(1..=100).contains(&limit) {
            return Err(Error::InvalidConfig);
        }
        let mut url = self.url(&["search"])?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("limit", &limit.to_string());
        self.get(url, false)
    }

    pub fn event(&self, citation: &str) -> Result<HostedEvent> {
        self.get(self.url(&["events", citation])?, false)
    }

    pub fn session(
        &self,
        citation: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<SessionPage> {
        if !(1..=100).contains(&limit) {
            return Err(Error::InvalidConfig);
        }
        let mut url = self.url(&["sessions", citation])?;
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string());
        if let Some(cursor) = cursor {
            url.query_pairs_mut().append_pair("cursor", cursor);
        }
        self.get(url, false)
    }

    pub fn receipt(&self, idempotency_key: &str) -> Result<Receipt> {
        self.get(self.url(&["receipts", idempotency_key])?, true)
    }

    pub(crate) fn begin_upload(&self, spec: &UploadSpec) -> Result<UploadStatus> {
        self.post(self.url(&["uploads"])?, spec)
    }

    pub(crate) fn publish(&self, request: &PublishRequest) -> Result<Receipt> {
        self.post(self.url(&["revisions"])?, request)
    }

    pub(crate) fn cancel_publish(
        &self,
        request: &CancelPublishRequest,
    ) -> Result<CancelPublishResponse> {
        self.post(self.url(&["operations", "cancel"])?, request)
    }

    pub(crate) fn upload_status(&self, id: &str) -> Result<UploadStatus> {
        self.get(self.url(&["uploads", id])?, true)
    }

    pub(crate) fn upload_chunk(&self, id: &str, offset: u64, bytes: &[u8]) -> Result<UploadStatus> {
        if bytes.is_empty() || bytes.len() > UPLOAD_CHUNK_BYTES {
            return Err(Error::TooLarge);
        }
        let mut url = self.url(&["uploads", id])?;
        url.query_pairs_mut()
            .append_pair("offset", &offset.to_string());
        let request = self
            .request("PUT", &url, true)?
            .set("Content-Type", "application/octet-stream");
        decode(request.send_bytes(bytes))
    }

    pub(crate) fn url(&self, suffix: &[&str]) -> Result<Url> {
        let mut path = vec!["v1", "collections", self.connection.collection.as_str()];
        path.extend_from_slice(suffix);
        self.connection.endpoint.route(&path)
    }

    pub(crate) fn get<T: DeserializeOwned>(&self, url: Url, publish: bool) -> Result<T> {
        decode(self.request("GET", &url, publish)?.call())
    }

    pub(crate) fn post<T: DeserializeOwned>(&self, url: Url, body: &impl Serialize) -> Result<T> {
        self.post_with_credential(url, body, true)
    }

    // A read+manage credential can administer without granting this client
    // permission to upload. Only administrative routes use this fallback.
    fn admin_post<T: DeserializeOwned>(&self, url: Url, body: &impl Serialize) -> Result<T> {
        self.post_with_credential(url, body, self.credentials.publish().is_ok())
    }

    fn post_with_credential<T: DeserializeOwned>(
        &self,
        url: Url,
        body: &impl Serialize,
        publish: bool,
    ) -> Result<T> {
        let bytes = serde_json::to_vec(body).map_err(|_| Error::InvalidConfig)?;
        decode(
            self.request("POST", &url, publish)?
                .set("Content-Type", "application/json")
                .send_bytes(&bytes),
        )
    }

    fn request(&self, method: &str, url: &Url, publish: bool) -> Result<ureq::Request> {
        let token = if publish {
            self.credentials.publish()?
        } else {
            self.credentials.read()?
        };
        Ok(self
            .agent
            .request(method, url.as_str())
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "application/json"))
    }
}

impl SharingStore {
    pub fn remote_client(&self) -> Result<RemoteClient> {
        let settings = self.settings()?.ok_or(Error::NotConnected)?;
        RemoteClient::new(settings.connection, settings.credentials)
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(REQUEST_TIMEOUT)
        .timeout_connect(Duration::from_secs(3))
        .timeout_read(REQUEST_TIMEOUT)
        .timeout_write(REQUEST_TIMEOUT)
        .try_proxy_from_env(false)
        .build()
}

fn decode<T: DeserializeOwned>(
    response: std::result::Result<ureq::Response, ureq::Error>,
) -> Result<T> {
    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(status, _)) => return Err(Error::http(status)),
        // ureq errors can include URLs and data from remote messages. Never
        // retain them as a source/backtrace/display string in sharing status.
        Err(ureq::Error::Transport(_)) => return Err(Error::Unavailable),
    };
    if !(200..300).contains(&response.status()) {
        return Err(Error::http(response.status()));
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Unavailable)?;
    if bytes.len() as u64 > RESPONSE_BYTES {
        return Err(Error::TooLarge);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)
}
