mod codex_union;
mod generation_witness;
mod source_helpers;
mod storage;

use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::{
    automatic_source_backed_route_identity, explicit_source_catalog_lineage,
    provider_source_for_path, register_custom_history_source_backed_route,
    register_forgecode_explicit_source_backed_route, register_goose_source_backed_route,
    register_hermes_explicit_source_backed_route,
    register_landed_source_backed_route_with_data_root, register_lingma_source_backed_route,
    register_nanoclaw_source_backed_route_with_base_sources, register_warp_source_backed_route,
    source_backed_route_constructor, source_backed_route_inventory,
    validate_provider_source_roots_outside_data_root, DiscoveryReport, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    SourceBackedAutomaticRegistryBuild, SourceBackedProviderRegistry, SourceBackedRoute,
    SourceBackedRouteConstructor, SourceBackedRouteDriver, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteSelection, SourceBackedSelectorAuthority,
};
use ctx_history_core::{CaptureProvider, CertifiedSource, SourceAnchor, TypedKey};
use ctx_history_index::VerifiedIndex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use source_helpers::{custom_provider_source, goose_platform_root};
use storage::{
    authority_for, decode_digest, encode_hex, sort_and_validate_entries, validate_approved_path,
};
const CATALOG_SCHEMA_VERSION: u32 = 1;
const CATALOG_INTEGRITY_ALGORITHM: &str = "sha256";
const CATALOG_MAX_ENTRIES: usize = 256;
const CATALOG_MAX_PATH_BYTES: usize = 16 * 1024;
const CATALOG_REQUEST_WIRE_MAX_BYTES: usize = 20 * 1024;
const CUSTOM_SOURCE_FORMAT: &str = "ctx_history_jsonl_v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitSourceCatalogAuthority {
    schema_version: u32,
    revision: u64,
    integrity_sha256: [u8; 32],
    entries: Vec<CatalogEntry>,
}

impl ExplicitSourceCatalogAuthority {
    pub fn integrity_hex(&self) -> String {
        encode_hex(&self.integrity_sha256)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "schema_version": self.schema_version,
            "revision": self.revision,
            "integrity": {
                "algorithm": CATALOG_INTEGRITY_ALGORITHM,
                "digest": self.integrity_hex(),
            },
            "entries": self.entries,
        })
    }

    pub fn validate_source_roots(&self, data_root: &Path) -> Result<()> {
        let snapshot = self.snapshot();
        validate_explicit_source_catalog_snapshot_roots(data_root, &snapshot)
    }

    pub fn route_lineages(&self) -> BTreeSet<String> {
        self.entries
            .iter()
            .map(|entry| entry.catalog_lineage.clone())
            .collect()
    }

    pub fn relocation_authority(
        &self,
        old_path: &Path,
        bindings: &[ExplicitSourceCatalogRouteBinding],
    ) -> Result<Option<ExplicitSourceRelocationAuthority>> {
        let Some(entry) = self.entries.iter().find(|entry| entry.path == old_path) else {
            return Ok(None);
        };
        let binding = bindings
            .iter()
            .find(|binding| binding.catalog_lineage == entry.catalog_lineage)
            .ok_or_else(|| anyhow!("active explicit source lineage has no exact route binding"))?;
        let metadata = entry.route_metadata()?;
        Ok(Some(ExplicitSourceRelocationAuthority {
            revision: self.revision,
            provider: entry.provider()?,
            source_format: entry.source_format.clone(),
            certified_source_format: metadata.certified_source_format,
            path: entry.path.clone(),
            catalog_lineage: entry.lineage()?,
            route_identity: ctx_history_index::SourceRouteIdentity::from_sha256(
                binding.route_identity.clone(),
            )?,
        }))
    }

    fn snapshot(&self) -> ExplicitSourceCatalogSnapshot {
        ExplicitSourceCatalogSnapshot {
            entries: self.entries.clone(),
        }
    }

    pub fn from_json(value: &Value) -> Result<Self> {
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
        let mut entries = wire.entries;
        sort_and_validate_entries(&mut entries)?;
        let expected = authority_for(wire.revision, &entries)?;
        if expected.integrity_sha256 != decode_digest(&wire.integrity.digest)? {
            bail!("explicit source request overlay integrity does not match its entries");
        }
        expected.validate_request_wire_budget()?;
        Ok(expected)
    }

    fn validate_request_wire_budget(&self) -> Result<()> {
        if serde_json::to_vec(&self.to_json())
            .map_or(true, |wire| wire.len() > CATALOG_REQUEST_WIRE_MAX_BYTES)
        {
            bail!(
                "explicit source request overlay exceeds its {CATALOG_REQUEST_WIRE_MAX_BYTES}-byte wire bound"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ExplicitSourceCatalogUpsert {
    pub authority: ExplicitSourceCatalogAuthority,
    pub provider: CaptureProvider,
    pub source_format: &'static str,
    pub path: PathBuf,
    pub catalog_lineage: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitSourceCatalogRouteBinding {
    pub catalog_lineage: String,
    pub route_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitSourceRelocationAuthority {
    revision: u64,
    provider: CaptureProvider,
    source_format: String,
    certified_source_format: &'static str,
    path: PathBuf,
    catalog_lineage: [u8; 32],
    route_identity: ctx_history_index::SourceRouteIdentity,
}

impl ExplicitSourceCatalogUpsert {
    pub fn catalog_lineage_hex(&self) -> String {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relocate_from: Option<PathBuf>,
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
    entries: Vec<CatalogEntry>,
}

#[derive(Serialize)]
struct CatalogPayload<'a> {
    schema_version: u32,
    revision: u64,
    entries: &'a [CatalogEntry],
}

#[derive(Debug, Clone)]
struct ExplicitSourceCatalogSnapshot {
    entries: Vec<CatalogEntry>,
}

pub fn explicit_source_for_path(
    path: &Path,
    provider: Option<CaptureProvider>,
    custom_history_jsonl: bool,
) -> Result<ProviderSource> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("approve explicit source path {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "symlinked explicit provider source roots are rejected: {}",
            path.display()
        );
    }
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("approve explicit source path {}", path.display()))?;
    validate_approved_path(&canonical)?;

    let source = if custom_history_jsonl {
        custom_provider_source(canonical, true)?
    } else {
        let provider = provider
            .context("ctx import --path requires --provider for native provider history")?;
        provider_source_for_path(provider, canonical)
    };
    validate_enabled_source(&source)?;
    validate_catalog_registration_support(&source)?;
    Ok(source)
}

pub fn upsert_explicit_source(
    data_root: &Path,
    source: &ProviderSource,
) -> Result<ExplicitSourceCatalogUpsert> {
    validate_enabled_source(source)?;
    validate_catalog_registration_support(source)?;
    validate_explicit_source_root(data_root, source)?;
    let metadata = route_metadata(source.provider, source.source_format)?;
    let catalog_lineage = explicit_source_catalog_lineage(
        source.provider,
        metadata.certified_source_format,
        &source.path,
    );
    let mut entries = vec![CatalogEntry {
        provider: source.provider.as_str().to_owned(),
        source_format: source.source_format.to_owned(),
        path: source.path.clone(),
        catalog_lineage: encode_hex(&catalog_lineage),
        route_identity: None,
        relocate_from: None,
        enabled: true,
    }];
    sort_and_validate_entries(&mut entries)?;
    let authority = authority_for(1, &entries)?;
    authority.validate_request_wire_budget()?;

    Ok(ExplicitSourceCatalogUpsert {
        authority,
        provider: source.provider,
        source_format: metadata.source_format,
        path: source.path.clone(),
        catalog_lineage,
    })
}

pub fn relocate_explicit_source(
    data_root: &Path,
    source: &ProviderSource,
    relocation: ExplicitSourceRelocationAuthority,
) -> Result<ExplicitSourceCatalogUpsert> {
    validate_enabled_source(source)?;
    validate_catalog_registration_support(source)?;
    validate_explicit_source_root(data_root, source)?;
    if source.path == relocation.path {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::SourceChanged,
            "explicit relocation requires distinct old and new exact paths",
        )
        .into());
    }
    if source.provider != relocation.provider || source.source_format != relocation.source_format {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unsupported,
            "explicit relocation changed the certified provider or source format",
        )
        .into());
    }
    let metadata = route_metadata(source.provider, source.source_format)?;
    if metadata.certified_source_format != relocation.certified_source_format
        || source.provider != CaptureProvider::Custom
    {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unsupported,
            "explicit relocation requires an adapter with certified catalog-lineage source continuity",
        )
        .into());
    }
    let mut entries = vec![CatalogEntry {
        provider: source.provider.as_str().to_owned(),
        source_format: source.source_format.to_owned(),
        path: source.path.clone(),
        catalog_lineage: encode_hex(&relocation.catalog_lineage),
        route_identity: Some(relocation.route_identity.as_str().to_owned()),
        relocate_from: Some(relocation.path.clone()),
        enabled: true,
    }];
    sort_and_validate_entries(&mut entries)?;
    let authority = authority_for(relocation.revision.saturating_add(1), &entries)?;
    authority.validate_request_wire_budget()?;
    Ok(ExplicitSourceCatalogUpsert {
        authority,
        provider: source.provider,
        source_format: metadata.source_format,
        path: source.path.clone(),
        catalog_lineage: relocation.catalog_lineage,
    })
}

pub fn validate_explicit_relocation_source(old_path: &Path) -> Result<()> {
    validate_approved_path(old_path)?;
    match fs::symlink_metadata(old_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                format!(
                    "relocation refused because the old exact source is still available: {}",
                    old_path.display()
                ),
            )
            .into())
        }
        Err(error) => {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                format!("cannot certify old exact relocation source: {error}"),
            )
            .into())
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn explicit_source_catalog_authority_for_test(revision: u64) -> ExplicitSourceCatalogAuthority {
    authority_for(revision, &[]).expect("empty test request authority")
}

fn validate_explicit_source_catalog_snapshot_roots(
    data_root: &Path,
    snapshot: &ExplicitSourceCatalogSnapshot,
) -> Result<()> {
    for entry in snapshot.entries.iter().filter(|entry| entry.enabled) {
        let source = source_from_catalog_entry(entry, true)?;
        validate_explicit_source_root(data_root, &source)?;
    }
    Ok(())
}

fn remove_automatic_routes_shadowed_by_snapshot(
    report: &mut DiscoveryReport,
    snapshot: &ExplicitSourceCatalogSnapshot,
) {
    report.sources.retain(|source| {
        !snapshot.entries.iter().any(|entry| {
            entry.enabled
                && entry.provider().ok() == Some(source.provider)
                && entry.path == source.path
                && entry.certified_source_format().ok()
                    == route_metadata(source.provider, source.source_format)
                        .ok()
                        .map(|metadata| metadata.certified_source_format)
        })
    });
}

fn register_explicit_source_catalog_snapshot_routes(
    data_root: &Path,
    base_generation: Option<&VerifiedIndex>,
    build: &mut SourceBackedAutomaticRegistryBuild,
    snapshot: &ExplicitSourceCatalogSnapshot,
) -> Result<Vec<ExplicitSourceCatalogRouteBinding>> {
    for entry in &snapshot.entries {
        let provider = entry.provider()?;
        let certified_format = entry.certified_source_format()?;
        if build.registry.routes().any(|route| {
            route.selection == Some(SourceBackedRouteSelection::Automatic)
                && route.source.provider == provider
                && route.source.path == entry.path
                && route.certified_source_format == certified_format
        }) {
            bail!(
                "explicit source catalog authority {}/{} at {} was not removed from automatic discovery before registration",
                provider.as_str(),
                certified_format,
                entry.path.display()
            );
        }
    }

    let mut nanoclaw_lineages = HashSet::new();
    for entry in &snapshot.entries {
        if entry.provider()? == CaptureProvider::NanoClaw {
            nanoclaw_lineages.insert(entry.lineage()?);
        }
    }
    let needs_nanoclaw_checkpoint = !nanoclaw_lineages.is_empty();
    let base_certificates =
        if let Some(index) = base_generation.filter(|_| needs_nanoclaw_checkpoint) {
            let certificates = index
                .manifest()
                .sources
                .iter()
                .filter(|certificate| {
                    let source = certificate.observation().source();
                    source.provider() == CaptureProvider::NanoClaw.as_str()
                        && matches!(
                            source.anchor(),
                            SourceAnchor::CatalogLineage(lineage)
                                if nanoclaw_lineages.contains(lineage)
                        )
                })
                .cloned()
                .collect();
            certificates
        } else {
            Vec::new()
        };
    let automatic_codex_tree_has_current_or_retained_sources =
        build.registry.routes().any(|route| {
            route.selection == Some(SourceBackedRouteSelection::Automatic)
                && route.source.provider == CaptureProvider::Codex
                && route.source.source_format == "codex_session_jsonl_tree"
                && (route.source.exists
                    || base_generation.is_some_and(|index| {
                        route.route_identity.as_ref().is_some_and(|route_identity| {
                            index
                                .manifest()
                                .source_route(route_identity)
                                .is_some_and(|snapshot| !snapshot.sources().is_empty())
                        })
                    }))
        });

    let mut bindings = Vec::new();
    for entry in &snapshot.entries {
        let before = build
            .registry
            .routes()
            .filter_map(|route| route.route_identity.clone())
            .collect::<BTreeSet<_>>();
        let source = source_from_catalog_entry(entry, true)?;
        validate_explicit_source_root(data_root, &source)?;
        let automatic_route_retirement = if source.provider == CaptureProvider::NanoClaw {
            let identity = automatic_source_backed_route_identity(&source)?;
            base_generation
                .is_some_and(|index| index.manifest().source_route(&identity).is_some())
                .then_some(identity)
        } else {
            None
        };
        register_enabled_catalog_route(
            data_root,
            &mut build.registry,
            source,
            entry.lineage()?,
            &base_certificates,
            automatic_codex_tree_has_current_or_retained_sources,
        )
        .with_context(|| {
            format!(
                "register explicit request route {} {}",
                entry.provider,
                entry.path.display()
            )
        })?;
        if let Some(preserved) = entry.route_identity.as_ref() {
            let constructed = build
                .registry
                .routes()
                .filter_map(|route| route.route_identity.clone())
                .filter(|identity| !before.contains(identity))
                .collect::<Vec<_>>();
            let [constructed] = constructed.as_slice() else {
                bail!(
                    "relocated catalog lineage {} constructed {} routes instead of one",
                    entry.catalog_lineage,
                    constructed.len()
                );
            };
            build.registry.preserve_explicit_route_identity(
                constructed,
                ctx_history_index::SourceRouteIdentity::from_sha256(preserved.clone())?,
                entry.relocate_from.as_deref().ok_or_else(|| {
                    anyhow!("preserved relocation route has no exact old-path witness")
                })?,
            )?;
        }
        let added = build
            .registry
            .routes()
            .filter_map(|route| route.route_identity.clone())
            .filter(|identity| !before.contains(identity))
            .collect::<Vec<_>>();
        let [route_identity] = added.as_slice() else {
            bail!(
                "explicit catalog lineage {} registered {} executable routes instead of one",
                entry.catalog_lineage,
                added.len()
            );
        };
        if let Some(retired) = automatic_route_retirement {
            build
                .registry
                .retire_routes_after_success(route_identity, [retired])?;
        }
        bindings.push(ExplicitSourceCatalogRouteBinding {
            catalog_lineage: entry.catalog_lineage.clone(),
            route_identity: route_identity.as_str().to_owned(),
        });
    }
    Ok(bindings)
}

fn register_enabled_catalog_route(
    data_root: &Path,
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    lineage: [u8; 32],
    base_certificates: &[CertifiedSource],
    automatic_codex_tree_has_current_or_retained_sources: bool,
) -> Result<()> {
    if source.provider == CaptureProvider::Codex
        && source.source_format == "codex_session_jsonl_tree"
        && automatic_codex_tree_has_current_or_retained_sources
    {
        // Codex's current tree adapter reads every retained Codex tree source
        // from the writer, so two independently owned tree routes can claim
        // each other's sources. Keep the exact request terminal and typed
        // until the provider exposes route-scoped retained-source authority.
        let detail = "Codex session-tree exact import requires provider route-scoped retained-source ownership when an automatic Codex tree route is active";
        let failure_detail = detail.to_owned();
        let route = SourceBackedRoute::explicit_manual(
            source,
            SourceBackedSelectorAuthority::ExplicitPath,
            SourceBackedRouteDriver::new(
                move |_| {
                    Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Unsupported,
                        failure_detail.clone(),
                    ))
                },
                |_| false,
                |_| false,
            ),
        )?;
        registry.register(route);
        return Ok(());
    }
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
                register_nanoclaw_source_backed_route_with_base_sources(
                    registry,
                    source,
                    data_root,
                    lineage,
                    base_certificates,
                )?
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

fn validate_explicit_source_root(data_root: &Path, source: &ProviderSource) -> Result<()> {
    Ok(validate_provider_source_roots_outside_data_root(
        data_root,
        std::iter::once(source),
    )?)
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

#[cfg(test)]
include!("explicit_source_catalog/tests.rs");
