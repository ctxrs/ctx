use super::*;

/// Invocation-local resolver for exact Custom History JSONL ranges.
#[derive(Debug)]
pub(crate) struct CustomHistorySourceBackedResolver {
    routes: HashMap<SourceKey, CustomHistorySourceBackedRoute>,
}

impl CustomHistorySourceBackedResolver {
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

    fn hydrate_exact(
        &self,
        request: &EventHydrationRequest,
    ) -> CustomHistorySourceBackedResult<HydratedProviderRecord> {
        let route = self.route_for(request)?;
        hydrate_from_file(&route.opened, request)
    }
}

impl ContentSourceResolver for CustomHistorySourceBackedResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate_exact(request).map_err(hydration_failure)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let Some(first) = request.events().first() else {
            return Ok(Vec::new());
        };
        let route = self.route_for(first).map_err(hydration_failure)?;
        request
            .events()
            .iter()
            .map(|event| {
                let event_route = self.route_for(event).map_err(hydration_failure)?;
                if event_route.input != route.input {
                    return Err(HydrationFailure {
                        kind: HydrationFailureKind::InvalidLocator,
                        detail: "Custom History session hydration crossed explicit routes"
                            .to_owned(),
                    });
                }
                validate_session_membership(request.session_id(), event)
                    .map_err(hydration_failure)?;
                hydrate_from_file(&route.opened, event).map_err(hydration_failure)
            })
            .collect()
    }
}

fn validate_session_membership(
    requested_session_id: StableEntityId,
    event: &EventHydrationRequest,
) -> CustomHistorySourceBackedResult<()> {
    let (_, _, provider_key, source_id, session_id, _) = validate_locator(event.locator())?;
    let locator_session_id = custom_session_identity(
        event.locator().source(),
        &provider_key,
        &source_id,
        &session_id,
    )?;
    if locator_session_id != requested_session_id {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    }
    Ok(())
}

fn hydrate_from_file(
    file: &OpenedProviderSourceFile,
    request: &EventHydrationRequest,
) -> CustomHistorySourceBackedResult<HydratedProviderRecord> {
    let locator = request.locator();
    let (byte_offset, byte_length, provider_key, source_id, session_id, locator_event_key) =
        validate_locator(locator)?;
    if byte_length > CUSTOM_MAX_HYDRATED_RECORD_BYTES {
        return Err(CustomHistorySourceBackedError::LocatorRangeTooLarge);
    }
    let range_end = byte_offset
        .checked_add(byte_length)
        .ok_or(CustomHistorySourceBackedError::LocatorRangeTooLarge)?;
    if file.len() < range_end {
        return Err(CustomHistorySourceBackedError::LocatorRangeMissing);
    }
    if byte_offset != 0 {
        let boundary = file.read_exact_range(byte_offset.saturating_sub(1), 1, 1)?;
        if boundary != *b"\n" {
            return Err(CustomHistorySourceBackedError::InvalidLocator);
        }
    }
    let length = usize::try_from(byte_length)
        .map_err(|_| CustomHistorySourceBackedError::LocatorRangeTooLarge)?;
    let provider_bytes = file.read_exact_range(
        byte_offset,
        length,
        usize::try_from(CUSTOM_MAX_HYDRATED_RECORD_BYTES)
            .map_err(|_| CustomHistorySourceBackedError::LocatorRangeTooLarge)?,
    )?;
    if !provider_bytes.ends_with(b"\n") {
        return Err(CustomHistorySourceBackedError::InvalidLocator);
    }
    if &Sha256::digest(&provider_bytes)[..] != locator.record_digest() {
        return Err(CustomHistorySourceBackedError::LocatorDigestMismatch);
    }
    let record_bytes = provider_bytes
        .strip_suffix(b"\n")
        .unwrap_or(&provider_bytes);
    let record_bytes = record_bytes.strip_suffix(b"\r").unwrap_or(record_bytes);
    let CtxHistoryJsonlRecord::Event(event) = serde_json::from_slice(record_bytes)
        .map_err(|_| CustomHistorySourceBackedError::LocatorRecordMismatch)?
    else {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    };
    if event.source_id != source_id || event.session_id != session_id {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    }
    let actual_event_key = custom_event_typed_key(&event)?;
    if actual_event_key != locator_event_key {
        return Err(CustomHistorySourceBackedError::LocatorRecordMismatch);
    }
    let stable_session_id =
        custom_session_identity(locator.source(), &provider_key, &source_id, &session_id)?;
    let native_item_key = NativeItemKey::native_id(CUSTOM_EVENT_KEY_NAMESPACE, actual_event_key)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: locator.source(),
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
) -> CustomHistorySourceBackedResult<(u64, u64, String, String, String, TypedKey)> {
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
    Ok((
        *byte_offset,
        *byte_length,
        provider_key.clone(),
        source_id.clone(),
        session_id.clone(),
        event_key.clone(),
    ))
}

fn hydration_failure(error: CustomHistorySourceBackedError) -> HydrationFailure {
    let kind = match &error {
        CustomHistorySourceBackedError::LocatorDigestMismatch
        | CustomHistorySourceBackedError::LocatorRecordMismatch
        | CustomHistorySourceBackedError::Capture(
            CaptureError::SourceChangedDuringCapture
            | CaptureError::InvalidProviderTranscriptPath { .. },
        ) => HydrationFailureKind::StaleRecordEvidence,
        CustomHistorySourceBackedError::LocatorRangeMissing => HydrationFailureKind::MissingRecord,
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
