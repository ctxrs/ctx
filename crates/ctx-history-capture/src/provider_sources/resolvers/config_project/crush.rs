//! Crush provider resolution for the config/project resolver group.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
};

use ctx_history_core::TypedKey;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::provider_sources::{
    context::{DiscoveryContext, DiscoveryPlatform},
    selectors::{
        sort_paths, source_path_kind, SelectorFormat, SelectorReader, SourcePathKind,
        MAX_FINITE_SELECTOR_ENTRIES,
    },
    types::{DiscoveryReport, ProviderSourceSpec, ProviderSourceStatus},
};

use super::super::{path_presence, PathPresence};
use super::{
    add_manual_issue, add_source, git_bounded_ancestors, is_within, lexical_normalize,
    local_absolute_path, path_is_safe_for_automatic_read, read_optional, resolve_os_path,
    string_setting, structured, OptionalDocument, StringSetting, CRUSH_FORMAT,
    INVALID_SELECTOR_REASON, MANUAL_SELECTOR_REASON, UNSAFE_SELECTOR_REASON,
};

// Crush --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CrushOrigin {
    User,
    Project,
}

const CRUSH_PROJECT_KEY_DOMAIN: &str = "crush.project-working-directory.v0";
const CRUSH_INVENTORY_AUTHORITY_KEY: &str = "crush.official-project-catalog.v0";
const CRUSH_INVENTORY_REVISION_DOMAIN: &[u8] = b"ctx.crush.discovery.project-inventory.v0\0";

/// Exact provider selector provenance for one Crush project key.
///
/// Both variants carry the provider's working-directory key. The active key
/// comes from the exact discovery activity locator; the registered key comes
/// from the official `projects.json` `path` field. Neither is inferred from
/// the selected database path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CrushProjectSelectorKey {
    ActiveWorkingDirectory(PathBuf),
    RegisteredProject(PathBuf),
}

impl CrushProjectSelectorKey {
    pub(crate) fn typed_key(&self) -> Result<TypedKey, CrushProjectInventorySelectorError> {
        let path = match self {
            Self::ActiveWorkingDirectory(path) | Self::RegisteredProject(path) => path,
        };
        TypedKey::composite(vec![
            TypedKey::utf8(CRUSH_PROJECT_KEY_DOMAIN)
                .map_err(|_| CrushProjectInventorySelectorError::InvalidAuthorityKey)?,
            TypedKey::bytes(path.as_os_str().as_encoded_bytes().to_vec())
                .map_err(|_| CrushProjectInventorySelectorError::InvalidAuthorityKey)?,
        ])
        .map_err(|_| CrushProjectInventorySelectorError::InvalidAuthorityKey)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrushDiscoveredProjectDatabase {
    selector_key: CrushProjectSelectorKey,
    database_path: PathBuf,
}

impl CrushDiscoveredProjectDatabase {
    pub(crate) fn selector_key(&self) -> &CrushProjectSelectorKey {
        &self.selector_key
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrushDiscoveredProjectInventory {
    revision: Vec<u8>,
    databases: Vec<CrushDiscoveredProjectDatabase>,
}

impl CrushDiscoveredProjectInventory {
    pub(crate) fn authority_key(&self) -> Result<TypedKey, CrushProjectInventorySelectorError> {
        TypedKey::utf8(CRUSH_INVENTORY_AUTHORITY_KEY)
            .map_err(|_| CrushProjectInventorySelectorError::InvalidAuthorityKey)
    }

    pub(crate) fn revision(&self) -> &[u8] {
        &self.revision
    }

    pub(crate) fn databases(&self) -> &[CrushDiscoveredProjectDatabase] {
        &self.databases
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrushProjectInventorySelectorError {
    DiscoveryUnavailable,
    MissingProjectKey,
    AmbiguousDatabaseAuthority,
    InvalidAuthorityKey,
}

impl CrushProjectInventorySelectorError {
    pub(crate) const fn detail(self) -> &'static str {
        match self {
            Self::DiscoveryUnavailable => {
                "Crush official project selectors could not be observed completely"
            }
            Self::MissingProjectKey => {
                "Crush projects.json selected a database without its official project path key"
            }
            Self::AmbiguousDatabaseAuthority => {
                "Crush project selectors assign one database to multiple project keys"
            }
            Self::InvalidAuthorityKey => {
                "Crush project selector authority exceeds the typed-key contract"
            }
        }
    }
}

impl fmt::Display for CrushProjectInventorySelectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.detail())
    }
}

impl std::error::Error for CrushProjectInventorySelectorError {}

/// Rereadable authority over Crush's exact active/registered project set.
#[derive(Debug, Clone)]
pub(crate) struct CrushProjectInventorySelector {
    context: DiscoveryContext,
}

impl CrushProjectInventorySelector {
    pub(crate) fn new(context: DiscoveryContext) -> Self {
        Self { context }
    }

    pub(crate) fn observe(
        &self,
        spec: &ProviderSourceSpec,
    ) -> Result<CrushDiscoveredProjectInventory, CrushProjectInventorySelectorError> {
        discover_project_inventory(&self.context, spec)
    }
}

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let mut reader = SelectorReader::default();

    let user_config_dir = match crush_directory_selector(
        context,
        "CRUSH_GLOBAL_CONFIG",
        "XDG_CONFIG_HOME",
        context.home().join(".config").join("crush"),
        true,
    ) {
        Ok(path) => path,
        Err(()) => {
            add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
            return report;
        }
    };
    let data_config_dir = match crush_data_directory(context) {
        Ok(path) => path,
        Err(()) => {
            add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
            return report;
        }
    };

    let mut config_paths = vec![
        (user_config_dir.join("crush.json"), CrushOrigin::User),
        (data_config_dir.join("crush.json"), CrushOrigin::User),
    ];
    let project_ancestors = context.cwd().map(git_bounded_ancestors).unwrap_or_default();
    for ancestor in project_ancestors.iter().rev() {
        config_paths.push((ancestor.join(".crush.json"), CrushOrigin::Project));
        config_paths.push((ancestor.join("crush.json"), CrushOrigin::Project));
    }

    let mut selected = None::<(String, CrushOrigin)>;
    let mut config_blocked = false;
    let mut seen = HashSet::new();
    for (path, origin) in config_paths {
        if !seen.insert(path.clone()) {
            continue;
        }
        match read_optional(&mut reader, &path, SelectorFormat::Json) {
            Ok(OptionalDocument::Missing | OptionalDocument::Empty) => {}
            Ok(OptionalDocument::Present(document)) => {
                let Some(value) = structured(&document) else {
                    config_blocked = true;
                    break;
                };
                match string_setting(value, &["options", "data_directory"]) {
                    StringSetting::Missing => {}
                    StringSetting::Reset => {
                        selected = None;
                    }
                    StringSetting::Value(value) => {
                        selected = Some((value, origin));
                    }
                    StringSetting::Invalid => {
                        config_blocked = true;
                        break;
                    }
                }
            }
            Err(_) => {
                config_blocked = true;
                break;
            }
        }
    }

    if config_blocked {
        add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
    } else if let Some((raw, origin)) = selected {
        let Some(cwd) = context.cwd() else {
            add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
            add_crush_registry(&mut report, spec, &mut reader, &data_config_dir);
            return report;
        };
        let root = lexical_normalize(&cwd.join(raw));
        let boundary = project_ancestors
            .last()
            .unwrap_or(&cwd.to_path_buf())
            .clone();
        if origin == CrushOrigin::Project && !is_within(&root, &boundary) {
            add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
        } else {
            add_source(&mut report, spec, root.join("crush.db"), CRUSH_FORMAT);
        }
    } else if context.cwd().is_none() {
        add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
    } else {
        let cwd = context.cwd().expect("checked above");
        let Ok(root) = default_crush_root(cwd, &project_ancestors) else {
            add_manual_issue(&mut report, spec.provider, UNSAFE_SELECTOR_REASON);
            add_crush_registry(&mut report, spec, &mut reader, &data_config_dir);
            return report;
        };
        add_source(&mut report, spec, root.join("crush.db"), CRUSH_FORMAT);
    }

    add_crush_registry(&mut report, spec, &mut reader, &data_config_dir);
    report
}

fn discover_project_inventory(
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> Result<CrushDiscoveredProjectInventory, CrushProjectInventorySelectorError> {
    let report = resolve(context, spec);
    if !report.issues.is_empty() {
        return Err(CrushProjectInventorySelectorError::DiscoveryUnavailable);
    }
    let data_config_dir = crush_data_directory(context)
        .map_err(|_| CrushProjectInventorySelectorError::DiscoveryUnavailable)?;
    let mut reader = SelectorReader::default();
    let registered = read_crush_registry(&mut reader, &data_config_dir)
        .map_err(|_| CrushProjectInventorySelectorError::DiscoveryUnavailable)?;
    let mut registered_by_database = HashMap::new();
    for project in registered {
        let Some(project_path) = project.project_path else {
            return Err(CrushProjectInventorySelectorError::MissingProjectKey);
        };
        let Some(database_path) = project.database_path else {
            continue;
        };
        let selector_key = CrushProjectSelectorKey::RegisteredProject(project_path);
        match registered_by_database.insert(database_path, selector_key.clone()) {
            Some(previous) if previous != selector_key => {
                return Err(CrushProjectInventorySelectorError::AmbiguousDatabaseAuthority);
            }
            _ => {}
        }
    }

    let mut databases = Vec::new();
    for source in report.sources.into_iter().filter(|source| {
        matches!(
            source.status,
            ProviderSourceStatus::Available | ProviderSourceStatus::Empty
        )
    }) {
        let selector_key = if let Some(key) = registered_by_database.remove(&source.path) {
            key
        } else {
            let cwd = context
                .cwd()
                .ok_or(CrushProjectInventorySelectorError::MissingProjectKey)?;
            CrushProjectSelectorKey::ActiveWorkingDirectory(cwd.to_path_buf())
        };
        databases.push(CrushDiscoveredProjectDatabase {
            selector_key,
            database_path: source.path,
        });
    }
    databases.sort_by_cached_key(|database| {
        (
            super::super::super::selectors::encoded_path_sort_key(&database.database_path),
            match &database.selector_key {
                CrushProjectSelectorKey::ActiveWorkingDirectory(path)
                | CrushProjectSelectorKey::RegisteredProject(path) => {
                    super::super::super::selectors::encoded_path_sort_key(path)
                }
            },
        )
    });
    if databases
        .windows(2)
        .any(|pair| pair[0].database_path == pair[1].database_path)
    {
        return Err(CrushProjectInventorySelectorError::AmbiguousDatabaseAuthority);
    }

    let mut digest = Sha256::new();
    digest.update(CRUSH_INVENTORY_REVISION_DOMAIN);
    digest.update((databases.len() as u64).to_be_bytes());
    for database in &databases {
        let key = database.selector_key.typed_key()?;
        let encoded_key = serde_json::to_vec(&key)
            .map_err(|_| CrushProjectInventorySelectorError::InvalidAuthorityKey)?;
        digest.update((encoded_key.len() as u64).to_be_bytes());
        digest.update(encoded_key);
        let encoded_path = database.database_path.as_os_str().as_encoded_bytes();
        digest.update((encoded_path.len() as u64).to_be_bytes());
        digest.update(encoded_path);
    }
    Ok(CrushDiscoveredProjectInventory {
        revision: digest.finalize().to_vec(),
        databases,
    })
}

fn crush_directory_selector(
    context: &DiscoveryContext,
    primary: &str,
    secondary: &str,
    fallback: PathBuf,
    secondary_adds_crush: bool,
) -> Result<PathBuf, ()> {
    if let Some(raw) = context.env(primary).filter(|value| !value.is_empty()) {
        return resolve_os_path(raw, context.cwd());
    }
    if let Some(raw) = context.env(secondary).filter(|value| !value.is_empty()) {
        let path = resolve_os_path(raw, context.cwd())?;
        return Ok(if secondary_adds_crush {
            path.join("crush")
        } else {
            path
        });
    }
    Ok(fallback)
}

fn crush_data_directory(context: &DiscoveryContext) -> Result<PathBuf, ()> {
    let fallback = match context.platform() {
        DiscoveryPlatform::Windows => context
            .platform_dirs()
            .local_data
            .clone()
            .unwrap_or_else(|| context.home().join("AppData").join("Local"))
            .join("crush"),
        _ => context.home().join(".local").join("share").join("crush"),
    };
    crush_directory_selector(
        context,
        "CRUSH_GLOBAL_DATA",
        "XDG_DATA_HOME",
        fallback,
        true,
    )
}

fn default_crush_root(cwd: &Path, ancestors: &[PathBuf]) -> Result<PathBuf, ()> {
    for ancestor in ancestors {
        let candidate = ancestor.join(".crush");
        match path_presence(&candidate) {
            PathPresence::Missing => {}
            PathPresence::Present if path_is_safe_for_automatic_read(&candidate) => {
                return (source_path_kind(&candidate) == Ok(SourcePathKind::Directory))
                    .then_some(candidate)
                    .ok_or(());
            }
            PathPresence::Present | PathPresence::Unsupported | PathPresence::Unknown(_) => {
                return Err(())
            }
        }
    }
    Ok(cwd.join(".crush"))
}

fn add_crush_registry(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    reader: &mut SelectorReader,
    data_config_dir: &Path,
) {
    let projects = match read_crush_registry(reader, data_config_dir) {
        Ok(projects) => projects,
        Err(_) => {
            add_manual_issue(report, spec.provider, INVALID_SELECTOR_REASON);
            return;
        }
    };
    let mut paths = Vec::new();
    for project in projects {
        let Some(path) = project.database_path else {
            continue;
        };
        if local_absolute_path(&path) {
            paths.push(path);
        } else {
            add_manual_issue(report, spec.provider, MANUAL_SELECTOR_REASON);
        }
    }
    sort_paths(&mut paths);
    for path in paths {
        add_source(report, spec, path, CRUSH_FORMAT);
    }
}

#[derive(Debug)]
struct RegisteredCrushProject {
    project_path: Option<PathBuf>,
    database_path: Option<PathBuf>,
}

fn read_crush_registry(
    reader: &mut SelectorReader,
    data_config_dir: &Path,
) -> Result<Vec<RegisteredCrushProject>, ()> {
    let path = data_config_dir.join("projects.json");
    let document = match read_optional(reader, &path, SelectorFormat::Json).map_err(|_| ())? {
        OptionalDocument::Missing | OptionalDocument::Empty => return Ok(Vec::new()),
        OptionalDocument::Present(document) => document,
    };
    let projects = structured(&document)
        .and_then(|value| value.get("projects"))
        .and_then(Value::as_array)
        .ok_or(())?;
    if projects.len() > MAX_FINITE_SELECTOR_ENTRIES {
        return Err(());
    }
    projects
        .iter()
        .map(|project| {
            let project_path = project
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .filter(|path| local_absolute_path(path))
                .map(|path| lexical_normalize(&path));
            let database_path = project
                .get("data_dir")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .map(|root| lexical_normalize(&root).join("crush.db"));
            Ok(RegisteredCrushProject {
                project_path,
                database_path,
            })
        })
        .collect()
}
