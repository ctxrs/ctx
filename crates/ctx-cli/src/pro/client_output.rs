use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ctx_history_capture::{
    ImportProfile, OutputAssociations as CaptureOutputAssociations,
    OutputCommandContext as CaptureOutputCommandContext,
    OutputNativeCoordinate as CaptureOutputNativeCoordinate,
    OutputNativeCursor as CaptureOutputNativeCursor,
    OutputObservationKind as CaptureOutputObservationKind, OutputOutcome as CaptureOutputOutcome,
    OutputOutcomeMetadata as CaptureOutputOutcomeMetadata,
    OutputRepositoryContext as CaptureOutputRepositoryContext,
    OutputSourceIdentity as CaptureOutputSourceIdentity,
    OutputSourceLocator as CaptureOutputSourceLocator,
    ProOutputMaterializationPage as CaptureOutputPage,
    ProOutputObservation as CaptureOutputObservation, ProOutputPageResult, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition,
};
use ctx_history_core::database_path;
use ctx_history_store::Store;
use ctx_pro_host_protocol::{
    BeginOutputInventoryRequest, FinishOutputInventoryRequest, GraphState, HelperMessage,
    HostMessage, JournalCheckpoint, ObserveOutputSourceRequest, OutputAssociations,
    OutputCommandContext, OutputInventoryFinished, OutputNativeCoordinate, OutputNativeCursor,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputProgressRequest,
    OutputProgressResult, OutputRepositoryContext, OutputSourceAvailability,
    OutputSourceDisposition, OutputSourceIdentity, OutputSourceLocator,
    ProOutputMaterializationPage, ProOutputObservation, StatusRequest, TransientOutputContent,
    OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
};

use super::{protocol_error, stable_error_code, ProClient, BATCH_TIMEOUT};

struct SharedProClient {
    client: Mutex<ProClient>,
}

impl SharedProClient {
    fn new(client: ProClient) -> Self {
        Self {
            client: Mutex::new(client),
        }
    }

    fn exchange(
        &self,
        message: HostMessage,
        timeout: std::time::Duration,
    ) -> Result<HelperMessage> {
        // Each lane holds the client only for one request/response exchange.
        // The coordinator invokes canonical and output sequentially, so no
        // adapter callback can re-enter this mutex while it is locked.
        self.client
            .lock()
            .map_err(|_| anyhow!("helper_crashed: Pro client lock was poisoned"))?
            .exchange(message, timeout)
    }
}

/// One helper connection and one immutable import profile selected before provider parsing.
///
/// The caller passes `profile()` through the public profiled import entrypoint and calls
/// `finish()` only after the complete source inventory succeeds.
pub(crate) struct ProOutputImport {
    profile: ImportProfile,
    sink: Arc<ClientProOutputSink>,
    expected_ready: Option<(PathBuf, Arc<Mutex<JournalCheckpoint>>)>,
    finished: bool,
}

impl ProOutputImport {
    /// Selects CoreAndPro only when a helper negotiates the complete output capability.
    ///
    /// Import remains a Core operation when Pro is absent, disabled, unlicensed, or
    /// temporarily unavailable; later sink failures likewise never unwind Core commits.
    pub(crate) fn begin_if_available(data_root: &Path) -> Option<Self> {
        Self::begin(data_root).ok()
    }

    fn begin(data_root: &Path) -> Result<Self> {
        let db_path = database_path(data_root.to_path_buf());
        if !db_path.exists() {
            bail!(
                "source_unavailable: ctx Store is not initialized at {}; run ctx setup or ctx import first",
                db_path.display()
            );
        }
        let store = Store::open(&db_path).with_context(|| {
            format!(
                "source_unavailable: open canonical Store {}",
                db_path.display()
            )
        })?;
        let client = ProClient::connect(data_root, &super::nativepath_pro_capabilities())?;
        let client = Arc::new(SharedProClient::new(client));
        let checkpoint =
            super::prepare_nativepath_projection_journal(&store, &mut |message, timeout| {
                client.exchange(message, timeout)
            })?;
        let expected_ready = Arc::new(Mutex::new(checkpoint));
        let output = Self::begin_with_shared_client(
            Arc::clone(&client),
            Some((data_root.to_path_buf(), Arc::clone(&expected_ready))),
        )?;
        let canonical = NativePathCanonicalProAdapter {
            store,
            client,
            expected_ready,
            behind: Arc::clone(&output.sink.behind),
        };
        *output
            .sink
            .canonical
            .lock()
            .map_err(|_| anyhow!("helper_crashed: canonical adapter lock was poisoned"))? =
            Some(canonical);
        Ok(output)
    }

    pub(super) fn begin_with_client(
        client: ProClient,
        expected_ready: Option<(PathBuf, JournalCheckpoint)>,
    ) -> Result<Self> {
        let expected_ready = expected_ready
            .map(|(data_root, checkpoint)| (data_root, Arc::new(Mutex::new(checkpoint))));
        Self::begin_with_shared_client(Arc::new(SharedProClient::new(client)), expected_ready)
    }

    fn begin_with_shared_client(
        client: Arc<SharedProClient>,
        expected_ready: Option<(PathBuf, Arc<Mutex<JournalCheckpoint>>)>,
    ) -> Result<Self> {
        let progress = client.exchange(
            HostMessage::GetOutputProgress(OutputProgressRequest {
                sources: Vec::new(),
            }),
            BATCH_TIMEOUT,
        )?;
        let generation = output_inventory_generation(progress)?;
        let began = match client.exchange(
            HostMessage::BeginOutputInventory(BeginOutputInventoryRequest { generation }),
            BATCH_TIMEOUT,
        )? {
            HelperMessage::OutputInventoryBegan(began) => began,
            HelperMessage::Error(error) => return Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-output-inventory response"),
        };
        began
            .validate()
            .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
        if began.generation != generation {
            bail!("invalid_response: helper began the wrong output inventory generation");
        }
        let sink = Arc::new(ClientProOutputSink {
            client,
            inventory_generation: generation,
            materializer_revision: began.materializer_revision,
            behind: Arc::new(Mutex::new(None)),
            canonical: Mutex::new(None),
        });
        let sink_trait: Arc<dyn ProOutputSink> = sink.clone();
        let profile = ImportProfile::CoreAndPro(sink_trait);
        Ok(Self {
            profile,
            sink,
            expected_ready,
            finished: false,
        })
    }

    pub(crate) fn profile(&self) -> &ImportProfile {
        &self.profile
    }

    pub(crate) fn replay_only_profile(&self) -> ImportProfile {
        let sink: Arc<dyn ProOutputSink> = self.sink.clone();
        ImportProfile::ProReplayOnly(sink)
    }

    pub(crate) fn mark_output_replay_behind(&self, error: &anyhow::Error) {
        self.sink.mark_behind(ProOutputSinkError::new(
            "nativepath_output_replay",
            error.to_string(),
        ));
    }

    /// Advances canonical Pro after one or more NativePath Core groups have
    /// committed. Failure marks only Pro behind; Core remains committed and a
    /// later import retries from the retained journal.
    pub(crate) fn note_core_source_committed(&mut self) {
        self.sink.note_core_source_committed();
    }

    pub(crate) fn finish(mut self) -> Result<OutputInventoryFinished> {
        if let Some(error) = self
            .sink
            .behind
            .lock()
            .map_err(|_| anyhow!("helper_crashed: Pro output sink lock was poisoned"))?
            .clone()
        {
            bail!("{}: {}", error.code, error.message);
        }
        let expected_ready = self
            .expected_ready
            .as_ref()
            .map(|(data_root, checkpoint)| {
                let checkpoint = checkpoint
                    .lock()
                    .map_err(|_| anyhow!("helper_crashed: canonical checkpoint lock was poisoned"))?
                    .clone();
                Ok::<_, anyhow::Error>((data_root.clone(), checkpoint))
            })
            .transpose()?;
        if let Some((data_root, expected)) = expected_ready.as_ref() {
            super::verify_canonical_frontier(data_root, expected)?;
        }
        let response = self.sink.exchange(HostMessage::FinishOutputInventory(
            FinishOutputInventoryRequest {
                generation: self.sink.inventory_generation,
            },
        ))?;
        let finished = match response {
            HelperMessage::OutputInventoryFinished(finished) => finished,
            HelperMessage::Error(error) => return Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-output-inventory response"),
        };
        if finished.generation != self.sink.inventory_generation {
            bail!("invalid_response: helper finished the wrong output inventory generation");
        }
        if let Some((data_root, expected)) = expected_ready.as_ref() {
            let status = match self.sink.exchange(HostMessage::Status(StatusRequest {}))? {
                HelperMessage::Status(status) => status,
                HelperMessage::Error(error) => return Err(protocol_error(error)),
                _ => bail!("invalid_response: helper returned a non-status response"),
            };
            if status.state != GraphState::Ready || status.checkpoint.as_ref() != Some(expected) {
                bail!(
                    "not_materialized: helper did not publish the verified canonical graph after output catch-up"
                );
            }
            super::verify_canonical_frontier(data_root, expected)?;
        }
        self.finished = true;
        Ok(finished)
    }

    pub(crate) fn finish_warning(error: &anyhow::Error) -> String {
        let code = stable_error_code(error).unwrap_or("pro_output_unavailable");
        format!(
            "Core history update succeeded, but Pro output catch-up remains incomplete ({code}); a later import or refresh will retry it"
        )
    }
}

struct NativePathCanonicalProAdapter {
    store: Store,
    client: Arc<SharedProClient>,
    expected_ready: Arc<Mutex<JournalCheckpoint>>,
    behind: Arc<Mutex<Option<ProOutputSinkError>>>,
}

trait CanonicalProProgression {
    fn materialize_current_store_frontier(&mut self) -> Result<()>;
    fn mark_behind(&mut self, error: anyhow::Error);
}

fn note_core_source_committed<C: CanonicalProProgression>(progression: Option<&mut C>) {
    let Some(progression) = progression else {
        return;
    };
    if let Err(error) = progression.materialize_current_store_frontier() {
        progression.mark_behind(error);
    }
}

fn canonical_frontier_needs_sync(
    expected_ready: &JournalCheckpoint,
    target: &JournalCheckpoint,
) -> bool {
    expected_ready != target
}

impl CanonicalProProgression for NativePathCanonicalProAdapter {
    fn materialize_current_store_frontier(&mut self) -> Result<()> {
        let target = self
            .store
            .projection_journal_snapshot(None)
            .context("source_unavailable: freeze current canonical journal frontier")?
            .frozen_through;
        let protocol_target = super::protocol_checkpoint(target.clone());
        let frontier_needs_sync = {
            let expected_ready = self
                .expected_ready
                .lock()
                .map_err(|_| anyhow!("helper_crashed: canonical checkpoint lock was poisoned"))?;
            canonical_frontier_needs_sync(&expected_ready, &protocol_target)
        };
        // Source-level callers may conservatively report changed work that did
        // not add canonical journal records. Do not contact the helper twice
        // for an already-published frontier.
        if !frontier_needs_sync {
            return Ok(());
        }
        super::sync_nativepath_group_through(&self.store, &target, &mut |message, timeout| {
            self.client.exchange(message, timeout)
        })?;
        *self
            .expected_ready
            .lock()
            .map_err(|_| anyhow!("helper_crashed: canonical checkpoint lock was poisoned"))? =
            protocol_target;
        Ok(())
    }

    fn mark_behind(&mut self, error: anyhow::Error) {
        if let Ok(mut behind) = self.behind.lock() {
            behind.get_or_insert_with(|| {
                ProOutputSinkError::new(
                    stable_error_code(&error).unwrap_or("pro_canonical_unavailable"),
                    error.to_string(),
                )
            });
        }
    }
}

impl Drop for ProOutputImport {
    fn drop(&mut self) {
        // An unfinished inventory deliberately remains incomplete. The helper uses that state to
        // invalidate missing-source conclusions after a failed or interrupted Core import.
        let _ = self.finished;
    }
}

fn output_inventory_generation(response: HelperMessage) -> Result<u64> {
    match response {
        HelperMessage::OutputProgress(progress) => next_output_inventory_generation(progress),
        HelperMessage::Error(error)
            if error.class == ctx_pro_host_protocol::ErrorClass::NotMaterialized =>
        {
            Ok(1)
        }
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-output-progress response"),
    }
}

fn next_output_inventory_generation(progress: OutputProgressResult) -> Result<u64> {
    if progress.inventory_generation == 0 {
        return Ok(1);
    }
    if progress.inventory_complete {
        progress.inventory_generation.checked_add(1).ok_or_else(|| {
            anyhow!("invalid_response: helper output inventory generation is exhausted")
        })
    } else {
        Ok(progress.inventory_generation)
    }
}

struct ClientProOutputSink {
    client: Arc<SharedProClient>,
    inventory_generation: u64,
    materializer_revision: String,
    behind: Arc<Mutex<Option<ProOutputSinkError>>>,
    canonical: Mutex<Option<NativePathCanonicalProAdapter>>,
}

impl ClientProOutputSink {
    fn exchange(&self, message: HostMessage) -> Result<HelperMessage> {
        self.client.exchange(message, BATCH_TIMEOUT)
    }

    fn sink_error(error: anyhow::Error) -> ProOutputSinkError {
        ProOutputSinkError::new(
            stable_error_code(&error).unwrap_or("pro_output_unavailable"),
            error.to_string(),
        )
    }

    fn note_core_source_committed(&self) {
        match self.canonical.lock() {
            Ok(mut canonical) => note_core_source_committed(canonical.as_mut()),
            Err(_) => self.mark_behind(ProOutputSinkError::new(
                "helper_crashed",
                "canonical adapter lock was poisoned",
            )),
        }
    }
}

impl ProOutputSink for ClientProOutputSink {
    fn inventory_generation(&self) -> u64 {
        self.inventory_generation
    }

    fn materializer_revision(&self) -> &str {
        &self.materializer_revision
    }

    fn observe_source(
        &self,
        source: &CaptureOutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        let source = protocol_source(source);
        let observed = self
            .exchange(HostMessage::ObserveOutputSource(
                ObserveOutputSourceRequest {
                    generation: self.inventory_generation,
                    source: source.clone(),
                    availability: OutputSourceAvailability::Available,
                },
            ))
            .map_err(Self::sink_error)?;
        match observed {
            HelperMessage::OutputSourceObserved(observed)
                if observed.generation == self.inventory_generation
                    && observed.source == source
                    && observed.availability == OutputSourceAvailability::Available => {}
            HelperMessage::Error(error) => return Err(Self::sink_error(protocol_error(error))),
            _ => {
                return Err(ProOutputSinkError::new(
                    "invalid_response",
                    "helper returned an invalid output-source acknowledgement",
                ));
            }
        }
        let progress = self
            .exchange(HostMessage::GetOutputProgress(OutputProgressRequest {
                sources: vec![source.clone()],
            }))
            .map_err(Self::sink_error)?;
        match progress {
            HelperMessage::OutputProgress(progress) => {
                if progress.inventory_generation != self.inventory_generation
                    || progress.sources.len() > 1
                    || progress
                        .sources
                        .first()
                        .is_some_and(|value| value.source != source)
                {
                    return Err(ProOutputSinkError::new(
                        "invalid_response",
                        "helper returned invalid output progress",
                    ));
                }
                progress
                    .sources
                    .into_iter()
                    .next()
                    .map(capture_progress)
                    .transpose()
            }
            HelperMessage::Error(error) => Err(Self::sink_error(protocol_error(error))),
            _ => Err(ProOutputSinkError::new(
                "invalid_response",
                "helper returned a non-output-progress response",
            )),
        }
    }

    fn materialize_page(
        &self,
        page: CaptureOutputPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        // NativePath providers invoke the output sink only after the matching
        // Core page commits. Advance canonical Pro through that durable
        // frontier first. A canonical failure marks only Pro behind and does
        // not prevent the independent output lane from attempting its page.
        self.note_core_source_committed();
        let response = self
            .exchange(HostMessage::MaterializeOutputPage(protocol_page(page)?))
            .map_err(Self::sink_error)?;
        match response {
            HelperMessage::OutputPageMaterialized(result) => Ok(ProOutputPageResult {
                source_epoch: result.source_epoch,
                committed_cursor: capture_cursor(result.committed_cursor)?,
                accepted_outputs: result.accepted_outputs,
                materialized_facts: result.materialized_facts,
                replayed: result.replayed,
            }),
            HelperMessage::Error(error) => Err(Self::sink_error(protocol_error(error))),
            _ => Err(ProOutputSinkError::new(
                "invalid_response",
                "helper returned a non-output-page response",
            )),
        }
    }

    fn mark_behind(&self, error: ProOutputSinkError) {
        if let Ok(mut behind) = self.behind.lock() {
            behind.get_or_insert(error);
        }
    }
}

fn protocol_source(source: &CaptureOutputSourceIdentity) -> OutputSourceIdentity {
    OutputSourceIdentity {
        provider: source.provider.clone(),
        namespace_id: source.namespace_id.clone(),
        source_id: source.source_id.clone(),
    }
}

fn protocol_cursor(cursor: CaptureOutputNativeCursor) -> OutputNativeCursor {
    OutputNativeCursor {
        version: cursor.version,
        payload_base64: STANDARD.encode(cursor.payload),
    }
}

fn capture_cursor(
    cursor: OutputNativeCursor,
) -> std::result::Result<CaptureOutputNativeCursor, ProOutputSinkError> {
    cursor.validate().map_err(|error| {
        ProOutputSinkError::new(
            "invalid_response",
            format!("invalid output cursor: {}", error.message),
        )
    })?;
    let payload = STANDARD.decode(cursor.payload_base64).map_err(|_| {
        ProOutputSinkError::new(
            "invalid_response",
            "helper returned invalid output cursor base64",
        )
    })?;
    Ok(CaptureOutputNativeCursor {
        version: cursor.version,
        payload,
    })
}

fn capture_progress(
    progress: ctx_pro_host_protocol::OutputSourceProgress,
) -> std::result::Result<ProOutputProgress, ProOutputSinkError> {
    Ok(ProOutputProgress {
        source_epoch: progress.source_epoch,
        observed_revision: progress.observed_revision,
        cursor: progress.cursor.map(capture_cursor).transpose()?,
        parser_revision: progress.parser_revision,
        materializer_revision: progress.materializer_revision,
        terminal: progress.terminal,
    })
}

fn protocol_page(
    page: CaptureOutputPage,
) -> std::result::Result<ProOutputMaterializationPage, ProOutputSinkError> {
    let observations = page
        .observations
        .into_iter()
        .map(protocol_observation)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let page = ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: page.inventory_generation,
        source: protocol_source(&page.source),
        source_epoch: page.source_epoch,
        observed_revision: page.observed_revision,
        parser_revision: page.parser_revision,
        materializer_revision: page.materializer_revision,
        disposition: match page.disposition {
            ProOutputSourceDisposition::AppendOrResume => OutputSourceDisposition::AppendOrResume,
            ProOutputSourceDisposition::NewSource => OutputSourceDisposition::NewSource,
            ProOutputSourceDisposition::Rewrite => OutputSourceDisposition::Rewrite,
        },
        expected_prior_source_epoch: page.expected_prior_source_epoch,
        expected_prior_cursor: page.expected_prior_cursor.map(protocol_cursor),
        next_safe_cursor: protocol_cursor(page.next_safe_cursor),
        terminal: page.terminal,
        observations,
    };
    page.validate().map_err(|error| {
        ProOutputSinkError::new(
            "invalid_request",
            format!("invalid transient output page: {}", error.message),
        )
    })?;
    Ok(page)
}

fn protocol_observation(
    observation: CaptureOutputObservation,
) -> std::result::Result<ProOutputObservation, ProOutputSinkError> {
    let content = TransientOutputContent::from_bytes(&observation.content).ok_or_else(|| {
        ProOutputSinkError::new(
            "pro_output_record_too_large",
            "transient output exceeds the accepted 16 MiB record bound",
        )
    })?;
    Ok(ProOutputObservation {
        kind: match observation.kind {
            CaptureOutputObservationKind::Command => OutputObservationKind::Command,
            CaptureOutputObservationKind::Tool => OutputObservationKind::Tool,
        },
        coordinate: protocol_coordinate(observation.coordinate),
        occurred_at_unix_ms: observation.occurred_at_unix_ms,
        associations: protocol_associations(observation.associations),
        call_id: observation.call_id,
        command: observation.command.map(protocol_command),
        outcome: protocol_outcome(observation.outcome),
        locator: protocol_locator(observation.locator),
        content,
    })
}

fn protocol_coordinate(value: CaptureOutputNativeCoordinate) -> OutputNativeCoordinate {
    OutputNativeCoordinate {
        unit_key: value.unit_key,
        native_sequence: value.native_sequence,
        native_record_id: value.native_record_id,
        source_record_ordinal: value.source_record_ordinal,
        source_record_subrecord_index: value.source_record_subrecord_index,
        byte_start: value.byte_start,
        byte_end_exclusive: value.byte_end_exclusive,
    }
}

fn protocol_associations(value: CaptureOutputAssociations) -> OutputAssociations {
    OutputAssociations {
        direct_session_id: value.direct_session_id,
        root_session_id: value.root_session_id,
        parent_session_id: value.parent_session_id,
        provider_session_id: value.provider_session_id,
        agent_id: value.agent_id,
        repository: value.repository.map(protocol_repository),
    }
}

fn protocol_repository(value: CaptureOutputRepositoryContext) -> OutputRepositoryContext {
    OutputRepositoryContext {
        repository_id: value.repository_id,
        checkout_id: value.checkout_id,
        worktree_id: value.worktree_id,
        object_format: value.object_format,
    }
}

fn protocol_command(value: CaptureOutputCommandContext) -> OutputCommandContext {
    OutputCommandContext {
        tool_name: value.tool_name,
        command: value.command,
        working_directory: value.working_directory,
    }
}

fn protocol_outcome(value: CaptureOutputOutcomeMetadata) -> OutputOutcomeMetadata {
    OutputOutcomeMetadata {
        outcome: match value.outcome {
            CaptureOutputOutcome::Success => OutputOutcome::Success,
            CaptureOutputOutcome::Failure => OutputOutcome::Failure,
            CaptureOutputOutcome::Timeout => OutputOutcome::Timeout,
            CaptureOutputOutcome::Unknown => OutputOutcome::Unknown,
        },
        exit_code: value.exit_code,
        duration_ms: value.duration_ms,
    }
}

fn protocol_locator(value: CaptureOutputSourceLocator) -> OutputSourceLocator {
    OutputSourceLocator {
        version: value.version,
        kind: value.kind,
        payload_base64: STANDARD.encode(value.payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_pro_host_protocol::{ErrorClass, JournalPosition, ProtocolError};

    #[derive(Default)]
    struct TestCanonicalProgression {
        attempts: usize,
        fail: bool,
        behind: Option<String>,
    }

    impl CanonicalProProgression for TestCanonicalProgression {
        fn materialize_current_store_frontier(&mut self) -> Result<()> {
            self.attempts += 1;
            if self.fail {
                bail!("helper_timeout: injected canonical failure");
            }
            Ok(())
        }

        fn mark_behind(&mut self, error: anyhow::Error) {
            self.behind = Some(error.to_string());
        }
    }

    #[test]
    fn first_activation_starts_inventory_when_output_progress_is_absent() {
        let generation = output_inventory_generation(HelperMessage::Error(ProtocolError::new(
            ErrorClass::NotMaterialized,
            "graph does not exist",
        )))
        .expect("first activation generation");

        assert_eq!(generation, 1);
    }

    #[test]
    fn inventory_generation_resumes_incomplete_and_advances_complete_runs() {
        assert_eq!(
            next_output_inventory_generation(OutputProgressResult {
                inventory_generation: 7,
                inventory_complete: false,
                sources: Vec::new(),
            })
            .unwrap(),
            7
        );
        assert_eq!(
            next_output_inventory_generation(OutputProgressResult {
                inventory_generation: 7,
                inventory_complete: true,
                sources: Vec::new(),
            })
            .unwrap(),
            8
        );
        assert_eq!(
            next_output_inventory_generation(OutputProgressResult {
                inventory_generation: 0,
                inventory_complete: false,
                sources: Vec::new(),
            })
            .unwrap(),
            1
        );
    }

    #[test]
    fn finish_failure_warning_is_explicit_and_nonfatal() {
        let warning = ProOutputImport::finish_warning(&anyhow!("helper_timeout"));

        assert!(warning.contains("Core history update succeeded"));
        assert!(warning.contains("Pro output catch-up remains incomplete"));
        assert!(warning.contains("helper_timeout"));
    }

    #[test]
    fn committed_core_progression_is_best_effort_and_marks_only_pro_behind() {
        let mut progression = TestCanonicalProgression {
            fail: true,
            ..TestCanonicalProgression::default()
        };

        note_core_source_committed(Some(&mut progression));

        assert_eq!(progression.attempts, 1);
        assert_eq!(
            progression.behind.as_deref(),
            Some("helper_timeout: injected canonical failure")
        );
    }

    #[test]
    fn unchanged_canonical_frontier_skips_duplicate_progression() {
        let current = JournalCheckpoint {
            position: JournalPosition {
                generation: 7,
                sequence: 11,
            },
            contract_fingerprint: "f".repeat(64),
            cumulative_digest: "d".repeat(64),
        };
        let mut advanced = current.clone();
        advanced.position.sequence += 1;

        assert!(!canonical_frontier_needs_sync(&current, &current));
        assert!(canonical_frontier_needs_sync(&current, &advanced));
    }
}
