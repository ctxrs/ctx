use ctx_attribution_index::MAX_EVENT_INDEX_PAGE_ITEMS;
use std::collections::{BTreeSet, VecDeque};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicUsize, Ordering as AtomicOrdering},
    mpsc::{Receiver, SyncSender, sync_channel},
};
use std::thread;

use crate::materializer::{CoreGenerationStart, SegmentMaterializer};
use crate::protocol::{
    CoreEventDelta, CoreEventDeltaPage, CoreEventReplacement, CoreEventState, CoreEventTombstone,
    CoreGenerationHead, CoreMaterializationReceipt, CoreSourceDelta, CoreSourceDeltaPage,
    CoreSourceReconciliation, CoreSourceState, MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
    MAX_CORE_EVENT_DELTA_PAGE_ITEMS, MAX_CORE_EVENT_DELTA_PAGES, MAX_CORE_SOURCE_DELTA_PAGE_ITEMS,
    MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES, MAX_CORE_SOURCE_STATES,
};
use anyhow::{Result, anyhow, bail};
use ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES;
#[cfg(test)]
use ctx_history_index_format::GenerationManifest;
#[cfg(test)]
use ctx_history_index_query::{SourceEventCursor, VerifiedIndex};
use ctx_history_snapshot_reader::{
    CoreRecordPageCursor, MAX_SOURCE_MANIFEST_PAGE_ITEMS, SnapshotPageBudget,
};
use serde::Serialize;

use super::worker_budget::{CoreWorkerLaunchSelection, MAX_CORE_PREFETCH_WORKERS};
use ctx_history_snapshot_reader::CoreSnapshot;

const MAX_CORE_RECORD_PAGE_ENCODED_PAYLOAD_BYTES: usize = MAX_ENCODED_CORE_RECORD_BYTES;
const CORE_RECORD_PAGE_BUDGET: SnapshotPageBudget = SnapshotPageBudget::new(
    MAX_CORE_RECORD_PAGE_ENCODED_PAYLOAD_BYTES,
    MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
);
#[path = "feed/batching.rs"]
pub mod batching;
#[path = "feed/ordered_prefetch.rs"]
mod ordered_prefetch;

use batching::*;
use ordered_prefetch::*;

#[derive(Clone)]
enum CoreFeedRecordCursor {
    Snapshot(CoreRecordPageCursor),
    #[cfg(test)]
    Query(SourceEventCursor),
}

struct CoreFeedRecordPageItem {
    core_record: ctx_history_core::CoreRecord,
    core_record_sha256: String,
}

struct CoreFeedRecordPage {
    generation_id: String,
    source: ctx_history_core::SourceKey,
    items: Vec<CoreFeedRecordPageItem>,
    encoded_core_bytes: usize,
    content_bytes: usize,
    next_cursor: Option<CoreFeedRecordCursor>,
    terminal: bool,
}

#[derive(Clone)]
struct CoreFeedSchema {
    generation_manifest_version: u32,
    identity_version: u16,
    core_record_version: u32,
    core_record_contract_fingerprint: String,
    lexical_schema_version: u32,
    lexical_analyzer_version: u32,
    policy_schema_hash: String,
}

trait CoreFeedSnapshot: Sync {
    fn generation_id(&self) -> &str;
    fn schema(&self) -> CoreFeedSchema;
    fn source_states(&self) -> Result<Vec<CoreSourceState>>;
    fn record_page(
        &self,
        source: &ctx_history_core::SourceKey,
        cursor: Option<&CoreFeedRecordCursor>,
        limit: usize,
        budget: SnapshotPageBudget,
    ) -> Result<CoreFeedRecordPage>;
}

#[path = "feed/snapshot_impl.rs"]
mod snapshot_impl;

#[derive(Debug, Clone)]
pub enum CoreMaterializationSyncOutcome {
    Finished {
        receipt: CoreMaterializationReceipt,
        did_work: bool,
    },
}

pub fn sync_generation_pinned_core(
    data_root: &Path,
    index: &CoreSnapshot,
    materializer: &mut SegmentMaterializer,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<CoreMaterializationSyncOutcome> {
    let selection = CoreWorkerLaunchSelection::from_runtime();
    let (receipt, did_work) =
        sync_core_feed_with_launch(data_root, index, materializer, cancelled, selection)?;
    Ok(CoreMaterializationSyncOutcome::Finished { receipt, did_work })
}

fn ensure_materialization_active(
    _data_root: &Path,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<()> {
    if cancelled.is_some_and(|check| check()) {
        return Err(crate::materializer::SegmentMaterializerError::Cancelled.into());
    }
    Ok(())
}

fn sync_core_feed_with_launch(
    data_root: &Path,
    index: &dyn CoreFeedSnapshot,
    materializer: &mut SegmentMaterializer,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    selection: CoreWorkerLaunchSelection,
) -> Result<(CoreMaterializationReceipt, bool)> {
    match sync_core_feed_attempt_with_launch(data_root, index, materializer, cancelled, selection) {
        Err(error) if stable_core_error_code(&error) == Some("needs_rebuild") => {
            sync_core_feed_attempt_with_launch(data_root, index, materializer, cancelled, selection)
        }
        result => result,
    }
}

fn sync_core_feed_attempt_with_launch(
    data_root: &Path,
    index: &dyn CoreFeedSnapshot,
    materializer: &mut SegmentMaterializer,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    selection: CoreWorkerLaunchSelection,
) -> Result<(CoreMaterializationReceipt, bool)> {
    let options = selection.execution_options();
    let credits = Arc::new(EncodedPageCredits::new(CORE_PREFETCH_ENCODED_BYTE_BUDGET));
    let instrumentation = Arc::new(CorePrefetchInstrumentation::default());
    let sources = index.source_states()?;
    let head = core_generation_head(index, &sources)?;
    if head.core_generation_id != index.generation_id() {
        bail!(
            "core_generation_mismatch: generation head {} does not match pinned Core {}",
            head.core_generation_id,
            index.generation_id()
        );
    }
    ensure_materialization_active(data_root, cancelled)?;
    let mut session = match materializer.start_core_generation(head.clone())? {
        CoreGenerationStart::Current(receipt) => return Ok((receipt, false)),
        CoreGenerationStart::Started(session) => session,
    };
    let materialization_id = session.materialization_id().to_owned();

    let maximum_reconciliations = sources
        .len()
        .checked_add(MAX_CORE_SOURCE_STATES)
        .ok_or_else(|| anyhow!("invalid_response: source reconciliation bound overflowed"))?;
    let deltas = core_snapshot_deltas(&sources);
    let delta_pages = build_delta_pages(&materialization_id, index.generation_id(), deltas)?;
    let mut next_materialize_index = 0_u32;
    let mut reconciled_source_ids = BTreeSet::new();
    let mut reconcile_sources = Vec::new();
    for page in delta_pages {
        ensure_materialization_active(data_root, cancelled)?;
        for reconciliation in session.reconcile_source_page(page)? {
            if reconciliation.materialize_index != next_materialize_index {
                bail!("invalid_response: Core reconciliation indices are not contiguous");
            }
            next_materialize_index = next_materialize_index
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid_response: Core reconciliation index overflowed"))?;
            let source_id = reconciliation.delta.source().identity().digest();
            if !reconciled_source_ids.insert(source_id) {
                bail!("invalid_response: Core reconciliations repeat a stable source identity");
            }
            let current_source = sources
                .binary_search_by_key(&source_id, |state| state.source.identity().digest())
                .ok()
                .map(|index| &sources[index]);
            match &reconciliation.delta {
                CoreSourceDelta::Present(state) if current_source != Some(state) => {
                    bail!("invalid_response: Core reconciliation carries a stale current source");
                }
                CoreSourceDelta::Removed(_) if current_source.is_some() => {
                    bail!("invalid_response: Core reconciliation removes a current source");
                }
                _ => {}
            }
            if reconcile_sources.len() >= maximum_reconciliations {
                bail!("invalid_response: Core reconciliations exceed source bounds");
            }
            reconcile_sources.push(reconciliation);
        }
    }
    session.start_progress(u32::try_from(reconcile_sources.len())?);
    reconcile_ordered_source_events(
        index,
        &mut session,
        &materialization_id,
        reconcile_sources,
        data_root,
        cancelled,
        OrderedReconciliationOptions {
            prefetch_parallelism: options.prefetch_parallelism,
        },
        &credits,
        &instrumentation,
    )?;

    let (encoded_credit_final_bytes, _) = credits.snapshot()?;
    if encoded_credit_final_bytes != 0 {
        bail!("internal: Core prefetch credits remained live after reconciliation");
    }
    ensure_materialization_active(data_root, cancelled)?;
    // The supplied CoreSnapshot retains the exact generation until publication
    // completes. No durable deferred-job hold is needed by this synchronous owner.
    let receipt = session.activate_cancellable(cancelled)?;
    Ok((receipt, true))
}

#[cfg(test)]
#[path = "feed/tests.rs"]
mod tests;

#[path = "feed/reconciliation.rs"]
mod reconciliation;
use reconciliation::*;

pub(crate) fn stable_core_error_code(error: &anyhow::Error) -> Option<&'static str> {
    // Direct sessions retain their typed errors through anyhow context. Consult
    // that owner before the string codes still supplied by application ports.
    match error.downcast_ref::<crate::materializer::SegmentMaterializerError>() {
        Some(crate::materializer::SegmentMaterializerError::RebuildRequired) => {
            return Some("needs_rebuild");
        }
        Some(
            crate::materializer::SegmentMaterializerError::Bounds
            | crate::materializer::SegmentMaterializerError::BoundDetail(_),
        ) => return Some("bounds"),
        Some(crate::materializer::SegmentMaterializerError::Cancelled) => return Some("cancelled"),
        _ => {}
    }
    error.chain().find_map(
        |cause| match cause.to_string().split(':').next().unwrap_or_default() {
            "not_materialized" => Some("not_materialized"),
            "needs_rebuild" => Some("needs_rebuild"),
            "partial" => Some("partial"),
            "needs_resume" => Some("needs_resume"),
            "protocol_mismatch" => Some("protocol_mismatch"),
            "source_unavailable" => Some("source_unavailable"),
            "source_busy" => Some("source_busy"),
            "unsafe_path" => Some("unsafe_path"),
            "corrupt_core" => Some("corrupt_core"),
            "bounds" => Some("bounds"),
            "stale_source" => Some("stale_source"),
            "repository_unavailable" => Some("repository_unavailable"),
            "resource_not_found" => Some("resource_not_found"),
            "operation_unavailable" => Some("operation_unavailable"),
            "stale_fact" => Some("stale_fact"),
            "line_out_of_range" => Some("line_out_of_range"),
            "stale_snapshot" => Some("stale_snapshot"),
            "ambiguous" => Some("ambiguous"),
            "corrupt_graph" => Some("corrupt_graph"),
            "invalid_request" => Some("invalid_request"),
            "invalid_response" => Some("invalid_response"),
            "cancelled" => Some("cancelled"),
            _ => None,
        },
    )
}

#[cfg(test)]
#[path = "feed_typed_error_classification_tests.rs"]
mod typed_error_classification_tests;
