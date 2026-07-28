use std::path::Path;

use ctx_history_core::{CaptureProvider, TypedKey};
use thiserror::Error;

use super::{
    context::{DiscoveryContext, DiscoveryPlatform},
    resolvers::resolve_lingma_with_authority,
    specs::provider_source_spec,
    types::{ProviderSource, ProviderSourceStatus},
};

const LINGMA_INVENTORY_AUTHORITY_KEY: &str = "lingma.official-installed-client-catalog.v0";
const LINGMA_LINEAGE_DOMAIN: &str = "lingma.database-catalog-lineage.v0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LingmaVscodeClient {
    Stable,
    Insiders,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LingmaVscodeProfile {
    Base,
    Named(Vec<u8>),
}

/// Provider-native catalog slot for one Lingma database.
///
/// Named profile/product bytes come from the exact directory entry selected
/// by the installed-client resolver. Database paths never participate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LingmaDatabaseCatalogLineage {
    VscodeSharedDefault,
    VscodeSelected {
        client: LingmaVscodeClient,
        profile: LingmaVscodeProfile,
    },
    JetBrainsSharedDefault,
    JetBrainsSelected {
        product: Vec<u8>,
    },
}

impl LingmaDatabaseCatalogLineage {
    pub(crate) fn typed_key(&self) -> Result<TypedKey, LingmaDiscoveryUnavailable> {
        let domain = TypedKey::utf8(LINGMA_LINEAGE_DOMAIN)
            .map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)?;
        let parts = match self {
            Self::VscodeSharedDefault => vec![
                domain,
                TypedKey::utf8("vscode-shared-default")
                    .map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)?,
            ],
            Self::VscodeSelected { client, profile } => {
                let client = match client {
                    LingmaVscodeClient::Stable => "stable",
                    LingmaVscodeClient::Insiders => "insiders",
                };
                let profile = match profile {
                    LingmaVscodeProfile::Base => TypedKey::utf8("base")
                        .map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)?,
                    LingmaVscodeProfile::Named(name) => TypedKey::bytes(name.clone())
                        .map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)?,
                };
                vec![
                    domain,
                    TypedKey::utf8("vscode-selected")
                        .map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)?,
                    TypedKey::utf8(client)
                        .map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)?,
                    profile,
                ]
            }
            Self::JetBrainsSharedDefault => vec![
                domain,
                TypedKey::utf8("jetbrains-shared-default")
                    .map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)?,
            ],
            Self::JetBrainsSelected { product } => vec![
                domain,
                TypedKey::utf8("jetbrains-selected")
                    .map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)?,
                TypedKey::bytes(product.clone())
                    .map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)?,
            ],
        };
        TypedKey::composite(parts).map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredLingmaDatabase {
    source: ProviderSource,
    catalog_lineage: LingmaDatabaseCatalogLineage,
}

impl DiscoveredLingmaDatabase {
    pub(crate) const fn new(
        source: ProviderSource,
        catalog_lineage: LingmaDatabaseCatalogLineage,
    ) -> Self {
        Self {
            source,
            catalog_lineage,
        }
    }

    pub fn source(&self) -> &ProviderSource {
        &self.source
    }

    pub fn path(&self) -> &Path {
        &self.source.path
    }

    pub fn catalog_lineage(&self) -> &LingmaDatabaseCatalogLineage {
        &self.catalog_lineage
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LingmaDiscoveredInventory {
    databases: Vec<DiscoveredLingmaDatabase>,
}

impl LingmaDiscoveredInventory {
    pub(crate) fn new(databases: Vec<DiscoveredLingmaDatabase>) -> Self {
        Self { databases }
    }

    pub fn authority_key(&self) -> Result<TypedKey, LingmaDiscoveryUnavailable> {
        TypedKey::utf8(LINGMA_INVENTORY_AUTHORITY_KEY)
            .map_err(|_| LingmaDiscoveryUnavailable::InvalidAuthorityKey)
    }

    pub fn databases(&self) -> &[DiscoveredLingmaDatabase] {
        &self.databases
    }
}

#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum LingmaDiscoveryUnavailable {
    #[error("Lingma installed-client discovery is unavailable on {platform:?}")]
    UnsupportedPlatform { platform: DiscoveryPlatform },
    #[error("the Lingma provider discovery specification is unavailable")]
    ProviderSpecUnavailable,
    #[error("Lingma installed-client selectors could not be observed completely")]
    SelectorUnavailable,
    #[error("one Lingma database is selected by multiple installed-client catalog slots")]
    AmbiguousDatabaseAuthority,
    #[error("Lingma installed-client authority exceeds the typed-key contract")]
    InvalidAuthorityKey,
    #[error("the source was not selected by authoritative Lingma discovery")]
    SourceNotSelected,
}

impl LingmaDiscoveryUnavailable {
    pub const fn detail(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform { .. } => {
                "Lingma installed-client authority is unavailable on this platform"
            }
            Self::ProviderSpecUnavailable => {
                "Lingma provider discovery specification is unavailable"
            }
            Self::SelectorUnavailable => {
                "Lingma installed-client selectors could not be observed completely"
            }
            Self::AmbiguousDatabaseAuthority => {
                "one Lingma database is selected by multiple installed-client catalog slots"
            }
            Self::InvalidAuthorityKey => {
                "Lingma installed-client authority exceeds the typed-key contract"
            }
            Self::SourceNotSelected => {
                "Lingma source is absent from authoritative installed-client discovery"
            }
        }
    }
}

/// Rereadable installed-client selector used for opening and closing inventory
/// observations.
#[derive(Debug, Clone)]
pub struct LingmaInventorySelector {
    context: DiscoveryContext,
}

impl LingmaInventorySelector {
    pub fn new(context: DiscoveryContext) -> Self {
        Self { context }
    }

    pub fn observe(&self) -> Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable> {
        discover_lingma_inventory_with_authority(&self.context)
    }
}

pub fn discover_lingma_inventory_with_authority(
    context: &DiscoveryContext,
) -> Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable> {
    if matches!(context.platform(), DiscoveryPlatform::OtherUnix) {
        return Err(LingmaDiscoveryUnavailable::UnsupportedPlatform {
            platform: context.platform(),
        });
    }
    let spec = provider_source_spec(CaptureProvider::Lingma)
        .ok_or(LingmaDiscoveryUnavailable::ProviderSpecUnavailable)?;
    let (report, discovered) = resolve_lingma_with_authority(context, spec);
    if !report.issues.is_empty() {
        return Err(LingmaDiscoveryUnavailable::SelectorUnavailable);
    }
    let mut available = discovered
        .into_iter()
        .filter(|database| {
            matches!(
                database.source.status,
                ProviderSourceStatus::Available | ProviderSourceStatus::Empty
            )
        })
        .collect::<Vec<_>>();
    available.sort_by_cached_key(|database| {
        database.source.path.as_os_str().as_encoded_bytes().to_vec()
    });
    for pair in available.windows(2) {
        if pair[0].source.path == pair[1].source.path
            && pair[0].catalog_lineage != pair[1].catalog_lineage
        {
            return Err(LingmaDiscoveryUnavailable::AmbiguousDatabaseAuthority);
        }
    }
    available.dedup_by(|left, right| {
        left.source.path == right.source.path && left.catalog_lineage == right.catalog_lineage
    });
    for database in &available {
        database.catalog_lineage.typed_key()?;
    }
    Ok(LingmaDiscoveredInventory::new(available))
}

pub fn resolve_lingma_discovery_authority(
    context: &DiscoveryContext,
    selected_source: &ProviderSource,
) -> Result<DiscoveredLingmaDatabase, LingmaDiscoveryUnavailable> {
    discover_lingma_inventory_with_authority(context)?
        .databases
        .into_iter()
        .find(|database| database.source == *selected_source)
        .ok_or(LingmaDiscoveryUnavailable::SourceNotSelected)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use rusqlite::Connection;

    use super::*;
    use crate::provider_sources::DiscoveryPlatformDirs;

    fn context(root: &Path, platform: DiscoveryPlatform) -> DiscoveryContext {
        DiscoveryContext::new(
            root.join("home"),
            root.join("cwd"),
            platform,
            DiscoveryPlatformDirs {
                config: Some(root.join("config")),
                ..DiscoveryPlatformDirs::default()
            },
        )
    }

    fn write_database(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "create table chat_record (\
                    session_id text, request_id text, chat_prompt text, summary text, \
                    error_result text, gmt_create integer, extra text);",
            )
            .unwrap();
    }

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn vscode_inventory_retains_client_and_profile_catalog_lineage() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let context = context(temp.path(), DiscoveryPlatform::Linux);
        let base_root = temp.path().join("base-storage");
        let profile_root = temp.path().join("profile-storage");
        let base_db = base_root.join("sharedClientCache/cache/db/local.db");
        let profile_db = profile_root.join("sharedClientCache/cache/db/local.db");
        write_database(&base_db);
        write_database(&profile_db);
        write(
            &temp.path().join("config/Code/User/settings.json"),
            &format!(
                r#"{{"QoderCN.LocalMachineStoragePath":{}}}"#,
                serde_json::to_string(base_root.to_str().unwrap()).unwrap()
            ),
        );
        write(
            &temp
                .path()
                .join("config/Code/User/profiles/work/settings.json"),
            &format!(
                r#"{{"QoderCN.LocalMachineStoragePath":{}}}"#,
                serde_json::to_string(profile_root.to_str().unwrap()).unwrap()
            ),
        );

        let inventory = LingmaInventorySelector::new(context).observe().unwrap();
        assert_eq!(inventory.databases().len(), 2);
        assert!(inventory.databases().iter().any(|database| {
            database.path() == base_db
                && database.catalog_lineage()
                    == &LingmaDatabaseCatalogLineage::VscodeSelected {
                        client: LingmaVscodeClient::Stable,
                        profile: LingmaVscodeProfile::Base,
                    }
        }));
        assert!(inventory.databases().iter().any(|database| {
            database.path() == profile_db
                && database.catalog_lineage()
                    == &LingmaDatabaseCatalogLineage::VscodeSelected {
                        client: LingmaVscodeClient::Stable,
                        profile: LingmaVscodeProfile::Named(b"work".to_vec()),
                    }
        }));
        assert_ne!(inventory.authority_key().unwrap(), TypedKey::Null);
    }

    #[test]
    fn duplicate_database_with_distinct_client_slots_fails_closed() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let context = context(temp.path(), DiscoveryPlatform::Linux);
        let shared_root = temp.path().join("shared-storage");
        write_database(&shared_root.join("sharedClientCache/cache/db/local.db"));
        let setting = format!(
            r#"{{"QoderCN.LocalMachineStoragePath":{}}}"#,
            serde_json::to_string(shared_root.to_str().unwrap()).unwrap()
        );
        write(
            &temp.path().join("config/Code/User/settings.json"),
            &setting,
        );
        write(
            &temp
                .path()
                .join("config/Code/User/profiles/work/settings.json"),
            &setting,
        );

        assert_eq!(
            LingmaInventorySelector::new(context).observe(),
            Err(LingmaDiscoveryUnavailable::AmbiguousDatabaseAuthority)
        );
    }
}
