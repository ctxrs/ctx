use std::collections::BTreeSet;

use super::*;

pub(super) fn result(
    request: StatusRequest,
    currentness: CoreProjectionCurrentness,
    core_receipt: Option<CoreMaterializationReceipt>,
    core_preparation_peak_workers: u16,
    journal_finish_activity: JournalFinishActivity,
) -> ctx_pro_host_protocol::StatusResult {
    let coverage = match currentness {
        CoreProjectionCurrentness::NotMaterialized => {
            ctx_pro_host_protocol::MaterializedCoverage::NotMaterialized
        }
        CoreProjectionCurrentness::Current
            if core_receipt
                .as_ref()
                .is_some_and(|receipt| receipt.event_count == 0) =>
        {
            ctx_pro_host_protocol::MaterializedCoverage::Empty
        }
        CoreProjectionCurrentness::Current => {
            ctx_pro_host_protocol::MaterializedCoverage::Abstained
        }
        CoreProjectionCurrentness::Partial
        | CoreProjectionCurrentness::Finalizing
        | CoreProjectionCurrentness::Stale
        | CoreProjectionCurrentness::NeedsRebuild => {
            ctx_pro_host_protocol::MaterializedCoverage::Partial
        }
    };
    let storage_evidence =
        core_receipt
            .as_ref()
            .map(|_| ctx_pro_host_protocol::ProStorageEvidence {
                graph_manifest_schema: 3,
                flat_format_version: 2,
                materializer_checkpoint_version: 5,
                journal_pack_format_version: 3,
                legacy_journals_written: 0,
                journal_pages_written: 2,
                journal_packs_written: 1,
                journal_finish_activity,
            });
    ctx_pro_host_protocol::StatusResult {
        currentness,
        requested_core_generation_id: request.requested_core_generation_id,
        core_receipt,
        coverage,
        repository_coverage: ctx_pro_host_protocol::RepositoryCoverage::default(),
        core_preparation_peak_workers,
        access: ctx_pro_host_protocol::ProAccessStatus {
            entitlement: ctx_pro_host_protocol::ProAccessState::Available,
            graph_key: ctx_pro_host_protocol::ProAccessState::Available,
            local_repository: ctx_pro_host_protocol::ProAccessState::Unavailable,
        },
        supported_operations: BTreeSet::new(),
        available_operations: BTreeSet::new(),
        finalization_progress: None,
        storage_evidence,
    }
}

fn assert_no_pages_or_finish(consumer: &Consumer) {
    assert!(consumer.delta_pages.is_empty());
    assert!(consumer.state_requests.is_empty());
    assert!(consumer.event_pages.is_empty());
    assert!(consumer.finish.is_none());
    assert!(consumer.finish_requests.is_empty());
}

#[test]
fn contradictory_replayed_status_fails_before_pages_or_finish() {
    let (_temp, index) = single_source_index("contradictory-status.jsonl", vec!["body".to_owned()]);
    let mut consumer = Consumer::new();
    consumer.replay_begin = true;
    consumer.replay_status_currentness = Some(CoreProjectionCurrentness::NeedsRebuild);

    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid_response: replayed Core materialization reported contradictory NeedsRebuild status"
    );
    assert_eq!(consumer.status_exchanges, 1);
    assert_no_pages_or_finish(&consumer);
}

#[test]
fn replayed_status_error_is_preserved_without_a_guessed_finish() {
    let (_temp, index) = single_source_index("status-error.jsonl", vec!["body".to_owned()]);
    let mut consumer = Consumer::new();
    consumer.replay_begin = true;
    consumer.pre_finish_status_error = Some("synthetic_status_error: candidate unavailable".into());

    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert_eq!(
        error.to_string(),
        "synthetic_status_error: candidate unavailable"
    );
    assert_eq!(consumer.status_exchanges, 1);
    assert_no_pages_or_finish(&consumer);
}

#[test]
fn partial_prior_receipt_revision_mismatch_fails_before_pages_or_finish() {
    let (_temp, index) =
        single_source_index("partial-revision-mismatch.jsonl", vec!["body".to_owned()]);
    let prior = receipt_for(&index, "test-core-materializer-v1");
    let mut contradictory = prior.clone();
    contradictory.materializer_revision = "test-core-materializer-v2".to_owned();
    let mut consumer = Consumer::new();
    consumer.replay_begin = true;
    consumer.replay_status_currentness = Some(CoreProjectionCurrentness::Partial);
    consumer.last_receipt = Some(contradictory);

    let error = sync_core_feed(&index, Some(&prior), &mut consumer).unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid_response: partial Core materialization prior receipt does not match its begin request"
    );
    assert_no_pages_or_finish(&consumer);
}

#[test]
fn current_receipt_source_or_revision_mismatch_fails_before_pages_or_finish() {
    let (_temp, index) =
        single_source_index("current-receipt-mismatch.jsonl", vec!["body".to_owned()]);
    let prior = receipt_for(&index, "test-core-materializer-v1");

    let mut source_mismatch = Consumer::new();
    source_mismatch.replay_begin = true;
    source_mismatch.replay_status_currentness = Some(CoreProjectionCurrentness::Current);
    let mut wrong_source = prior.clone();
    wrong_source.source_snapshot_sha256 = "f".repeat(64);
    source_mismatch.last_receipt = Some(wrong_source);
    let error = sync_core_feed(&index, Some(&prior), &mut source_mismatch).unwrap_err();
    assert!(error
        .to_string()
        .contains("receipt belongs to a different generation contract"));
    assert_no_pages_or_finish(&source_mismatch);

    let mut revision_mismatch = Consumer::new();
    revision_mismatch.replay_begin = true;
    revision_mismatch.replay_status_currentness = Some(CoreProjectionCurrentness::Current);
    let mut wrong_revision = prior.clone();
    wrong_revision.materializer_revision = "test-core-materializer-v2".to_owned();
    revision_mismatch.last_receipt = Some(wrong_revision);
    let error = sync_core_feed(&index, Some(&prior), &mut revision_mismatch).unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid_response: current Core replay receipt changed materializer revision"
    );
    assert_no_pages_or_finish(&revision_mismatch);
}

#[test]
fn begin_materialization_mismatch_fails_before_status_pages_or_finish() {
    let (_temp, index) = single_source_index(
        "begin-materialization-mismatch.jsonl",
        vec!["body".to_owned()],
    );
    let mut consumer = Consumer::new();
    consumer.replay_begin = true;
    consumer.wrong_begin_materialization = true;

    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert!(error
        .to_string()
        .contains("begin response does not match its request CAS"));
    assert_eq!(consumer.status_exchanges, 0);
    assert_no_pages_or_finish(&consumer);
}

#[test]
fn replayed_status_generation_mismatch_fails_before_pages_or_finish() {
    let (_temp, index) =
        single_source_index("status-generation-mismatch.jsonl", vec!["body".to_owned()]);
    let prior = receipt_for(&index, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    consumer.replay_begin = true;
    consumer.replay_status_currentness = Some(CoreProjectionCurrentness::Partial);
    consumer.last_receipt = Some(prior.clone());
    consumer.status_generation_override = Some("f".repeat(64));

    let error = sync_core_feed(&index, Some(&prior), &mut consumer).unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid_response: replayed Core status did not echo the requested generation"
    );
    assert_no_pages_or_finish(&consumer);
}

#[test]
fn terminal_pages_share_one_source_pinned_multi_source_envelope() {
    let temp = tempdir().unwrap();
    let first = source("batch-source-first.jsonl");
    let second = source("batch-source-second.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(&mut writer, &first, 1, vec!["first".to_owned()]);
    add_source(&mut writer, &second, 1, vec!["second".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    assert_eq!(report.event_delta_pages, 2);
    assert_eq!(report.event_mutations, 2);
    assert_eq!(consumer.event_exchanges, 1);
    assert_eq!(consumer.event_exchange_page_ids.len(), 1);
    assert_eq!(consumer.event_exchange_page_ids[0].len(), 2);
    assert!(consumer.event_exchange_page_ids[0]
        .iter()
        .all(|page| page.1 == 0));
    assert_ne!(
        consumer.event_exchange_page_ids[0][0].0,
        consumer.event_exchange_page_ids[0][1].0
    );
    assert!(consumer.event_pages.iter().all(|page| page.terminal));
    assert_eq!(consumer.finish.as_ref().unwrap().event_delta_pages, 2);
    assert_eq!(consumer.status_exchanges, 1);
}

#[test]
fn post_finish_status_rejects_a_peak_above_the_launched_helper_limit() {
    let temp = tempdir().unwrap();
    let source = source("worker-peak-over-limit.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();
    consumer.core_preparation_peak_workers = 2;
    let selection = worker_selection_for_test(64, Some(1));

    let error = sync_core_feed_with_launch(&index, None, &mut consumer, selection).unwrap_err();
    assert!(error
        .to_string()
        .contains("exceeded the requested helper limit 1"));
    assert!(consumer.finish.is_some());
    assert_eq!(consumer.status_exchanges, 1);
}
