use super::*;

pub(super) fn replay_outputs_or_mark_behind(
    store: &Store,
    source: &KiroSource,
    context: &ProviderAdapterContext,
    profile: &ImportProfile,
) {
    let Some(sink) = profile.sink() else {
        return;
    };
    if let Err(error) = replay_outputs(store, source, context, sink.as_ref()) {
        sink.mark_behind(ProOutputSinkError::new(
            "kiro_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    store: &Store,
    source: &KiroSource,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    source.revalidate()?;
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &source.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Kiro output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let core = KiroStoreCursor::decode(committed.provider_cursor())?;
    if core.locator_identity != source.locator_identity
        || core.source_revision != source.source_revision
        || !core.terminal
    {
        return Err(CaptureError::InvalidPayload(
            "Kiro output replay does not match terminal committed Core authority".to_owned(),
        ));
    }
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::KiroCli.as_str().to_owned(),
        namespace_id: source.configured_source_root.display().to_string(),
        source_id: source.locator_identity.clone(),
    };
    let progress = sink
        .observe_source(&output_source)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let mut state =
        KiroOutputState::new(output_source, progress, &core, sink.materializer_revision())?;
    if state.already_terminal {
        return Ok(());
    }
    let expected = state
        .expected_sink_frontier
        .clone()
        .unwrap_or(safe_frontier(&KiroFrontier::initial(source.tables))?);
    let next = safe_frontier(&core.frontier)?;
    let conservative_serialized_bytes = 64_usize
        .saturating_mul(1024)
        .saturating_add(source.locator_identity.len())
        .saturating_add(source.configured_source_root.as_os_str().len())
        .saturating_add(core.source_revision.len())
        .saturating_add(expected.bytes.len())
        .saturating_add(next.bytes.len());
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: core.source_revision,
        parser_revision: KIRO_OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier.take(),
        observations: Vec::new(),
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::KiroCli.as_str(), &source.locator_identity),
        expected,
        next,
        true,
        NativePageAccounting {
            logical_units: 0,
            conservative_serialized_bytes,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Err(failure) = process_pro_replay_only(replay, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "kiro_nativepath_output_page",
            format!("{:?}", failure.output_error),
        ));
    }
    Ok(())
}

struct KiroOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    already_terminal: bool,
}

impl KiroOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        core: &KiroStoreCursor,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
                already_terminal: false,
            });
        };
        let prior = progress
            .cursor
            .as_ref()
            .map(|cursor| {
                if cursor.version != KIRO_NATIVE_CURSOR_VERSION {
                    return Err(CaptureError::InvalidPayload(
                        "Kiro output cursor version is unsupported".to_owned(),
                    ));
                }
                let _: KiroFrontier = serde_json::from_slice(&cursor.payload)?;
                NativeSafeFrontier::new(cursor.version, cursor.payload.clone())
                    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
            })
            .transpose()?;
        let terminal_frontier = safe_frontier(&core.frontier)?;
        let compatible = progress.parser_revision == KIRO_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == materializer_revision
            && progress.observed_revision == core.source_revision
            && prior.as_ref() == Some(&terminal_frontier);
        if compatible && progress.terminal {
            return Ok(Self {
                source,
                source_epoch: progress.source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier: prior,
                disposition: ProOutputSourceDisposition::AppendOrResume,
                already_terminal: true,
            });
        }
        Ok(Self {
            source,
            source_epoch: progress.source_epoch.checked_add(1).ok_or(
                CaptureError::SystemInvariant("Kiro output source epoch overflowed"),
            )?,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: prior,
            disposition: ProOutputSourceDisposition::Rewrite,
            already_terminal: false,
        })
    }
}

fn safe_frontier(frontier: &KiroFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(KIRO_NATIVE_CURSOR_VERSION, serde_json::to_vec(frontier)?)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn kiro_locator(phase: KiroPhase, rowid: i64) -> Result<NativeLocator> {
    let mut bytes = Vec::with_capacity(9);
    bytes.push(phase.tag());
    bytes.extend_from_slice(&((rowid as u64) ^ (1_u64 << 63)).to_be_bytes());
    NativeLocator::new(KIRO_LOCATOR_KIND, bytes)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn estimated_event_bytes(event: &KiroNativeEvent, touches: &[KiroFileTouch]) -> usize {
    let event_bytes = serde_json::to_vec(&json!({
        "payload": event.payload,
        "metadata": event.metadata,
        "cursor": event.cursor,
        "provider_event_hash": event.provider_event_hash,
    }))
    .map_or(usize::MAX, |bytes| bytes.len());
    touches
        .iter()
        .fold(event_bytes.saturating_add(1024), |total, touch| {
            total
                .saturating_add(touch.path.len())
                .saturating_add(touch.old_path.as_deref().map(str::len).unwrap_or_default())
                .saturating_add(
                    serde_json::to_vec(&touch.metadata).map_or(usize::MAX, |bytes| bytes.len()),
                )
                .saturating_add(512)
        })
}

pub(super) fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

pub(super) fn hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}
