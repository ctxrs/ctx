use std::{
    collections::BTreeSet,
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use ctx_history_capture_model::ProviderSource;
#[cfg(test)]
use ctx_history_core::CertifiedSourceInventory;
use ctx_history_core::{
    CaptureProvider, SourceAnchor, SourceAnchorScope, SourceInventoryObservation, SourceKey,
    TypedKey,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    discover_provider_sources_for_provider_with_context, DiscoveryContext, ProviderSourceStatus,
    ASTRBOT_SQLITE_SOURCE_FORMAT,
};

#[cfg(test)]
use crate::{
    common::io::ProviderSourceRoot,
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceReadSnapshot,
    },
    CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::{
    AstrBotSourceBackedErrorV0, AstrBotSourceBackedResultV0, INVENTORY_AUTHORITY_KEY,
    INVENTORY_AUTHORITY_NAMESPACE, INVENTORY_REVISION_KIND, LAUNCHER_SOURCE_NAMESPACE,
    SELECTED_SOURCE_NAMESPACE, SOURCE_IDENTITY_VERSION, SOURCE_SCHEMA_VARIANT,
};
#[cfg(test)]
use super::{INVENTORY_DISCOVERY_REVISION, SQLITE_SOURCE_INVALID_REASON};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AstrBotSourceIdentityV0 {
    SelectedCore,
    LauncherInstance(String),
}

#[derive(Debug, Clone)]
pub(crate) struct AstrBotSourceBackedSourceV0 {
    pub(super) path: PathBuf,
    pub(super) identity: AstrBotSourceIdentityV0,
    pub(super) source_key: SourceKey,
}

impl AstrBotSourceBackedSourceV0 {
    pub(crate) fn source_key(&self) -> &SourceKey {
        &self.source_key
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AstrBotSourceBackedInventoryV0 {
    observation: SourceInventoryObservation,
    pub(super) sources: Vec<AstrBotSourceBackedSourceV0>,
}

impl AstrBotSourceBackedInventoryV0 {
    #[cfg(test)]
    pub(crate) fn discover(context: &DiscoveryContext) -> AstrBotSourceBackedResultV0<Self> {
        Self::discover_scoped(context, SourceAnchorScope::Unqualified)
    }

    pub(crate) fn discover_scoped(
        context: &DiscoveryContext,
        source_scope: SourceAnchorScope,
    ) -> AstrBotSourceBackedResultV0<Self> {
        let report =
            discover_provider_sources_for_provider_with_context(context, CaptureProvider::AstrBot);
        if !report.issues.is_empty() {
            return Err(AstrBotSourceBackedErrorV0::IncompleteInventory {
                issues: report.issues.len(),
            });
        }
        let observation = inventory_observation(&report)?;
        let mut selected_core = false;
        let mut seen = BTreeSet::new();
        let mut sources = Vec::new();
        for candidate in &report.sources {
            match candidate.status {
                ProviderSourceStatus::Missing => continue,
                ProviderSourceStatus::Available => {}
                status => {
                    return Err(AstrBotSourceBackedErrorV0::NonAdmissibleSource {
                        path: candidate.path.clone(),
                        status: status.as_str(),
                    });
                }
            }
            if candidate.source_format != ASTRBOT_SQLITE_SOURCE_FORMAT {
                return Err(AstrBotSourceBackedErrorV0::NonAdmissibleSource {
                    path: candidate.path.clone(),
                    status: "unexpected_source_format",
                });
            }
            let identity = launcher_instance_identity(context.home(), &candidate.path)
                .map(AstrBotSourceIdentityV0::LauncherInstance)
                .unwrap_or(AstrBotSourceIdentityV0::SelectedCore);
            if identity == AstrBotSourceIdentityV0::SelectedCore {
                if selected_core {
                    return Err(AstrBotSourceBackedErrorV0::DuplicateSelectedCore);
                }
                selected_core = true;
            }
            let source_key = source_key_scoped(&identity, source_scope)?;
            if !seen.insert(source_key.identity().digest()) {
                return Err(AstrBotSourceBackedErrorV0::DuplicateSourceIdentity(
                    source_key.identity().to_string(),
                ));
            }
            sources.push(AstrBotSourceBackedSourceV0 {
                path: candidate.path.clone(),
                identity,
                source_key,
            });
        }
        sources.sort_by(|left, right| left.identity.cmp(&right.identity));
        Ok(Self {
            observation,
            sources,
        })
    }

    pub(crate) fn released_scoped(
        identity_home: &Path,
        identity_source: &ProviderSource,
        scan_path: &Path,
        source_scope: SourceAnchorScope,
    ) -> AstrBotSourceBackedResultV0<Self> {
        let identity = launcher_instance_identity(identity_home, &identity_source.path)
            .map(AstrBotSourceIdentityV0::LauncherInstance)
            .unwrap_or(AstrBotSourceIdentityV0::SelectedCore);
        let source_key = source_key_scoped(&identity, source_scope)?;
        let mut digest = Sha256::new();
        digest.update(b"ctx-astrbot-source-inventory-observation-v0\0");
        digest.update(1_u64.to_be_bytes());
        let path = identity_source.path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        digest.update((identity_source.source_format.len() as u64).to_be_bytes());
        digest.update(identity_source.source_format.as_bytes());
        digest.update(identity_source.status.as_str().as_bytes());
        let observation = SourceInventoryObservation::new(
            CaptureProvider::AstrBot.as_str(),
            INVENTORY_AUTHORITY_NAMESPACE,
            TypedKey::utf8(INVENTORY_AUTHORITY_KEY)?,
            INVENTORY_REVISION_KIND,
            digest.finalize().to_vec(),
        )?;
        Ok(Self {
            observation,
            sources: vec![AstrBotSourceBackedSourceV0 {
                path: scan_path.to_path_buf(),
                identity,
                source_key,
            }],
        })
    }

    pub(crate) fn sources(&self) -> &[AstrBotSourceBackedSourceV0] {
        &self.sources
    }

    pub(crate) fn observation(&self) -> &SourceInventoryObservation {
        &self.observation
    }

    #[cfg(test)]
    pub(crate) fn certify(
        &self,
        closing: &Self,
    ) -> AstrBotSourceBackedResultV0<CertifiedSourceInventory> {
        Ok(CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            INVENTORY_DISCOVERY_REVISION,
            self.sources
                .iter()
                .map(|source| source.source_key.clone())
                .collect(),
        )?)
    }
}

#[cfg(test)]
pub(super) fn open_root_authorized_snapshot(
    data_root: &Path,
    path: &Path,
) -> AstrBotSourceBackedResultV0<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook(data_root, path, || {})
}

#[cfg(test)]
pub(super) fn open_root_authorized_snapshot_with_hook(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
) -> AstrBotSourceBackedResultV0<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let database_leaf =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: SQLITE_SOURCE_INVALID_REASON,
            })?;
    let source_root = ProviderSourceRoot::open(parent)?;
    let source_directory = source_root.directory()?;
    let parent_handle = source_directory
        .try_clone_authority_handle()
        .map_err(CaptureError::from)?;
    let sqlite_authority =
        retain_sqlite_source_directory_authority(data_root, &parent_handle, parent)?;
    let sqlite_snapshot =
        open_root_handle_sqlite_source_snapshot(&sqlite_authority, database_leaf)?;
    after_authorize();
    sqlite_snapshot.revalidate()?;
    source_directory.revalidate()?;
    source_root.revalidate()?;
    let connection = sqlite_snapshot.connection()?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| AstrBotSourceBackedErrorV0::CountOverflow)?;
    connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(CaptureError::from)?;
    Ok((source_root, sqlite_snapshot))
}

#[cfg(test)]
pub(super) fn source_key(
    identity: &AstrBotSourceIdentityV0,
) -> AstrBotSourceBackedResultV0<SourceKey> {
    source_key_scoped(identity, SourceAnchorScope::Unqualified)
}

pub(super) fn source_key_scoped(
    identity: &AstrBotSourceIdentityV0,
    source_scope: SourceAnchorScope,
) -> AstrBotSourceBackedResultV0<SourceKey> {
    let (namespace, key) = match identity {
        AstrBotSourceIdentityV0::SelectedCore => {
            (SELECTED_SOURCE_NAMESPACE, TypedKey::utf8("selected-core")?)
        }
        AstrBotSourceIdentityV0::LauncherInstance(instance) => {
            (LAUNCHER_SOURCE_NAMESPACE, TypedKey::utf8(instance.clone())?)
        }
    };
    Ok(SourceKey::derive_scoped(
        CaptureProvider::AstrBot.as_str(),
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        SOURCE_IDENTITY_VERSION,
        SourceAnchor::provider_native(namespace, key)?,
        source_scope,
    )?)
}

fn launcher_instance_identity(home: &Path, path: &Path) -> Option<String> {
    let root = home.join(".astrbot_launcher").join("instances");
    let relative = path.strip_prefix(root).ok()?;
    let components = relative.components().collect::<Vec<_>>();
    let [Component::Normal(instance), Component::Normal(core), Component::Normal(data), Component::Normal(database)] =
        components.as_slice()
    else {
        return None;
    };
    if core != &OsStr::new("core")
        || data != &OsStr::new("data")
        || database != &OsStr::new("data_v4.db")
    {
        return None;
    }
    Uuid::parse_str(instance.to_str()?)
        .ok()
        .map(|id| id.to_string())
}

fn inventory_observation(
    report: &crate::DiscoveryReport,
) -> AstrBotSourceBackedResultV0<SourceInventoryObservation> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-astrbot-source-inventory-observation-v0\0");
    digest.update((report.sources.len() as u64).to_be_bytes());
    for source in &report.sources {
        let path = source.path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        digest.update((source.source_format.len() as u64).to_be_bytes());
        digest.update(source.source_format.as_bytes());
        digest.update(source.status.as_str().as_bytes());
    }
    Ok(SourceInventoryObservation::new(
        CaptureProvider::AstrBot.as_str(),
        INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::utf8(INVENTORY_AUTHORITY_KEY)?,
        INVENTORY_REVISION_KIND,
        digest.finalize().to_vec(),
    )?)
}
