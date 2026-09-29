use std::{
    fmt,
    fs::File,
    path::{Path, PathBuf},
};

use ctx_history_platform::platform_security::{
    create_private_directory_all, create_private_file_new, verify_private_directory,
};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

use crate::{private_file, Error, Result, SharingPolicy};

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
                current.credentials = credentials;
                current
            }
            None => Settings {
                connection,
                credentials,
                policy: None,
                paused: false,
            },
        };
        private_file::write(&self.root.join("settings.json"), &settings)
    }

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
