mod storage;

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
    register_hermes_explicit_source_backed_route,
    register_landed_source_backed_route_with_data_root, register_lingma_source_backed_route,
    register_nanoclaw_source_backed_route, register_warp_source_backed_route,
    source_backed_route_constructor, source_backed_route_inventory,
    validate_provider_source_roots_outside_data_root, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    SourceBackedAutomaticRegistryBuild, SourceBackedHydrationSupport, SourceBackedProviderRegistry,
    SourceBackedRoute, SourceBackedRouteConstructor, SourceBackedRouteDriver,
    SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteSelection,
    SourceBackedSelectorAuthority,
};
use ctx_history_core::{
    platform_security::establish_private_data_root, CaptureProvider, CertifiedSourceDeletion,
    CertifiedSourceInventory, HydrationFailure, HydrationFailureKind, SourceAnchor,
    SourceInventoryObservation, SourceKey, TypedKey,
};
use ctx_history_index::VerifiedIndex;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{provider_args::ImportFormatArg, ImportArgs};

use storage::{
    authority_for, catalog_root, decode_digest, encode_hex, load_catalog, load_catalog_unlocked,
    open_catalog_lock, random_catalog_lineage, sort_and_validate_entries, validate_approved_path,
    write_catalog_snapshot,
};

#[cfg(test)]
use storage::catalog_revision_filename;

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
    validate_provider_source_roots_outside_data_root(data_root, std::iter::once(source))?;
    establish_private_data_root(data_root).context("protect ctx data root for source catalog")?;
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

pub(crate) fn validate_explicit_source_catalog_roots(data_root: &Path) -> Result<()> {
    let snapshot = load_catalog(data_root)?;
    for entry in snapshot.entries.iter().filter(|entry| entry.enabled) {
        let source = source_from_catalog_entry(entry, true)?;
        validate_provider_source_roots_outside_data_root(data_root, std::iter::once(&source))?;
    }
    Ok(())
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
            validate_provider_source_roots_outside_data_root(data_root, std::iter::once(&source))?;
            register_enabled_catalog_route(
                data_root,
                &mut build.registry,
                source,
                entry.lineage()?,
            )
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
    data_root: &Path,
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
                register_nanoclaw_source_backed_route(registry, source, data_root, lineage)?
            }
            provider => bail!(
                "{} has an unknown catalog-lineage registration",
                provider.as_str()
            ),
        },
        SourceBackedRouteConstructor::ProviderSource => match source.provider {
            CaptureProvider::ForgeCode => {
                register_forgecode_explicit_source_backed_route(
                    registry, source, data_root, lineage,
                )?
            }
            CaptureProvider::Hermes => register_hermes_explicit_source_backed_route(
                registry,
                source,
                data_root,
                SourceAnchor::CatalogLineage(lineage),
            )?,
            _ => register_landed_source_backed_route_with_data_root(
                registry,
                source,
                SourceBackedRouteSelection::ExplicitManual,
                data_root,
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
                    data_root,
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
                data_root,
                format!("ctx-catalog:{}", encode_hex(&lineage)),
            )?;
        }
        SourceBackedRouteConstructor::SelectedWithRetainedRoutes => {
            let platform_root = goose_platform_root(&source.path)?;
            register_goose_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::ExplicitManual,
                data_root,
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

include!("explicit_source_catalog/tests.rs");
