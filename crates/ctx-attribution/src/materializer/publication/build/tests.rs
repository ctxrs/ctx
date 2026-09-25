use std::collections::BTreeSet;
use std::path::Path;

use crate::graph::segment::{
    EventIndexSource, EventLineageTables, IndexedCoreEventLineage, IndexedCoreEventOriginKind,
    IndexedCoreEventState, SegmentRef,
};
use crate::protocol::{IDENTITY_VERSION, SourceAnchor, StableEntityKind};
use sha2::Digest as _;

use super::super::super::SegmentMaterializerError;
use super::super::super::model::{
    EVENT_INDEX_PUBLICATION_MEMORY_RESERVE, MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS,
    MAX_PUBLICATION_FLAT_RETAINED_BYTES, MAX_PUBLICATION_FLAT_WRITER_ADDITIONAL_BYTES,
    MAX_PUBLICATION_IN_FLIGHT_BYTES, MaterializerMetrics,
};
use super::super::io::{plan_event_index_segment, plan_flat_segment};
use super::encoding::PublicationSink;
use super::planning::{
    PublicationJob, PublicationJobKind, PublicationPipeline, PublicationPipelineMetrics,
    install_publication_test_hook,
};

fn rollover_source() -> EventIndexSource {
    EventIndexSource::new(
        crate::protocol::SourceKey::derive(
            "publication-rollover",
            "event_index_test",
            "test",
            1,
            SourceAnchor::CatalogLineage([0x4a; 32]),
        )
        .expect("rollover source identity"),
    )
    .expect("rollover event-index source")
}

fn rollover_record(source: &EventIndexSource, tag: u8) -> IndexedCoreEventState {
    let digest = [tag; 32];
    let mut uuid_bytes = [0_u8; 16];
    uuid_bytes.copy_from_slice(&digest[..16]);
    uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
    uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
    let event_id = serde_json::from_value(serde_json::json!({
        "contract_version": IDENTITY_VERSION,
        "entity_kind": StableEntityKind::Event,
        "digest": digest,
        "source_digest": source.source.identity().digest(),
        "source_descriptor_digest": source.source.exact_descriptor_digest(),
        "uuid": uuid::Uuid::from_bytes(uuid_bytes),
    }))
    .expect("rollover event identity");
    let mut session_digest = digest;
    session_digest[31] ^= 0x80;
    let mut session_uuid_bytes = [0_u8; 16];
    session_uuid_bytes.copy_from_slice(&session_digest[..16]);
    session_uuid_bytes[6] = 0x80 | (session_uuid_bytes[6] & 0x0f);
    session_uuid_bytes[8] = 0x80 | (session_uuid_bytes[8] & 0x3f);
    let session_id = serde_json::from_value(serde_json::json!({
        "contract_version": IDENTITY_VERSION,
        "entity_kind": StableEntityKind::Session,
        "digest": session_digest,
        "source_digest": source.source.identity().digest(),
        "source_descriptor_digest": source.source.exact_descriptor_digest(),
        "uuid": uuid::Uuid::from_bytes(session_uuid_bytes),
    }))
    .expect("rollover session identity");
    IndexedCoreEventState {
        source_storage_key: source.storage_key.clone(),
        event_id,
        lineage: IndexedCoreEventLineage {
            session_id,
            parent_session_id: None,
            root_session_id: Some(session_id),
            session_relationship: crate::protocol::SessionRelationshipKind::Root,
            origin_kind: IndexedCoreEventOriginKind::UniqueToSession,
            copied_from: None,
        },
        event_sequence: u64::from(tag),
        core_record_sha256: format!("{tag:064x}"),
        core_record_leaf_sha256: format!("{:064x}", u16::from(tag) + 1),
        flat_record_count: 0,
        event_output_root: format!("{:064x}", u16::from(tag) + 2),
        coverage: crate::graph::segment_state::SegmentCoreCoverage {
            repository_candidate_events: 0,
            logical_binding_events: 0,
            certified_live_root_access_events: 0,
            file_evidence_events: 0,
            exact_commit_evidence_events: 0,
            exact_pull_request_evidence_events: 0,
            bounded_omission_events: 0,
        },
    }
}

fn empty_publication_jobs() -> Result<Vec<PublicationJob>, SegmentMaterializerError> {
    let flat = plan_flat_segment(&"0".repeat(64), 1, 0)?;
    let index = plan_event_index_segment(&"0".repeat(64), 1, 1)?;
    Ok(vec![
        PublicationJob {
            plan: flat,
            accounted_bytes: MAX_PUBLICATION_FLAT_WRITER_ADDITIONAL_BYTES,
            kind: PublicationJobKind::Flat {
                records: Vec::new(),
                tombstones: Vec::new(),
            },
        },
        PublicationJob {
            plan: index,
            accounted_bytes: EVENT_INDEX_PUBLICATION_MEMORY_RESERVE,
            kind: PublicationJobKind::EventIndex {
                sources: Vec::new(),
                records: Vec::new(),
                tombstones: Vec::new(),
                lineage: EventLineageTables {
                    sessions: Vec::new(),
                    copied_origins: Vec::new(),
                },
            },
        },
    ])
}

fn run_publication_jobs(
    root: &Path,
    worker_limit: usize,
    jobs: Vec<PublicationJob>,
) -> Result<(Vec<SegmentRef>, PublicationPipelineMetrics), SegmentMaterializerError> {
    super::super::super::locking::prepare_private_root(root)?;
    let mut references = jobs
        .iter()
        .map(|job| job.plan.placeholder())
        .collect::<Vec<_>>();
    let mut pipeline = PublicationPipeline::new(root, worker_limit)?;
    for job in jobs {
        pipeline.dispatch(job)?;
    }
    pipeline.drain_pending(&mut references)?;
    // A completed epoch may be observed repeatedly until more work is
    // dispatched. This no-work barrier must not change references or
    // cumulative completion accounting.
    pipeline.drain_pending(&mut references)?;
    let metrics = pipeline.metrics()?;
    Ok((references, metrics))
}

type PublicationHashes = ([u8; 32], [u8; 32]);

fn publication_hashes(
    root: &Path,
    references: &[SegmentRef],
) -> Result<Vec<PublicationHashes>, SegmentMaterializerError> {
    references
        .iter()
        .map(|reference| {
            let segment_bytes =
                std::fs::read(root.join(&reference.file_name)).map_err(|source| {
                    SegmentMaterializerError::Io {
                        operation: "read publication parity segment_bytes",
                        path: root.join(&reference.file_name),
                        source,
                    }
                })?;
            let (mut plain, pinned) = super::super::io::open_segment_pinned(root, reference)?;
            let plaintext = plain.read_all()?;
            pinned.verify_identity()?;
            Ok((
                sha2::Sha256::digest(plaintext.as_slice()).into(),
                sha2::Sha256::digest(segment_bytes).into(),
            ))
        })
        .collect()
}

#[test]
fn flat_publication_byte_and_index_rollover_accept_exact_and_flush_one_over() {
    let directory = tempfile::tempdir().expect("temporary publication root");

    let mut sink = PublicationSink::new(directory.path(), &"0".repeat(64), 1, 1, 4_096)
        .expect("bounded publication sink");
    let final_frame = crate::graph::segment::MAX_FLAT_RECORD_FRAME_BYTES;
    sink.flat_retained_bytes = MAX_PUBLICATION_FLAT_RETAINED_BYTES - final_frame;
    sink.flat_index_associations = MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS - 2;
    sink.flat_tombstones
        .push(crate::graph::segment::EventTombstone {
            source_id: "source".to_owned(),
            event_id: "event".to_owned(),
            event_sequence: 1,
        });
    assert!(
        !sink
            .flat_batch_requires_flush(final_frame, 2, true)
            .expect("exact threshold decision")
    );
    sink.charge_flat_work(final_frame, 2)
        .expect("exact ceilings must be admitted");
    assert_eq!(
        sink.flat_retained_bytes,
        MAX_PUBLICATION_FLAT_RETAINED_BYTES
    );
    assert_eq!(
        sink.flat_index_associations,
        MAX_PUBLICATION_FLAT_INDEX_ASSOCIATIONS
    );
    assert!(
        sink.flat_batch_requires_flush(1, 0, true)
            .expect("one-over byte threshold decision")
    );
    assert!(
        sink.flat_batch_requires_flush(0, 1, true)
            .expect("one-over association threshold decision")
    );
}

#[test]
fn event_index_rollover_re_registers_the_source_for_the_next_batch() {
    let directory = tempfile::tempdir().expect("temporary publication root");

    let root = directory.path().join("graph");
    super::super::super::locking::prepare_private_root(&root)
        .expect("owner-private publication root");
    let mut sink =
        PublicationSink::new(&root, &"0".repeat(64), 1, 1, 4_096).expect("publication sink");
    let source = rollover_source();

    sink.push_index_record(&source, rollover_record(&source, 1))
        .expect("first index row");
    sink.flush_index().expect("forced rollover");
    sink.push_index_record(&source, rollover_record(&source, 2))
        .expect("post-rollover index row");
    sink.flush_index().expect("final index batch");

    let mut metrics = MaterializerMetrics::default();
    sink.finish_publication_jobs(&mut metrics)
        .expect("every index batch must carry its source dictionary");
    assert_eq!(sink.references.len(), 2);
    assert!(
        sink.references
            .iter()
            .all(|reference| reference.role == crate::graph::segment::EVENT_STATE_INDEX_ROLE)
    );
}

#[test]
fn one_and_two_worker_publication_are_byte_identical_and_ordered() {
    let serial_directory = tempfile::tempdir().expect("serial publication root");
    let parallel_directory = tempfile::tempdir().expect("parallel publication root");
    let serial_root = serial_directory.path().join("graph");
    let parallel_root = parallel_directory.path().join("graph");

    let jobs = empty_publication_jobs().expect("planned publication jobs");

    let (serial_references, serial_metrics) =
        run_publication_jobs(&serial_root, 1, jobs.clone()).expect("one-worker publication");
    let _hook = install_publication_test_hook(&parallel_root, 2, BTreeSet::from([0, 1]), None)
        .expect("parallel publication rendezvous");
    let (parallel_references, parallel_metrics) =
        run_publication_jobs(&parallel_root, 2, jobs).expect("two-worker publication");

    assert_eq!(parallel_references, serial_references);
    let parallel_hashes = publication_hashes(&parallel_root, &parallel_references)
        .expect("parallel publication hashes");
    let serial_hashes =
        publication_hashes(&serial_root, &serial_references).expect("serial publication hashes");
    assert_eq!(parallel_hashes, serial_hashes);
    assert_eq!(
        serial_references
            .iter()
            .map(|reference| (reference.ordinal, reference.role))
            .collect::<Vec<_>>(),
        vec![
            (0, crate::graph::segment::FLAT_SERVING_ROLE),
            (1, crate::graph::segment::EVENT_STATE_INDEX_ROLE),
        ]
    );
    assert_eq!(serial_metrics.peak_workers, 1);
    assert_eq!(parallel_metrics.peak_workers, 2);
    assert_eq!(
        parallel_metrics.peak_in_flight_bytes,
        MAX_PUBLICATION_FLAT_WRITER_ADDITIONAL_BYTES + EVENT_INDEX_PUBLICATION_MEMORY_RESERVE
    );
    for metrics in [serial_metrics, parallel_metrics] {
        assert_eq!(metrics.jobs_started, 2);
        assert_eq!(metrics.jobs_completed, 2);
        assert_eq!(metrics.checked_readbacks, 2);
        assert!(metrics.peak_workers <= metrics.worker_limit);
        assert!(metrics.peak_in_flight_bytes <= metrics.in_flight_byte_limit);
    }
    println!(
        "publication-pipeline-parity worker_peaks={}/{} peak_in_flight_bytes={} readbacks={} plaintext_sha256={} segment_bytes_sha256={}",
        serial_metrics.peak_workers,
        parallel_metrics.peak_workers,
        parallel_metrics.peak_in_flight_bytes,
        parallel_metrics.checked_readbacks,
        parallel_hashes
            .iter()
            .map(|(plaintext, _)| hex::encode(plaintext))
            .collect::<Vec<_>>()
            .join(","),
        parallel_hashes
            .iter()
            .map(|(_, segment_bytes)| hex::encode(segment_bytes))
            .collect::<Vec<_>>()
            .join(","),
    );
}

#[test]
fn producer_dispatch_failure_drains_and_reports_the_earliest_worker_error() {
    let directory = tempfile::tempdir().expect("dispatch failure root");
    let root = directory.path().join("graph");

    super::super::super::locking::prepare_private_root(&root).expect("prepared publication root");
    let hook = install_publication_test_hook(&root, 1, BTreeSet::new(), Some(0))
        .expect("worker failure hook");
    let mut jobs = empty_publication_jobs().expect("planned publication jobs");
    let mut second = jobs.pop().expect("second publication job");
    let first = jobs.pop().expect("first publication job");
    let mut pipeline = PublicationPipeline::new(&root, 1).expect("publication pipeline");
    pipeline.dispatch(first).expect("first worker dispatched");
    second.accounted_bytes = MAX_PUBLICATION_IN_FLIGHT_BYTES + 1;
    assert!(matches!(
        pipeline.dispatch(second),
        Err(SegmentMaterializerError::Corrupt(
            "injected publication worker failure"
        ))
    ));
    assert!(pipeline.in_flight.is_empty());
    assert_eq!(pipeline.in_flight_bytes, 0);
    let workers = hook.worker_receipt().expect("drained worker receipt");
    assert_eq!(workers.workers_entered, 1);
    assert_eq!(workers.workers_exited, 1);
    assert_eq!(workers.active_workers, 0);
}
