use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceInventory, ContentSourceResolver,
};
use ctx_history_index::LexicalDocument;

use super::{
    opencode_family_source_backed_registrations, OpenCodeSourceBackedError,
    OpenCodeSourceBackedRegistration, OpenCodeSourceBackedResult, OpenCodeSourceMutationPolicy,
    OpenCodeSourceTerminalFence, SOURCE_BACKED_PAGE_ROWS,
};
use crate::{
    provider::source_backed::{
        certify_captured_route_inventory, executable_route, invalid_route, provider_format_scope,
        route_coordinator_error, route_error, SourceBackedCoordinatorResult,
        SourceBackedGenerationSink, SourceBackedProviderRegistry, SourceBackedRevalidationTarget,
        SourceBackedRouteDriver, SourceBackedRouteError, SourceBackedRouteErrorKind,
        SourceBackedRouteResult, SourceBackedRouteSelection, SourceBackedSelectorAuthority,
    },
    CaptureError, ProviderSource,
};

#[derive(Debug)]
struct TerminalEvidence {
    certificate: CertifiedSource,
    inventory: CertifiedSourceInventory,
    fence: OpenCodeSourceTerminalFence,
}

#[derive(Debug, Default)]
struct TerminalState {
    evidence: Option<TerminalEvidence>,
    fence_result: Option<bool>,
}

pub(crate) fn register_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let registration = registration_for_provider(source.provider).ok_or_else(|| {
        invalid_route(
            source.provider,
            "provider is not part of the OpenCode SQLite family",
        )
    })?;
    let driver = source_backed_route_driver(registration, &source);
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn source_backed_route_driver(
    registration: OpenCodeSourceBackedRegistration,
    route: &ProviderSource,
) -> SourceBackedRouteDriver {
    let OpenCodeSourceMutationPolicy::UnchangedOrReplace = registration.mutation_policy();
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
            scan_replacement(
                registration,
                &scan_path,
                &inventory_route,
                sink,
                &scan_terminal,
            )
        },
        provider_format_scope(registration.provider(), registration.source_format()),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => revalidate_source(
                registration,
                &revalidation_path,
                expected,
                &revalidation_terminal,
            ),
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        move |request| {
            registration
                .exact_resolver(hydration_path.clone())
                .hydrate_event(request)
        },
    )
    .with_complete_inventory_revalidation(move |expected| {
        revalidate_inventory(
            registration,
            &inventory_revalidation_path,
            expected,
            &inventory_terminal,
        )
    })
    .with_batch_hydration(move |request| {
        registration
            .exact_resolver(batch_hydration_path.clone())
            .hydrate_batch(request)
    })
}

fn scan_replacement(
    registration: OpenCodeSourceBackedRegistration,
    path: &Path,
    route: &ProviderSource,
    sink: &mut SourceBackedGenerationSink<'_>,
    terminal: &Mutex<TerminalState>,
) -> SourceBackedRouteResult<()> {
    let mut began = false;
    let mut sink_failure = None;
    let scan = registration
        .scan(path, &mut |page| {
            stream_page(sink, page, &mut began, &mut sink_failure)
        })
        .map_err(route_error)?;
    validate_scan_receipt(&scan)?;
    if let Some(error) = sink_failure {
        return Err(error);
    }
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
    let mut terminal = terminal.lock().map_err(|_| {
        SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "OpenCode-family terminal evidence lock was poisoned",
        )
    })?;
    terminal.evidence = Some(TerminalEvidence {
        certificate: scan.certificate,
        inventory,
        fence: scan.terminal_fence,
    });
    terminal.fence_result = None;
    Ok(())
}

fn validate_scan_receipt(scan: &super::OpenCodeSourceBackedScan) -> SourceBackedRouteResult<()> {
    let counts = scan.certificate.counts();
    let page_rows = SOURCE_BACKED_PAGE_ROWS as u64;
    let expected_pages = counts.indexed_documents / page_rows
        + u64::from(!counts.indexed_documents.is_multiple_of(page_rows));
    let expected_peak = counts.indexed_documents.min(page_rows);
    let expected_schema_variant = format!("opencode-family-{}-v1", scan.schema_family);
    if scan.row_decode_passes != 1
        || scan.decoded_rows != counts.complete_records
        || scan.emitted_pages != expected_pages
        || scan.peak_buffered_rows != expected_peak
        || scan.source.schema_variant() != expected_schema_variant
    {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "OpenCode-family scan receipt violated the one-pass bounded-stream contract",
        ));
    }
    Ok(())
}

fn stream_page(
    sink: &mut SourceBackedGenerationSink<'_>,
    page: Vec<LexicalDocument>,
    began: &mut bool,
    sink_failure: &mut Option<SourceBackedRouteError>,
) -> OpenCodeSourceBackedResult<()> {
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
) -> OpenCodeSourceBackedResult<()> {
    let route_error = route_coordinator_error(error);
    let detail = route_error.to_string();
    *sink_failure = Some(route_error);
    Err(OpenCodeSourceBackedError::Capture(
        CaptureError::InvalidPayload(detail),
    ))
}

fn reset_terminal(terminal: &Mutex<TerminalState>) -> SourceBackedRouteResult<()> {
    *terminal.lock().map_err(|_| {
        SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "OpenCode-family terminal evidence lock was poisoned",
        )
    })? = TerminalState::default();
    Ok(())
}

fn revalidate_source(
    registration: OpenCodeSourceBackedRegistration,
    path: &Path,
    expected: &CertifiedSource,
    terminal: &Mutex<TerminalState>,
) -> bool {
    revalidate_terminal(registration, path, terminal, |terminal| {
        terminal.certificate == *expected
    })
}

fn revalidate_inventory(
    registration: OpenCodeSourceBackedRegistration,
    path: &Path,
    expected: &CertifiedSourceInventory,
    terminal: &Mutex<TerminalState>,
) -> bool {
    revalidate_terminal(registration, path, terminal, |terminal| {
        terminal.inventory == *expected
    })
}

fn revalidate_terminal(
    registration: OpenCodeSourceBackedRegistration,
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
        registration.terminal_fence(path, &fence).unwrap_or(false)
    })
}

pub(super) fn cache_terminal_fence_result(
    cached: &mut Option<bool>,
    evaluate: impl FnOnce() -> bool,
) -> bool {
    if let Some(result) = *cached {
        return result;
    }
    let result = evaluate();
    *cached = Some(result);
    result
}

fn registration_for_provider(
    provider: CaptureProvider,
) -> Option<OpenCodeSourceBackedRegistration> {
    opencode_family_source_backed_registrations()
        .into_iter()
        .find(|registration| registration.provider() == provider)
}
