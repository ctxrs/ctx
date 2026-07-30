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
    Resolver(#[from] SourceResolverContractError),
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
    #[error("Hermes source-backed locator is not a supported message row")]
    InvalidLocator,
    #[error("Hermes source-backed locator references a stale source snapshot")]
    StaleSourceEvidence,
    #[error("Hermes source-backed locator references a stale logical row")]
    StaleRecordEvidence,
    #[error("Hermes source-backed locator row is missing")]
    MissingRecord,
}

pub(crate) type HermesSourceBackedResult<T> = Result<T, HermesSourceBackedError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HermesSourceSelection {
    DefaultProfile,
    NamedProfile(String),
    Explicit,
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceCandidate {
    pub(super) data_root: PathBuf,
    pub(super) path: PathBuf,
    pub(super) source: SourceKey,
    // Selection and status remain discovery provenance for release reporting.
    #[allow(dead_code)]
    selection: HermesSourceSelection,
    #[allow(dead_code)]
    status: ProviderSourceStatus,
}

impl HermesSourceCandidate {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    #[allow(dead_code)]
    pub(crate) fn selection(&self) -> &HermesSourceSelection {
        &self.selection
    }

    #[allow(dead_code)]
    pub(crate) fn status(&self) -> ProviderSourceStatus {
        self.status
    }

    pub(crate) fn automatic(
        data_root: impl Into<PathBuf>,
        source: ProviderSource,
    ) -> HermesSourceBackedResult<Self> {
        let selection = automatic_selection(&source.path)?;
        let profile = match &selection {
            HermesSourceSelection::DefaultProfile => "default",
            HermesSourceSelection::NamedProfile(profile) => profile.as_str(),
            HermesSourceSelection::Explicit => {
                return Err(HermesSourceBackedError::InvalidProfilePath(source.path));
            }
        };
        let anchor = SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(profile)?,
        )?;
        Ok(Self {
            data_root: data_root.into(),
            path: source.path,
            source: hermes_source_key(anchor)?,
            selection,
            status: source.status,
        })
    }
}

#[derive(Debug, Clone)]
// The bounded inventory shape remains the authoritative automatic-discovery
// evidence even while production registration supplies explicit candidates.
#[allow(dead_code)]
pub(crate) struct HermesSourceInventory {
    pub(crate) sources: Vec<HermesSourceCandidate>,
    pub(crate) issues: Vec<DiscoveryIssue>,
}

/// Inventories only the selected ordinary profile or the bounded Gateway
/// multiplex set admitted by the existing Hermes discovery resolver.
#[allow(dead_code)]
pub(crate) fn discover_hermes_source_backed(
    data_root: &Path,
    context: &DiscoveryContext,
) -> HermesSourceBackedResult<HermesSourceInventory> {
    let report =
        discover_provider_sources_for_provider_with_context(context, CaptureProvider::Hermes);
    let mut sources = Vec::with_capacity(report.sources.len());
    for source in report.sources {
        if source.source_format == HERMES_SQLITE_SOURCE_FORMAT {
            sources.push(HermesSourceCandidate::automatic(data_root, source)?);
        }
    }
    Ok(HermesSourceInventory {
        sources,
        issues: report.issues,
    })
}

/// Admits an explicitly selected Hermes database with caller-owned persistent
/// lineage. This is the only provider-local entry point for inactive profiles.
pub(crate) fn hermes_source_backed_explicit(
    data_root: impl Into<PathBuf>,
    path: impl Into<PathBuf>,
    anchor: SourceAnchor,
) -> HermesSourceBackedResult<HermesSourceCandidate> {
    let path = path.into();
    let status = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            ProviderSourceStatus::Available
        }
        Ok(_) => ProviderSourceStatus::Unknown,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProviderSourceStatus::Missing,
        Err(_) => ProviderSourceStatus::Unknown,
    };
    Ok(HermesSourceCandidate {
        data_root: data_root.into(),
        path,
        source: hermes_source_key(anchor)?,
        selection: HermesSourceSelection::Explicit,
        status,
    })
}

fn automatic_selection(path: &Path) -> HermesSourceBackedResult<HermesSourceSelection> {
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
        Ok(HermesSourceSelection::NamedProfile(profile.to_owned()))
    } else {
        Ok(HermesSourceSelection::DefaultProfile)
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
    // Stable lineage remains part of the exact session materialization record.
    #[allow(dead_code)]
    pub(crate) session_id: StableEntityId,
    #[allow(dead_code)]
    pub(crate) parent_session_id: Option<StableEntityId>,
    // Root identity, primary classification, and source timestamps remain part
    // of the exact session record used by non-Core materializers.
    #[allow(dead_code)]
    pub(crate) root_session_id: StableEntityId,
    pub(crate) provider_session_id: String,
    pub(crate) provider_parent_session_id: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) source_path: String,
    pub(crate) agent_type: String,
    #[allow(dead_code)]
    pub(crate) is_primary: bool,
    #[allow(dead_code)]
    pub(crate) started_at_unix_ms: i64,
    #[allow(dead_code)]
    pub(crate) ended_at_unix_ms: Option<i64>,
    pub(crate) workspace: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) locator: SourceRecordLocator,
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceBackedRejection {
    // Exact provider position is retained with rejection diagnostics.
    #[allow(dead_code)]
    pub(super) phase: HermesPhase,
    #[allow(dead_code)]
    pub(crate) rowid: i64,
    #[allow(dead_code)]
    pub(crate) ordinal: u64,
    pub(crate) reason: String,
}

// Records are emitted in bounded provider pages. Boxing each 1,400-byte event to
// approach the 960-byte session variant would allocate on the ingestion path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub(crate) enum HermesSourceBackedRecord {
    Session(HermesSourceBackedSession),
    Event(LexicalDocument),
    Rejected(HermesSourceBackedRejection),
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceBackedPage {
    pub(crate) records: Vec<HermesSourceBackedRecord>,
    // Provider-owned bytes remain bounded-page accounting evidence.
    #[allow(dead_code)]
    pub(crate) owned_bytes: usize,
}
