use std::collections::{BTreeSet, VecDeque};
use std::io::{self, Write};
use std::sync::{
    atomic::{AtomicUsize, Ordering as AtomicOrdering},
    mpsc::{sync_channel, Receiver, SyncSender},
    Arc, Condvar, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES;
use ctx_history_index::{
    CoreEventPageBudget, CoreSourceEventPagePlan, GenerationManifest, SourceEventCursor,
    StoredCoreRecordJson, VerifiedIndex,
};
use ctx_pro_host_protocol::{
    core_record_digests_from_encoded, ApplyCoreEventDeltaPagesRequest,
    ApplyCoreSourceDeltaPageRequest, BeginCoreMaterializationRequest, Capability,
    ContinueCoreMaterializationRequest, CoreEventDelta, CoreEventDeltaPage, CoreEventReplacement,
    CoreEventState, CoreEventStatePage, CoreEventStatePageRequest, CoreEventTombstone,
    CoreGenerationHead, CoreMaterializationBegan, CoreMaterializationFinalizationPending,
    CoreMaterializationFinished, CoreMaterializationReceipt, CoreMaterializationReceiptIdentity,
    CoreProjectionCurrentness, CoreSourceDelta, CoreSourceDeltaPage, CoreSourceDeltaPageApplied,
    CoreSourceReconciliation, CoreSourceState, ErrorClass, FinishCoreMaterializationRequest,
    HelperMessage, HostMessage, StatusRequest, MAX_CORE_EVENT_DELTA_PAGES,
    MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES, MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
    MAX_CORE_EVENT_DELTA_PAGE_ITEMS, MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES,
    MAX_CORE_EVENT_STATE_PAGE_ITEMS, MAX_CORE_SOURCE_DELTA_PAGE_ITEMS,
    MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES, MAX_CORE_SOURCE_STATES,
};
#[cfg(test)]
use ctx_pro_host_protocol::{
    core_record_sha256, CoreMaterializationFinalizationPhase,
    CoreMaterializationFinalizationProgress, CoreSourceRemoval,
};
use serde::Serialize;

use super::*;
#[cfg(test)]
use crate::pro::core_worker_budget::{
    core_launch_product_budget, worker_selection_for_test, CoreLaunchProductBudget,
    MAX_HELPER_PREPARATION_WORKERS,
};
use crate::pro::core_worker_budget::{CoreWorkerLaunchSelection, MAX_CORE_PREFETCH_WORKERS};
#[cfg(test)]
use ctx_pro_host_protocol::JournalFinishActivity;

// Complete content remains capped at 16 MiB. JSON escaping can expand one
// otherwise-valid Core record, so encoded paging admits Core's validated
// singleton maximum while the protocol's larger wire bound covers the
// envelope.
const MAX_CORE_RECORD_PAGE_ENCODED_PAYLOAD_BYTES: usize = MAX_ENCODED_CORE_RECORD_BYTES;
const CORE_RECORD_PAGE_BUDGET: CoreEventPageBudget = CoreEventPageBudget::new(
    MAX_CORE_RECORD_PAGE_ENCODED_PAYLOAD_BYTES,
    MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
);
#[path = "core_materialization_feed/batching.rs"]
mod batching;
#[path = "core_materialization_feed/ordered_prefetch.rs"]
mod ordered_prefetch;

use batching::*;
use ordered_prefetch::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreFeedMode {
    Fresh,
    PartialResume,
    CurrentReplay,
}

#[derive(Debug, Clone)]
pub(super) struct CoreMaterializationSyncReport {
    pub(super) receipt: CoreMaterializationReceipt,
    pub(super) helper_artifact_sha256: String,
    #[cfg(test)]
    pub(super) changed_sources: u64,
    #[cfg(test)]
    pub(super) removed_sources: u64,
    #[cfg(test)]
    pub(super) event_delta_pages: u64,
    #[cfg(test)]
    pub(super) event_mutations: u64,
    #[cfg(test)]
    prefetch: CorePrefetchInstrumentationSnapshot,
    pub(super) replayed: bool,
}

#[derive(Debug, Clone)]
pub(super) enum CoreMaterializationSyncProgress {
    Finished(CoreMaterializationSyncReport),
    FinalizationPending(CoreMaterializationFinalizationPending),
}

#[derive(Debug)]
enum CoreMaterializationFinalizationStep {
    Finished(CoreMaterializationFinished),
    Pending(CoreMaterializationFinalizationPending),
}

trait CoreMaterializationConsumer {
    fn status(&mut self, request: StatusRequest) -> Result<ctx_pro_host_protocol::StatusResult>;

    fn begin(
        &mut self,
        request: BeginCoreMaterializationRequest,
    ) -> Result<CoreMaterializationBegan>;

    fn apply_source_delta(
        &mut self,
        request: ApplyCoreSourceDeltaPageRequest,
    ) -> Result<CoreSourceDeltaPageApplied>;

    fn event_states(&mut self, request: CoreEventStatePageRequest) -> Result<CoreEventStatePage>;

    fn apply_event_delta_pages(&mut self, pages: Vec<CoreEventDeltaPage>) -> Result<()>;

    fn apply_prepared_event_delta_pages(
        &mut self,
        request: PreparedEventDeltaPagesRequest,
    ) -> Result<()> {
        self.apply_event_delta_pages(request.into_typed_pages())
    }

    fn finish(
        &mut self,
        request: FinishCoreMaterializationRequest,
    ) -> Result<CoreMaterializationFinalizationStep>;

    fn continue_finalization(
        &mut self,
        request: ContinueCoreMaterializationRequest,
    ) -> Result<CoreMaterializationFinalizationStep>;
}

struct ProtocolCoreMaterializationConsumer {
    client: ProClient,
}

impl ProtocolCoreMaterializationConsumer {
    fn exchange(&mut self, message: HostMessage) -> Result<HelperMessage> {
        self.client.exchange(message, BATCH_TIMEOUT)
    }
}

fn map_core_finalization_response(
    message: HelperMessage,
) -> Result<CoreMaterializationFinalizationStep> {
    match message {
        HelperMessage::CoreMaterializationFinished(response) => {
            Ok(CoreMaterializationFinalizationStep::Finished(response))
        }
        HelperMessage::CoreMaterializationFinalizationPending(response) => {
            Ok(CoreMaterializationFinalizationStep::Pending(response))
        }
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-Core-finalization response"),
    }
}

impl ProClient {
    fn exchange_prepared_core_event_delta_pages(
        &mut self,
        request: &PreparedEventDeltaPagesRequest,
        timeout: Duration,
    ) -> Result<HelperMessage> {
        self.exchange_with_frame_writer(timeout, |stdin, sequence, request_id| {
            request
                .write_frame(stdin, sequence, request_id)
                .context("helper_crashed: write framed request")
        })
    }
}

impl CoreMaterializationConsumer for ProtocolCoreMaterializationConsumer {
    fn status(&mut self, request: StatusRequest) -> Result<ctx_pro_host_protocol::StatusResult> {
        match self.exchange(HostMessage::Status(request))? {
            HelperMessage::Status(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-status response"),
        }
    }

    fn begin(
        &mut self,
        request: BeginCoreMaterializationRequest,
    ) -> Result<CoreMaterializationBegan> {
        match self.exchange(HostMessage::BeginCoreMaterialization(request))? {
            HelperMessage::CoreMaterializationBegan(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-begin response"),
        }
    }

    fn apply_source_delta(
        &mut self,
        request: ApplyCoreSourceDeltaPageRequest,
    ) -> Result<CoreSourceDeltaPageApplied> {
        match self.exchange(HostMessage::ApplyCoreSourceDeltaPage(request))? {
            HelperMessage::CoreSourceDeltaPageApplied(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-delta response"),
        }
    }

    fn event_states(&mut self, request: CoreEventStatePageRequest) -> Result<CoreEventStatePage> {
        match self.exchange(HostMessage::CoreEventStatePage(request))? {
            HelperMessage::CoreEventStatePage(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-event-state response"),
        }
    }

    fn apply_event_delta_pages(&mut self, pages: Vec<CoreEventDeltaPage>) -> Result<()> {
        apply_batched_event_delta_pages_with(pages, &mut |message, remaining| {
            self.client.exchange_borrowed(message, remaining)
        })
    }

    fn apply_prepared_event_delta_pages(
        &mut self,
        request: PreparedEventDeltaPagesRequest,
    ) -> Result<()> {
        apply_prepared_batched_event_delta_pages_with(request, &mut |request, remaining| {
            self.client
                .exchange_prepared_core_event_delta_pages(request, remaining)
        })
    }

    fn finish(
        &mut self,
        request: FinishCoreMaterializationRequest,
    ) -> Result<CoreMaterializationFinalizationStep> {
        map_core_finalization_response(
            self.exchange(HostMessage::FinishCoreMaterialization(request))?,
        )
    }

    fn continue_finalization(
        &mut self,
        request: ContinueCoreMaterializationRequest,
    ) -> Result<CoreMaterializationFinalizationStep> {
        map_core_finalization_response(
            self.exchange(HostMessage::ContinueCoreMaterialization(request))?,
        )
    }
}

pub(super) fn sync_generation_pinned_core(
    data_root: &Path,
    index: &VerifiedIndex,
) -> Result<CoreMaterializationSyncProgress> {
    let selection = CoreWorkerLaunchSelection::from_runtime();
    let required = BTreeSet::from([Capability::Status, Capability::CoreMaterialization]);
    let client = ProClient::connect(data_root, &required)?;
    let helper_artifact_sha256 = client.helper_artifact_sha256()?.to_owned();
    let mut consumer = ProtocolCoreMaterializationConsumer { client };
    let status = consumer.status(StatusRequest {
        requested_core_generation_id: Some(index.generation_id().to_owned()),
    })?;
    status
        .validate()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    let mut progress = if status.currentness == CoreProjectionCurrentness::Finalizing {
        continue_core_finalization(index, &status, &mut consumer, selection)?
    } else {
        sync_core_feed_progress_with_launch(
            index,
            status.core_receipt.as_ref(),
            &mut consumer,
            selection,
        )?
    };
    if let CoreMaterializationSyncProgress::Finished(report) = &mut progress {
        report.helper_artifact_sha256 = helper_artifact_sha256;
    }
    Ok(progress)
}

#[cfg(test)]
fn sync_core_feed<C: CoreMaterializationConsumer>(
    index: &VerifiedIndex,
    prior_receipt: Option<&CoreMaterializationReceipt>,
    consumer: &mut C,
) -> Result<CoreMaterializationSyncReport> {
    let selection = CoreWorkerLaunchSelection::from_runtime();
    sync_core_feed_with_launch(index, prior_receipt, consumer, selection)
}

#[cfg(test)]
fn sync_core_feed_progress<C: CoreMaterializationConsumer>(
    index: &VerifiedIndex,
    prior_receipt: Option<&CoreMaterializationReceipt>,
    consumer: &mut C,
) -> Result<CoreMaterializationSyncProgress> {
    sync_core_feed_progress_with_launch(
        index,
        prior_receipt,
        consumer,
        CoreWorkerLaunchSelection::from_runtime(),
    )
}

#[cfg(test)]
fn sync_core_feed_with_options<C: CoreMaterializationConsumer>(
    index: &VerifiedIndex,
    prior_receipt: Option<&CoreMaterializationReceipt>,
    consumer: &mut C,
    options: CoreFeedExecutionOptions,
) -> Result<CoreMaterializationSyncReport> {
    sync_core_feed_with_launch(
        index,
        prior_receipt,
        consumer,
        CoreWorkerLaunchSelection::explicit_test(options.prefetch_parallelism),
    )
}

#[cfg(test)]
fn sync_core_feed_with_launch<C: CoreMaterializationConsumer>(
    index: &VerifiedIndex,
    prior_receipt: Option<&CoreMaterializationReceipt>,
    consumer: &mut C,
    selection: CoreWorkerLaunchSelection,
) -> Result<CoreMaterializationSyncReport> {
    match sync_core_feed_progress_with_launch(index, prior_receipt, consumer, selection)? {
        CoreMaterializationSyncProgress::Finished(report) => Ok(report),
        CoreMaterializationSyncProgress::FinalizationPending(_) => {
            bail!("finalization_pending: test caller expected a terminal Core receipt")
        }
    }
}

fn sync_core_feed_progress_with_launch<C: CoreMaterializationConsumer>(
    index: &VerifiedIndex,
    prior_receipt: Option<&CoreMaterializationReceipt>,
    consumer: &mut C,
    selection: CoreWorkerLaunchSelection,
) -> Result<CoreMaterializationSyncProgress> {
    match sync_core_feed_attempt_with_launch(index, prior_receipt, consumer, selection) {
        Err(error) if crate::pro::stable_error_code(&error) == Some("needs_rebuild") => {
            sync_core_feed_attempt_with_launch(index, prior_receipt, consumer, selection)
        }
        result => result,
    }
}

fn sync_core_feed_attempt_with_launch<C: CoreMaterializationConsumer>(
    index: &VerifiedIndex,
    prior_receipt: Option<&CoreMaterializationReceipt>,
    consumer: &mut C,
    selection: CoreWorkerLaunchSelection,
) -> Result<CoreMaterializationSyncProgress> {
    let options = selection.execution_options();
    let credits = Arc::new(EncodedPageCredits::new(CORE_PREFETCH_ENCODED_BYTE_BUDGET));
    let instrumentation = Arc::new(CorePrefetchInstrumentation::default());
    let sources = core_source_states(index.manifest())?;
    let head = core_generation_head(index, &sources)?;
    if head.core_generation_id != index.generation_id() {
        bail!(
            "core_generation_mismatch: generation head {} does not match pinned Core {}",
            head.core_generation_id,
            index.generation_id()
        );
    }
    if let Some(receipt) = prior_receipt {
        receipt
            .validate()
            .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    }
    let expected_prior_receipt = prior_receipt
        .map(CoreMaterializationReceiptIdentity::from_receipt)
        .transpose()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    let begin = BeginCoreMaterializationRequest {
        head: head.clone(),
        expected_prior_receipt: expected_prior_receipt.clone(),
    };
    let begin_identity = begin
        .acknowledgement_identity()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let began = consumer.begin(begin)?;
    began
        .validate_for_identity(&begin_identity)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;

    let feed_mode = if began.replayed {
        let replay_status = consumer.status(StatusRequest {
            requested_core_generation_id: Some(head.core_generation_id.clone()),
        })?;
        replay_status
            .validate()
            .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
        replayed_core_feed_mode(
            &head,
            &began,
            expected_prior_receipt.as_ref(),
            &replay_status,
        )?
    } else {
        CoreFeedMode::Fresh
    };

    let mut source_delta_pages = 0_u32;
    let mut changed_sources = 0_u32;
    let mut removed_sources = 0_u32;
    let mut event_mutations = 0_u64;
    let mut event_delta_pages = 0_u64;

    if feed_mode != CoreFeedMode::CurrentReplay {
        let maximum_reconciliations = sources
            .len()
            .checked_add(MAX_CORE_SOURCE_STATES)
            .ok_or_else(|| anyhow!("invalid_response: source reconciliation bound overflowed"))?;
        let deltas = core_snapshot_deltas(&sources);
        let delta_pages =
            build_delta_pages(&began.materialization_id, index.generation_id(), deltas)?;
        source_delta_pages = u32::try_from(delta_pages.len())
            .map_err(|_| anyhow!("invalid_request: Core delta page count overflowed"))?;
        let mut next_materialize_index = 0_u32;
        let mut reconciled_source_ids = BTreeSet::new();
        let mut reconcile_sources = Vec::new();
        for page in delta_pages {
            let mut acknowledgement_page_index = 0_u32;
            loop {
                let request = ApplyCoreSourceDeltaPageRequest {
                    page: page.clone(),
                    acknowledgement_page_index,
                };
                request
                    .validate()
                    .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
                let acknowledgement_identity = request.acknowledgement_identity();
                let applied = consumer.apply_source_delta(request)?;
                applied
                    .validate_for_identity(&acknowledgement_identity)
                    .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
                changed_sources = changed_sources
                    .checked_add(applied.changed_sources)
                    .ok_or_else(|| anyhow!("invalid_response: changed-source count overflowed"))?;
                removed_sources = removed_sources
                    .checked_add(applied.removed_sources)
                    .ok_or_else(|| anyhow!("invalid_response: removal count overflowed"))?;
                let acknowledgement_terminal = applied.acknowledgement_terminal;
                for reconciliation in applied.reconcile_sources {
                    if reconciliation.materialize_index != next_materialize_index {
                        bail!("invalid_response: Core reconciliation indices are not contiguous");
                    }
                    next_materialize_index =
                        next_materialize_index.checked_add(1).ok_or_else(|| {
                            anyhow!("invalid_response: Core reconciliation index overflowed")
                        })?;
                    let source_id = reconciliation.delta.source().identity().digest();
                    if !reconciled_source_ids.insert(source_id) {
                        bail!(
                            "invalid_response: Core reconciliations repeat a stable source identity"
                        );
                    }
                    let current_source = sources
                        .binary_search_by_key(&source_id, |state| state.source.identity().digest())
                        .ok()
                        .map(|index| &sources[index]);
                    match &reconciliation.delta {
                        CoreSourceDelta::Present(state) => {
                            if current_source != Some(state) {
                                bail!(
                                    "invalid_response: Core reconciliation carries a stale current source"
                                );
                            }
                        }
                        CoreSourceDelta::Removed(_) => {
                            if current_source.is_some() {
                                bail!(
                                    "invalid_response: Core reconciliation removes a current source"
                                );
                            }
                        }
                    }
                    if reconcile_sources.len() >= maximum_reconciliations {
                        bail!(
                            "invalid_response: Core reconciliations exceed current and prior source bounds"
                        );
                    }
                    reconcile_sources.push(reconciliation);
                }
                if acknowledgement_terminal {
                    break;
                }
                acknowledgement_page_index =
                    acknowledgement_page_index.checked_add(1).ok_or_else(|| {
                        anyhow!("invalid_response: Core acknowledgement page index overflowed")
                    })?;
            }
        }
        let report = reconcile_ordered_source_events(
            index,
            &began.materialization_id,
            reconcile_sources,
            consumer,
            OrderedReconciliationOptions {
                prefetch_parallelism: options.prefetch_parallelism,
                exchange_mode: if feed_mode == CoreFeedMode::PartialResume {
                    EventDeltaExchangeMode::OnePagePerExchange
                } else {
                    EventDeltaExchangeMode::Normal
                },
            },
            &credits,
            &instrumentation,
        )?;
        event_delta_pages = report.pages;
        event_mutations = report.mutations;
    }

    let finish = FinishCoreMaterializationRequest {
        materialization_id: began.materialization_id,
        head: head.clone(),
        expected_prior_receipt,
        source_delta_pages,
        changed_sources,
        removed_sources,
        event_delta_pages: u32::try_from(event_delta_pages)
            .map_err(|_| anyhow!("invalid_request: Core event delta page count overflowed"))?,
        event_mutations,
    };
    finish
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let (encoded_credit_final_bytes, encoded_credit_high_water_bytes) = credits.snapshot()?;
    if encoded_credit_final_bytes != 0 {
        bail!("internal: Core prefetch credits remained live after reconciliation");
    }
    #[cfg(not(test))]
    let _ = encoded_credit_high_water_bytes;

    match consumer.finish(finish.clone())? {
        CoreMaterializationFinalizationStep::Pending(pending) => {
            pending
                .validate_for_finish(&finish)
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            Ok(CoreMaterializationSyncProgress::FinalizationPending(
                pending,
            ))
        }
        CoreMaterializationFinalizationStep::Finished(finished) => {
            validate_finished_core(
                &head,
                &finished,
                Some(&began.materializer_revision),
                consumer,
                selection,
            )?;
            Ok(CoreMaterializationSyncProgress::Finished(
                CoreMaterializationSyncReport {
                    receipt: finished.receipt,
                    helper_artifact_sha256: String::new(),
                    #[cfg(test)]
                    changed_sources: u64::from(changed_sources),
                    #[cfg(test)]
                    removed_sources: u64::from(removed_sources),
                    #[cfg(test)]
                    event_delta_pages,
                    #[cfg(test)]
                    event_mutations,
                    #[cfg(test)]
                    prefetch: instrumentation
                        .snapshot(encoded_credit_high_water_bytes, encoded_credit_final_bytes),
                    replayed: began.replayed || finished.replayed,
                },
            ))
        }
    }
}

fn continue_core_finalization<C: CoreMaterializationConsumer>(
    index: &VerifiedIndex,
    status: &ctx_pro_host_protocol::StatusResult,
    consumer: &mut C,
    selection: CoreWorkerLaunchSelection,
) -> Result<CoreMaterializationSyncProgress> {
    let progress = status.finalization_progress.clone().ok_or_else(|| {
        anyhow!("invalid_response: finalizing Core status omitted durable progress")
    })?;
    if progress.core_generation_id != index.generation_id() {
        bail!("invalid_response: finalizing Core status belongs to a different generation");
    }
    let request = ContinueCoreMaterializationRequest {
        expected_progress: progress,
    };
    request
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    match consumer.continue_finalization(request.clone())? {
        CoreMaterializationFinalizationStep::Pending(pending) => {
            pending
                .validate_for_continue(&request)
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            Ok(CoreMaterializationSyncProgress::FinalizationPending(
                pending,
            ))
        }
        CoreMaterializationFinalizationStep::Finished(finished) => {
            let sources = core_source_states(index.manifest())?;
            let head = core_generation_head(index, &sources)?;
            validate_finished_core(&head, &finished, None, consumer, selection)?;
            Ok(CoreMaterializationSyncProgress::Finished(
                CoreMaterializationSyncReport {
                    receipt: finished.receipt,
                    helper_artifact_sha256: String::new(),
                    #[cfg(test)]
                    changed_sources: 0,
                    #[cfg(test)]
                    removed_sources: 0,
                    #[cfg(test)]
                    event_delta_pages: 0,
                    #[cfg(test)]
                    event_mutations: 0,
                    #[cfg(test)]
                    prefetch: CorePrefetchInstrumentationSnapshot::default(),
                    replayed: finished.replayed,
                },
            ))
        }
    }
}

fn validate_finished_core<C: CoreMaterializationConsumer>(
    head: &CoreGenerationHead,
    finished: &CoreMaterializationFinished,
    expected_materializer_revision: Option<&str>,
    consumer: &mut C,
    selection: CoreWorkerLaunchSelection,
) -> Result<()> {
    finished
        .receipt
        .validate_for_head(head)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    if expected_materializer_revision
        .is_some_and(|revision| finished.receipt.materializer_revision != revision)
    {
        bail!("invalid_response: terminal Core receipt changed materializer revision");
    }
    let post_finish_status = consumer.status(StatusRequest {
        requested_core_generation_id: Some(head.core_generation_id.clone()),
    })?;
    post_finish_status
        .validate()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    if post_finish_status.currentness != CoreProjectionCurrentness::Current
        || post_finish_status.core_receipt.as_ref() != Some(&finished.receipt)
    {
        bail!("invalid_response: post-finish status did not expose the terminal Core receipt");
    }
    let _finish_activity = &post_finish_status
        .storage_evidence
        .as_ref()
        .ok_or_else(|| anyhow!("invalid_response: post-finish status omitted storage evidence"))?
        .journal_finish_activity;
    selection.validate_observed_helper_peak(post_finish_status.core_preparation_peak_workers)
}

fn replayed_core_feed_mode(
    head: &CoreGenerationHead,
    began: &CoreMaterializationBegan,
    expected_prior_receipt: Option<&CoreMaterializationReceiptIdentity>,
    status: &ctx_pro_host_protocol::StatusResult,
) -> Result<CoreFeedMode> {
    if status.requested_core_generation_id.as_deref() != Some(&head.core_generation_id) {
        bail!("invalid_response: replayed Core status did not echo the requested generation");
    }

    match status.currentness {
        CoreProjectionCurrentness::Partial => {
            let status_prior_receipt = status
                .core_receipt
                .as_ref()
                .map(CoreMaterializationReceiptIdentity::from_receipt)
                .transpose()
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            if status_prior_receipt.as_ref() != expected_prior_receipt {
                bail!(
                    "invalid_response: partial Core materialization prior receipt does not match its begin request"
                );
            }
            Ok(CoreFeedMode::PartialResume)
        }
        CoreProjectionCurrentness::Current => {
            let receipt = status.core_receipt.as_ref().ok_or_else(|| {
                anyhow!("invalid_response: current Core replay omitted its terminal receipt")
            })?;
            receipt
                .validate_for_head(head)
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            if receipt.materializer_revision != began.materializer_revision {
                bail!(
                    "invalid_response: current Core replay receipt changed materializer revision"
                );
            }
            Ok(CoreFeedMode::CurrentReplay)
        }
        currentness => bail!(
            "invalid_response: replayed Core materialization reported contradictory {currentness:?} status"
        ),
    }
}

fn core_source_states(manifest: &GenerationManifest) -> Result<Vec<CoreSourceState>> {
    let mut states = manifest
        .sources
        .iter()
        .zip(&manifest.core_record_aggregates)
        .map(|(source, aggregate)| {
            Ok(CoreSourceState {
                source: source.observation().source().clone(),
                core_record_accumulator: aggregate.core_record_accumulator().to_owned(),
                event_count: source.counts().indexed_documents,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    states.sort_by_key(|state| state.source.identity().digest());
    for pair in states.windows(2) {
        if pair[0].source.identity().digest() >= pair[1].source.identity().digest() {
            bail!("invalid_request: Core sources are not unique by stable identity");
        }
    }
    Ok(states)
}

fn core_generation_head(
    index: &VerifiedIndex,
    sources: &[CoreSourceState],
) -> Result<CoreGenerationHead> {
    core_generation_head_from_manifest(index.manifest(), index.generation_id(), sources)
}

fn core_generation_head_from_manifest(
    manifest: &GenerationManifest,
    generation_id: &str,
    sources: &[CoreSourceState],
) -> Result<CoreGenerationHead> {
    CoreGenerationHead::new(
        generation_id,
        manifest.manifest_version,
        manifest.identity_version,
        manifest.core_record_contract_fingerprint.clone(),
        manifest.lexical_schema_version,
        manifest.lexical_analyzer_version,
        manifest.policy_schema_hash.clone(),
        sources,
    )
    .map_err(|error| anyhow!("invalid_request: {}", error.message))
}

fn core_snapshot_deltas(sources: &[CoreSourceState]) -> Vec<CoreSourceDelta> {
    sources
        .iter()
        .cloned()
        .map(CoreSourceDelta::Present)
        .collect()
}

#[derive(Debug, thiserror::Error)]
enum CoreSourceDeltaPageBuildError {
    #[error("invalid_request: one Core source delta exceeds its wire bound")]
    OversizedSingleton,
    #[error("invalid_request: Core delta page index overflowed")]
    PageIndexOverflow,
    #[error("invalid_request: Core source delta page byte accounting overflowed")]
    ByteCountOverflow,
    #[error("invalid_request: Core source delta page encoding failed")]
    Encoding(#[source] serde_json::Error),
    #[error("invalid_request: {0}")]
    InvalidPage(String),
}

fn build_delta_pages(
    materialization_id: &str,
    generation_id: &str,
    deltas: Vec<CoreSourceDelta>,
) -> Result<Vec<CoreSourceDeltaPage>, CoreSourceDeltaPageBuildError> {
    build_delta_pages_with_wire_bound(
        materialization_id,
        generation_id,
        deltas,
        MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES,
    )
}

fn build_delta_pages_with_wire_bound(
    materialization_id: &str,
    generation_id: &str,
    deltas: Vec<CoreSourceDelta>,
    maximum_wire_bytes: usize,
) -> Result<Vec<CoreSourceDeltaPage>, CoreSourceDeltaPageBuildError> {
    if deltas.is_empty() {
        return CoreSourceDeltaPage::new(materialization_id, generation_id, 0, true, Vec::new())
            .map(|page| vec![page])
            .map_err(|error| CoreSourceDeltaPageBuildError::InvalidPage(error.message));
    }

    let mut pages = Vec::new();
    let mut page_index = 0_u32;
    let mut current = Vec::with_capacity(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS);
    let mut encoded_delta_items_bytes = 0_usize;
    let mut empty_nonterminal_wire_bytes =
        empty_source_delta_page_wire_bytes(materialization_id, generation_id, page_index, false)?;
    let mut remaining = deltas.into_iter().peekable();

    // A populated page is exactly its empty envelope plus each independently
    // encoded delta and the intervening commas. Charge every delta once, but
    // rebuild the tiny envelope whenever page_index or terminal changes.
    while let Some(delta) = remaining.next() {
        let terminal = remaining.peek().is_none();
        let encoded_delta_bytes = encoded_json_len(&delta)?;
        let candidate_delta_items_bytes = encoded_delta_items_bytes
            .checked_add(usize::from(!current.is_empty()))
            .and_then(|bytes| bytes.checked_add(encoded_delta_bytes))
            .ok_or(CoreSourceDeltaPageBuildError::ByteCountOverflow)?;
        let empty_wire_bytes = if terminal {
            empty_source_delta_page_wire_bytes(materialization_id, generation_id, page_index, true)?
        } else {
            empty_nonterminal_wire_bytes
        };
        let candidate_wire_bytes = empty_wire_bytes
            .checked_add(candidate_delta_items_bytes)
            .ok_or(CoreSourceDeltaPageBuildError::ByteCountOverflow)?;

        if current.len() == MAX_CORE_SOURCE_DELTA_PAGE_ITEMS
            || candidate_wire_bytes > maximum_wire_bytes
        {
            if current.is_empty() {
                return Err(CoreSourceDeltaPageBuildError::OversizedSingleton);
            }
            pages.push(validated_source_delta_page(
                materialization_id,
                generation_id,
                page_index,
                false,
                std::mem::replace(
                    &mut current,
                    Vec::with_capacity(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS),
                ),
            )?);
            page_index = page_index
                .checked_add(1)
                .ok_or(CoreSourceDeltaPageBuildError::PageIndexOverflow)?;
            empty_nonterminal_wire_bytes = empty_source_delta_page_wire_bytes(
                materialization_id,
                generation_id,
                page_index,
                false,
            )?;
            let singleton_empty_wire_bytes = if terminal {
                empty_source_delta_page_wire_bytes(
                    materialization_id,
                    generation_id,
                    page_index,
                    true,
                )?
            } else {
                empty_nonterminal_wire_bytes
            };
            if singleton_empty_wire_bytes
                .checked_add(encoded_delta_bytes)
                .is_none_or(|bytes| bytes > maximum_wire_bytes)
            {
                return Err(CoreSourceDeltaPageBuildError::OversizedSingleton);
            }
            encoded_delta_items_bytes = encoded_delta_bytes;
            current.push(delta);
        } else {
            encoded_delta_items_bytes = candidate_delta_items_bytes;
            current.push(delta);
        }
    }

    pages.push(validated_source_delta_page(
        materialization_id,
        generation_id,
        page_index,
        true,
        current,
    )?);
    Ok(pages)
}

fn validated_source_delta_page(
    materialization_id: &str,
    generation_id: &str,
    page_index: u32,
    terminal: bool,
    deltas: Vec<CoreSourceDelta>,
) -> Result<CoreSourceDeltaPage, CoreSourceDeltaPageBuildError> {
    CoreSourceDeltaPage::new(
        materialization_id,
        generation_id,
        page_index,
        terminal,
        deltas,
    )
    .map_err(|error| CoreSourceDeltaPageBuildError::InvalidPage(error.message))
}

fn empty_source_delta_page_wire_bytes(
    materialization_id: &str,
    generation_id: &str,
    page_index: u32,
    terminal: bool,
) -> Result<usize, CoreSourceDeltaPageBuildError> {
    encoded_json_len(&CoreSourceDeltaPage {
        materialization_id: materialization_id.to_owned(),
        core_generation_id: generation_id.to_owned(),
        page_index,
        terminal,
        deltas: Vec::new(),
    })
}

fn encoded_json_len<T: Serialize + ?Sized>(
    value: &T,
) -> Result<usize, CoreSourceDeltaPageBuildError> {
    #[derive(Default)]
    struct Counter(usize);

    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("encoded length overflowed"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut counter = Counter::default();
    serde_json::to_writer(&mut counter, value).map_err(CoreSourceDeltaPageBuildError::Encoding)?;
    Ok(counter.0)
}

#[derive(Debug, Clone, Copy)]
struct EventReconciliationReport {
    pages: u64,
    mutations: u64,
}

fn reconcile_source_events<C: CoreMaterializationConsumer>(
    generation_id: &str,
    materialization_id: &str,
    reconciliation: CoreSourceReconciliation,
    current_pages: &mut CurrentPageStream<'_>,
    consumer: &mut C,
    pending_batch: &mut EventDeltaPageBatchBuilder,
    event_delta_exchange_mode: EventDeltaExchangeMode,
) -> Result<EventReconciliationReport> {
    let mut state_after = None;
    let mut state_page_index = 0_u32;
    let mut state_terminal = false;
    let mut states = VecDeque::<CoreEventState>::new();
    let mut current_terminal = current_pages.initially_terminal();
    let mut current = VecDeque::<PreparedCurrentRecord>::new();
    let mut current_credit = None;
    let mut event_page_index = 0_u32;
    let mut pending = EventDeltaPageBuilder::new(
        materialization_id,
        generation_id,
        &reconciliation,
        event_page_index,
    )?;
    let mut pages = 0_u64;
    let mut mutations = 0_u64;

    loop {
        if states.is_empty() && !state_terminal {
            let request = CoreEventStatePageRequest {
                materialization_id: materialization_id.to_owned(),
                core_generation_id: generation_id.to_owned(),
                reconciliation: reconciliation.clone(),
                page_index: state_page_index,
                after_event_id: state_after,
                maximum_items: u32::try_from(MAX_CORE_EVENT_STATE_PAGE_ITEMS)
                    .map_err(|_| anyhow!("invalid_request: Core event state limit overflowed"))?,
            };
            request
                .validate()
                .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
            let response = consumer.event_states(request.clone())?;
            response
                .validate_for(&request)
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            state_terminal = response.terminal;
            if let Some(last) = response.states.last() {
                state_after = Some(last.event_id);
            }
            states.extend(response.states);
            state_page_index = state_page_index
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid_request: Core event state page overflowed"))?;
        }
        if current.is_empty() {
            current_credit = None;
            if !current_terminal {
                let PreparedCurrentPage {
                    records,
                    terminal,
                    _encoded_credit,
                } = current_pages.next_page()?;
                current = records.into();
                current_terminal = terminal;
                current_credit = Some(_encoded_credit);
            }
        }

        let delta = match (states.front(), current.front()) {
            (None, None) if state_terminal && current_terminal => break,
            (None, None) => continue,
            (Some(state), None) => {
                let state = state.clone();
                states.pop_front();
                Some(PreparedEventDelta::tombstoned(CoreEventTombstone {
                    event_id: state.event_id,
                    prior_core_record_sha256: state.core_record_sha256,
                }))
            }
            (None, Some(_)) => {
                Some(PreparedEventDelta::added(current.pop_front().ok_or_else(
                    || anyhow!("internal: missing current Core event"),
                )?))
            }
            (Some(state), Some(record)) => {
                match state
                    .event_id
                    .digest()
                    .cmp(&record.record.event_id.digest())
                {
                    std::cmp::Ordering::Less => {
                        let state = states
                            .pop_front()
                            .ok_or_else(|| anyhow!("internal: missing prior Core event state"))?;
                        Some(PreparedEventDelta::tombstoned(CoreEventTombstone {
                            event_id: state.event_id,
                            prior_core_record_sha256: state.core_record_sha256,
                        }))
                    }
                    std::cmp::Ordering::Greater => {
                        Some(PreparedEventDelta::added(current.pop_front().ok_or_else(
                            || anyhow!("internal: missing current Core event"),
                        )?))
                    }
                    std::cmp::Ordering::Equal => {
                        let state = states
                            .pop_front()
                            .ok_or_else(|| anyhow!("internal: missing prior Core event state"))?;
                        let prepared = current
                            .pop_front()
                            .ok_or_else(|| anyhow!("internal: missing current Core event"))?;
                        (state.requires_replacement
                            || prepared.core_record_sha256 != state.core_record_sha256)
                            .then(|| {
                                PreparedEventDelta::replaced(state.core_record_sha256, prepared)
                            })
                    }
                }
            }
        };
        let Some(delta) = delta else {
            continue;
        };
        mutations = mutations
            .checked_add(1)
            .ok_or_else(|| anyhow!("invalid_request: Core event mutation count overflowed"))?;
        if pending.is_full() {
            let (deltas, wire_bytes) = pending.into_deltas_with_wire_bytes(false)?;
            send_event_delta_page(
                consumer,
                pending_batch,
                materialization_id,
                generation_id,
                &reconciliation,
                event_page_index,
                false,
                deltas,
                wire_bytes,
                event_delta_exchange_mode,
            )?;
            pages = pages.saturating_add(1);
            event_page_index = event_page_index
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid_request: Core event delta page overflowed"))?;
            pending = EventDeltaPageBuilder::new(
                materialization_id,
                generation_id,
                &reconciliation,
                event_page_index,
            )?;
        }
        if let Some(overflow) = pending.try_push(delta)? {
            if pending.is_empty() {
                return Err(anyhow!(
                    "invalid_request: one Core event delta exceeds its page bound"
                ));
            }
            let (deltas, wire_bytes) = pending.into_deltas_with_wire_bytes(false)?;
            send_event_delta_page(
                consumer,
                pending_batch,
                materialization_id,
                generation_id,
                &reconciliation,
                event_page_index,
                false,
                deltas,
                wire_bytes,
                event_delta_exchange_mode,
            )?;
            pages = pages.saturating_add(1);
            event_page_index = event_page_index
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid_request: Core event delta page overflowed"))?;
            pending = EventDeltaPageBuilder::new(
                materialization_id,
                generation_id,
                &reconciliation,
                event_page_index,
            )?;
            // The legacy builder carried a byte-split overflow directly into
            // the next page. Preserve that behavior so a final singleton is
            // judged with its actual `terminal: true` encoding at seal time.
            pending.push_split_overflow(overflow)?;
        }
    }

    let (deltas, wire_bytes) = pending.into_deltas_with_wire_bytes(true)?;
    send_event_delta_page(
        consumer,
        pending_batch,
        materialization_id,
        generation_id,
        &reconciliation,
        event_page_index,
        true,
        deltas,
        wire_bytes,
        event_delta_exchange_mode,
    )?;
    pages = pages.saturating_add(1);
    drop(current_credit);
    Ok(EventReconciliationReport { pages, mutations })
}

#[cfg(test)]
#[path = "core_materialization_feed/tests.rs"]
mod tests;
