use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::{
    provider_source_for_path, register_custom_history_source_backed_route,
    register_forgecode_explicit_source_backed_route, register_goose_source_backed_route,
    register_hermes_explicit_source_backed_route, register_landed_source_backed_route,
    register_lingma_source_backed_route, register_nanoclaw_source_backed_route,
    register_warp_source_backed_route, source_backed_route_constructor,
    source_backed_route_inventory, ProviderCatalogSupport, ProviderImportSupport, ProviderSource,
    ProviderSourceKind, ProviderSourceStatus, SourceBackedAutomaticRegistryBuild,
    SourceBackedHydrationSupport, SourceBackedProviderRegistry, SourceBackedRoute,
    SourceBackedRouteConstructor, SourceBackedRouteDriver, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteSelection, SourceBackedSelectorAuthority,
};
use ctx_history_core::{
    CaptureProvider, CertifiedSourceDeletion, CertifiedSourceInventory, HydrationFailure,
    HydrationFailureKind, SourceAnchor, SourceInventoryObservation, SourceKey, TypedKey,
};
use ctx_history_index::VerifiedIndex;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{provider_args::ImportFormatArg, ImportArgs};

const CATALOG_DIRECTORY: &str = "catalogs/explicit-sources";
const CATALOG_LOCK_FILE: &str = "catalog.lock";
const CATALOG_SCHEMA_VERSION: u32 = 1;
const CATALOG_INTEGRITY_ALGORITHM: &str = "sha256";
const CATALOG_FILE_PREFIX: &str = "catalog-";
const CATALOG_FILE_SUFFIX: &str = ".json";
const CATALOG_STAGING_PREFIX: &str = ".catalog-write-";
const CATALOG_STAGING_SUFFIX: &str = ".tmp";
const CATALOG_MAX_BYTES: u64 = 256 * 1024;
const CATALOG_MAX_ENTRIES: usize = 256;
const CATALOG_MAX_PATH_BYTES: usize = 16 * 1024;
const CATALOG_INVENTORY_NAMESPACE: &str = "ctx.explicit-source-catalog-entry";
const CATALOG_INVENTORY_REVISION_KIND: &str = "ctx-explicit-source-catalog-sha256-v1";
const CATALOG_DISCOVERY_REVISION: &str = "ctx-explicit-source-catalog-v1";
const CUSTOM_SOURCE_FORMAT: &str = "ctx_history_jsonl_v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExplicitSourceCatalogAuthority {
    schema_version: u32,
    revision: u64,
    integrity_sha256: [u8; 32],
}

impl ExplicitSourceCatalogAuthority {
    #[cfg(test)]
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn integrity_hex(&self) -> String {
        encode_hex(&self.integrity_sha256)
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "schema_version": self.schema_version,
            "revision": self.revision,
            "integrity": {
                "algorithm": CATALOG_INTEGRITY_ALGORITHM,
                "digest": self.integrity_hex(),
            },
        })
    }

    pub(crate) fn from_json(value: &Value) -> Result<Self> {
        let wire: CatalogAuthorityWire = serde_json::from_value(value.clone())
            .context("decode explicit source catalog request metadata")?;
        if wire.schema_version != CATALOG_SCHEMA_VERSION {
            bail!(
                "unsupported explicit source catalog metadata schema {}",
                wire.schema_version
            );
        }
        if wire.integrity.algorithm != CATALOG_INTEGRITY_ALGORITHM {
            bail!(
                "unsupported explicit source catalog integrity algorithm `{}`",
                wire.integrity.algorithm
            );
        }
        Ok(Self {
            schema_version: wire.schema_version,
            revision: wire.revision,
            integrity_sha256: decode_digest(&wire.integrity.digest)?,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExplicitSourceCatalogUpsert {
    pub(crate) authority: ExplicitSourceCatalogAuthority,
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) catalog_lineage: [u8; 32],
    pub(crate) changed: bool,
}

impl ExplicitSourceCatalogUpsert {
    pub(crate) fn catalog_lineage_hex(&self) -> String {
        encode_hex(&self.catalog_lineage)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CatalogEntry {
    provider: String,
    source_format: String,
    path: PathBuf,
    catalog_lineage: String,
    enabled: bool,
}

impl CatalogEntry {
    fn provider(&self) -> Result<CaptureProvider> {
        self.provider.parse().with_context(|| {
            format!(
                "invalid explicit source catalog provider `{}`",
                self.provider
            )
        })
    }

    fn lineage(&self) -> Result<[u8; 32]> {
        decode_digest(&self.catalog_lineage).context("decode explicit source catalog lineage")
    }

    fn route_metadata(
        &self,
    ) -> Result<&'static ctx_history_capture::SourceBackedProviderRouteMetadata> {
        route_metadata(self.provider()?, &self.source_format)
    }

    fn certified_source_format(&self) -> Result<&'static str> {
        Ok(self.route_metadata()?.certified_source_format)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    schema_version: u32,
    revision: u64,
    entries: Vec<CatalogEntry>,
    integrity: CatalogIntegrity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogIntegrity {
    algorithm: String,
    digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogAuthorityWire {
    schema_version: u32,
    revision: u64,
    integrity: CatalogIntegrity,
}

#[derive(Serialize)]
struct CatalogPayload<'a> {
    schema_version: u32,
    revision: u64,
    entries: &'a [CatalogEntry],
}

#[derive(Debug, Clone)]
struct ExplicitSourceCatalogSnapshot {
    authority: ExplicitSourceCatalogAuthority,
    entries: Vec<CatalogEntry>,
}

impl ExplicitSourceCatalogSnapshot {
    fn empty() -> Result<Self> {
        let entries = Vec::new();
        let authority = authority_for(0, &entries)?;
        Ok(Self { authority, entries })
    }
}

pub(crate) fn explicit_source_for_import(args: &ImportArgs) -> Result<Option<ProviderSource>> {
    let Some(path) = args.path.as_deref() else {
        return Ok(None);
    };
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("approve explicit source path {}", path.display()))?;
    validate_approved_path(&canonical)?;

    let source = if let Some(format) = args.input_format {
        match format {
            ImportFormatArg::CtxHistoryJsonlV1 => custom_provider_source(canonical, true)?,
        }
    } else {
        let provider = args
            .provider
            .context("ctx import --path requires --provider for native provider history")?
            .capture_provider();
        provider_source_for_path(provider, canonical)
    };
    validate_enabled_source(&source)?;
    validate_catalog_registration_support(&source)?;
    Ok(Some(source))
}

pub(crate) fn upsert_explicit_source(
    data_root: &Path,
    source: &ProviderSource,
) -> Result<ExplicitSourceCatalogUpsert> {
    validate_enabled_source(source)?;
    validate_catalog_registration_support(source)?;
    let metadata = route_metadata(source.provider, source.source_format)?;
    let catalog_root = catalog_root(data_root);
    fs::create_dir_all(&catalog_root).with_context(|| {
        format!(
            "create explicit source catalog directory {}",
            catalog_root.display()
        )
    })?;
    let lock = open_catalog_lock(&catalog_root, true)?;
    FileExt::lock_exclusive(&lock).context("lock explicit source catalog for update")?;
    let mut snapshot = load_catalog_unlocked(&catalog_root)?;

    let existing_index = snapshot
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            (entry.provider == source.provider.as_str()
                && entry
                    .certified_source_format()
                    .is_ok_and(|format| format == metadata.certified_source_format))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    if existing_index.len() > 1 {
        bail!(
            "explicit source catalog contains duplicate {}/{} authority",
            source.provider.as_str(),
            metadata.certified_source_format
        );
    }

    let (catalog_lineage, changed) = if let Some(index) = existing_index.first().copied() {
        let entry = &mut snapshot.entries[index];
        let lineage = entry.lineage()?;
        if entry.path == source.path && entry.source_format == source.source_format && entry.enabled
        {
            (lineage, false)
        } else {
            if entry.enabled
                && entry.path != source.path
                && entry.path.try_exists().with_context(|| {
                    format!("check prior explicit source path {}", entry.path.display())
                })?
            {
                bail!(
                    "explicit {}/{} authority is already enabled at {}; refusing to treat {} as a move while the prior path still exists",
                    source.provider.as_str(),
                    metadata.certified_source_format,
                    entry.path.display(),
                    source.path.display()
                );
            }
            entry.source_format = source.source_format.to_owned();
            entry.path = source.path.clone();
            entry.enabled = true;
            (lineage, true)
        }
    } else {
        let lineage = random_catalog_lineage();
        snapshot.entries.push(CatalogEntry {
            provider: source.provider.as_str().to_owned(),
            source_format: source.source_format.to_owned(),
            path: source.path.clone(),
            catalog_lineage: encode_hex(&lineage),
            enabled: true,
        });
        (lineage, true)
    };

    if changed {
        sort_and_validate_entries(&mut snapshot.entries)?;
        let revision = snapshot
            .authority
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("explicit source catalog revision overflow"))?;
        snapshot.authority = authority_for(revision, &snapshot.entries)?;
        write_catalog_snapshot(&catalog_root, &snapshot)?;
    }

    Ok(ExplicitSourceCatalogUpsert {
        authority: snapshot.authority,
        provider: source.provider,
        source_format: metadata.source_format,
        path: source.path.clone(),
        catalog_lineage,
        changed,
    })
}

#[allow(dead_code)] // The catalog management command consumes this narrow seam next.
pub(crate) fn disable_explicit_source(
    data_root: &Path,
    provider: CaptureProvider,
    source_format: &str,
) -> Result<ExplicitSourceCatalogAuthority> {
    let metadata = route_metadata(provider, source_format)?;
    let catalog_root = catalog_root(data_root);
    let lock = open_catalog_lock(&catalog_root, false)?;
    FileExt::lock_exclusive(&lock).context("lock explicit source catalog for disable")?;
    let mut snapshot = load_catalog_unlocked(&catalog_root)?;
    let mut changed = false;
    let mut found = false;
    for entry in &mut snapshot.entries {
        if entry.provider == provider.as_str()
            && entry.certified_source_format()? == metadata.certified_source_format
        {
            if found {
                bail!(
                    "explicit source catalog contains duplicate {}/{} authority",
                    provider.as_str(),
                    metadata.certified_source_format
                );
            }
            found = true;
            if entry.enabled {
                entry.enabled = false;
                changed = true;
            }
        }
    }
    if !found {
        bail!(
            "explicit source catalog has no {}/{} entry to disable",
            provider.as_str(),
            metadata.certified_source_format
        );
    }
    if changed {
        let revision = snapshot
            .authority
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("explicit source catalog revision overflow"))?;
        snapshot.authority = authority_for(revision, &snapshot.entries)?;
        write_catalog_snapshot(&catalog_root, &snapshot)?;
    }
    Ok(snapshot.authority)
}

pub(crate) fn load_explicit_source_catalog_authority(
    data_root: &Path,
) -> Result<ExplicitSourceCatalogAuthority> {
    Ok(load_catalog(data_root)?.authority)
}

pub(crate) fn register_explicit_source_catalog_routes(
    data_root: &Path,
    index_root: &Path,
    build: &mut SourceBackedAutomaticRegistryBuild,
) -> Result<ExplicitSourceCatalogAuthority> {
    let snapshot = load_catalog(data_root)?;
    let automatic_authorities = build
        .registry
        .routes()
        .filter(|route| route.selection == Some(SourceBackedRouteSelection::Automatic))
        .map(|route| {
            (
                route.source.provider,
                route.certified_source_format.to_owned(),
            )
        })
        .collect::<HashSet<_>>();
    for entry in &snapshot.entries {
        let provider = entry.provider()?;
        let certified_format = entry.certified_source_format()?;
        if automatic_authorities.contains(&(provider, certified_format.to_owned())) {
            bail!(
                "explicit source catalog authority {}/{} conflicts with automatic provider discovery; disable the catalog entry or remove the duplicate automatic authority",
                provider.as_str(),
                certified_format
            );
        }
    }

    let needs_base_sources = snapshot.entries.iter().any(|entry| !entry.enabled);
    let base_sources = if needs_base_sources && index_root.join("meta.json").is_file() {
        VerifiedIndex::open(index_root)
            .with_context(|| {
                format!(
                    "open source-backed generation for catalog reconciliation {}",
                    index_root.display()
                )
            })?
            .manifest()
            .sources
            .iter()
            .map(|source| source.observation().source().clone())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    for entry in &snapshot.entries {
        if entry.enabled {
            let source = source_from_catalog_entry(entry, true)?;
            register_enabled_catalog_route(&mut build.registry, source, entry.lineage()?)
                .with_context(|| {
                    format!(
                        "register explicit catalog route {} {}",
                        entry.provider,
                        entry.path.display()
                    )
                })?;
        } else {
            register_disabled_catalog_route(
                data_root,
                &snapshot,
                entry,
                &base_sources,
                &mut build.registry,
            )?;
        }
    }
    Ok(snapshot.authority)
}

fn register_enabled_catalog_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    lineage: [u8; 32],
) -> Result<()> {
    let constructor = source_backed_route_constructor(source.provider).ok_or_else(|| {
        anyhow!(
            "{} has no source-backed registration constructor",
            source.provider.as_str()
        )
    })?;
    match constructor {
        SourceBackedRouteConstructor::CatalogLineage => match source.provider {
            CaptureProvider::Custom => {
                register_custom_history_source_backed_route(registry, source, lineage)?
            }
            CaptureProvider::NanoClaw => {
                register_nanoclaw_source_backed_route(registry, source, lineage)?
            }
            provider => bail!(
                "{} has an unknown catalog-lineage registration",
                provider.as_str()
            ),
        },
        SourceBackedRouteConstructor::ProviderSource => match source.provider {
            CaptureProvider::ForgeCode => {
                register_forgecode_explicit_source_backed_route(registry, source, lineage)?
            }
            CaptureProvider::Hermes => register_hermes_explicit_source_backed_route(
                registry,
                source,
                SourceAnchor::CatalogLineage(lineage),
            )?,
            _ => register_landed_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::ExplicitManual,
            )?,
        },
        SourceBackedRouteConstructor::FiniteInventory => match source.provider {
            CaptureProvider::Lingma => {
                let authority = TypedKey::bytes(lineage.to_vec())?;
                let database_lineage = TypedKey::bytes(lineage.to_vec())?;
                let path = source.path.clone();
                register_lingma_source_backed_route(
                    registry,
                    source,
                    SourceBackedRouteSelection::ExplicitManual,
                    authority,
                    vec![(path, database_lineage)],
                )?;
            }
            CaptureProvider::Crush => bail!(
                "crush explicit source format has no externally constructible finite-inventory adapter; no legacy import fallback was used"
            ),
            provider => bail!(
                "{} has an unknown finite-inventory registration",
                provider.as_str()
            ),
        },
        SourceBackedRouteConstructor::DiscoveryContext => bail!(
            "{} explicit source format requires provider discovery authority and cannot be cataloged by path; no legacy import fallback was used",
            source.provider.as_str()
        ),
        SourceBackedRouteConstructor::ExactCwd => bail!(
            "{} does not expose an explicit source-backed adapter; no legacy import fallback was used",
            source.provider.as_str()
        ),
        SourceBackedRouteConstructor::NamedSurface => {
            register_warp_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::ExplicitManual,
                format!("ctx-catalog:{}", encode_hex(&lineage)),
            )?;
        }
        SourceBackedRouteConstructor::SelectedWithRetainedRoutes => {
            let platform_root = goose_platform_root(&source.path)?;
            register_goose_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::ExplicitManual,
                platform_root,
                Vec::new(),
            )?;
        }
    }
    Ok(())
}

fn register_disabled_catalog_route(
    data_root: &Path,
    snapshot: &ExplicitSourceCatalogSnapshot,
    entry: &CatalogEntry,
    base_sources: &[SourceKey],
    registry: &mut SourceBackedProviderRegistry,
) -> Result<()> {
    let provider = entry.provider()?;
    let lineage = entry.lineage()?;
    let certified_format = entry.certified_source_format()?;
    let lineage_targets = base_sources
        .iter()
        .filter(|source| {
            source.provider() == provider.as_str()
                && source.source_format() == certified_format
                && source.anchor() == &SourceAnchor::CatalogLineage(lineage)
        })
        .cloned()
        .collect::<Vec<_>>();
    let targets = if lineage_targets.is_empty() {
        base_sources
            .iter()
            .filter(|source| {
                source.provider() == provider.as_str() && source.source_format() == certified_format
            })
            .cloned()
            .collect::<Vec<_>>()
    } else {
        lineage_targets
    };
    let inventory = catalog_disabled_inventory(&snapshot.authority, entry)?;
    let scan_inventory = inventory.clone();
    let scan_targets = targets.clone();
    let owned_targets = targets.clone();
    let revalidation_root = data_root.to_path_buf();
    let expected_authority = snapshot.authority.clone();
    let expected_lineage = lineage;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            for source in &scan_targets {
                if sink.base_source(source).is_none() {
                    continue;
                }
                let deletion =
                    CertifiedSourceDeletion::from_inventory(source.clone(), &scan_inventory)
                        .map_err(route_internal_error)?;
                sink.delete_source(deletion, scan_inventory.clone())
                    .map_err(route_internal_error)?;
            }
            Ok(())
        },
        move |candidate| {
            owned_targets
                .iter()
                .any(|source| source.exact_descriptor_eq(candidate))
        },
        move |target| {
            let ctx_history_capture::SourceBackedRevalidationTarget::Deletion(deletion) = target
            else {
                return false;
            };
            let Ok(current) = load_catalog(&revalidation_root) else {
                return false;
            };
            if current.authority != expected_authority {
                return false;
            }
            let Some(entry) = current
                .entries
                .iter()
                .find(|entry| entry.lineage().ok() == Some(expected_lineage))
            else {
                return false;
            };
            if entry.enabled {
                return false;
            }
            catalog_disabled_inventory(&current.authority, entry)
                .is_ok_and(|inventory| deletion.verifies(&inventory))
        },
        move |_| {
            Err(HydrationFailure {
                kind: HydrationFailureKind::ConfirmedDeleted,
                detail: "the explicit source was disabled by complete catalog authority".to_owned(),
            })
        },
    );
    let source = source_from_catalog_entry(entry, false)?;
    registry.register(SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::ExplicitPath,
        driver,
    )?);
    Ok(())
}

fn catalog_disabled_inventory(
    authority: &ExplicitSourceCatalogAuthority,
    entry: &CatalogEntry,
) -> Result<CertifiedSourceInventory> {
    let provider = entry.provider()?;
    let opening = SourceInventoryObservation::new(
        provider.as_str(),
        CATALOG_INVENTORY_NAMESPACE,
        TypedKey::bytes(entry.lineage()?.to_vec())?,
        CATALOG_INVENTORY_REVISION_KIND,
        authority.integrity_sha256.to_vec(),
    )?;
    Ok(CertifiedSourceInventory::certify(
        opening.clone(),
        opening,
        CATALOG_DISCOVERY_REVISION,
        Vec::new(),
    )?)
}

fn route_internal_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

fn validate_catalog_registration_support(source: &ProviderSource) -> Result<()> {
    let metadata = route_metadata(source.provider, source.source_format)?;
    if !metadata.explicit_manual {
        bail!(
            "{} source format `{}` has no explicit source-backed adapter; no legacy import fallback was used",
            source.provider.as_str(),
            source.source_format
        );
    }
    if let Some(reason) = metadata.unsupported_reason {
        bail!(
            "{} source format `{}` is not source-backed: {reason}; no legacy import fallback was used",
            source.provider.as_str(),
            source.source_format
        );
    }
    if metadata.exact_hydration == SourceBackedHydrationSupport::Unsupported {
        bail!(
            "{} source format `{}` has no exact source-backed resolver; no legacy import fallback was used",
            source.provider.as_str(),
            source.source_format
        );
    }
    match source_backed_route_constructor(source.provider) {
        Some(SourceBackedRouteConstructor::FiniteInventory)
            if source.provider == CaptureProvider::Crush =>
        {
            bail!(
                "crush explicit source format has no externally constructible finite-inventory adapter; no legacy import fallback was used"
            )
        }
        Some(SourceBackedRouteConstructor::DiscoveryContext) => bail!(
            "{} explicit source format requires provider discovery authority and cannot be cataloged by path; no legacy import fallback was used",
            source.provider.as_str()
        ),
        Some(SourceBackedRouteConstructor::ExactCwd) => bail!(
            "{} does not expose an explicit source-backed adapter; no legacy import fallback was used",
            source.provider.as_str()
        ),
        Some(_) => Ok(()),
        None => bail!(
            "{} has no source-backed registration constructor; no legacy import fallback was used",
            source.provider.as_str()
        ),
    }
}

fn validate_enabled_source(source: &ProviderSource) -> Result<()> {
    validate_approved_path(&source.path)?;
    if !source
        .path
        .try_exists()
        .with_context(|| format!("check explicit source path {}", source.path.display()))?
    {
        bail!(
            "explicit source path {} is unavailable; missing paths are not deletion authority",
            source.path.display()
        );
    }
    if source.status != ProviderSourceStatus::Available
        || !source.import_support.is_importable()
        || source.source_kind == ProviderSourceKind::DetectionOnly
        || source.unsupported_reason.is_some()
    {
        let reason = source
            .unsupported_reason
            .unwrap_or("the provider path or format is not supported");
        bail!(
            "{} explicit source {} is not importable: {reason}",
            source.provider.as_str(),
            source.path.display()
        );
    }
    Ok(())
}

fn route_metadata(
    provider: CaptureProvider,
    source_format: &str,
) -> Result<&'static ctx_history_capture::SourceBackedProviderRouteMetadata> {
    source_backed_route_inventory()
        .iter()
        .find(|route| route.provider == provider && route.source_format == source_format)
        .ok_or_else(|| {
            anyhow!(
                "{} source format `{source_format}` has no landed source-backed adapter; no legacy import fallback was used",
                provider.as_str()
            )
        })
}

fn source_from_catalog_entry(
    entry: &CatalogEntry,
    require_available: bool,
) -> Result<ProviderSource> {
    let provider = entry.provider()?;
    let metadata = entry.route_metadata()?;
    let exists = entry
        .path
        .try_exists()
        .with_context(|| format!("check catalog source path {}", entry.path.display()))?;
    if require_available && !exists {
        bail!(
            "enabled explicit catalog source {} is unavailable; missing paths are not deletion authority",
            entry.path.display()
        );
    }
    if provider == CaptureProvider::Custom {
        return custom_provider_source(entry.path.clone(), exists);
    }
    if exists {
        let observed = provider_source_for_path(provider, entry.path.clone());
        if observed.source_format != metadata.source_format {
            bail!(
                "explicit catalog source {} changed format from `{}` to `{}`",
                entry.path.display(),
                metadata.source_format,
                observed.source_format
            );
        }
        validate_enabled_source(&observed)?;
        return Ok(observed);
    }
    Ok(ProviderSource {
        provider,
        path: entry.path.clone(),
        exists: false,
        source_format: metadata.source_format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Missing,
        unsupported_reason: None,
    })
}

fn custom_provider_source(path: PathBuf, exists: bool) -> Result<ProviderSource> {
    if exists {
        let metadata = fs::metadata(&path)
            .with_context(|| format!("inspect Custom History source {}", path.display()))?;
        if !metadata.is_file() {
            bail!(
                "Custom History source must be one regular JSONL file: {}",
                path.display()
            );
        }
    }
    Ok(ProviderSource {
        provider: CaptureProvider::Custom,
        path,
        exists,
        source_format: CUSTOM_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status: if exists {
            ProviderSourceStatus::Available
        } else {
            ProviderSourceStatus::Missing
        },
        unsupported_reason: None,
    })
}

fn goose_platform_root(database: &Path) -> Result<PathBuf> {
    let sessions = database.parent().ok_or_else(|| {
        anyhow!(
            "Goose database has no sessions directory: {}",
            database.display()
        )
    })?;
    sessions.parent().map(Path::to_path_buf).ok_or_else(|| {
        anyhow!(
            "Goose database has no platform root: {}",
            database.display()
        )
    })
}

fn validate_approved_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!(
            "explicit source catalog paths must be absolute: {}",
            path.display()
        );
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!(
            "explicit source catalog paths must be normalized: {}",
            path.display()
        );
    }
    let text = path.to_str().ok_or_else(|| {
        anyhow!(
            "explicit source catalog paths must be valid UTF-8: {}",
            path.display()
        )
    })?;
    if text.len() > CATALOG_MAX_PATH_BYTES {
        bail!(
            "explicit source catalog path exceeds {CATALOG_MAX_PATH_BYTES} bytes: {}",
            path.display()
        );
    }
    Ok(())
}

fn sort_and_validate_entries(entries: &mut Vec<CatalogEntry>) -> Result<()> {
    if entries.len() > CATALOG_MAX_ENTRIES {
        bail!("explicit source catalog exceeds its {CATALOG_MAX_ENTRIES}-entry bound");
    }
    entries.sort_by(|left, right| {
        (
            left.provider.as_str(),
            left.source_format.as_str(),
            left.catalog_lineage.as_str(),
        )
            .cmp(&(
                right.provider.as_str(),
                right.source_format.as_str(),
                right.catalog_lineage.as_str(),
            ))
    });
    let mut lineages = HashSet::new();
    let mut authorities = HashSet::new();
    for entry in entries.iter() {
        validate_approved_path(&entry.path)?;
        let provider = entry.provider()?;
        let lineage = entry.lineage()?;
        if !lineages.insert(lineage) {
            bail!("explicit source catalog contains duplicate catalog lineage");
        }
        let metadata = entry.route_metadata()?;
        if !metadata.explicit_manual || metadata.unsupported_reason.is_some() {
            bail!(
                "{} source format `{}` is not an enabled explicit source-backed contract",
                provider.as_str(),
                entry.source_format
            );
        }
        if !authorities.insert((provider, metadata.certified_source_format)) {
            bail!(
                "explicit source catalog contains duplicate {}/{} authority",
                provider.as_str(),
                metadata.certified_source_format
            );
        }
    }
    Ok(())
}

fn catalog_root(data_root: &Path) -> PathBuf {
    data_root.join(CATALOG_DIRECTORY)
}

fn load_catalog(data_root: &Path) -> Result<ExplicitSourceCatalogSnapshot> {
    let root = catalog_root(data_root);
    if !root
        .try_exists()
        .with_context(|| format!("check explicit source catalog directory {}", root.display()))?
    {
        return ExplicitSourceCatalogSnapshot::empty();
    }
    let lock = open_catalog_lock(&root, false)?;
    FileExt::lock_shared(&lock).context("lock explicit source catalog for read")?;
    load_catalog_unlocked(&root)
}

fn load_catalog_unlocked(root: &Path) -> Result<ExplicitSourceCatalogSnapshot> {
    if !root.exists() {
        return ExplicitSourceCatalogSnapshot::empty();
    }
    let mut revisions = Vec::new();
    for item in fs::read_dir(root)
        .with_context(|| format!("read explicit source catalog {}", root.display()))?
    {
        let item = item?;
        let name = item.file_name();
        let name = name.to_str().ok_or_else(|| {
            anyhow!("explicit source catalog contains a non-UTF-8 state filename")
        })?;
        if name == CATALOG_LOCK_FILE
            || (name.starts_with(CATALOG_STAGING_PREFIX) && name.ends_with(CATALOG_STAGING_SUFFIX))
        {
            continue;
        }
        let revision = parse_catalog_revision_filename(name)
            .ok_or_else(|| anyhow!("unexpected explicit source catalog state file `{name}`"))?;
        revisions.push((revision, item.path()));
    }
    let Some((filename_revision, path)) =
        revisions.into_iter().max_by_key(|(revision, _)| *revision)
    else {
        return ExplicitSourceCatalogSnapshot::empty();
    };
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("inspect explicit source catalog {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!(
            "explicit source catalog revision is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() > CATALOG_MAX_BYTES {
        bail!(
            "explicit source catalog {} exceeds its {CATALOG_MAX_BYTES}-byte bound",
            path.display()
        );
    }
    let file = File::open(&path)
        .with_context(|| format!("open explicit source catalog {}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(CATALOG_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read explicit source catalog {}", path.display()))?;
    if bytes.len() as u64 > CATALOG_MAX_BYTES {
        bail!(
            "explicit source catalog {} exceeds its {CATALOG_MAX_BYTES}-byte bound",
            path.display()
        );
    }
    let mut wire: CatalogFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("decode explicit source catalog {}", path.display()))?;
    if wire.schema_version != CATALOG_SCHEMA_VERSION {
        bail!(
            "unsupported explicit source catalog schema {}",
            wire.schema_version
        );
    }
    if wire.revision != filename_revision {
        bail!(
            "explicit source catalog filename revision {filename_revision} does not match body revision {}",
            wire.revision
        );
    }
    if wire.integrity.algorithm != CATALOG_INTEGRITY_ALGORITHM {
        bail!(
            "unsupported explicit source catalog integrity algorithm `{}`",
            wire.integrity.algorithm
        );
    }
    let expected = authority_for(wire.revision, &wire.entries)?;
    if decode_digest(&wire.integrity.digest)? != expected.integrity_sha256 {
        bail!(
            "explicit source catalog integrity check failed for {}",
            path.display()
        );
    }
    let original = wire.entries.clone();
    sort_and_validate_entries(&mut wire.entries)?;
    if wire.entries != original {
        bail!("explicit source catalog entries are not in canonical order");
    }
    Ok(ExplicitSourceCatalogSnapshot {
        authority: expected,
        entries: wire.entries,
    })
}

fn open_catalog_lock(root: &Path, create: bool) -> Result<File> {
    let path = root.join(CATALOG_LOCK_FILE);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&path)
        .with_context(|| format!("open explicit source catalog lock {}", path.display()))
}

fn write_catalog_snapshot(root: &Path, snapshot: &ExplicitSourceCatalogSnapshot) -> Result<()> {
    let path = root.join(catalog_revision_filename(snapshot.authority.revision));
    if path.exists() {
        bail!(
            "explicit source catalog revision already exists: {}",
            path.display()
        );
    }
    let wire = CatalogFile {
        schema_version: CATALOG_SCHEMA_VERSION,
        revision: snapshot.authority.revision,
        entries: snapshot.entries.clone(),
        integrity: CatalogIntegrity {
            algorithm: CATALOG_INTEGRITY_ALGORITHM.to_owned(),
            digest: snapshot.authority.integrity_hex(),
        },
    };
    let mut bytes = serde_json::to_vec_pretty(&wire)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > CATALOG_MAX_BYTES {
        bail!("explicit source catalog revision would exceed its {CATALOG_MAX_BYTES}-byte bound");
    }
    let staged = root.join(format!(
        "{CATALOG_STAGING_PREFIX}{}{CATALOG_STAGING_SUFFIX}",
        Uuid::new_v4()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&staged)
        .with_context(|| format!("create staged explicit source catalog {}", staged.display()))?;
    let write_result = (|| -> Result<()> {
        file.write_all(&bytes)
            .context("write staged explicit source catalog")?;
        file.sync_all()
            .context("sync staged explicit source catalog")?;
        fs::rename(&staged, &path).with_context(|| {
            format!(
                "publish explicit source catalog revision {}",
                snapshot.authority.revision
            )
        })?;
        sync_catalog_directory(root)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    write_result
}

#[cfg(unix)]
fn sync_catalog_directory(root: &Path) -> Result<()> {
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync explicit source catalog directory {}", root.display()))
}

#[cfg(not(unix))]
fn sync_catalog_directory(_root: &Path) -> Result<()> {
    Ok(())
}

fn authority_for(
    revision: u64,
    entries: &[CatalogEntry],
) -> Result<ExplicitSourceCatalogAuthority> {
    let payload = serde_json::to_vec(&CatalogPayload {
        schema_version: CATALOG_SCHEMA_VERSION,
        revision,
        entries,
    })?;
    let mut digest = Sha256::new();
    digest.update(b"ctx.explicit-source-catalog-v1\0");
    digest.update(payload);
    Ok(ExplicitSourceCatalogAuthority {
        schema_version: CATALOG_SCHEMA_VERSION,
        revision,
        integrity_sha256: digest.finalize().into(),
    })
}

fn catalog_revision_filename(revision: u64) -> String {
    format!("{CATALOG_FILE_PREFIX}{revision:020}{CATALOG_FILE_SUFFIX}")
}

fn parse_catalog_revision_filename(name: &str) -> Option<u64> {
    let revision = name
        .strip_prefix(CATALOG_FILE_PREFIX)?
        .strip_suffix(CATALOG_FILE_SUFFIX)?;
    if revision.len() != 20 || !revision.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    revision.parse().ok()
}

fn random_catalog_lineage() -> [u8; 32] {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut lineage = [0_u8; 32];
    lineage[..16].copy_from_slice(first.as_bytes());
    lineage[16..].copy_from_slice(second.as_bytes());
    lineage
}

fn encode_hex(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_digest(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("catalog digest must be 64 hexadecimal characters");
    }
    let mut decoded = [0_u8; 32];
    for (index, output) in decoded.iter_mut().enumerate() {
        let offset = index * 2;
        *output = (decode_nibble(value.as_bytes()[offset])? << 4)
            | decode_nibble(value.as_bytes()[offset + 1])?;
    }
    Ok(decoded)
}

fn decode_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("invalid hexadecimal catalog digest"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ctx_history_capture::SourceBackedRefreshExecutor;
    use ctx_history_index::WriterOptions;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn durable_state_path_is_purpose_based() {
        assert_eq!(
            catalog_root(Path::new("ctx-data")),
            Path::new("ctx-data/catalogs/explicit-sources")
        );
    }

    fn custom_source(path: &Path) -> ProviderSource {
        custom_provider_source(path.to_path_buf(), true).unwrap()
    }

    fn write_custom_history(path: &Path, marker: &str) {
        let records = [
            json!({
                "record_type": "manifest",
                "schema_version": "ctx-history-jsonl-v1",
            }),
            json!({
                "record_type": "source",
                "source_id": "catalog-source",
                "provider_key": "catalog-provider",
                "source_format": "catalog-jsonl",
            }),
            json!({
                "record_type": "session",
                "source_id": "catalog-source",
                "session_id": "catalog-session",
                "started_at": "2026-07-28T12:00:00Z",
            }),
            json!({
                "record_type": "event",
                "source_id": "catalog-source",
                "session_id": "catalog-session",
                "event_index": 0,
                "event_type": "message",
                "role": "user",
                "occurred_at": "2026-07-28T12:00:01Z",
                "payload": {"text": marker},
                "preview": marker,
            }),
        ];
        fs::write(
            path,
            records
                .into_iter()
                .map(|record| record.to_string())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
    }

    fn empty_build() -> SourceBackedAutomaticRegistryBuild {
        SourceBackedAutomaticRegistryBuild {
            registry: SourceBackedProviderRegistry::new(),
            issues: Vec::new(),
            discovery_duration: Duration::ZERO,
        }
    }

    fn refresh_catalog(data_root: &Path, index_root: &Path) {
        let mut build = empty_build();
        register_explicit_source_catalog_routes(data_root, index_root, &mut build).unwrap();
        SourceBackedRefreshExecutor::new(build.registry, WriterOptions::default())
            .refresh(
                index_root,
                |_: ctx_history_capture::SourceBackedRefreshProgress| {
                    Ok::<(), SourceBackedRouteError>(())
                },
            )
            .unwrap();
    }

    #[test]
    fn first_add_and_idempotent_upsert_are_metadata_only() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("custom.jsonl");
        write_custom_history(&source_path, "catalog first add");
        let source = custom_source(&source_path);

        let first = upsert_explicit_source(temp.path(), &source).unwrap();
        let second = upsert_explicit_source(temp.path(), &source).unwrap();

        assert!(first.changed);
        assert!(!second.changed);
        assert_eq!(first.authority, second.authority);
        assert_eq!(first.catalog_lineage, second.catalog_lineage);
        assert_eq!(first.authority.revision(), 1);
        assert!(!ctx_history_core::database_path(temp.path().to_path_buf()).exists());
        let bytes = fs::read(catalog_root(temp.path()).join(catalog_revision_filename(1))).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        for forbidden in [
            "preview",
            "payload",
            "credential",
            "event_id",
            "session_id",
            "raw_source_path",
        ] {
            assert!(!text.contains(forbidden), "{forbidden} leaked into catalog");
        }
    }

    #[test]
    fn path_move_preserves_lineage_only_after_prior_path_is_absent() {
        let temp = tempdir().unwrap();
        let first_path = temp.path().join("first.jsonl");
        let second_path = temp.path().join("second.jsonl");
        write_custom_history(&first_path, "first path");
        write_custom_history(&second_path, "second path");
        let first = upsert_explicit_source(temp.path(), &custom_source(&first_path)).unwrap();

        let conflict =
            upsert_explicit_source(temp.path(), &custom_source(&second_path)).unwrap_err();
        assert!(conflict.to_string().contains("prior path still exists"));

        fs::remove_file(&first_path).unwrap();
        let moved = upsert_explicit_source(temp.path(), &custom_source(&second_path)).unwrap();
        assert_eq!(moved.catalog_lineage, first.catalog_lineage);
        assert_eq!(moved.authority.revision(), 2);
    }

    #[test]
    fn disable_publishes_certified_deletion_but_missing_enabled_path_does_not() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("custom.jsonl");
        let index_root = temp.path().join("index");
        write_custom_history(&source_path, "certified catalog deletion");
        let source = custom_source(&source_path);
        upsert_explicit_source(temp.path(), &source).unwrap();
        refresh_catalog(temp.path(), &index_root);
        let first = VerifiedIndex::open(&index_root).unwrap();
        let first_generation = first.generation_id().to_owned();
        assert_eq!(first.manifest().sources.len(), 1);

        fs::remove_file(&source_path).unwrap();
        let mut missing_build = empty_build();
        let missing =
            register_explicit_source_catalog_routes(temp.path(), &index_root, &mut missing_build)
                .unwrap_err();
        assert!(missing
            .to_string()
            .contains("missing paths are not deletion authority"));
        let retained = VerifiedIndex::open(&index_root).unwrap();
        assert_eq!(retained.generation_id(), first_generation);
        assert_eq!(retained.manifest().sources.len(), 1);

        disable_explicit_source(temp.path(), CaptureProvider::Custom, CUSTOM_SOURCE_FORMAT)
            .unwrap();
        refresh_catalog(temp.path(), &index_root);
        let deleted = VerifiedIndex::open(&index_root).unwrap();
        assert!(deleted.manifest().sources.is_empty());
        assert_eq!(deleted.manifest().removals.len(), 1);
        assert!(deleted.manifest().removals[0]
            .deletion()
            .verifies(deleted.manifest().removals[0].inventory()));
    }

    #[test]
    fn custom_history_refreshes_end_to_end_without_work_sqlite() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("custom.jsonl");
        let index_root = temp.path().join("index");
        write_custom_history(&source_path, "source catalog end to end");
        upsert_explicit_source(temp.path(), &custom_source(&source_path)).unwrap();

        refresh_catalog(temp.path(), &index_root);

        let verified = VerifiedIndex::open(&index_root).unwrap();
        assert_eq!(verified.manifest().sources.len(), 1);
        assert_eq!(verified.manifest().indexed_documents, 1);
        assert!(!ctx_history_core::database_path(temp.path().to_path_buf()).exists());
    }

    #[test]
    fn malformed_committed_catalog_fails_closed_but_abandoned_staging_is_ignored() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("custom.jsonl");
        write_custom_history(&source_path, "catalog integrity");
        upsert_explicit_source(temp.path(), &custom_source(&source_path)).unwrap();
        let root = catalog_root(temp.path());
        fs::write(
            root.join(format!(
                "{CATALOG_STAGING_PREFIX}orphan{CATALOG_STAGING_SUFFIX}"
            )),
            b"{malformed",
        )
        .unwrap();
        assert_eq!(load_catalog(temp.path()).unwrap().authority.revision(), 1);

        fs::write(
            root.join(catalog_revision_filename(1)),
            b"{\"schema_version\":1}",
        )
        .unwrap();
        let error = load_catalog(temp.path()).unwrap_err();
        assert!(error.to_string().contains("decode explicit source catalog"));
    }

    #[test]
    fn unsupported_explicit_format_is_rejected_before_catalog_creation() {
        let temp = tempdir().unwrap();
        let rollout = temp.path().join("rollout.jsonl");
        fs::write(
            &rollout,
            r#"{"timestamp":"2026-07-28T12:00:00Z","type":"event_msg","payload":{}}"#,
        )
        .unwrap();
        let source = provider_source_for_path(CaptureProvider::Codex, rollout);
        let error = upsert_explicit_source(temp.path(), &source).unwrap_err();
        assert!(error.to_string().contains("not source-backed"));
        assert!(!catalog_root(temp.path()).exists());
    }

    #[test]
    fn automatic_and_explicit_authorities_merge_or_fail_without_double_ingestion() {
        let temp = tempdir().unwrap();
        let custom = temp.path().join("custom.jsonl");
        write_custom_history(&custom, "automatic explicit merge");
        upsert_explicit_source(temp.path(), &custom_source(&custom)).unwrap();

        let automatic_source_path = temp.path().join("automatic.jsonl");
        fs::write(
            &automatic_source_path,
            r#"{"session_id":"one","ts":1,"text":"automatic"}"#,
        )
        .unwrap();
        let automatic_source =
            provider_source_for_path(CaptureProvider::Codex, automatic_source_path);
        let mut automatic_registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route(
            &mut automatic_registry,
            automatic_source,
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
        let mut build = SourceBackedAutomaticRegistryBuild {
            registry: automatic_registry,
            issues: Vec::new(),
            discovery_duration: Duration::ZERO,
        };
        register_explicit_source_catalog_routes(
            temp.path(),
            &temp.path().join("index"),
            &mut build,
        )
        .unwrap();
        assert_eq!(build.registry.executable_route_count(), 2);

        let native = tempdir().unwrap();
        let prompt = native.path().join("history.jsonl");
        fs::write(&prompt, r#"{"session_id":"one","ts":1,"text":"prompt"}"#).unwrap();
        let source = provider_source_for_path(CaptureProvider::Codex, prompt);
        upsert_explicit_source(native.path(), &source).unwrap();
        let mut duplicate_registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route(
            &mut duplicate_registry,
            source,
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
        let mut duplicate = SourceBackedAutomaticRegistryBuild {
            registry: duplicate_registry,
            issues: Vec::new(),
            discovery_duration: Duration::ZERO,
        };
        let error = register_explicit_source_catalog_routes(
            native.path(),
            &native.path().join("index"),
            &mut duplicate,
        )
        .unwrap_err();
        assert!(error.to_string().contains("conflicts with automatic"));
        assert_eq!(duplicate.registry.executable_route_count(), 1);
    }
}
