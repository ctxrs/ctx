use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, EventHydrationRequest, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind,
};

use super::{discover, hydration_error, observe_opened_file, FamilyResident, JsonlFamilyAdapter};

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
        let opened = leaf
            .open_verified()
            .map_err(|error| hydration_error(HydrationFailureKind::StaleRecordEvidence, error))?;
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
        if observe_opened_file(leaf.source_path(), opened.as_ref())
            .map_err(|error| hydration_error(HydrationFailureKind::StaleRecordEvidence, error))?
            != *leaf.observation()
            || inventory.revalidate_root().is_err()
        {
            return Err(hydration_error(
                HydrationFailureKind::StaleRecordEvidence,
                "JSONL source changed during grouped hydration",
            ));
        }
        Ok(records)
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
