use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexHydratedRecordV0 {
    pub provider_bytes: Vec<u8>,
    pub decoded_display_text: Option<String>,
}

/// One-invocation resolver for locator-backed event and session rendering.
///
/// Discovery is intentionally paid once so rendering a session does not
/// recatalog every provider tree for every event.
#[derive(Debug)]
pub struct CodexLocatorResolverV0 {
    sources_by_native_session: HashMap<String, (CodexCatalogSource, SourceKey)>,
}

impl CodexLocatorResolverV0 {
    pub fn discover<I, P>(session_roots: I) -> CodexSourceBackedResultV0<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut sources_by_native_session = HashMap::new();
        for session_root in session_roots {
            let retained = discover_codex_session_catalog_retained(session_root.as_ref())?;
            let discovery = super::discover_codex_catalog_sources(&retained.sessions);
            if retained.summary.failed_sessions != 0 || !discovery.rejections.is_empty() {
                return Err(CodexSourceBackedErrorV0::IncompleteCatalog {
                    rejected: discovery.rejections.len(),
                    failed: retained.summary.failed_sessions,
                });
            }
            let sources = bind_catalog_capabilities(
                discovery.sources,
                &retained.root,
                session_root.as_ref(),
            )?;
            for (source, source_key, native_session_id) in bind_source_keys(sources)? {
                if sources_by_native_session
                    .insert(native_session_id.clone(), (source, source_key))
                    .is_some()
                {
                    return Err(CodexSourceBackedErrorV0::DuplicateNativeSessionId(
                        native_session_id,
                    ));
                }
            }
        }
        Ok(Self {
            sources_by_native_session,
        })
    }

    pub(super) fn from_bound_sources(
        sources: impl IntoIterator<Item = (CodexCatalogSource, SourceKey, String)>,
    ) -> CodexSourceBackedResultV0<Self> {
        let mut sources_by_native_session = HashMap::new();
        for (source, source_key, native_session_id) in sources {
            if sources_by_native_session
                .insert(native_session_id.clone(), (source, source_key))
                .is_some()
            {
                return Err(CodexSourceBackedErrorV0::DuplicateNativeSessionId(
                    native_session_id,
                ));
            }
        }
        Ok(Self {
            sources_by_native_session,
        })
    }

    pub fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> CodexSourceBackedResultV0<CodexHydratedRecordV0> {
        locator.validate_contract()?;
        let (native_session_id, byte_offset, byte_length, physical_ordinal) =
            validate_codex_locator(locator)?;
        if byte_length > MAX_HYDRATED_CODEX_RECORD_BYTES {
            return Err(CodexSourceBackedErrorV0::LocatorRangeTooLarge);
        }

        let (source, source_key) = self
            .sources_by_native_session
            .get(&native_session_id)
            .ok_or_else(|| {
                CodexSourceBackedErrorV0::LocatorSourceNotFound(native_session_id.clone())
            })?;
        if !source_key.exact_descriptor_eq(locator.source()) {
            return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
        }

        hydrate_codex_source_record(source, locator, byte_offset, byte_length, physical_ordinal)
    }

    pub(crate) fn hydrate_event_request(
        &self,
        request: &EventHydrationRequest,
    ) -> CodexSourceBackedResultV0<HydratedProviderRecord> {
        let (native_session_id, _, _, physical_ordinal) =
            validate_codex_locator(request.locator())?;
        let event_id = codex_event_identity(
            request.locator().source(),
            &native_session_id,
            physical_ordinal,
        )?;
        if event_id != request.event_id() {
            return Err(CodexSourceBackedErrorV0::LocatorEventMismatch);
        }
        let hydrated = self.hydrate(request.locator())?;
        let provider_bytes = hydrated
            .decoded_display_text
            .ok_or(CodexSourceBackedErrorV0::LocatorRecordNotDisplayable)?
            .into_bytes();
        Ok(HydratedProviderRecord {
            event_id,
            provider_bytes,
        })
    }

    pub(crate) fn hydrate_batch_request(
        &self,
        request: &BatchHydrationRequest,
    ) -> CodexSourceBackedResultV0<BatchHydrationResult> {
        let Some(first) = request.events().first() else {
            return Ok(BatchHydrationResult::new(Vec::new())?);
        };
        let (first_native_session_id, _, _, _) = validate_codex_event_request(first)?;
        let (source, source_key) = self
            .sources_by_native_session
            .get(&first_native_session_id)
            .ok_or_else(|| {
                CodexSourceBackedErrorV0::LocatorSourceNotFound(first_native_session_id.clone())
            })?;
        if !source_key.exact_descriptor_eq(first.locator().source()) {
            return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
        }

        let mut reads = Vec::with_capacity(request.len());
        for (caller_index, event) in request.events().iter().enumerate() {
            let (native_session_id, byte_offset, byte_length, physical_ordinal) =
                validate_codex_event_request(event)?;
            if native_session_id != first_native_session_id
                || !source_key.exact_descriptor_eq(event.locator().source())
            {
                return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
            }
            reads.push(CodexBatchReadV0 {
                caller_index,
                event,
                byte_offset,
                byte_length,
                physical_ordinal,
            });
        }
        reads.sort_by_key(|read| (read.byte_offset, read.physical_ordinal, read.caller_index));

        let opened = open_codex_hydration_source(source)?;
        let opening_observation =
            opened_codex_file_observation(&source.source_path, opened.file())?;
        opened
            .revalidate()
            .map_err(normalize_codex_hydration_capture_error)?;
        if opening_observation != source.catalog_observation {
            return Err(CodexSourceBackedErrorV0::Capture(
                CaptureError::SourceChangedDuringCapture,
            ));
        }
        let mut reader = opened.file().try_clone()?;
        let mut ordered = vec![None; request.len()];
        #[cfg(test)]
        CODEX_BATCH_READ_OFFSETS.with(|offsets| offsets.borrow_mut().clear());
        for read in reads {
            #[cfg(test)]
            CODEX_BATCH_READ_OFFSETS.with(|offsets| offsets.borrow_mut().push(read.byte_offset));
            let hydrated = hydrate_codex_source_record_from_batch_reader(
                &mut reader,
                opened.len(),
                read.event.locator(),
                read.byte_offset,
                read.byte_length,
                read.physical_ordinal,
            )?;
            let provider_bytes = hydrated
                .decoded_display_text
                .ok_or(CodexSourceBackedErrorV0::LocatorRecordNotDisplayable)?
                .into_bytes();
            ordered[read.caller_index] = Some(HydratedProviderRecord {
                event_id: read.event.event_id(),
                provider_bytes,
            });
        }
        opened
            .revalidate()
            .map_err(normalize_codex_hydration_capture_error)?;
        let records = ordered
            .into_iter()
            .map(|record| record.ok_or(CodexSourceBackedErrorV0::LocatorEventMismatch))
            .collect::<CodexSourceBackedResultV0<Vec<_>>>()?;
        Ok(BatchHydrationResult::new(records)?)
    }
}

#[derive(Debug)]
struct CodexBatchReadV0<'a> {
    caller_index: usize,
    event: &'a EventHydrationRequest,
    byte_offset: u64,
    byte_length: u64,
    physical_ordinal: u64,
}

pub fn hydrate_codex_locator(
    session_root: impl AsRef<Path>,
    locator: &SourceRecordLocator,
) -> CodexSourceBackedResultV0<CodexHydratedRecordV0> {
    CodexLocatorResolverV0::discover([session_root])?.hydrate(locator)
}

fn open_codex_hydration_source(
    source: &CodexCatalogSource,
) -> CodexSourceBackedResultV0<Arc<OpenedProviderSourceFile>> {
    #[cfg(test)]
    CODEX_HYDRATION_SOURCE_OPEN_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
    open_codex_source_capability(source)
        .map_err(normalize_codex_hydration_capture_error)
        .map_err(Into::into)
}

fn normalize_codex_hydration_capture_error(error: CaptureError) -> CaptureError {
    match error {
        CaptureError::InvalidProviderTranscriptPath {
            reason: "provider source changed while its authority handle was retained",
            ..
        } => CaptureError::SourceChangedDuringCapture,
        other => other,
    }
}

fn hydrate_codex_source_record(
    source: &CodexCatalogSource,
    locator: &SourceRecordLocator,
    byte_offset: u64,
    byte_length: u64,
    physical_ordinal: u64,
) -> CodexSourceBackedResultV0<CodexHydratedRecordV0> {
    let byte_length =
        usize::try_from(byte_length).map_err(|_| CodexSourceBackedErrorV0::LocatorRangeTooLarge)?;
    let opened = open_codex_hydration_source(source)?;
    if byte_offset != 0 {
        let boundary = opened.read_exact_range(byte_offset.saturating_sub(1), 1, 1)?;
        if boundary != *b"\n" {
            return Err(CodexSourceBackedErrorV0::LocatorRecordBoundaryMismatch);
        }
    }
    let provider_bytes = opened
        .read_exact_range(
            byte_offset,
            byte_length,
            MAX_HYDRATED_CODEX_RECORD_BYTES as usize,
        )
        .map_err(|error| match error {
            CaptureError::InvalidPayload(_) => CodexSourceBackedErrorV0::LocatorRangeMissing,
            other => CodexSourceBackedErrorV0::Capture(other),
        })?;
    if !provider_bytes.ends_with(b"\n") {
        return Err(CodexSourceBackedErrorV0::LocatorRecordBoundaryMismatch);
    }
    let actual_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
    if &actual_digest != locator.record_digest() {
        return Err(CodexSourceBackedErrorV0::LocatorDigestMismatch);
    }
    let decoded_display_text = decode_exact_display_text(&provider_bytes, physical_ordinal)?;
    opened
        .revalidate()
        .map_err(normalize_codex_hydration_capture_error)?;
    Ok(CodexHydratedRecordV0 {
        provider_bytes,
        decoded_display_text,
    })
}

fn hydrate_codex_source_record_from_batch_reader(
    reader: &mut (impl Read + Seek),
    source_length: u64,
    locator: &SourceRecordLocator,
    byte_offset: u64,
    byte_length: u64,
    physical_ordinal: u64,
) -> CodexSourceBackedResultV0<CodexHydratedRecordV0> {
    let byte_length =
        usize::try_from(byte_length).map_err(|_| CodexSourceBackedErrorV0::LocatorRangeTooLarge)?;
    let byte_length_u64 =
        u64::try_from(byte_length).map_err(|_| CodexSourceBackedErrorV0::LocatorRangeTooLarge)?;
    let record_end = byte_offset
        .checked_add(byte_length_u64)
        .ok_or(CodexSourceBackedErrorV0::LocatorRangeMissing)?;
    if record_end > source_length {
        return Err(CodexSourceBackedErrorV0::LocatorRangeMissing);
    }

    let boundary_length = usize::from(byte_offset != 0);
    let read_length = byte_length
        .checked_add(boundary_length)
        .ok_or(CodexSourceBackedErrorV0::LocatorRangeTooLarge)?;
    let read_offset = byte_offset.saturating_sub(boundary_length as u64);
    reader.seek(SeekFrom::Start(read_offset))?;
    let mut bytes = vec![0_u8; read_length];
    reader.read_exact(&mut bytes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            CodexSourceBackedErrorV0::Capture(CaptureError::SourceChangedDuringCapture)
        } else {
            CodexSourceBackedErrorV0::Capture(CaptureError::Io(error))
        }
    })?;
    if boundary_length != 0 && bytes.first() != Some(&b'\n') {
        return Err(CodexSourceBackedErrorV0::LocatorRecordBoundaryMismatch);
    }
    let provider_bytes = bytes.split_off(boundary_length);
    if !provider_bytes.ends_with(b"\n") {
        return Err(CodexSourceBackedErrorV0::LocatorRecordBoundaryMismatch);
    }
    let actual_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
    if &actual_digest != locator.record_digest() {
        return Err(CodexSourceBackedErrorV0::LocatorDigestMismatch);
    }
    let decoded_display_text = decode_exact_display_text(&provider_bytes, physical_ordinal)?;
    Ok(CodexHydratedRecordV0 {
        provider_bytes,
        decoded_display_text,
    })
}

fn validate_codex_event_request(
    request: &EventHydrationRequest,
) -> CodexSourceBackedResultV0<(String, u64, u64, u64)> {
    request.validate_contract()?;
    let (native_session_id, byte_offset, byte_length, physical_ordinal) =
        validate_codex_locator(request.locator())?;
    let event_id = codex_event_identity(
        request.locator().source(),
        &native_session_id,
        physical_ordinal,
    )?;
    if event_id != request.event_id() {
        return Err(CodexSourceBackedErrorV0::LocatorEventMismatch);
    }
    Ok((
        native_session_id,
        byte_offset,
        byte_length,
        physical_ordinal,
    ))
}

pub(super) fn validate_codex_locator(
    locator: &SourceRecordLocator,
) -> CodexSourceBackedResultV0<(String, u64, u64, u64)> {
    if locator.source().provider() != CaptureProvider::Codex.as_str()
        || locator.source().source_format() != CODEX_SESSION_SOURCE_FORMAT
        || locator.source().schema_variant() != CODEX_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    }
    let SourceAnchor::ProviderNative { namespace, key } = locator.source().anchor() else {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    };
    let TypedKey::Utf8(source_native_session_id) = key else {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    };
    if namespace != CODEX_SOURCE_ANCHOR_NAMESPACE {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    };
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(source_native_session_id.clone()))
        || native_event_key.as_ref() != Some(&TypedKey::U64(*physical_ordinal))
        || *byte_length == 0
        || *byte_length > MAX_HYDRATED_CODEX_RECORD_BYTES
    {
        return Err(CodexSourceBackedErrorV0::InvalidCodexLocator);
    }
    Ok((
        source_native_session_id.clone(),
        *byte_offset,
        *byte_length,
        *physical_ordinal,
    ))
}

fn decode_exact_display_text(
    provider_bytes: &[u8],
    _physical_ordinal: u64,
) -> CodexSourceBackedResultV0<Option<String>> {
    let record = provider_bytes.strip_suffix(b"\n").unwrap_or(provider_bytes);
    let record = record.strip_suffix(b"\r").unwrap_or(record);
    let probe = classify_codex_record(record)?;
    let envelope: Value = serde_json::from_slice(record)?;
    let Some(payload) = envelope.get("payload") else {
        return Ok(None);
    };
    Ok(match source_backed_display_text(&probe, payload) {
        CodexSourceBackedDocumentEligibility::Eligible(text) => Some(text),
        CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay
        | CodexSourceBackedDocumentEligibility::ParserRevisionGap => None,
    })
}
