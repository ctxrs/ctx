use super::super::*;
use super::lifecycle::{
    captured_omitted_sources, captured_source_plans, stage_captured_exact_replay,
    CapturedSourcePlan, WriterCaptureSink,
};
use super::observation::{
    capture_route_evidence, captured_route_changed, captured_route_contract,
    captured_route_internal, finish_captured_route_evidence, validate_captured_route_ownership,
    CapturedRouteEvidence, CapturedRouteInventoryAuthority, ProviderCaptureCallback,
    ProviderCaptureSink,
};
use super::receipts::SourcePredicate;
use std::sync::Mutex;

/// Adapts a provider's complete route capture into generation staging.
///
/// The callback must enumerate every currently owned source or return
/// `Unavailable`; an empty successful capture is authoritative. Captures that
/// do not expose a provider-native inventory receive a route-scoped inventory
/// derived from their complete certified source set.
///
/// The first pass plans exact replay without constructing an `IndexWriter`.
/// Adapters can expose provider-native append-prefix receipts through the
/// capture sink. Changed sources without such evidence are deliberately
/// replacement-only, while unchanged siblings still use exact replay.
pub(crate) fn captured_route_driver(
    route: &ProviderSource,
    capture: impl Fn(&mut dyn ProviderCaptureSink) -> SourceBackedRouteResult<()>
        + Send
        + Sync
        + 'static,
    owns_source: impl Fn(&SourceKey) -> bool + Send + Sync + 'static,
    hydrate: impl Fn(&EventHydrationRequest) -> Result<HydratedProviderRecord, HydrationFailure>
        + Send
        + Sync
        + 'static,
) -> SourceBackedRouteDriver {
    let authority = CapturedRouteInventoryAuthority::new(route);
    let capture: Arc<ProviderCaptureCallback> = Arc::new(capture);
    let scan_capture = Arc::clone(&capture);
    let terminal_capture = Arc::clone(&capture);
    let terminal_inventory_capture = Arc::clone(&capture);
    let scan_authority = authority.clone();
    let terminal_authority = authority.clone();
    let terminal_inventory_authority = authority;
    let owns_source: Arc<SourcePredicate> = Arc::new(owns_source);
    let scan_owns_source = Arc::clone(&owns_source);
    let driver_owns_source = owns_source;
    let terminal_evidence = Arc::new(Mutex::new(
        None::<Result<CapturedRouteEvidence, SourceBackedRouteError>>,
    ));
    let scan_terminal_evidence = Arc::clone(&terminal_evidence);
    let source_terminal_evidence = Arc::clone(&terminal_evidence);
    let inventory_terminal_evidence = terminal_evidence;
    SourceBackedRouteDriver::new(
        move |sink| {
            reset_captured_terminal_evidence(&scan_terminal_evidence)?;
            let base_sources = sink
                .writer
                .base_manifest()
                .map(|manifest| {
                    manifest
                        .sources
                        .iter()
                        .filter(|certificate| scan_owns_source(certificate.observation().source()))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let staged =
                capture_route_evidence(scan_capture.as_ref(), &scan_authority, &base_sources)?;
            validate_captured_route_ownership(&staged, scan_owns_source.as_ref())?;
            let plans = captured_source_plans(&staged, &base_sources);
            let omitted = captured_omitted_sources(&staged, &base_sources);
            let exact_route = omitted.is_empty()
                && plans
                    .values()
                    .all(|plan| matches!(plan, CapturedSourcePlan::Exact { .. }));

            if exact_route {
                for plan in plans.values() {
                    let CapturedSourcePlan::Exact { base, expected } = plan else {
                        return Err(captured_route_internal(
                            "captured exact route contained a replacement plan",
                        ));
                    };
                    stage_captured_exact_replay(sink, base, expected.clone())?;
                }
                sink.certify_complete_inventory(staged.inventory)
                    .map_err(route_coordinator_error)?;
                return Ok(());
            }

            let mut bridge = WriterCaptureSink {
                sink,
                plans: &plans,
                active: None,
                certificates: Vec::new(),
                append_proofs: HashMap::new(),
                inventory: None,
            };
            scan_capture(&mut bridge)?;
            if bridge.active.is_some() {
                return Err(captured_route_internal(
                    "provider capture ended with an uncertified active source",
                ));
            }
            let current = finish_captured_route_evidence(
                bridge.certificates,
                bridge.append_proofs,
                bridge.inventory,
                &scan_authority,
            )?;
            if current.certificates != staged.certificates
                || current.append_proofs != staged.append_proofs
                || current.inventory != staged.inventory
            {
                return Err(captured_route_changed(
                    "provider capture changed between planning and staging",
                ));
            }
            bridge
                .sink
                .certify_complete_inventory(current.inventory.clone())
                .map_err(route_coordinator_error)?;
            for base in omitted {
                let deletion = CertifiedSourceDeletion::from_inventory(
                    base.observation().source().clone(),
                    &current.inventory,
                )
                .map_err(captured_route_contract)?;
                bridge
                    .sink
                    .delete_source(deletion, current.inventory.clone())
                    .map_err(route_coordinator_error)?;
            }
            Ok(())
        },
        move |source| driver_owns_source(source),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                with_cached_captured_route_evidence(
                    &source_terminal_evidence,
                    terminal_capture.as_ref(),
                    &terminal_authority,
                    |evidence| {
                        evidence
                            .certificates_by_identity
                            .get(&expected.observation().source().identity().digest())
                            == Some(expected)
                    },
                )
                .unwrap_or(false)
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                with_cached_captured_route_evidence(
                    &source_terminal_evidence,
                    terminal_capture.as_ref(),
                    &terminal_authority,
                    |evidence| captured_deletion_verifies(deletion, evidence),
                )
                .unwrap_or(false)
            }
        },
        hydrate,
    )
    .with_complete_inventory_revalidation(move |expected| {
        with_cached_captured_route_evidence(
            &inventory_terminal_evidence,
            terminal_inventory_capture.as_ref(),
            &terminal_inventory_authority,
            |evidence| evidence.inventory == *expected,
        )
        .unwrap_or(false)
    })
}

fn reset_captured_terminal_evidence(
    evidence: &Mutex<Option<Result<CapturedRouteEvidence, SourceBackedRouteError>>>,
) -> SourceBackedRouteResult<()> {
    let mut evidence = evidence
        .lock()
        .map_err(|_| captured_route_internal("captured route evidence lock was poisoned"))?;
    *evidence = None;
    Ok(())
}

fn with_cached_captured_route_evidence<T>(
    cached: &Mutex<Option<Result<CapturedRouteEvidence, SourceBackedRouteError>>>,
    capture: &ProviderCaptureCallback,
    authority: &CapturedRouteInventoryAuthority,
    evaluate: impl FnOnce(&CapturedRouteEvidence) -> T,
) -> Option<T> {
    let mut cached = cached.lock().ok()?;
    if cached.is_none() {
        *cached = Some(capture_route_evidence(capture, authority, &[]));
    }
    cached.as_ref()?.as_ref().ok().map(evaluate)
}

fn captured_deletion_verifies(
    deletion: &CertifiedSourceDeletion,
    evidence: &CapturedRouteEvidence,
) -> bool {
    let inventory = &evidence.inventory;
    deletion.source().provider() == inventory.observation().provider()
        && deletion.inventory() == inventory.observation()
        && deletion.discovery_revision() == inventory.discovery_revision()
        && deletion.inventory_digest() == inventory.inventory_digest()
        && deletion.observed_sources() == inventory.observed_sources() as u64
        && !evidence
            .certificates_by_identity
            .contains_key(&deletion.source().identity().digest())
}
