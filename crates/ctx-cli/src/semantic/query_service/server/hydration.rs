use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

#[cfg(test)]
use ctx_history_core::SourceRecordLocator;
use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, ContentSourceResolver, EventHydrationRequest,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, StableEntityId,
};
use ctx_history_index::{EventRecord, VerifiedIndex};
use serde_json::{json, Value};

use crate::output::compact_json;
use crate::semantic::source_backed_refresh_coordinator::{
    SourceBackedRefreshCoordinator, SourceBackedResolverAccessError,
};

use super::super::hydration_budget::{
    provider_body_is_admitted, provider_read_reservation_bytes, provider_read_size_is_exact,
    retained_response_item_content_charge, retained_response_items_metadata_charge,
    successful_response_envelope_charge, unknown_provider_read_reservation_bytes,
    HydrationBudgetError, HydrationBudgetSnapshot, HydrationByteBudget,
};
use super::request_validation::{
    hydration_failure_kind_name, source_hydration_mode, source_hydration_protocol_failure,
    valid_source_generation_id, SourceHydrationBatchItem, SourceHydrationMode,
};
use super::{
    DAEMON_SOURCE_HYDRATION_MAX_ITEMS, DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    DAEMON_SOURCE_HYDRATION_MAX_WORKERS,
};

struct SourceHydrationGroup {
    exact_read_size: bool,
    first_position: usize,
    positions: Vec<usize>,
    requests: Vec<EventHydrationRequest>,
}

struct SourceHydrationWork {
    first_position: usize,
    positions: Vec<usize>,
    requests: Vec<EventHydrationRequest>,
}

struct SourceHydrationPlan {
    exact_work: Vec<SourceHydrationWork>,
    unknown_work: Vec<SourceHydrationWork>,
    retained_preflight_bytes: usize,
    preflight_reservation_bytes: usize,
    unknown_item_reservation_bytes: usize,
}

struct HydratedSourceItem {
    position: usize,
    event_id: StableEntityId,
    text: String,
}

struct HydratedSourceWork {
    items: Vec<HydratedSourceItem>,
    retained_bytes: usize,
}

enum SourceHydrationWorkFailure {
    Budget(HydrationBudgetError),
    Resolver(HydrationFailure),
}

struct GenerationBoundHydrationResolver<'a, R> {
    index: &'a VerifiedIndex,
    resolver: &'a R,
}

impl<R> GenerationBoundHydrationResolver<'_, R> {
    fn validate(
        &self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<(), HydrationFailure> {
        let event = self
            .index
            .event_by_id(request.event_id().as_uuid())
            .map_err(|error| HydrationFailure {
                kind: HydrationFailureKind::TemporarilyUnavailable,
                detail: format!(
                    "read generation-bound hydration event {}: {error}",
                    request.event_id()
                ),
            })?
            .ok_or_else(|| HydrationFailure {
                kind: HydrationFailureKind::MissingRecord,
                detail: format!(
                    "source generation omitted hydration event {}",
                    request.event_id()
                ),
            })?;
        validate_generation_bound_request(&event, request)
    }
}

impl<R> ContentSourceResolver for GenerationBoundHydrationResolver<'_, R>
where
    R: ContentSourceResolver,
{
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        self.validate(request)?;
        self.resolver.hydrate_event(request)
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
        for event in request.events() {
            self.validate(event)?;
        }
        self.resolver.hydrate_batch(request)
    }
}

fn validate_generation_bound_request(
    event: &EventRecord,
    request: &EventHydrationRequest,
) -> std::result::Result<(), HydrationFailure> {
    if request.event_id() != event.event_id
        || request.locator() != &event.locator
        || request.source_path_hint() != event.source_path.as_deref()
    {
        return Err(HydrationFailure {
            kind: HydrationFailureKind::InvalidLocator,
            detail: format!(
                "hydration locator for {} does not match the active source generation",
                request.event_id()
            ),
        });
    }
    Ok(())
}

pub(in crate::semantic) fn handle_source_hydration_batch(
    data_root: &Path,
    source_refresh: &SourceBackedRefreshCoordinator,
    request: &Value,
) -> Value {
    let generation_id = request
        .get("generation_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !valid_source_generation_id(generation_id) {
        return source_hydration_protocol_failure(
            "invalid_generation",
            "invalid_locator",
            "source hydration generation ID must be a lowercase SHA-256 digest",
            false,
        );
    }
    let retained = match source_refresh.resolver_for_generation(data_root, generation_id) {
        Ok(retained) => retained,
        Err(error @ SourceBackedResolverAccessError::Missing { .. }) => {
            return source_hydration_protocol_failure(
                "resolver_generation_unavailable",
                "temporarily_unavailable",
                &error.to_string(),
                source_refresh.has_pending_request(),
            );
        }
        Err(error @ SourceBackedResolverAccessError::GenerationMismatch { .. }) => {
            return source_hydration_protocol_failure(
                "resolver_generation_mismatch",
                "stale_source_evidence",
                &error.to_string(),
                source_refresh.has_pending_request(),
            );
        }
    };
    if retained.generation_id() != generation_id {
        return source_hydration_protocol_failure(
            "resolver_generation_mismatch",
            "stale_source_evidence",
            "daemon resolver changed while accepting the hydration batch",
            true,
        );
    }
    let Some(index) = retained.verified_index() else {
        let failure = HydrationFailure {
            kind: HydrationFailureKind::TemporarilyUnavailable,
            detail: format!(
                "daemon resolver for source generation {generation_id} has no verified index authority"
            ),
        };
        source_refresh.handle_hydration_failure(data_root, generation_id, failure.clone());
        return source_hydration_protocol_failure(
            "resolver_generation_unavailable",
            hydration_failure_kind_name(failure.kind),
            &failure.detail,
            source_refresh.has_pending_request(),
        );
    };
    let resolver = GenerationBoundHydrationResolver {
        index: index.as_ref(),
        resolver: retained.resolver(),
    };
    handle_source_hydration_batch_with(request, generation_id, &resolver, |failure| {
        source_refresh.handle_hydration_failure(data_root, generation_id, failure.clone());
        source_refresh.has_pending_request()
    })
}

pub(in crate::semantic) fn handle_source_hydration_batch_with<R, Refresh>(
    request: &Value,
    retained_generation_id: &str,
    resolver: &R,
    refresh: Refresh,
) -> Value
where
    R: ContentSourceResolver + Sync,
    Refresh: Fn(&HydrationFailure) -> bool,
{
    handle_source_hydration_batch_with_budget(
        request,
        retained_generation_id,
        resolver,
        refresh,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    )
    .0
}

fn handle_source_hydration_batch_with_budget<R, Refresh>(
    request: &Value,
    retained_generation_id: &str,
    resolver: &R,
    refresh: Refresh,
    budget_limit: usize,
) -> (Value, HydrationBudgetSnapshot)
where
    R: ContentSourceResolver + Sync,
    Refresh: Fn(&HydrationFailure) -> bool,
{
    let generation_id = request
        .get("generation_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if generation_id != retained_generation_id {
        return (
            source_hydration_protocol_failure(
                "resolver_generation_mismatch",
                "stale_source_evidence",
                &format!(
                    "requested source generation {generation_id:?}, retained {retained_generation_id:?}"
                ),
                false,
            ),
            HydrationBudgetSnapshot::default(),
        );
    }
    let budget = HydrationByteBudget::new(budget_limit);
    let response = (|| {
        let mode = match source_hydration_mode(request) {
            Ok(mode) => mode,
            Err(error) => {
                return source_hydration_protocol_failure(
                    "invalid_request",
                    "invalid_locator",
                    &format!("{error:#}"),
                    false,
                );
            }
        };
        let Some(values) = request.get("items").and_then(Value::as_array) else {
            return source_hydration_protocol_failure(
                "invalid_request",
                "invalid_locator",
                "source hydration request has no item array",
                false,
            );
        };
        if values.is_empty() || values.len() > DAEMON_SOURCE_HYDRATION_MAX_ITEMS {
            return source_hydration_protocol_failure(
                "item_limit",
                "invalid_locator",
                &format!(
                    "source hydration batch has {} items; expected 1..={DAEMON_SOURCE_HYDRATION_MAX_ITEMS}",
                    values.len()
                ),
                false,
            );
        }
        let mut requests = Vec::with_capacity(values.len());
        for value in values {
            let item: SourceHydrationBatchItem = match serde_json::from_value(value.clone()) {
                Ok(item) => item,
                Err(error) => {
                    return source_hydration_protocol_failure(
                        "invalid_request",
                        "invalid_locator",
                        &format!("decode typed source hydration item: {error}"),
                        false,
                    );
                }
            };
            let request =
                match EventHydrationRequest::new(item.event_identity, item.locator.clone())
                    .and_then(|request| request.with_source_path_hint(item.source_path))
                {
                    Ok(request) => request,
                    Err(error) => {
                        return source_hydration_protocol_failure(
                            "invalid_request",
                            "invalid_locator",
                            &format!("validate source hydration locator: {error}"),
                            false,
                        );
                    }
                };
            requests.push(request);
        }
        let batch = match BatchHydrationRequest::new(requests) {
            Ok(batch) => batch,
            Err(error) => {
                return source_hydration_protocol_failure(
                    "invalid_request",
                    "invalid_locator",
                    &format!("validate ordered source hydration batch: {error}"),
                    false,
                )
            }
        };
        let mut grouped =
            BTreeMap::<([u8; 32], bool), (usize, Vec<usize>, Vec<EventHydrationRequest>)>::new();
        for (position, request) in batch.events().iter().enumerate() {
            let exact_read_size = provider_read_size_is_exact(request);
            let key = (
                request.locator().source().exact_descriptor_digest(),
                exact_read_size,
            );
            let group = grouped
                .entry(key)
                .or_insert_with(|| (position, Vec::new(), Vec::new()));
            group.1.push(position);
            group.2.push(request.clone());
        }
        let groups = grouped
            .into_values()
            .map(
                |(first_position, positions, requests)| SourceHydrationGroup {
                    exact_read_size: provider_read_size_is_exact(&requests[0]),
                    first_position,
                    positions,
                    requests,
                },
            )
            .collect::<Vec<_>>();
        let envelope_charge = match successful_response_envelope_charge(retained_generation_id) {
            Ok(charge) => charge,
            Err(error) => return source_hydration_budget_failure(error),
        };
        let plan = match plan_source_hydration_work(groups, mode, envelope_charge, batch.len()) {
            Ok(plan) => plan,
            Err(error) => return source_hydration_budget_failure(error),
        };
        // Fixed response charges and every certified JSONL range are admitted
        // as one request before any resolver or result collection is entered.
        let preflight_reservation = match budget.reserve(plan.preflight_reservation_bytes) {
            Ok(reservation) => reservation,
            Err(error) => return source_hydration_budget_failure(error),
        };
        let SourceHydrationPlan {
            exact_work,
            mut unknown_work,
            retained_preflight_bytes,
            unknown_item_reservation_bytes,
            ..
        } = plan;
        let exact_work_len = exact_work.len();
        let exact_results = hydrate_source_work_batch(resolver, &exact_work, mode, &budget);
        if let Some(failure) = exact_results.iter().find_map(|(_, result)| match result {
            Err(SourceHydrationWorkFailure::Resolver(failure)) => Some(failure),
            _ => None,
        }) {
            let refresh_scheduled = refresh(failure);
            return source_hydration_protocol_failure(
                "source_hydration_failed",
                hydration_failure_kind_name(failure.kind),
                &failure.detail,
                refresh_scheduled,
            );
        }
        if source_hydration_work_budget_failed(&exact_results, exact_work_len, &budget) {
            return source_hydration_budget_failure(HydrationBudgetError::Exhausted);
        }
        let (exact_retained_bytes, exact_items) = match hydrated_source_work_totals(&exact_results)
        {
            Ok(totals) => totals,
            Err(error) => {
                budget.cancel_exhausted();
                return source_hydration_budget_failure(error);
            }
        };
        let retained_bytes = match retained_preflight_bytes.checked_add(exact_retained_bytes) {
            Some(bytes) => bytes,
            None => {
                budget.cancel_exhausted();
                return source_hydration_budget_failure(HydrationBudgetError::Exhausted);
            }
        };
        if let Err(error) = preflight_reservation.commit(retained_bytes, exact_items) {
            return source_hydration_budget_failure(error);
        }
        let mut hydrated_work = exact_results
            .into_iter()
            .filter_map(|(_, result)| result.ok())
            .collect::<Vec<_>>();

        // Unknown-size coordinates cannot be admitted request-wide at their
        // 16 MiB ceiling without rejecting ordinary batches. Join each wave,
        // commit its actual retained content, then size the next wave from the
        // remaining shared budget. Performance residual: a same-source group
        // is split into at-most-three-item resolver batches, so SQLite may use
        // multiple short snapshots. Removing that cost would require a later
        // budget-aware streaming provider batch, not a wider provider API here.
        while !unknown_work.is_empty() {
            let max_wave_items = budget.available_when_idle() / unknown_item_reservation_bytes;
            if max_wave_items == 0 {
                budget.cancel_exhausted();
                return source_hydration_budget_failure(HydrationBudgetError::Exhausted);
            }
            let wave = take_unknown_source_hydration_wave(&mut unknown_work, max_wave_items);
            let wave_items = wave.iter().try_fold(0_usize, |items, work| {
                items.checked_add(work.requests.len())
            });
            let Some(wave_items) = wave_items.filter(|items| *items > 0) else {
                budget.cancel_exhausted();
                return source_hydration_budget_failure(HydrationBudgetError::Exhausted);
            };
            let wave_reservation_bytes =
                match unknown_item_reservation_bytes.checked_mul(wave_items) {
                    Some(bytes) => bytes,
                    None => {
                        budget.cancel_exhausted();
                        return source_hydration_budget_failure(HydrationBudgetError::Exhausted);
                    }
                };
            let wave_reservation = match budget.reserve(wave_reservation_bytes) {
                Ok(reservation) => reservation,
                Err(error) => return source_hydration_budget_failure(error),
            };
            let wave_work_len = wave.len();
            let wave_results = hydrate_source_work_batch(resolver, &wave, mode, &budget);
            if let Some(failure) = wave_results.iter().find_map(|(_, result)| match result {
                Err(SourceHydrationWorkFailure::Resolver(failure)) => Some(failure),
                _ => None,
            }) {
                let refresh_scheduled = refresh(failure);
                return source_hydration_protocol_failure(
                    "source_hydration_failed",
                    hydration_failure_kind_name(failure.kind),
                    &failure.detail,
                    refresh_scheduled,
                );
            }
            if source_hydration_work_budget_failed(&wave_results, wave_work_len, &budget) {
                return source_hydration_budget_failure(HydrationBudgetError::Exhausted);
            }
            let (wave_retained_bytes, committed_items) =
                match hydrated_source_work_totals(&wave_results) {
                    Ok(totals) => totals,
                    Err(error) => {
                        budget.cancel_exhausted();
                        return source_hydration_budget_failure(error);
                    }
                };
            if let Err(error) = wave_reservation.commit(wave_retained_bytes, committed_items) {
                return source_hydration_budget_failure(error);
            }
            hydrated_work.extend(
                wave_results
                    .into_iter()
                    .filter_map(|(_, result)| result.ok()),
            );
        }

        let mut ordered = (0..batch.len())
            .map(|_| None)
            .collect::<Vec<Option<HydratedSourceItem>>>();
        for hydrated in hydrated_work {
            for item in hydrated.items {
                let Some(slot) = ordered.get_mut(item.position) else {
                    return source_hydration_protocol_failure(
                        "invalid_resolver_response",
                        "invalid_locator",
                        "source resolver returned an out-of-range event",
                        false,
                    );
                };
                if slot.replace(item).is_some() {
                    return source_hydration_protocol_failure(
                        "invalid_resolver_response",
                        "invalid_locator",
                        "source resolver returned a duplicate event",
                        false,
                    );
                }
            }
        }
        let mut response_items = Vec::with_capacity(batch.len());
        for (request, slot) in batch.events().iter().zip(&mut ordered) {
            let Some(item) = slot.take() else {
                return source_hydration_protocol_failure(
                    "invalid_resolver_response",
                    "missing_record",
                    &format!(
                        "source resolver omitted requested event {}",
                        request.event_id()
                    ),
                    false,
                );
            };
            if item.event_id != request.event_id() {
                return source_hydration_protocol_failure(
                    "invalid_resolver_response",
                    "invalid_locator",
                    "source resolver returned a mismatched event",
                    false,
                );
            }
            response_items.push(json!({
                "event_id": item.event_id.as_uuid(),
                "text": item.text,
            }));
        }
        compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "generation_id": retained_generation_id,
            "items": response_items,
        }))
    })();
    (response, budget.snapshot())
}

fn plan_source_hydration_work(
    groups: Vec<SourceHydrationGroup>,
    mode: SourceHydrationMode,
    envelope_charge: usize,
    item_count: usize,
) -> std::result::Result<SourceHydrationPlan, HydrationBudgetError> {
    let display_copy_bytes = match mode {
        SourceHydrationMode::SearchDisplay { max_chars } => max_chars
            .checked_mul(4)
            .ok_or(HydrationBudgetError::Exhausted)?,
        SourceHydrationMode::Complete => 0,
    };
    let retained_preflight_bytes = envelope_charge
        .checked_add(retained_response_items_metadata_charge(item_count)?)
        .ok_or(HydrationBudgetError::Exhausted)?;
    let mut preflight_reservation_bytes = retained_preflight_bytes;
    let mut exact_work = Vec::with_capacity(groups.len());
    let mut unknown_work = Vec::with_capacity(groups.len());
    for group in groups {
        let work = SourceHydrationWork {
            first_position: group.first_position,
            positions: group.positions,
            requests: group.requests,
        };
        if group.exact_read_size {
            for request in &work.requests {
                let item_bytes = provider_read_reservation_bytes(request, display_copy_bytes)?;
                preflight_reservation_bytes = preflight_reservation_bytes
                    .checked_add(item_bytes)
                    .ok_or(HydrationBudgetError::Exhausted)?;
            }
            exact_work.push(work);
        } else {
            unknown_work.push(work);
        }
    }
    exact_work.sort_by_key(|item| item.first_position);
    unknown_work.sort_by_key(|item| item.first_position);
    Ok(SourceHydrationPlan {
        exact_work,
        unknown_work,
        retained_preflight_bytes,
        preflight_reservation_bytes,
        unknown_item_reservation_bytes: unknown_provider_read_reservation_bytes(
            display_copy_bytes,
        )?,
    })
}

fn hydrate_source_work_batch<R>(
    resolver: &R,
    work: &[SourceHydrationWork],
    mode: SourceHydrationMode,
    budget: &HydrationByteBudget,
) -> Vec<(
    usize,
    std::result::Result<HydratedSourceWork, SourceHydrationWorkFailure>,
)>
where
    R: ContentSourceResolver + Sync,
{
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(work.len()));
    std::thread::scope(|scope| {
        for _ in 0..work.len().min(DAEMON_SOURCE_HYDRATION_MAX_WORKERS) {
            scope.spawn(|| loop {
                if budget.is_cancelled() {
                    break;
                }
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = work.get(index) else {
                    break;
                };
                if budget.is_cancelled() {
                    break;
                }
                let result = hydrate_source_work(resolver, item, mode);
                match &result {
                    Err(SourceHydrationWorkFailure::Budget(HydrationBudgetError::Exhausted)) => {
                        budget.cancel_exhausted()
                    }
                    Err(_) => budget.cancel(),
                    Ok(_) => {}
                }
                results
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push((item.first_position, result));
            });
        }
    });
    let mut results = results
        .into_inner()
        .unwrap_or_else(|error| error.into_inner());
    results.sort_by_key(|(position, _)| *position);
    results
}

fn source_hydration_work_budget_failed(
    results: &[(
        usize,
        std::result::Result<HydratedSourceWork, SourceHydrationWorkFailure>,
    )],
    expected_work: usize,
    budget: &HydrationByteBudget,
) -> bool {
    results.iter().any(|(_, result)| {
        matches!(
            result,
            Err(SourceHydrationWorkFailure::Budget(
                HydrationBudgetError::Cancelled | HydrationBudgetError::Exhausted
            ))
        )
    }) || results.len() != expected_work
        || budget.snapshot().exhausted
}

fn hydrated_source_work_totals(
    results: &[(
        usize,
        std::result::Result<HydratedSourceWork, SourceHydrationWorkFailure>,
    )],
) -> std::result::Result<(usize, usize), HydrationBudgetError> {
    results.iter().try_fold(
        (0_usize, 0_usize),
        |(retained_bytes, items), (_, result)| {
            let Ok(hydrated) = result else {
                return Err(HydrationBudgetError::Exhausted);
            };
            Ok((
                retained_bytes
                    .checked_add(hydrated.retained_bytes)
                    .ok_or(HydrationBudgetError::Exhausted)?,
                items
                    .checked_add(hydrated.items.len())
                    .ok_or(HydrationBudgetError::Exhausted)?,
            ))
        },
    )
}

fn take_unknown_source_hydration_wave(
    pending: &mut Vec<SourceHydrationWork>,
    max_items: usize,
) -> Vec<SourceHydrationWork> {
    let mut counts = vec![0_usize; pending.len()];
    let mut remaining = max_items;
    for (count, work) in counts.iter_mut().zip(pending.iter()) {
        if remaining == 0 {
            break;
        }
        if !work.requests.is_empty() {
            *count = 1;
            remaining -= 1;
        }
    }
    while remaining > 0 {
        let mut advanced = false;
        for (count, work) in counts.iter_mut().zip(pending.iter()) {
            if remaining == 0 {
                break;
            }
            if *count > 0 && *count < work.requests.len() {
                *count += 1;
                remaining -= 1;
                advanced = true;
            }
        }
        if !advanced {
            break;
        }
    }

    let mut wave = Vec::new();
    for (work, count) in pending.iter_mut().zip(counts) {
        if count == 0 {
            continue;
        }
        let positions = work.positions.drain(..count).collect::<Vec<_>>();
        let requests = work.requests.drain(..count).collect::<Vec<_>>();
        wave.push(SourceHydrationWork {
            first_position: positions[0],
            positions,
            requests,
        });
    }
    pending.retain(|work| !work.requests.is_empty());
    wave.sort_by_key(|work| work.first_position);
    wave
}

fn hydrate_source_work(
    resolver: &impl ContentSourceResolver,
    work: &SourceHydrationWork,
    mode: SourceHydrationMode,
) -> std::result::Result<HydratedSourceWork, SourceHydrationWorkFailure> {
    let request = BatchHydrationRequest::new(work.requests.clone()).map_err(|error| {
        SourceHydrationWorkFailure::Resolver(HydrationFailure {
            kind: HydrationFailureKind::InvalidLocator,
            detail: format!("validate grouped source hydration request: {error}"),
        })
    })?;
    let result = resolver
        .hydrate_batch(&request)
        .map_err(SourceHydrationWorkFailure::Resolver)?;
    result
        .validate_for_request(&request)
        .map_err(SourceHydrationWorkFailure::Resolver)?;
    let mut retained_bytes = 0_usize;
    let items = request
        .events()
        .iter()
        .zip(&work.positions)
        .zip(result.into_records())
        .map(|((expected, position), record)| {
            if record.event_id != expected.event_id() {
                return Err(SourceHydrationWorkFailure::Resolver(HydrationFailure {
                    kind: HydrationFailureKind::InvalidLocator,
                    detail: format!(
                        "source resolver reordered event {} as {}",
                        expected.event_id(),
                        record.event_id
                    ),
                }));
            }
            if !provider_body_is_admitted(
                expected,
                record.provider_bytes.len(),
                record.provider_bytes.capacity(),
            ) {
                return Err(SourceHydrationWorkFailure::Budget(
                    HydrationBudgetError::Exhausted,
                ));
            }
            let text = String::from_utf8(record.provider_bytes).map_err(|error| {
                SourceHydrationWorkFailure::Resolver(HydrationFailure {
                    kind: HydrationFailureKind::UnsupportedParserRevision,
                    detail: format!(
                        "source resolver returned non-UTF-8 display content for {}: {}",
                        expected.event_id(),
                        error.utf8_error()
                    ),
                })
            })?;
            if text.is_empty() {
                return Err(SourceHydrationWorkFailure::Resolver(HydrationFailure {
                    kind: HydrationFailureKind::MissingRecord,
                    detail: format!(
                        "source resolver returned empty display content for {}",
                        expected.event_id()
                    ),
                }));
            }
            let text = match mode {
                SourceHydrationMode::SearchDisplay { max_chars } => {
                    bounded_search_display_text(text, max_chars)
                }
                SourceHydrationMode::Complete => text,
            };
            retained_bytes = retained_bytes
                .checked_add(
                    retained_response_item_content_charge(&text)
                        .map_err(SourceHydrationWorkFailure::Budget)?,
                )
                .ok_or(SourceHydrationWorkFailure::Budget(
                    HydrationBudgetError::Exhausted,
                ))?;
            Ok(HydratedSourceItem {
                position: *position,
                event_id: record.event_id,
                text,
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(HydratedSourceWork {
        items,
        retained_bytes,
    })
}

fn bounded_search_display_text(text: String, max_chars: usize) -> String {
    let end = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(index, _)| index);
    let mut bounded = String::with_capacity(end);
    bounded.push_str(&text[..end]);
    bounded
}

fn source_hydration_budget_failure(_error: HydrationBudgetError) -> Value {
    compact_json(json!({
        "ok": false,
        "schema_version": 1,
        "code": "hydration_budget_exceeded",
        "failure_kind": "content_too_large",
        "detail": "source hydration request exceeds the daemon byte budget",
        "refresh_scheduled": false,
        "retryable": false,
    }))
}

#[cfg(test)]
mod tests;
