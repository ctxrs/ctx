use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceInventory, ContentSourceResolver,
};
use ctx_history_index::LexicalDocument;

use super::{
    hydration::hydration_failure_from_error, scan_kiro_source_backed, terminal_fence_matches,
    KiroLocatorResolverV0, KiroSourceBackedErrorV0, KiroSourceBackedResultV0, KiroSourceBackedScan,
    KiroSourceTerminalFence, SOURCE_BACKED_PAGE_ROWS,
};
use crate::{
    provider::source_backed::{
        certify_captured_route_inventory, executable_route, provider_format_scope,
        route_coordinator_error, route_error, SourceBackedCoordinatorResult,
        SourceBackedGenerationSink, SourceBackedProviderRegistry, SourceBackedRevalidationTarget,
        SourceBackedRouteDriver, SourceBackedRouteError, SourceBackedRouteErrorKind,
        SourceBackedRouteResult, SourceBackedRouteSelection, SourceBackedSelectorAuthority,
    },
    CaptureError, ProviderSource, KIRO_SQLITE_SOURCE_FORMAT,
};

#[derive(Debug)]
struct TerminalEvidence {
    certificate: CertifiedSource,
    inventory: CertifiedSourceInventory,
    fence: KiroSourceTerminalFence,
}

#[derive(Debug, Default)]
struct TerminalState {
    evidence: Option<TerminalEvidence>,
    fence_result: Option<bool>,
}

pub(crate) fn register(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = source_backed_route_driver(&source);
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn source_backed_route_driver(route: &ProviderSource) -> SourceBackedRouteDriver {
    let path = route.path.clone();
    let scan_path = path.clone();
    let revalidation_path = path.clone();
    let inventory_revalidation_path = path.clone();
    let hydration_path = path.clone();
    let batch_hydration_path = path;
    let inventory_route = route.clone();
    let terminal = Arc::new(Mutex::new(TerminalState::default()));
    let scan_terminal = Arc::clone(&terminal);
    let revalidation_terminal = Arc::clone(&terminal);
    let inventory_terminal = terminal;

    SourceBackedRouteDriver::new(
        move |sink| {
            reset_terminal(&scan_terminal)?;
            scan_replacement(&scan_path, &inventory_route, sink, &scan_terminal)
        },
        provider_format_scope(CaptureProvider::KiroCli, KIRO_SQLITE_SOURCE_FORMAT),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                revalidate_source(&revalidation_path, expected, &revalidation_terminal)
            }
            SourceBackedRevalidationTarget::Deletion(expected) => {
                revalidate_deletion(&revalidation_path, expected, &revalidation_terminal)
            }
        },
        move |request| {
            KiroLocatorResolverV0::discover(hydration_path.clone(), KIRO_SQLITE_SOURCE_FORMAT)
                .map_err(hydration_failure_from_error)?
                .hydrate_event(request)
        },
    )
    .with_complete_inventory_revalidation(move |expected| {
        revalidate_inventory(&inventory_revalidation_path, expected, &inventory_terminal)
    })
    .with_batch_hydration(move |request| {
        KiroLocatorResolverV0::discover(batch_hydration_path.clone(), KIRO_SQLITE_SOURCE_FORMAT)
            .map_err(hydration_failure_from_error)?
            .hydrate_batch(request)
    })
}

fn scan_replacement(
    path: &Path,
    route: &ProviderSource,
    sink: &mut SourceBackedGenerationSink<'_>,
    terminal: &Mutex<TerminalState>,
) -> SourceBackedRouteResult<()> {
    let mut began = false;
    let mut sink_failure = None;
    let scan = scan_kiro_source_backed(path, KIRO_SQLITE_SOURCE_FORMAT, &mut |page| {
        stream_page(sink, page, &mut began, &mut sink_failure)
    });
    if let Some(error) = sink_failure {
        return Err(error);
    }
    let scan = scan.map_err(route_error)?;
    validate_scan_receipt(&scan)?;
    if !began {
        sink.begin_source(scan.source.clone())
            .map_err(route_coordinator_error)?;
    }
    sink.certify_source(scan.certificate.clone())
        .map_err(route_coordinator_error)?;
    let inventory =
        certify_captured_route_inventory(route, std::slice::from_ref(&scan.certificate))?;
    sink.certify_complete_inventory(inventory.clone())
        .map_err(route_coordinator_error)?;
    let mut terminal = terminal.lock().map_err(|_| terminal_lock_error())?;
    terminal.evidence = Some(TerminalEvidence {
        certificate: scan.certificate,
        inventory,
        fence: scan.terminal_fence,
    });
    terminal.fence_result = None;
    Ok(())
}

fn validate_scan_receipt(scan: &KiroSourceBackedScan) -> SourceBackedRouteResult<()> {
    let indexed = scan.certificate.counts().indexed_documents;
    let page_rows = SOURCE_BACKED_PAGE_ROWS as u64;
    let expected_pages = indexed / page_rows + u64::from(!indexed.is_multiple_of(page_rows));
    let expected_peak = indexed.min(page_rows);
    let complete = scan.certificate.counts().complete_records;
    if scan.row_decode_passes != 1
        || scan.decoded_rows > complete
        || (scan.decoded_rows == 0) != (complete == 0)
        || scan.emitted_pages != expected_pages
        || scan.peak_buffered_rows != expected_peak
    {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "Kiro scan receipt violated the one-pass bounded-stream contract",
        ));
    }
    Ok(())
}

fn stream_page(
    sink: &mut SourceBackedGenerationSink<'_>,
    page: Vec<LexicalDocument>,
    began: &mut bool,
    sink_failure: &mut Option<SourceBackedRouteError>,
) -> KiroSourceBackedResultV0<()> {
    for document in page {
        if !*began {
            if let Err(error) = sink.begin_source(document.source.clone()) {
                return bridge_sink_error(error, sink_failure);
            }
            *began = true;
        }
        if let Err(error) = sink.add_document(document) {
            return bridge_sink_error(error, sink_failure);
        }
    }
    Ok(())
}

fn bridge_sink_error(
    error: crate::provider::source_backed::SourceBackedCoordinatorError,
    sink_failure: &mut Option<SourceBackedRouteError>,
) -> KiroSourceBackedResultV0<()> {
    let route_error = route_coordinator_error(error);
    let detail = route_error.to_string();
    *sink_failure = Some(route_error);
    Err(KiroSourceBackedErrorV0::Capture(
        CaptureError::InvalidPayload(detail),
    ))
}

fn reset_terminal(terminal: &Mutex<TerminalState>) -> SourceBackedRouteResult<()> {
    *terminal.lock().map_err(|_| terminal_lock_error())? = TerminalState::default();
    Ok(())
}

fn revalidate_source(
    path: &Path,
    expected: &CertifiedSource,
    terminal: &Mutex<TerminalState>,
) -> bool {
    revalidate_terminal(path, terminal, |evidence| evidence.certificate == *expected)
}

fn revalidate_deletion(
    path: &Path,
    expected: &ctx_history_core::CertifiedSourceDeletion,
    terminal: &Mutex<TerminalState>,
) -> bool {
    revalidate_terminal(path, terminal, |evidence| {
        expected.verifies(&evidence.inventory)
    })
}

fn revalidate_inventory(
    path: &Path,
    expected: &CertifiedSourceInventory,
    terminal: &Mutex<TerminalState>,
) -> bool {
    revalidate_terminal(path, terminal, |evidence| evidence.inventory == *expected)
}

fn revalidate_terminal(
    path: &Path,
    terminal: &Mutex<TerminalState>,
    expected_matches: impl FnOnce(&TerminalEvidence) -> bool,
) -> bool {
    let Ok(mut terminal) = terminal.lock() else {
        return false;
    };
    let Some(evidence) = terminal.evidence.as_ref() else {
        return false;
    };
    if !expected_matches(evidence) {
        return false;
    }
    let fence = evidence.fence.clone();
    cache_terminal_fence_result(&mut terminal.fence_result, || {
        terminal_fence_matches(path, &fence).unwrap_or(false)
    })
}

fn cache_terminal_fence_result(cached: &mut Option<bool>, evaluate: impl FnOnce() -> bool) -> bool {
    if let Some(result) = *cached {
        return result;
    }
    let result = evaluate();
    *cached = Some(result);
    result
}

fn terminal_lock_error() -> SourceBackedRouteError {
    SourceBackedRouteError::new(
        SourceBackedRouteErrorKind::Internal,
        "Kiro terminal evidence lock was poisoned",
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::cache_terminal_fence_result;

    #[test]
    fn terminal_fence_result_is_cached_for_all_revalidation_consumers() {
        let evaluations = Cell::new(0);
        let mut cached = None;
        assert!(cache_terminal_fence_result(&mut cached, || {
            evaluations.set(evaluations.get() + 1);
            true
        }));
        assert!(cache_terminal_fence_result(&mut cached, || {
            evaluations.set(evaluations.get() + 1);
            false
        }));
        assert_eq!(evaluations.get(), 1);
    }

    #[test]
    fn failed_terminal_fence_is_cached_fail_closed() {
        let evaluations = Cell::new(0);
        let mut cached = None;
        assert!(!cache_terminal_fence_result(&mut cached, || {
            evaluations.set(evaluations.get() + 1);
            false
        }));
        assert!(!cache_terminal_fence_result(&mut cached, || {
            evaluations.set(evaluations.get() + 1);
            true
        }));
        assert_eq!(evaluations.get(), 1);
    }
}
