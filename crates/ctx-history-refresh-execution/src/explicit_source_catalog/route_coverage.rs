use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceRouteCoverageKey {
    pub(super) provider: CaptureProvider,
    pub(super) certified_source_format: String,
    pub(super) path: PathBuf,
}

impl SourceRouteCoverageKey {
    pub(super) fn from_entry(entry: &CatalogEntry) -> Result<Self> {
        Ok(Self {
            provider: entry.provider()?,
            certified_source_format: entry.certified_source_format()?.to_owned(),
            path: canonicalize_route_coverage_path(&entry.path)?,
        })
    }

    pub(super) fn from_source(source: &ProviderSource) -> Result<Self> {
        Ok(Self {
            provider: source.provider,
            certified_source_format: route_metadata(source.provider, source.source_format)?
                .certified_source_format
                .to_owned(),
            path: canonicalize_route_coverage_path(&source.path)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AutomaticRouteCoverageBinding {
    pub(super) route_identity: ctx_history_index::SourceRouteIdentity,
    pub(super) workset: SourceBackedRefreshWorkset,
    pub(super) root_specificity: usize,
}

pub(super) fn automatic_route_coverage_binding(
    registry: &SourceBackedProviderRegistry,
    entry: &CatalogEntry,
) -> Result<Option<AutomaticRouteCoverageBinding>> {
    if !entry.enabled || entry.route_identity.is_some() || entry.relocate_from.is_some() {
        return Ok(None);
    }
    let requested = SourceRouteCoverageKey::from_entry(entry)?;
    let requested_kind = route_coverage_path_kind(&requested.path)?;
    let mut matched = None;
    for route in registry.routes().filter(|route| {
        route.source.provider == requested.provider
            && route.certified_source_format == requested.certified_source_format
    }) {
        let Some(route_identity) = route.route_identity.as_ref() else {
            continue;
        };
        let Some(registration_sources) =
            registry.catalog_coverage_route_registration_sources(route_identity)
        else {
            continue;
        };
        let Some(candidate) = route_coverage_binding(
            &requested,
            requested_kind,
            route_identity,
            registration_sources,
        )?
        else {
            continue;
        };
        select_route_coverage_binding(&requested.path, &mut matched, candidate)?;
    }
    Ok(matched)
}

pub(super) fn installed_automatic_route_coverage(
    catalog: &SourceBackedWatchCatalog,
    entry: &CatalogEntry,
) -> Result<Option<AutomaticRouteCoverageBinding>> {
    if !entry.enabled || entry.route_identity.is_some() || entry.relocate_from.is_some() {
        return Ok(None);
    }
    let requested = SourceRouteCoverageKey::from_entry(entry)?;
    let requested_kind = route_coverage_path_kind(&requested.path)?;
    let mut matched = None;
    for route_identity in catalog.route_ids() {
        let Some(registration_sources) =
            catalog.catalog_coverage_route_registration_sources(route_identity)
        else {
            continue;
        };
        let Some(candidate) = route_coverage_binding(
            &requested,
            requested_kind,
            route_identity,
            registration_sources,
        )?
        else {
            continue;
        };
        select_route_coverage_binding(&requested.path, &mut matched, candidate)?;
    }
    Ok(matched)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RouteCoveragePathKind {
    File,
    Directory,
    Missing,
}

pub(super) fn route_coverage_binding<'a>(
    requested: &SourceRouteCoverageKey,
    requested_kind: RouteCoveragePathKind,
    route_identity: &ctx_history_index::SourceRouteIdentity,
    registration_sources: impl IntoIterator<Item = &'a ProviderSource>,
) -> Result<Option<AutomaticRouteCoverageBinding>> {
    let mut best = None;
    for source in registration_sources {
        let registered = SourceRouteCoverageKey::from_source(source)?;
        if registered.provider != requested.provider
            || registered.certified_source_format != requested.certified_source_format
        {
            continue;
        }
        let workset = if registered.path == requested.path {
            Some(SourceBackedRefreshWorkset::Exhaustive)
        } else if requested.path.starts_with(&registered.path)
            && route_coverage_path_kind(&registered.path)? == RouteCoveragePathKind::Directory
        {
            Some(match requested_kind {
                RouteCoveragePathKind::File => {
                    SourceBackedRefreshWorkset::members([requested.path.clone()])
                }
                RouteCoveragePathKind::Directory | RouteCoveragePathKind::Missing => {
                    SourceBackedRefreshWorkset::Exhaustive
                }
            })
        } else {
            None
        };
        let Some(workset) = workset else {
            continue;
        };
        let candidate = AutomaticRouteCoverageBinding {
            route_identity: route_identity.clone(),
            workset,
            root_specificity: registered.path.components().count(),
        };
        if best
            .as_ref()
            .is_none_or(|current: &AutomaticRouteCoverageBinding| {
                candidate.root_specificity > current.root_specificity
            })
        {
            best = Some(candidate);
        }
    }
    Ok(best)
}

pub(super) fn select_route_coverage_binding(
    requested_path: &Path,
    selected: &mut Option<AutomaticRouteCoverageBinding>,
    candidate: AutomaticRouteCoverageBinding,
) -> Result<()> {
    let Some(current) = selected else {
        *selected = Some(candidate);
        return Ok(());
    };
    match candidate.root_specificity.cmp(&current.root_specificity) {
        std::cmp::Ordering::Greater => *current = candidate,
        std::cmp::Ordering::Less => {}
        std::cmp::Ordering::Equal if candidate.route_identity == current.route_identity => {
            current.workset.merge(candidate.workset);
        }
        std::cmp::Ordering::Equal => {
            bail!(
                "explicit source {} has ambiguous equal-specificity automatic route coverage",
                requested_path.display()
            )
        }
    }
    Ok(())
}

fn route_coverage_path_kind(path: &Path) -> Result<RouteCoveragePathKind> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(RouteCoveragePathKind::File),
        Ok(metadata) if metadata.is_dir() => Ok(RouteCoveragePathKind::Directory),
        Ok(_) => Ok(RouteCoveragePathKind::Missing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(RouteCoveragePathKind::Missing)
        }
        Err(error) => {
            Err(error).with_context(|| format!("inspect route coverage path {}", path.display()))
        }
    }
}

fn canonicalize_route_coverage_path(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut suffix = Vec::new();
            let mut ancestor = path;
            loop {
                match fs::canonicalize(ancestor) {
                    Ok(mut canonical) => {
                        for component in suffix.iter().rev() {
                            canonical.push(component);
                        }
                        return Ok(canonical);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let name = ancestor.file_name().ok_or_else(|| {
                            anyhow!(
                                "route coverage path {} has no existing canonical ancestor",
                                path.display()
                            )
                        })?;
                        suffix.push(name.to_os_string());
                        ancestor = ancestor.parent().ok_or_else(|| {
                            anyhow!(
                                "route coverage path {} has no existing canonical ancestor",
                                path.display()
                            )
                        })?;
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("canonicalize route coverage path {}", path.display())
                        })
                    }
                }
            }
        }
        Err(error) => Err(error)
            .with_context(|| format!("canonicalize route coverage path {}", path.display())),
    }
}
