use super::*;

#[derive(Debug, Error)]
pub(crate) enum HermesSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
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

pub(crate) type HermesSourceBackedResult<T> = Result<T, HermesSourceBackedError>;

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceCandidate {
    pub(super) data_root: PathBuf,
    pub(super) path: PathBuf,
    pub(super) source: SourceKey,
}

impl HermesSourceCandidate {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn automatic(
        data_root: impl Into<PathBuf>,
        source: ProviderSource,
    ) -> HermesSourceBackedResult<Self> {
        let profile = automatic_profile(&source.path)?;
        let anchor = SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(&profile)?,
        )?;
        Ok(Self {
            data_root: data_root.into(),
            path: source.path,
            source: hermes_source_key(anchor)?,
        })
    }
}

/// Admits an explicitly selected Hermes database with caller-owned persistent
/// lineage. This is the only provider-local entry point for inactive profiles.
pub(crate) fn hermes_source_backed_explicit(
    data_root: impl Into<PathBuf>,
    path: impl Into<PathBuf>,
    anchor: SourceAnchor,
) -> HermesSourceBackedResult<HermesSourceCandidate> {
    let path = path.into();
    Ok(HermesSourceCandidate {
        data_root: data_root.into(),
        path,
        source: hermes_source_key(anchor)?,
    })
}

fn automatic_profile(path: &Path) -> HermesSourceBackedResult<String> {
    let Some(parent) = path.parent() else {
        return Err(HermesSourceBackedError::InvalidProfilePath(
            path.to_path_buf(),
        ));
    };
    if parent.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("profiles")) {
        let profile = parent
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| HermesSourceBackedError::InvalidProfilePath(path.to_path_buf()))?;
        Ok(profile.to_owned())
    } else {
        Ok("default".to_owned())
    }
}

fn hermes_source_key(anchor: SourceAnchor) -> HermesSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive(
        CaptureProvider::Hermes.as_str(),
        HERMES_SQLITE_SOURCE_FORMAT,
        HERMES_SOURCE_SCHEMA_VARIANT,
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
    pub(crate) agent_type: String,
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
