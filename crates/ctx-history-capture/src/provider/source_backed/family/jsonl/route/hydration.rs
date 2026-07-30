#[cfg(test)]
use std::cell::RefCell;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, EventHydrationRequest, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind,
};

use super::{
    discover, hydration_error, observe_opened_file_same_object, FamilyResident, JsonlFamilyAdapter,
};

#[cfg(test)]
thread_local! {
    static AFTER_GROUP_OPEN_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn set_after_jsonl_group_open_hook(hook: impl FnOnce() + 'static) {
    AFTER_GROUP_OPEN_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_jsonl_group_open_hook() {
    AFTER_GROUP_OPEN_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

pub(super) fn hydrate_single(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    resident: &Mutex<FamilyResident>,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let mut records = hydrate_group(adapter, root, resident, std::slice::from_ref(request))?;
    records.pop().ok_or_else(|| {
        hydration_error(
            HydrationFailureKind::InvalidLocator,
            "JSONL single hydration returned no record",
        )
    })
}

pub(super) fn hydrate_batch(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    resident: &Mutex<FamilyResident>,
    request: &BatchHydrationRequest,
) -> Result<BatchHydrationResult, HydrationFailure> {
    let records = hydrate_group(adapter, root, resident, request.events())?;
    let result = BatchHydrationResult::new(records)
        .map_err(|error| hydration_error(HydrationFailureKind::InvalidLocator, error))?;
    result.validate_for_request(request)?;
    Ok(result)
}

fn hydrate_group(
    adapter: &dyn JsonlFamilyAdapter,
    root: &Path,
    resident: &Mutex<FamilyResident>,
    requests: &[EventHydrationRequest],
) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let source = requests[0].locator().source();
    if requests
        .iter()
        .any(|request| !request.locator().source().exact_descriptor_eq(source))
    {
        return Err(hydration_error(
            HydrationFailureKind::InvalidLocator,
            "JSONL hydration batch spans exact sources",
        ));
    }
    let result = (|| {
        let mut resident = resident.lock().map_err(|_| {
            hydration_error(
                HydrationFailureKind::TemporarilyUnavailable,
                "JSONL resident catalog lock was poisoned",
            )
        })?;
        if resident.hydration_inventory.is_none() {
            let inventory = discover(adapter, root).map_err(|error| {
                hydration_error(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            if inventory.root_missing() {
                return Err(hydration_error(
                    HydrationFailureKind::TemporarilyUnavailable,
                    "provider JSONL root is unavailable",
                ));
            }
            resident.hydration_inventory = Some(inventory);
        }
        let inventory = resident.hydration_inventory.as_ref().ok_or_else(|| {
            hydration_error(
                HydrationFailureKind::TemporarilyUnavailable,
                "JSONL resident inventory is absent",
            )
        })?;
        let leaf = inventory
            .leaves()
            .iter()
            .find(|leaf| leaf.source().exact_descriptor_eq(source))
            .ok_or_else(|| {
                hydration_error(
                    HydrationFailureKind::ConfirmedDeleted,
                    "exact JSONL source is absent from the resident inventory",
                )
            })?;
        for attempt in 0..2 {
            let (opened, opening_observation) = leaf.open_for_hydration().map_err(|error| {
                hydration_error(HydrationFailureKind::StaleRecordEvidence, error)
            })?;
            #[cfg(test)]
            run_after_jsonl_group_open_hook();
            let mut hydrator = adapter.hydrator(leaf, Arc::clone(&opened))?;
            let mut records = Vec::with_capacity(requests.len());
            for request in requests {
                let record = hydrator.hydrate(request)?;
                if record.event_id != request.event_id() {
                    return Err(hydration_error(
                        HydrationFailureKind::InvalidLocator,
                        "JSONL hydrator changed the requested event identity",
                    ));
                }
                records.push(record);
            }
            hydrator.finish()?;
            let closing_observation =
                observe_opened_file_same_object(leaf.source_path(), opened.as_ref()).map_err(
                    |error| hydration_error(HydrationFailureKind::StaleRecordEvidence, error),
                )?;
            if inventory.revalidate_root().is_err() {
                return Err(hydration_error(
                    HydrationFailureKind::StaleRecordEvidence,
                    "JSONL source authority changed during grouped hydration",
                ));
            }
            if closing_observation == opening_observation {
                return Ok(records);
            }
            if attempt == 0
                && !leaf.whole_record
                && opening_observation.is_same_file_growth_to(&closing_observation)
            {
                continue;
            }
            return Err(hydration_error(
                HydrationFailureKind::StaleRecordEvidence,
                "JSONL source changed during grouped hydration",
            ));
        }
        Err(hydration_error(
            HydrationFailureKind::StaleRecordEvidence,
            "JSONL source did not stabilize during grouped hydration",
        ))
    })();
    if result.as_ref().is_err_and(|failure| {
        matches!(
            failure.kind,
            HydrationFailureKind::StaleRecordEvidence | HydrationFailureKind::ConfirmedDeleted
        )
    }) {
        if let Ok(mut resident) = resident.lock() {
            resident.hydration_inventory = None;
        }
    }
    result
}
