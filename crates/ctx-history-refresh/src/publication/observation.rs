use super::*;

/// Samples exact provider-neutral targets before parsing begins.
///
/// Terminal JSONL revalidation can accept same-file growth after the scanned
/// prefix. A later sample could therefore certify bytes absent from the Core
/// generation. A pre-scan token is either bound to captured state or causes a
/// conservative warm refresh when the target changes during the scan.
pub(crate) fn admitted_route_observations(
    registry: &SourceBackedProviderRegistry,
    scope: &SourceBackedRefreshScope,
) -> BTreeMap<SourceRouteIdentity, String> {
    let SourceBackedRefreshScope::Exact(routes) = scope else {
        return BTreeMap::new();
    };
    let catalog = registry.watch_catalog();
    routes
        .iter()
        .filter_map(|route| {
            catalog
                .certify_route_observation(route)
                .map(|observation| (route.clone(), observation))
        })
        .collect()
}

#[cfg(test)]
thread_local! {
    static AFTER_CAPTURE_SCAN_BEFORE_METADATA_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn install_after_capture_scan_before_metadata_hook_for_test(
    hook: impl FnOnce() + 'static,
) {
    AFTER_CAPTURE_SCAN_BEFORE_METADATA_HOOK.with(|slot| {
        let previous = slot.replace(Some(Box::new(hook)));
        assert!(
            previous.is_none(),
            "capture metadata test hooks must not nest"
        );
    });
}

#[cfg(test)]
pub(crate) fn run_after_capture_scan_before_metadata_hook() {
    AFTER_CAPTURE_SCAN_BEFORE_METADATA_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
pub(crate) fn run_after_capture_scan_before_metadata_hook() {}
