use std::collections::HashMap;

use super::*;

#[cfg(test)]
std::thread_local! {
    static AFTER_PROMPT_HYDRATION_OBSERVATION_HOOK:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn install_after_prompt_hydration_observation_hook(hook: impl FnOnce() + 'static) {
    AFTER_PROMPT_HYDRATION_OBSERVATION_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "prompt-history hydration hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_prompt_hydration_observation_hook() {
    let hook = AFTER_PROMPT_HYDRATION_OBSERVATION_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromptHydrationObservation {
    len: u64,
    modified: Option<std::time::SystemTime>,
    readonly: bool,
    ordinary_file_token: [u8; 32],
}

/// Invocation-local resolver for exact prompt-history JSONL ranges.
#[derive(Debug)]
pub(crate) struct CodexPromptHistorySourceBackedResolverV0 {
    routes: HashMap<SourceKey, CodexPromptHistorySourceBackedSourceV0>,
}

impl CodexPromptHistorySourceBackedResolverV0 {
    pub(crate) fn new(
        routes: impl IntoIterator<Item = CodexPromptHistorySourceBackedSourceV0>,
    ) -> CodexPromptHistorySourceBackedResultV0<Self> {
        let mut registered = HashMap::<SourceKey, CodexPromptHistorySourceBackedSourceV0>::new();
        for route in routes {
            if let Some(existing) = registered.get(&route.source) {
                if !existing.source.exact_descriptor_eq(&route.source)
                    || existing.input != route.input
                {
                    return Err(CodexPromptHistorySourceBackedErrorV0::DuplicateResolverSource);
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
    ) -> CodexPromptHistorySourceBackedResultV0<&CodexPromptHistorySourceBackedSourceV0> {
        request.locator().validate_contract()?;
        let route = self
            .routes
            .get(request.locator().source())
            .ok_or(CodexPromptHistorySourceBackedErrorV0::LocatorSourceNotFound)?;
        if !route.source.exact_descriptor_eq(request.locator().source()) {
            return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
        }
        Ok(route)
    }

    fn hydrate_exact(
        &self,
        request: &EventHydrationRequest,
    ) -> CodexPromptHistorySourceBackedResultV0<HydratedProviderRecord> {
        let route = self.route_for(request)?;
        hydrate_from_source(route, request)
    }
}

impl ContentSourceResolver for CodexPromptHistorySourceBackedResolverV0 {
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
        let first_route = self.route_for(first).map_err(hydration_failure)?;
        request
            .events()
            .iter()
            .map(|event| {
                let route = self.route_for(event).map_err(hydration_failure)?;
                if route.input != first_route.input {
                    return Err(HydrationFailure {
                        kind: HydrationFailureKind::InvalidLocator,
                        detail: "Codex prompt-history session hydration crossed source routes"
                            .to_owned(),
                    });
                }
                let (_, _, _, native_session_id) =
                    validate_locator(event.locator()).map_err(hydration_failure)?;
                let session_id = stable_session_id(event.locator().source(), &native_session_id)
                    .map_err(hydration_failure)?;
                if session_id != request.session_id() {
                    return Err(HydrationFailure {
                        kind: HydrationFailureKind::InvalidLocator,
                        detail: "Codex prompt-history locator belongs to another session"
                            .to_owned(),
                    });
                }
                hydrate_from_source(route, event).map_err(hydration_failure)
            })
            .collect()
    }
}

fn hydrate_from_source(
    source: &CodexPromptHistorySourceBackedSourceV0,
    request: &EventHydrationRequest,
) -> CodexPromptHistorySourceBackedResultV0<HydratedProviderRecord> {
    let locator = request.locator();
    let (byte_offset, byte_length, physical_ordinal, native_session_id) =
        validate_locator(locator)?;
    for attempt in 0..2 {
        let opening = prompt_hydration_observation(&source.opened)?;
        #[cfg(test)]
        run_after_prompt_hydration_observation_hook();
        let range_end = byte_offset
            .checked_add(byte_length)
            .ok_or(CodexPromptHistorySourceBackedErrorV0::LocatorRangeTooLarge)?;
        if range_end > opening.len {
            return Err(CodexPromptHistorySourceBackedErrorV0::LocatorRangeMissing);
        }
        if byte_offset != 0 {
            let boundary =
                source
                    .opened
                    .read_exact_range_allow_append(byte_offset.saturating_sub(1), 1, 1)?;
            if boundary != *b"\n" {
                return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
            }
        }
        let length = usize::try_from(byte_length)
            .map_err(|_| CodexPromptHistorySourceBackedErrorV0::LocatorRangeTooLarge)?;
        let provider_bytes = source.opened.read_exact_range_allow_append(
            byte_offset,
            length,
            usize::try_from(MAX_HYDRATED_RECORD_BYTES)
                .map_err(|_| CodexPromptHistorySourceBackedErrorV0::LocatorRangeTooLarge)?,
        )?;
        if !provider_bytes.ends_with(b"\n") {
            return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
        }
        if &Sha256::digest(&provider_bytes)[..] != locator.record_digest() {
            return Err(CodexPromptHistorySourceBackedErrorV0::LocatorDigestMismatch);
        }
        let body = provider_bytes
            .strip_suffix(b"\n")
            .unwrap_or(&provider_bytes);
        let body = body.strip_suffix(b"\r").unwrap_or(body);
        let line: PromptLine = serde_json::from_slice(body)
            .map_err(|_| CodexPromptHistorySourceBackedErrorV0::LocatorRecordMismatch)?;
        if line.session_id != native_session_id
            || line.session_id.trim().is_empty()
            || chrono::DateTime::from_timestamp(line.ts, 0).is_none()
        {
            return Err(CodexPromptHistorySourceBackedErrorV0::LocatorRecordMismatch);
        }
        let session_id = stable_session_id(locator.source(), &line.session_id)?;
        let native_item_key = NativeItemKey::certified_position(
            EVENT_POSITION_KIND,
            TypedKey::U64(physical_ordinal),
            PositionStability::AppendStable,
        )?;
        let event_id = derive_event_id(EventIdentityInput {
            source: locator.source(),
            session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })?;
        if event_id != request.event_id() {
            return Err(CodexPromptHistorySourceBackedErrorV0::LocatorRecordMismatch);
        }
        let closing = prompt_hydration_observation(&source.opened)?;
        if closing == opening {
            return Ok(HydratedProviderRecord {
                event_id,
                provider_bytes: prompt_lexical_body(&line.text).into_bytes(),
            });
        }
        if attempt == 0 && closing.len > opening.len {
            continue;
        }
        return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
    }
    Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged)
}

fn prompt_hydration_observation(
    source: &OpenedProviderSourceFile,
) -> CodexPromptHistorySourceBackedResultV0<PromptHydrationObservation> {
    let (metadata, ordinary_file_token) = stable_current_ordinary_file_observation(source)?;
    Ok(PromptHydrationObservation {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        readonly: metadata.permissions().readonly(),
        ordinary_file_token,
    })
}

fn validate_locator(
    locator: &SourceRecordLocator,
) -> CodexPromptHistorySourceBackedResultV0<(u64, u64, u64, String)> {
    if locator.source().provider() != CaptureProvider::Codex.as_str()
        || locator.source().source_format() != SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != SOURCE_IDENTITY_VERSION
        || !matches!(locator.source().anchor(), SourceAnchor::CatalogLineage(_))
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key: Some(TypedKey::Utf8(native_session_id)),
        native_event_key: Some(TypedKey::U64(native_event_ordinal)),
    } = locator.coordinate()
    else {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
    };
    if *byte_length == 0 || *byte_length > MAX_HYDRATED_RECORD_BYTES {
        return Err(CodexPromptHistorySourceBackedErrorV0::LocatorRangeTooLarge);
    }
    if native_session_id.is_empty() || native_event_ordinal != physical_ordinal {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidLocator);
    }
    Ok((
        *byte_offset,
        *byte_length,
        *physical_ordinal,
        native_session_id.clone(),
    ))
}

fn hydration_failure(error: CodexPromptHistorySourceBackedErrorV0) -> HydrationFailure {
    let kind = match &error {
        CodexPromptHistorySourceBackedErrorV0::LocatorDigestMismatch
        | CodexPromptHistorySourceBackedErrorV0::LocatorRecordMismatch
        | CodexPromptHistorySourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture
            | CaptureError::InvalidProviderTranscriptPath { .. },
        ) => HydrationFailureKind::StaleRecordEvidence,
        CodexPromptHistorySourceBackedErrorV0::LocatorRangeMissing => {
            HydrationFailureKind::MissingRecord
        }
        CodexPromptHistorySourceBackedErrorV0::SourceChanged => {
            HydrationFailureKind::StaleSourceEvidence
        }
        CodexPromptHistorySourceBackedErrorV0::InvalidLocator
        | CodexPromptHistorySourceBackedErrorV0::Resolver(_)
        | CodexPromptHistorySourceBackedErrorV0::LocatorRangeTooLarge
        | CodexPromptHistorySourceBackedErrorV0::LocatorSourceNotFound
        | CodexPromptHistorySourceBackedErrorV0::DuplicateResolverSource => {
            HydrationFailureKind::InvalidLocator
        }
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}
