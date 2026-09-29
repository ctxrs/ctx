use std::{
    fmt,
    fs::File,
    path::{Path, PathBuf},
};

use ctx_history_platform::platform_security::{
    create_private_directory_all, create_private_file_new, verify_private_directory,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};

use crate::{private_file, Error, RemoteClient, Result, SharingPolicy};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Endpoint(String);

impl Endpoint {
    pub fn parse(value: &str) -> Result<Self> {
        let url = Url::parse(value).map_err(|_| Error::InvalidEndpoint)?;
        let loopback = match url.host() {
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(Error::InvalidEndpoint);
        }
        Ok(Self(url.as_str().trim_end_matches('/').to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn route(&self, segments: &[&str]) -> Result<Url> {
        let mut url = Url::parse(&self.0).map_err(|_| Error::InvalidEndpoint)?;
        url.path_segments_mut()
            .map_err(|_| Error::InvalidEndpoint)?
            .extend(segments.iter().copied());
        Ok(url)
    }
}

impl TryFrom<String> for Endpoint {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}
impl From<Endpoint> for String {
    fn from(value: Endpoint) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub endpoint: Endpoint,
    pub collection: String,
}

/// Tokens are accepted from protected files or caller-owned in-memory input
/// (for example a hidden stdin prompt), never from a command line in this API.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    read: Option<String>,
    publish: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials([REDACTED])")
    }
}

impl Credentials {
    /// One normal member/device secret may carry both rights. A separate
    /// read-only token is optional and is useful for a narrower agent client.
    pub fn device(token: String) -> Result<Self> {
        Self::new(Some(token.clone()), Some(token))
    }

    pub fn read_only(token: String) -> Result<Self> {
        Self::new(Some(token), None)
    }

    pub fn new(read: Option<String>, publish: Option<String>) -> Result<Self> {
        let credentials = Self { read, publish };
        credentials.validate()?;
        Ok(credentials)
    }

    pub fn from_files(read: Option<&Path>, publish: Option<&Path>) -> Result<Self> {
        Self::new(
            read.map(private_file::credential).transpose()?,
            publish.map(private_file::credential).transpose()?,
        )
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.read.is_none() && self.publish.is_none() {
            return Err(Error::MissingCredential);
        }
        if [&self.read, &self.publish]
            .into_iter()
            .flatten()
            .any(|v| v.is_empty() || v.len() > 16_384 || !v.bytes().all(|b| b.is_ascii_graphic()))
        {
            return Err(Error::Credentials);
        }
        Ok(())
    }

    pub(crate) fn read(&self) -> Result<&str> {
        self.read.as_deref().ok_or(Error::MissingCredential)
    }
    pub(crate) fn publish(&self) -> Result<&str> {
        self.publish.as_deref().ok_or(Error::MissingCredential)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    pub connection: Connection,
    pub credentials: Credentials,
    pub policy: Option<SharingPolicy>,
    pub paused: bool,
    /// Authenticated publisher to whom this saved sharing consent belongs.
    #[serde(default)]
    publisher: Option<String>,
    /// Receipt for the last locally saved enrollment, never its bearer secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enrollment: Option<SavedEnrollment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedEnrollment {
    fingerprint: String,
    principal: String,
    credential_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enrollment_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SharingStore {
    root: PathBuf,
}

impl SharingStore {
    /// No filesystem writes, default-home discovery, or network access.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Initial token connections are local. Replacing a credential bound by
    /// enrollment or sharing consent authenticates the replacement identity.
    /// Removing only publishing capability remains a local operation.
    pub fn connect(&self, connection: Connection, credentials: Credentials) -> Result<()> {
        if connection.collection.is_empty() {
            return Err(Error::InvalidConfig);
        }
        credentials.validate()?;
        create_private_directory_all(&self.root).map_err(|_| Error::State)?;
        let _lock = self.lock("settings.lock", false)?;
        let settings = match self.settings()? {
            Some(mut current) => {
                if current.connection != connection {
                    return Err(Error::DestinationChanged);
                }
                let authenticated = if current.enrollment.is_some()
                    && (credentials.read != current.credentials.read
                        || credentials.publish != current.credentials.publish)
                    && !(credentials.publish.is_none()
                        && credentials.read == current.credentials.read)
                {
                    let principal = authenticated_identity(&connection, &credentials)?;
                    if current
                        .enrollment
                        .as_ref()
                        .is_some_and(|saved| saved.principal != principal)
                    {
                        return Err(Error::Credentials);
                    }
                    Some(principal)
                } else {
                    None
                };
                if current.policy.is_some() {
                    if credentials.publish.is_none() {
                        current.paused = true;
                    } else if credentials.publish != current.credentials.publish {
                        let publisher = current.publisher.as_deref().ok_or(Error::Credentials)?;
                        let replacement = match authenticated {
                            Some(principal) => principal,
                            None => authenticated_publisher(&connection, &credentials)?,
                        };
                        if replacement != publisher {
                            return Err(Error::Credentials);
                        }
                    }
                }
                current.credentials = credentials;
                current
            }
            None => Settings {
                connection,
                credentials,
                policy: None,
                paused: false,
                publisher: None,
                enrollment: None,
            },
        };
        private_file::write(&self.root.join("settings.json"), &settings)
    }

    /// Redeem and save one enrollment, or recognize the exact saved enrollment
    /// without another exchange. Returns true when already saved; read_only can
    /// still narrow local publishing without replacing credentials.
    /// The optional principal is an invitation claim, not authentication proof.
    pub fn enroll(
        &self,
        connection: Connection,
        enrollment: &str,
        principal: Option<&str>,
        read_only: bool,
    ) -> Result<bool> {
        if connection.collection.is_empty() || principal.is_some_and(str::is_empty) {
            return Err(Error::InvalidConfig);
        }
        Credentials::read_only(enrollment.to_owned())?;
        create_private_directory_all(&self.root).map_err(|_| Error::State)?;
        // Serialize the check, redemption and save so concurrent scripted reruns
        // cannot both consume a single-use enrollment.
        let _lock = self.lock("settings.lock", false)?;
        let current = self.settings()?;
        let fingerprint = crate::capture::hex(&Sha256::digest(enrollment.as_bytes()));
        let expected = if let Some(current) = &current {
            if current.connection != connection {
                return Err(Error::DestinationChanged);
            }
            let bound = if current.policy.is_some() {
                Some(current.publisher.as_deref().ok_or(Error::Credentials)?)
            } else {
                current
                    .enrollment
                    .as_ref()
                    .map(|saved| saved.principal.as_str())
            };
            if let Some(saved) = &current.enrollment {
                if bound.is_some_and(|bound| bound != saved.principal)
                    || principal.is_some_and(|claim| claim != saved.principal)
                {
                    return Err(Error::Credentials);
                }
                if saved.fingerprint == fingerprint {
                    if read_only
                        && (current.credentials.publish.is_some()
                            || (current.policy.is_some() && !current.paused))
                    {
                        let mut narrowed = current.clone();
                        narrowed.credentials.publish = None;
                        narrowed.credentials.validate()?;
                        if narrowed.policy.is_some() {
                            narrowed.paused = true;
                        }
                        private_file::write(&self.root.join("settings.json"), &narrowed)?;
                    }
                    return Ok(true);
                }
            }
            let expected = match bound {
                Some(bound) => bound.to_owned(),
                None => authenticated_identity(&connection, &current.credentials)?,
            };
            if principal.is_some_and(|claim| claim != expected) {
                return Err(Error::Credentials);
            }
            Some(expected)
        } else {
            None
        };
        let issued = RemoteClient::enroll(&connection.endpoint, enrollment)?;
        if issued.collection != connection.collection {
            return Err(Error::Protocol);
        }
        if issued.principal.is_empty()
            || issued.credential.id.is_empty()
            || principal.is_some_and(|claim| claim != issued.principal)
            || expected
                .as_ref()
                .is_some_and(|expected| expected != &issued.principal)
        {
            return Err(Error::Credentials);
        }
        let credentials = if read_only || !issued.credential.grants.publish {
            Credentials::read_only(issued.credential.secret)?
        } else {
            Credentials::device(issued.credential.secret)?
        };
        let mut settings = current.unwrap_or_else(|| Settings {
            connection,
            credentials: credentials.clone(),
            policy: None,
            paused: false,
            publisher: None,
            enrollment: None,
        });
        if settings.policy.is_some() && credentials.publish.is_none() {
            settings.paused = true;
        }
        settings.credentials = credentials;
        settings.enrollment = Some(SavedEnrollment {
            fingerprint,
            principal: issued.principal,
            credential_id: issued.credential.id,
            enrollment_id: issued.enrollment_id,
        });
        private_file::write(&self.root.join("settings.json"), &settings)?;
        Ok(false)
    }

    /// First authorization binds the authenticated publisher online. Subsequent
    /// policy changes retain that binding and can be made while offline.
    pub fn set_policy(&self, policy: SharingPolicy) -> Result<()> {
        policy.validate()?;
        let _lock = self.lock("settings.lock", false)?;
        let mut settings = self.settings()?.ok_or(Error::NotConnected)?;
        settings.credentials.publish()?;
        if settings
            .policy
            .as_ref()
            .is_some_and(|p| p.revision >= policy.revision)
        {
            return Err(Error::PolicyConflict);
        }
        if settings.policy.is_none() {
            settings.publisher = Some(authenticated_publisher(
                &settings.connection,
                &settings.credentials,
            )?);
        } else if settings.publisher.is_none() {
            // Unbound, unpublished settings need explicit remove/reconnect;
            // current token input must not adopt an older policy's consent.
            return Err(Error::Credentials);
        }
        settings.policy = Some(policy);
        private_file::write(&self.root.join("settings.json"), &settings)
    }

    pub fn pause(&self, paused: bool) -> Result<()> {
        let _lock = self.lock("settings.lock", false)?;
        let mut settings = self.settings()?.ok_or(Error::NotConnected)?;
        settings.paused = paused;
        private_file::write(&self.root.join("settings.json"), &settings)
    }

    pub fn connection(&self) -> Result<Option<Connection>> {
        Ok(self.settings()?.map(|s| s.connection))
    }

    /// Last authenticated enrollment or sharing identity; no online access check.
    pub fn saved_principal(&self) -> Result<Option<String>> {
        Ok(self
            .settings()?
            .and_then(|s| s.enrollment.map(|saved| saved.principal).or(s.publisher)))
    }

    /// Local publishing capability only, not the credential's current server grant.
    pub fn has_publish_credential(&self) -> Result<bool> {
        Ok(self
            .settings()?
            .is_some_and(|s| s.credentials.publish.is_some()))
    }

    pub fn policy(&self) -> Result<Option<SharingPolicy>> {
        Ok(self.settings()?.and_then(|s| s.policy))
    }

    /// Disable publication before waiting for an already admitted request,
    /// then discard only this connection's credentials/checkpoints/backlog.
    /// Stable lock inodes remain so concurrent CLI operations cannot bypass a
    /// waiter by reopening a newly created lock. Remote history is untouched.
    pub fn remove(&self) -> Result<()> {
        if self.settings()?.is_none() {
            return Ok(());
        }
        let _settings = self.lock("settings.lock", false)?;
        let Some(mut settings) = self.settings()? else {
            return Ok(());
        };
        settings.paused = true;
        private_file::write(&self.root.join("settings.json"), &settings)?;
        let _uploader = self.lock("uploader.lock", false)?;
        self.cleanup_scratch()?;
        for name in ["queue", "receipts"] {
            match std::fs::remove_dir_all(self.root.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(Error::State),
            }
        }
        for name in ["capture.json", "last-error.json", "settings.json"] {
            match std::fs::remove_file(self.root.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(Error::State),
            }
        }
        private_file::sync_directory(&self.root)
    }

    pub(crate) fn settings(&self) -> Result<Option<Settings>> {
        // Absent configuration is the ordinary local-only install, including
        // roots that have never been created. Observation must stay read-only.
        if !self.root.try_exists().map_err(|_| Error::State)? {
            return Ok(None);
        }
        verify_private_directory(&self.root).map_err(|_| Error::State)?;
        let settings: Option<Settings> =
            private_file::read_optional(&self.root.join("settings.json"))?;
        if let Some(s) = &settings {
            s.credentials.validate()?;
            if let Some(policy) = &s.policy {
                policy.validate()?;
            }
        }
        Ok(settings)
    }

    pub(crate) fn lock(&self, name: &str, nonblocking: bool) -> Result<File> {
        verify_private_directory(&self.root).map_err(|_| Error::State)?;
        let path = self.root.join(name);
        let file = match create_private_file_new(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => private_file::open(&path)?,
            Err(_) => return Err(Error::State),
        };
        if nonblocking {
            fs2::FileExt::try_lock_exclusive(&file).map_err(|_| Error::Busy)?;
        } else {
            fs2::FileExt::lock_exclusive(&file).map_err(|_| Error::State)?;
        }
        Ok(file)
    }
}

fn authenticated_publisher(connection: &Connection, credentials: &Credentials) -> Result<String> {
    let status = RemoteClient::new(connection.clone(), credentials.clone())?.publisher_status()?;
    if status.collection != connection.collection {
        return Err(Error::Protocol);
    }
    if status.principal.is_empty() {
        return Err(Error::Credentials);
    }
    Ok(status.principal)
}

fn authenticated_identity(connection: &Connection, credentials: &Credentials) -> Result<String> {
    let identity = RemoteClient::new(connection.clone(), credentials.clone())?.whoami()?;
    if identity.collection != connection.collection {
        return Err(Error::Protocol);
    }
    if identity.principal.is_empty() {
        return Err(Error::Credentials);
    }
    Ok(identity.principal)
}

#[cfg(test)]
mod tests;
