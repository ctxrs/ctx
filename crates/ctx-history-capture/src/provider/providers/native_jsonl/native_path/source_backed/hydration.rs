use std::{collections::HashMap, path::Path, sync::Mutex};

#[cfg(test)]
use ctx_history_core::CertifiedSource;
use ctx_history_core::{
    derive_event_id, BatchHydrationRequest, BatchHydrationResult, EventHydrationRequest,
    EventIdentityInput, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
    NativeItemKey, NativeRecordCoordinate, PositionStability, SourceKey, SourceRecordLocator,
    SubrecordSelector, TypedKey,
};

#[cfg(test)]
use super::decode_certificate;
use super::{
    direct_jsonl_session_identity, DirectJsonlInventoryLeaf, DirectJsonlSourceAdapter,
    DirectJsonlSourceBackedError, DirectJsonlSourceBackedResult,
};
use crate::{
    provider::providers::native_jsonl::native_path::reader::hydrated_direct_jsonl_lexical_text,
    provider::source_backed::{
        family::jsonl::{visit_verified_ranges, JsonlHydrationRange},
        hydration_failure,
    },
};

#[cfg(test)]
std::thread_local! {
    static DIRECT_JSONL_HYDRATION_WORK: std::cell::Cell<DirectJsonlHydrationWork> =
        const { std::cell::Cell::new(DirectJsonlHydrationWork {
            inventory_scans: 0,
            source_binds: 0,
            leaf_opens: 0,
        }) };
}

#[cfg(test)]
pub(super) fn reset_hydration_work() {
    DIRECT_JSONL_HYDRATION_WORK.set(DirectJsonlHydrationWork::default());
}

#[cfg(test)]
pub(super) fn hydration_work() -> DirectJsonlHydrationWork {
    DIRECT_JSONL_HYDRATION_WORK.get()
}

#[cfg(test)]
fn record_hydration_work(inventories: usize, binds: usize, opens: usize) {
    let work = DIRECT_JSONL_HYDRATION_WORK.get();
    DIRECT_JSONL_HYDRATION_WORK.set(DirectJsonlHydrationWork {
        inventory_scans: work.inventory_scans.saturating_add(inventories),
        source_binds: work.source_binds.saturating_add(binds),
        leaf_opens: work.leaf_opens.saturating_add(opens),
    });
}

pub(super) fn hydrate_single(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    resident: &Mutex<Option<DirectJsonlHydrationCatalog>>,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let mut records =
        hydrate_resident_records(adapter, root, resident, std::slice::from_ref(request))?;
    records.pop().ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "direct JSONL single hydration returned no record",
        )
    })
}

pub(super) fn hydrate_batch(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    resident: &Mutex<Option<DirectJsonlHydrationCatalog>>,
    request: &BatchHydrationRequest,
) -> Result<BatchHydrationResult, HydrationFailure> {
    let records = hydrate_resident_records(adapter, root, resident, request.events())?;
    BatchHydrationResult::new(records).map_err(|error| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            format!("invalid direct JSONL batch hydration result: {error}"),
        )
    })
}

fn hydrate_resident_records(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    resident: &Mutex<Option<DirectJsonlHydrationCatalog>>,
    requests: &[EventHydrationRequest],
) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
    let mut resident = resident.lock().map_err(|_| {
        hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            "direct JSONL hydration catalog lock was poisoned",
        )
    })?;
    if resident.is_none() {
        *resident = Some(adapter.open_hydration_catalog(root).map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?);
    }
    match resident
        .as_mut()
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::TemporarilyUnavailable,
                "direct JSONL hydration catalog was absent",
            )
        })?
        .hydrate_resident_group(requests)
    {
        Ok(records) => Ok(records),
        Err(error) => {
            let failure = hydration_catalog_failure(error);
            if matches!(
                failure.kind,
                HydrationFailureKind::StaleRecordEvidence | HydrationFailureKind::ConfirmedDeleted
            ) {
                *resident = None;
            }
            Err(failure)
        }
    }
}

fn hydration_catalog_failure(error: DirectJsonlSourceBackedError) -> HydrationFailure {
    let kind = match &error {
        DirectJsonlSourceBackedError::SourceAbsent => HydrationFailureKind::ConfirmedDeleted,
        DirectJsonlSourceBackedError::InvalidLocator
        | DirectJsonlSourceBackedError::LocatorRangeTooLarge => {
            HydrationFailureKind::InvalidLocator
        }
        _ => HydrationFailureKind::StaleRecordEvidence,
    };
    hydration_failure(kind, error)
}

impl DirectJsonlSourceAdapter {
    /// Builds one retained inventory catalog for grouped hydration. Callers
    /// with a generation certificate bind without parsing the identity record;
    /// route batches bind once through the resident source catalog.
    pub(crate) fn open_hydration_catalog(
        self,
        root: &Path,
    ) -> DirectJsonlSourceBackedResult<DirectJsonlHydrationCatalog> {
        let inventory = self.discover(root)?;
        if !inventory.is_exact_complete() {
            return Err(DirectJsonlSourceBackedError::IncompleteInventory);
        }
        #[cfg(test)]
        record_hydration_work(1, 0, 0);
        Ok(DirectJsonlHydrationCatalog {
            adapter: self,
            inventory,
            resident_sources: HashMap::new(),
        })
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DirectJsonlHydrationWork {
    pub(crate) inventory_scans: usize,
    pub(crate) source_binds: usize,
    pub(crate) leaf_opens: usize,
}

pub(crate) struct DirectJsonlHydrationCatalog {
    adapter: DirectJsonlSourceAdapter,
    inventory: super::DirectJsonlSourceInventory,
    resident_sources: HashMap<[u8; 32], DirectJsonlResidentHydrationSource>,
}

#[derive(Clone)]
struct DirectJsonlResidentHydrationSource {
    source: SourceKey,
    native_session_id: String,
    leaf: DirectJsonlInventoryLeaf,
}

impl DirectJsonlHydrationCatalog {
    #[cfg(test)]
    pub(crate) fn hydrate_group(
        &mut self,
        certificate: &CertifiedSource,
        requests: &[EventHydrationRequest],
    ) -> DirectJsonlSourceBackedResult<Vec<HydratedProviderRecord>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let checkpoint = decode_certificate(self.adapter, certificate)?;
        let source = certificate.observation().source();
        if requests
            .iter()
            .any(|request| !request.locator().source().exact_descriptor_eq(source))
        {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        }
        let digest = source.exact_descriptor_digest();
        let binding = if let Some(binding) = self.resident_sources.get(&digest) {
            if !binding.source.exact_descriptor_eq(source) {
                return Err(DirectJsonlSourceBackedError::InvalidLocator);
            }
            binding.clone()
        } else {
            let leaf = self
                .inventory
                .leaves
                .iter()
                .find(|leaf| leaf.path == *checkpoint.physical.identity().source_path())
                .ok_or(DirectJsonlSourceBackedError::SourceAbsent)?
                .clone();
            let native_session_id = checkpoint
                .session
                .ok_or(DirectJsonlSourceBackedError::CountMismatch)?
                .native_session_id;
            let binding = DirectJsonlResidentHydrationSource {
                source: source.clone(),
                native_session_id,
                leaf,
            };
            self.resident_sources.insert(digest, binding.clone());
            #[cfg(test)]
            self.record_source_binding();
            binding
        };
        hydrate_bound_group(
            self.adapter,
            &binding.leaf,
            &binding.source,
            &binding.native_session_id,
            requests,
        )
    }

    pub(crate) fn hydrate_resident_group(
        &mut self,
        requests: &[EventHydrationRequest],
    ) -> DirectJsonlSourceBackedResult<Vec<HydratedProviderRecord>> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let source = first.locator().source();
        if requests
            .iter()
            .any(|request| !request.locator().source().exact_descriptor_eq(source))
        {
            return Err(DirectJsonlSourceBackedError::InvalidLocator);
        }
        let digest = source.exact_descriptor_digest();
        let binding = if let Some(binding) = self.resident_sources.get(&digest) {
            if !binding.source.exact_descriptor_eq(source) {
                return Err(DirectJsonlSourceBackedError::InvalidLocator);
            }
            binding.clone()
        } else {
            let (native_session_id, route_key) =
                hydration_locator_binding(self.adapter, source, first.locator())?;
            let expected_source = self.adapter.source_key(&native_session_id)?;
            if !expected_source.exact_descriptor_eq(source) {
                return Err(DirectJsonlSourceBackedError::InvalidLocator);
            }
            let leaf = self
                .inventory
                .leaves
                .iter()
                .find(|leaf| leaf.route_key == route_key)
                .ok_or(DirectJsonlSourceBackedError::SourceAbsent)?
                .clone();
            let binding = DirectJsonlResidentHydrationSource {
                source: source.clone(),
                native_session_id,
                leaf,
            };
            self.resident_sources.insert(digest, binding.clone());
            #[cfg(test)]
            self.record_source_binding();
            binding
        };
        hydrate_bound_group(
            self.adapter,
            &binding.leaf,
            &binding.source,
            &binding.native_session_id,
            requests,
        )
    }

    #[cfg(test)]
    fn record_source_binding(&self) {
        record_hydration_work(0, 1, 0);
    }
}

fn hydrate_bound_group(
    adapter: DirectJsonlSourceAdapter,
    leaf: &DirectJsonlInventoryLeaf,
    source: &SourceKey,
    native_session_id: &str,
    requests: &[EventHydrationRequest],
) -> DirectJsonlSourceBackedResult<Vec<HydratedProviderRecord>> {
    #[cfg(test)]
    record_hydration_work(0, 0, 1);
    let (current_leaf, source_file) =
        adapter.open_leaf_for_hydration(leaf, source, native_session_id)?;
    let mut sub_ordinals = Vec::with_capacity(requests.len());
    let mut ranges = Vec::with_capacity(requests.len());
    for request in requests {
        let (sub_ordinal, range) =
            hydration_locator_range(adapter, source, native_session_id, &leaf.route_key, request)?;
        sub_ordinals.push(sub_ordinal);
        ranges.push(range);
    }
    let records =
        visit_verified_ranges(&current_leaf.path, &source_file, &ranges, |index, bytes| {
            let value: serde_json::Value = serde_json::from_slice(bytes)?;
            let display_text =
                hydrated_direct_jsonl_lexical_text(adapter.provider, &value, sub_ordinals[index])?
                    .ok_or(DirectJsonlSourceBackedError::LocatorRecordNotRetained)?;
            Ok::<_, DirectJsonlSourceBackedError>(HydratedProviderRecord {
                event_id: requests[index].event_id(),
                provider_bytes: display_text.into_bytes(),
            })
        })?;
    adapter.revalidate_opened_hydration_identity(
        &current_leaf,
        &source_file,
        source,
        native_session_id,
    )?;
    Ok(records)
}

fn hydration_locator_binding(
    adapter: DirectJsonlSourceAdapter,
    source: &SourceKey,
    locator: &SourceRecordLocator,
) -> DirectJsonlSourceBackedResult<(String, Vec<u8>)> {
    locator.validate_contract()?;
    if !source.exact_descriptor_eq(locator.source())
        || locator.source().provider() != adapter.provider.as_str()
        || locator.source().source_format() != adapter.source_format
        || locator.source().schema_variant() != adapter.schema_variant
    {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        native_session_key,
        native_event_key,
        ..
    } = locator.coordinate()
    else {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    };
    let Some(TypedKey::Utf8(native_session_id)) = native_session_key else {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    };
    let Some(TypedKey::Composite(event_key)) = native_event_key else {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    };
    let Some(TypedKey::Bytes(route_key)) = event_key.get(2) else {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    };
    Ok((native_session_id.clone(), route_key.clone()))
}

fn hydration_locator_range(
    adapter: DirectJsonlSourceAdapter,
    source: &SourceKey,
    native_session_id: &str,
    route_key: &[u8],
    request: &EventHydrationRequest,
) -> DirectJsonlSourceBackedResult<(u32, JsonlHydrationRange)> {
    let locator = request.locator();
    locator.validate_contract()?;
    if !source.exact_descriptor_eq(locator.source())
        || locator.source().provider() != adapter.provider.as_str()
        || locator.source().source_format() != adapter.source_format
        || locator.source().schema_variant() != adapter.schema_variant
    {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    };
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(native_session_id.to_owned())) {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    }
    let Some(TypedKey::Composite(event_key)) = native_event_key else {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    };
    if event_key.len() != 3 || event_key.get(2) != Some(&TypedKey::Bytes(route_key.to_vec())) {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    }
    let Some(TypedKey::U64(sub_ordinal)) = event_key.get(1) else {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    };
    let sub_ordinal =
        u32::try_from(*sub_ordinal).map_err(|_| DirectJsonlSourceBackedError::InvalidLocator)?;
    let (_, session_id) = direct_jsonl_session_identity(adapter, native_session_id)?;
    let native_item_key = match event_key.first() {
        Some(TypedKey::Utf8(native_record_id)) => NativeItemKey::native_id(
            format!("{}.direct-jsonl-event", adapter.provider.as_str()),
            TypedKey::utf8(native_record_id)?,
        )?,
        Some(TypedKey::U64(raw_ordinal)) if raw_ordinal == physical_ordinal => {
            NativeItemKey::certified_position(
                format!("{}.direct-jsonl-ordinal", adapter.provider.as_str()),
                TypedKey::U64(*raw_ordinal),
                PositionStability::AppendStable,
            )?
        }
        _ => return Err(DirectJsonlSourceBackedError::InvalidLocator),
    };
    let subrecord_selector = (sub_ordinal != 0)
        .then(|| {
            SubrecordSelector::certified_position(
                "direct-jsonl-subrecord",
                TypedKey::U64(u64::from(sub_ordinal)),
                PositionStability::StableSlot,
            )
        })
        .transpose()?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "direct-jsonl-event",
        native_item_key: &native_item_key,
        subrecord_selector: subrecord_selector.as_ref(),
    })?;
    if event_id != request.event_id() {
        return Err(DirectJsonlSourceBackedError::InvalidLocator);
    }
    let byte_length = usize::try_from(*byte_length)
        .map_err(|_| DirectJsonlSourceBackedError::LocatorRangeTooLarge)?;
    let range = JsonlHydrationRange::new(*byte_offset, byte_length, *locator.record_digest())
        .map_err(|_| DirectJsonlSourceBackedError::LocatorRangeTooLarge)?;
    Ok((sub_ordinal, range))
}
