use std::ops::{Deref, DerefMut};

use ctx_history_capture_model::SourceRouteIdentity;

use super::SourceBackedRefreshScope;

/// Whether a route was selected by provider discovery or supplied manually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteSelection {
    Automatic,
    ExplicitManual,
}

/// Provider-specific authority that must survive central registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedSelectorAuthority {
    DiscoveredWinner,
    ExplicitPath,
    CatalogLineage,
    ExactCwd,
    NamedSurface,
    SelectedWithRetainedExplicit,
}

/// Filesystem authority exposed by one landed route to provider-neutral
/// daemon watchers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedWatchTargetKind {
    Path,
    SqliteDatabase,
}

/// Capture-side authority needed to construct one landed route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteConstructor {
    ProviderSource,
    CatalogLineage,
    FiniteInventory,
    DiscoveryContext,
    ExactCwd,
    NamedSurface,
    SelectedWithRetainedRoutes,
}

/// Runtime facts for one selected route. The source itself remains a generic
/// capture-side value so the runtime does not acquire provider dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRouteMetadata<S> {
    pub source: S,
    pub certified_source_format: &'static str,
    pub selection: Option<SourceBackedRouteSelection>,
    pub selector_authority: SourceBackedSelectorAuthority,
    pub unsupported_reason: Option<String>,
    pub route_identity: Option<SourceRouteIdentity>,
    pub watch_target_kind: SourceBackedWatchTargetKind,
}

/// Declarative inventory metadata for one landed route. Provider values and
/// capture-side construction policy remain generic parameters, keeping route
/// composition above the runtime boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBackedProviderRouteMetadata<P, C> {
    pub provider: P,
    pub source_format: &'static str,
    pub certified_source_format: &'static str,
    pub automatic: bool,
    pub explicit_manual: bool,
    pub selector_authority: SourceBackedSelectorAuthority,
    pub unsupported_reason: Option<&'static str>,
    pub constructor: C,
    pub watch_target_kind: SourceBackedWatchTargetKind,
}

/// Static route facts required by the neutral registry collection.
pub trait SourceBackedRegistryRoute: Sized {
    type Metadata;

    fn metadata(&self) -> &Self::Metadata;

    fn route_identity(&self) -> Option<&SourceRouteIdentity>;

    fn is_executable(&self) -> bool;

    fn has_certified_missing_paths(&self) -> bool;

    fn uses_parallel_leaf_workers(&self) -> bool;

    /// Coalesces two non-executable observations for the same route identity.
    fn absorb_certified_missing_route(&mut self, route: Self);
}

/// Ordered route inventory shared by refresh, publication, and watch façades.
///
/// Registration preserves the existing executable route, upgrades a missing
/// observation to an executable route, and coalesces repeated missing paths.
#[derive(Debug, Clone)]
pub struct SourceBackedRouteRegistry<R> {
    routes: Vec<R>,
}

impl<R> Default for SourceBackedRouteRegistry<R> {
    fn default() -> Self {
        Self { routes: Vec::new() }
    }
}

impl<R: SourceBackedRegistryRoute> SourceBackedRouteRegistry<R> {
    pub fn register(&mut self, route: R) {
        if let Some(identity) = route.route_identity() {
            if let Some(existing) = self
                .routes
                .iter_mut()
                .find(|existing| existing.route_identity() == Some(identity))
            {
                if existing.is_executable() {
                    return;
                }
                if route.is_executable() {
                    *existing = route;
                    return;
                }
                existing.absorb_certified_missing_route(route);
                return;
            }
        }
        self.routes.push(route);
    }

    pub fn routes(&self) -> impl ExactSizeIterator<Item = &R::Metadata> {
        self.routes.iter().map(SourceBackedRegistryRoute::metadata)
    }

    pub fn executable_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.is_executable())
            .count()
    }

    /// Returns whether any executable route selected by this exact refresh can
    /// consume the source-scanner half of the coordinated CPU budget.
    pub fn selected_routes_use_parallel_leaf_workers(
        &self,
        scope: &SourceBackedRefreshScope,
    ) -> bool {
        self.routes.iter().any(|route| {
            route.is_executable()
                && route.uses_parallel_leaf_workers()
                && match scope {
                    SourceBackedRefreshScope::All => true,
                    SourceBackedRefreshScope::Exact(selected) => route
                        .route_identity()
                        .is_some_and(|identity| selected.contains(identity)),
                }
        })
    }

    pub fn unsupported_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| !route.is_executable())
            .filter(|route| !route.has_certified_missing_paths())
            .count()
    }

    pub fn pop(&mut self) -> Option<R> {
        self.routes.pop()
    }

    /// Retains registered routes without exposing the registry's backing
    /// storage. Composition uses this only for one-way compatibility bridges.
    pub fn retain(&mut self, keep: impl FnMut(&R) -> bool) {
        self.routes.retain(keep);
    }
}

impl<R> Deref for SourceBackedRouteRegistry<R> {
    type Target = [R];

    fn deref(&self) -> &Self::Target {
        &self.routes
    }
}

impl<R> DerefMut for SourceBackedRouteRegistry<R> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.routes
    }
}

impl<'a, R> IntoIterator for &'a SourceBackedRouteRegistry<R> {
    type Item = &'a R;
    type IntoIter = std::slice::Iter<'a, R>;

    fn into_iter(self) -> Self::IntoIter {
        self.routes.iter()
    }
}

impl<'a, R> IntoIterator for &'a mut SourceBackedRouteRegistry<R> {
    type Item = &'a mut R;
    type IntoIter = std::slice::IterMut<'a, R>;

    fn into_iter(self) -> Self::IntoIter {
        self.routes.iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct Route {
        identity: SourceRouteIdentity,
        metadata: usize,
        executable: bool,
        missing: Vec<u8>,
        parallel: bool,
    }

    impl SourceBackedRegistryRoute for Route {
        type Metadata = usize;

        fn metadata(&self) -> &Self::Metadata {
            &self.metadata
        }

        fn route_identity(&self) -> Option<&SourceRouteIdentity> {
            Some(&self.identity)
        }

        fn is_executable(&self) -> bool {
            self.executable
        }

        fn has_certified_missing_paths(&self) -> bool {
            !self.missing.is_empty()
        }

        fn uses_parallel_leaf_workers(&self) -> bool {
            self.parallel
        }

        fn absorb_certified_missing_route(&mut self, mut route: Self) {
            self.missing.append(&mut route.missing);
            self.missing.sort_unstable();
            self.missing.dedup();
        }
    }

    fn identity(value: u8) -> SourceRouteIdentity {
        SourceRouteIdentity::from_sha256(format!("{value:02x}").repeat(32)).unwrap()
    }

    fn route(value: u8, executable: bool, missing: Vec<u8>, parallel: bool) -> Route {
        Route {
            identity: identity(value),
            metadata: usize::from(value),
            executable,
            missing,
            parallel,
        }
    }

    #[test]
    fn registry_upgrades_missing_routes_and_coalesces_missing_evidence() {
        let mut registry = SourceBackedRouteRegistry::default();
        registry.register(route(1, false, vec![3, 1], false));
        registry.register(route(1, false, vec![2, 3], false));
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].missing, vec![1, 2, 3]);

        registry.register(route(1, true, Vec::new(), true));
        registry.register(route(1, false, vec![4], false));
        assert_eq!(registry.len(), 1);
        assert!(registry[0].executable);
        assert!(registry[0].missing.is_empty());
    }

    #[test]
    fn registry_scope_and_counts_use_exact_route_identity() {
        let mut registry = SourceBackedRouteRegistry::default();
        registry.register(route(1, true, Vec::new(), false));
        registry.register(route(2, true, Vec::new(), true));
        registry.register(route(3, false, Vec::new(), false));
        registry.register(route(4, false, vec![1], false));

        assert_eq!(registry.executable_route_count(), 2);
        assert_eq!(registry.unsupported_route_count(), 1);
        assert!(registry.selected_routes_use_parallel_leaf_workers(
            &SourceBackedRefreshScope::exact([identity(2)])
        ));
        assert!(!registry.selected_routes_use_parallel_leaf_workers(
            &SourceBackedRefreshScope::exact([identity(1)])
        ));
    }
}
