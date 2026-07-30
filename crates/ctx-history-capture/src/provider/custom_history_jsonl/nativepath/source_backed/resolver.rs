use super::*;
use crate::provider::source_backed::family::jsonl::{visit_verified_ranges, JsonlHydrationRange};

/// Body-free route catalog for current-path exact hydration.
#[derive(Debug, Clone)]
struct CustomHistorySourceBackedCatalog {
    input: CustomHistorySourceBackedInput,
    source: SourceKey,
}

impl CustomHistorySourceBackedCatalog {
    fn new(input: CustomHistorySourceBackedInput) -> CustomHistorySourceBackedResult<Self> {
        let source = input.source_key()?;
        Ok(Self { input, source })
    }

    fn current_resolver(&self) -> Result<CustomHistorySourceBackedResolver, HydrationFailure> {
        let opened = match open_explicit_source(self.input.path()) {
            Ok(opened) => opened,
            Err(CustomHistorySourceBackedError::Capture(CaptureError::Io(error)))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Err(HydrationFailure {
                    kind: HydrationFailureKind::ConfirmedDeleted,
                    detail: "the explicit Custom History source is absent".to_owned(),
                });
            }
            Err(error) => return Err(hydration_failure(error)),
        };
        #[cfg(test)]
        record_custom_history_work(|work| {
            work.hydration_source_opens = work.hydration_source_opens.saturating_add(1);
        });
        CustomHistorySourceBackedResolver::new([route(&self.input, self.source.clone(), opened)])
            .map_err(hydration_failure)
    }
}

impl ContentSourceResolver for CustomHistorySourceBackedCatalog {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        self.current_resolver()?.hydrate_event(request)
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        self.current_resolver()?.hydrate_batch(request)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.current_resolver()?.hydrate_session(request)
    }
}

/// Invocation-local resolver for exact Custom History JSONL ranges.
#[derive(Debug)]
pub(crate) struct CustomHistorySourceBackedResolver {
    routes: HashMap<SourceKey, CustomHistorySourceBackedRoute>,
}

impl CustomHistorySourceBackedResolver {
    pub(crate) fn hydrate_current_event(
        input: &CustomHistorySourceBackedInput,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        CustomHistorySourceBackedCatalog::new(input.clone())
            .map_err(hydration_failure)?
            .hydrate_event(request)
    }

    pub(crate) fn hydrate_current_batch(
        input: &CustomHistorySourceBackedInput,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        CustomHistorySourceBackedCatalog::new(input.clone())
            .map_err(hydration_failure)?
            .hydrate_batch(request)
    }

    pub(crate) fn new(
        routes: impl IntoIterator<Item = CustomHistorySourceBackedRoute>,
    ) -> CustomHistorySourceBackedResult<Self> {
        let mut registered = HashMap::<SourceKey, CustomHistorySourceBackedRoute>::new();
        for route in routes {
            if let Some(existing) = registered.get(&route.source) {
                if !existing.source.exact_descriptor_eq(&route.source)
                    || existing.input != route.input
                {
                    return Err(CustomHistorySourceBackedError::DuplicateResolverSource);
                }
                continue;
            }
            registered.insert(route.source.clone(), route);
        }
        Ok(Self { routes: registered })
    }

    fn route_for(
        &self,
        request: &EventHydrationRequest,
    ) -> CustomHistorySourceBackedResult<&CustomHistorySourceBackedRoute> {
        request.locator().validate_contract()?;
        let route = self
            .routes
            .get(request.locator().source())
            .ok_or(CustomHistorySourceBackedError::LocatorSourceNotFound)?;
        if !route.source.exact_descriptor_eq(request.locator().source()) {
            return Err(CustomHistorySourceBackedError::InvalidLocator);
        }
        Ok(route)
    }

    fn hydrate_group(
        &self,
        requests: &[EventHydrationRequest],
    ) -> CustomHistorySourceBackedResult<Vec<HydratedProviderRecord>> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let route = self.route_for(first)?;
        for request in requests.iter().skip(1) {
            let event_route = self.route_for(request)?;
            if event_route.input != route.input {
                return Err(CustomHistorySourceBackedError::InvalidLocator);
            }
        }
        hydrate_from_file(route.input.path(), &route.opened, requests)
    }
}

impl ContentSourceResolver for CustomHistorySourceBackedResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let mut records = self
            .hydrate_group(std::slice::from_ref(request))
            .map_err(hydration_failure)?;
        records.pop().ok_or_else(|| HydrationFailure {
            kind: HydrationFailureKind::InvalidLocator,
            detail: "Custom History single hydration returned no record".to_owned(),
        })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let records = self
            .hydrate_group(request.events())
            .map_err(hydration_failure)?;
        let result = BatchHydrationResult::new(records).map_err(|error| HydrationFailure {
            kind: HydrationFailureKind::InvalidLocator,
            detail: format!("invalid Custom History hydration batch: {error}"),
        })?;
        result.validate_for_request(request)?;
        Ok(result)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = request.events().first() else {
            return Ok(Vec::new());
        };
        self.route_for(first).map_err(hydration_failure)?;
        for event in request.events() {
            validate_session_membership(request.session_id(), event).map_err(hydration_failure)?;
        }
        self.hydrate_group(request.events())
            .map_err(hydration_failure)
    }
}

fn validate_session_membership(
    requested_session_id: StableEntityId,
    event: &EventHydrationRequest,
) -> CustomHistorySourceBackedResult<()> {
    let locator = validate_locator(event.locator())?;
    let locator_session_id = custom_session_identity(
        event.locator().source(),
        &locator.provider_key,
        &locator.source_id,
        &locator.session_id,
    )?;
    if locator_session_id != requested_session_id {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    }
    Ok(())
}

#[derive(Debug)]
struct ValidatedCustomLocator {
    byte_offset: u64,
    byte_length: usize,
    provider_key: String,
    source_id: String,
    session_id: String,
    event_key: TypedKey,
}

#[derive(Debug)]
struct PlannedHydrationRange {
    byte_offset: u64,
    request_index: usize,
    boundary: bool,
    range: JsonlHydrationRange,
}

fn hydrate_from_file(
    source_path: &Path,
    file: &Arc<OpenedProviderSourceFile>,
    requests: &[EventHydrationRequest],
) -> CustomHistorySourceBackedResult<Vec<HydratedProviderRecord>> {
    let mut locators = Vec::with_capacity(requests.len());
    let mut planned = Vec::with_capacity(requests.len().saturating_mul(2));
    for (request_index, request) in requests.iter().enumerate() {
        let locator = validate_locator(request.locator())?;
        if locator.byte_offset != 0 {
            planned.push(PlannedHydrationRange {
                byte_offset: locator.byte_offset.saturating_sub(1),
                request_index,
                boundary: true,
                range: JsonlHydrationRange::new(
                    locator.byte_offset.saturating_sub(1),
                    1,
                    Sha256::digest(b"\n").into(),
                )?,
            });
        }
        planned.push(PlannedHydrationRange {
            byte_offset: locator.byte_offset,
            request_index,
            boundary: false,
            range: JsonlHydrationRange::new(
                locator.byte_offset,
                locator.byte_length,
                *request.locator().record_digest(),
            )?,
        });
        locators.push(locator);
    }
    planned.sort_by_key(|range| (range.byte_offset, !range.boundary, range.request_index));
    let ranges = planned
        .iter()
        .map(|planned| planned.range)
        .collect::<Vec<_>>();
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.hydration_passes = work.hydration_passes.saturating_add(1);
    });
    let visited =
        visit_verified_ranges(source_path, file, &ranges, |range_index, provider_bytes| {
            let range = planned
                .get(range_index)
                .ok_or(CustomHistorySourceBackedError::InvalidLocator)?;
            if range.boundary {
                if provider_bytes != b"\n" {
                    return Err(CustomHistorySourceBackedError::InvalidLocator);
                }
                return Ok(None);
            }
            let request = requests
                .get(range.request_index)
                .ok_or(CustomHistorySourceBackedError::InvalidLocator)?;
            let locator = locators
                .get(range.request_index)
                .ok_or(CustomHistorySourceBackedError::InvalidLocator)?;
            Ok(Some(hydrate_record(request, locator, provider_bytes)?))
        })?;
    let mut records = (0..requests.len())
        .map(|_| None)
        .collect::<Vec<Option<HydratedProviderRecord>>>();
    for (range, record) in planned.iter().zip(visited) {
        let Some(record) = record else {
            continue;
        };
        let slot = records
            .get_mut(range.request_index)
            .ok_or(CustomHistorySourceBackedError::InvalidLocator)?;
        if slot.replace(record).is_some() {
            return Err(CustomHistorySourceBackedError::InvalidLocator);
        }
    }
    let records = records
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or(CustomHistorySourceBackedError::InvalidLocator)?;
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.hydrated_records = work.hydrated_records.saturating_add(records.len());
    });
    Ok(records)
}

fn hydrate_record(
    request: &EventHydrationRequest,
    locator: &ValidatedCustomLocator,
    provider_bytes: &[u8],
) -> CustomHistorySourceBackedResult<HydratedProviderRecord> {
    if !provider_bytes.ends_with(b"\n") {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    }
    let record_bytes = provider_bytes.strip_suffix(b"\n").unwrap_or(provider_bytes);
    let record_bytes = record_bytes.strip_suffix(b"\r").unwrap_or(record_bytes);
    let CtxHistoryJsonlRecord::Event(event) = serde_json::from_slice(record_bytes)
        .map_err(|_| CustomHistorySourceBackedError::LocatorRecordMismatch)?
    else {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    };
    if event.source_id != locator.source_id.as_str()
        || event.session_id != locator.session_id.as_str()
    {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    }
    let actual_event_key = custom_event_typed_key(&event)?;
    if actual_event_key != locator.event_key {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    }
    let stable_session_id = custom_session_identity(
        request.locator().source(),
        &locator.provider_key,
        &locator.source_id,
        &locator.session_id,
    )?;
    let native_item_key = NativeItemKey::native_id(CUSTOM_EVENT_KEY_NAMESPACE, actual_event_key)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: request.locator().source(),
        session_id: stable_session_id,
        logical_item_kind: CUSTOM_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    if event_id != request.event_id() {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    }
    Ok(HydratedProviderRecord {
        event_id,
        provider_bytes: lexical_body(&event).into_bytes(),
    })
}

fn validate_locator(
    locator: &SourceRecordLocator,
) -> CustomHistorySourceBackedResult<ValidatedCustomLocator> {
    if locator.source().provider() != CaptureProvider::Custom.as_str()
        || locator.source().source_format() != CUSTOM_ROUTE_SOURCE_FORMAT
        || locator.source().schema_variant() != CUSTOM_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != CUSTOM_SOURCE_IDENTITY_VERSION
        || !matches!(locator.source().anchor(), SourceAnchor::CatalogLineage(_))
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        native_session_key: Some(TypedKey::Composite(session_key)),
        native_event_key: Some(event_key),
        ..
    } = locator.coordinate()
    else {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    };
    let [TypedKey::Utf8(provider_key), TypedKey::Utf8(source_id), TypedKey::Utf8(session_id)] =
        session_key.as_slice()
    else {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    };
    if *byte_length == 0 || *byte_length > CUSTOM_MAX_HYDRATED_RECORD_BYTES {
        return Err(CustomHistorySourceBackedError::LocatorRangeTooLarge);
    }
    Ok(ValidatedCustomLocator {
        byte_offset: *byte_offset,
        byte_length: usize::try_from(*byte_length)
            .map_err(|_| CustomHistorySourceBackedError::LocatorRangeTooLarge)?,
        provider_key: provider_key.clone(),
        source_id: source_id.clone(),
        session_id: session_id.clone(),
        event_key: event_key.clone(),
    })
}

fn hydration_failure(error: CustomHistorySourceBackedError) -> HydrationFailure {
    let kind = match &error {
        CustomHistorySourceBackedError::LocatorRecordMismatch
        | CustomHistorySourceBackedError::Capture(
            CaptureError::SourceChangedDuringCapture
            | CaptureError::InvalidPayload(_)
            | CaptureError::InvalidProviderTranscriptPath { .. },
        ) => HydrationFailureKind::StaleRecordEvidence,
        CustomHistorySourceBackedError::InvalidLocator
        | CustomHistorySourceBackedError::Resolver(_)
        | CustomHistorySourceBackedError::LocatorRangeTooLarge
        | CustomHistorySourceBackedError::LocatorSourceNotFound
        | CustomHistorySourceBackedError::DuplicateResolverSource => {
            HydrationFailureKind::InvalidLocator
        }
        CustomHistorySourceBackedError::InventoryChanged => {
            HydrationFailureKind::StaleSourceEvidence
        }
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}
