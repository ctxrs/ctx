use super::*;

#[derive(Debug, Error)]
pub(crate) enum HermesSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error("{primary}; SQLite snapshot finalization also failed: {finalization}")]
    SqliteFinalization {
        primary: Box<HermesSourceBackedError>,
        finalization: Box<SqliteSourceAccessError>,
    },
    #[error(transparent)]
    Route(#[from] SourceBackedRouteError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Hermes source-backed source has an invalid profile path: {0:?}")]
    InvalidProfilePath(PathBuf),
    #[error("Hermes source-backed source changed while its snapshot was scanned")]
    SourceChanged,
    #[error("Hermes source-backed source counters overflowed")]
    CountOverflow,
    #[error("Hermes source-backed logical-row digest is malformed")]
    InvalidLogicalDigest,
}

impl crate::provider_sources::SqliteSourceErrorComposition for HermesSourceBackedError {
    fn compose_sqlite_source_finalization(self, finalization: SqliteSourceAccessError) -> Self {
        Self::SqliteFinalization {
            primary: Box::new(self),
            finalization: Box::new(finalization),
        }
    }
}

pub(crate) type HermesSourceBackedResult<T> = Result<T, HermesSourceBackedError>;

#[derive(Debug, Clone)]
pub struct HermesSourceCandidate {
    pub(super) data_root: PathBuf,
    pub(super) path: PathBuf,
    pub(super) source: SourceKey,
}

impl HermesSourceCandidate {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn automatic(
        data_root: impl Into<PathBuf>,
        source: ProviderSource,
    ) -> HermesSourceBackedResult<Self> {
        Self::automatic_scoped(data_root, source, SourceAnchorScope::Unqualified)
    }

    pub(crate) fn automatic_scoped(
        data_root: impl Into<PathBuf>,
        source: ProviderSource,
        source_scope: SourceAnchorScope,
    ) -> HermesSourceBackedResult<Self> {
        let profile = hermes_automatic_profile_name(&source.path)?;
        let anchor = SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(&profile)?,
        )?;
        Ok(Self {
            data_root: data_root.into(),
            path: source.path,
            source: hermes_source_key_scoped(anchor, source_scope)?,
        })
    }

    pub(crate) fn released_scoped(
        data_root: impl Into<PathBuf>,
        source: ProviderSource,
        identity_path: &Path,
        source_scope: SourceAnchorScope,
    ) -> HermesSourceBackedResult<Self> {
        let profile = hermes_automatic_profile_name(identity_path)?;
        let anchor = SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(&profile)?,
        )?;
        Ok(Self {
            data_root: data_root.into(),
            path: source.path,
            source: hermes_source_key_scoped(anchor, source_scope)?,
        })
    }
}

/// Admits an explicitly selected Hermes database with caller-owned persistent
/// lineage. This is the only provider-local entry point for inactive profiles.
#[cfg(test)]
pub(crate) fn hermes_source_backed_explicit(
    data_root: impl Into<PathBuf>,
    path: impl Into<PathBuf>,
    anchor: SourceAnchor,
) -> HermesSourceBackedResult<HermesSourceCandidate> {
    hermes_source_backed_explicit_scoped(data_root, path, anchor, SourceAnchorScope::Unqualified)
}

pub(crate) fn hermes_source_backed_explicit_scoped(
    data_root: impl Into<PathBuf>,
    path: impl Into<PathBuf>,
    anchor: SourceAnchor,
    source_scope: SourceAnchorScope,
) -> HermesSourceBackedResult<HermesSourceCandidate> {
    let path = path.into();
    Ok(HermesSourceCandidate {
        data_root: data_root.into(),
        path,
        source: hermes_source_key_scoped(anchor, source_scope)?,
    })
}

pub(crate) fn hermes_automatic_profile_name(path: &Path) -> HermesSourceBackedResult<String> {
    let Some(parent) = path.parent() else {
        return Err(HermesSourceBackedError::InvalidProfilePath(
            path.to_path_buf(),
        ));
    };
    if parent.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("profiles")) {
        let profile = parent
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| valid_automatic_profile_name(name))
            .ok_or_else(|| HermesSourceBackedError::InvalidProfilePath(path.to_path_buf()))?;
        Ok(profile.to_owned())
    } else {
        Ok("default".to_owned())
    }
}

fn valid_automatic_profile_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    name.len() <= 64
        && !matches!(
            name,
            "default" | "hermes" | "test" | "tmp" | "root" | "sudo"
        )
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
}

fn hermes_source_key_scoped(
    anchor: SourceAnchor,
    source_scope: SourceAnchorScope,
) -> HermesSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive_scoped(
        CaptureProvider::Hermes.as_str(),
        HERMES_SQLITE_SOURCE_FORMAT,
        HERMES_PROFILE_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
        source_scope,
    )?)
}

pub(super) fn hermes_session_source_key(
    profile_source: &SourceKey,
    provider_session_id: &str,
) -> HermesSourceBackedResult<SourceKey> {
    let profile_identity = TypedKey::bytes(profile_source.identity().encode_canonical()?.to_vec())?;
    let session_identity = TypedKey::utf8(provider_session_id)?;
    let anchor = SourceAnchor::provider_native(
        HERMES_SESSION_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::composite(vec![profile_identity, session_identity])?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Hermes.as_str(),
        HERMES_SQLITE_SOURCE_FORMAT,
        HERMES_SESSION_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceBackedSession {
    pub(crate) provider_session_id: String,
    pub(crate) provider_parent_session_id: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) source_path: String,
    pub(crate) workspace: Option<String>,
    pub(crate) cwd: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceBackedRejection {
    pub(crate) reason: String,
}

// Records are emitted in bounded provider pages. Boxing each 1,400-byte event to
// approach the 960-byte session variant would allocate on the ingestion path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub(crate) enum HermesSourceBackedRecord {
    Session(HermesSourceBackedSession),
    Event(CoreRecord),
    Rejected(HermesSourceBackedRejection),
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceBackedPage {
    pub(crate) records: Vec<HermesSourceBackedRecord>,
    pub(crate) completed_bytes: u64,
}
