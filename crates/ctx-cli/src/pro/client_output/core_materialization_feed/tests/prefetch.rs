use super::*;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy)]
enum PrefetchFixture {
    ManyTiny,
    Balanced,
    OversizedSingletonAmongNormalSources,
}

fn maximum_encoded_body(source: &SourceKey) -> String {
    let minimum_body = "x".to_owned();
    let minimum_encoded_bytes = record(source, 1, minimum_body.clone())
        .encode_stored()
        .unwrap()
        .len();
    let available = MAX_ENCODED_CORE_RECORD_BYTES - minimum_encoded_bytes;
    let escaped_units = available / "\\u0000".len();
    let plain_units = available % "\\u0000".len();
    let mut body = minimum_body;
    body.push_str(&"\0".repeat(escaped_units));
    body.push_str(&"x".repeat(plain_units));
    let encoded = record(source, 1, body.clone()).encode_stored().unwrap();
    assert_eq!(encoded.len(), MAX_ENCODED_CORE_RECORD_BYTES);
    body
}

fn prefetch_fixture(case: PrefetchFixture) -> (tempfile::TempDir, VerifiedIndex, usize, usize) {
    let temp = tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let (source_count, record_count) = match case {
        PrefetchFixture::ManyTiny => {
            for source_index in 0..48 {
                let source = source(&format!("prefetch-tiny-{source_index:02}.jsonl"));
                add_source(
                    &mut writer,
                    &source,
                    u8::try_from(source_index + 1).unwrap(),
                    vec![format!("tiny body {source_index}")],
                );
            }
            (48, 48)
        }
        PrefetchFixture::Balanced => {
            let sources = 12;
            let records_per_source = 48;
            for source_index in 0..sources {
                let source = source(&format!("prefetch-balanced-{source_index:02}.jsonl"));
                add_source(
                    &mut writer,
                    &source,
                    u8::try_from(source_index + 1).unwrap(),
                    (0..records_per_source)
                        .map(|record_index| {
                            format!(
                                "balanced {source_index}:{record_index} {}",
                                "b".repeat(16 * 1024)
                            )
                        })
                        .collect(),
                );
            }
            (sources, sources * records_per_source)
        }
        PrefetchFixture::OversizedSingletonAmongNormalSources => {
            let whale = source("prefetch-maximum-encoded.jsonl");
            let whale_digest = whale.identity().digest();
            let mut lower_peers = Vec::new();
            let mut upper_peers = Vec::new();
            for candidate_index in 0.. {
                let candidate = source(&format!("prefetch-whale-peer-{candidate_index}.jsonl"));
                let candidate_digest = candidate.identity().digest();
                let (peers, needed) = if candidate_digest < whale_digest {
                    (&mut lower_peers, 4)
                } else {
                    (&mut upper_peers, 4)
                };
                if peers.len() < needed {
                    peers.push(candidate);
                }
                if lower_peers.len() == 4 && upper_peers.len() == 4 {
                    break;
                }
            }
            let whale_body = maximum_encoded_body(&whale);
            add_source(&mut writer, &whale, 1, vec![whale_body]);
            for (source_index, source) in lower_peers.into_iter().chain(upper_peers).enumerate() {
                add_source(
                    &mut writer,
                    &source,
                    u8::try_from(source_index + 2).unwrap(),
                    vec![format!("whale peer {source_index}")],
                );
            }
            (9, 9)
        }
    };
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    (temp, index, source_count, record_count)
}

fn oversized_nonterminal_before_maximum_singleton_fixture() -> (tempfile::TempDir, VerifiedIndex) {
    let temp = tempdir().unwrap();
    let mut candidates = (0..64)
        .map(|index| source(&format!("prefetch-ordered-oversize-{index:02}.jsonl")))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|source| source.identity().digest());
    let earlier = candidates.first().unwrap().clone();
    let later = candidates.last().unwrap().clone();
    assert!(earlier.identity().digest() < later.identity().digest());

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(
        &mut writer,
        &earlier,
        1,
        vec![
            maximum_encoded_body(&earlier),
            "normal successor after oversized nonterminal".to_owned(),
        ],
    );
    add_source(&mut writer, &later, 2, vec![maximum_encoded_body(&later)]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let ordered = core_source_states(index.manifest()).unwrap();
    assert_eq!(ordered.len(), 2);
    assert!(ordered[0].source.exact_descriptor_eq(&earlier));
    assert!(ordered[1].source.exact_descriptor_eq(&later));
    (temp, index)
}

fn sync_prefetch_fixture(
    index: &VerifiedIndex,
    parallelism: usize,
) -> (Vec<u8>, [u8; 32], CorePrefetchInstrumentationSnapshot) {
    let mut consumer = Consumer::new();
    let report = sync_core_feed_with_options(
        index,
        None,
        &mut consumer,
        CoreFeedExecutionOptions {
            prefetch_parallelism: parallelism,
        },
    )
    .unwrap();
    let protocol_bytes = serde_json::to_vec(&(
        &report.receipt,
        report.changed_sources,
        report.removed_sources,
        report.event_delta_pages,
        report.event_mutations,
        &consumer.delta_pages,
        &consumer.event_exchange_page_ids,
        &consumer.event_pages,
        &consumer.finish,
    ))
    .unwrap();
    let digest = Sha256::digest(&protocol_bytes).into();
    (protocol_bytes, digest, report.prefetch)
}

#[test]
fn ordered_prefetch_is_byte_exact_and_bounded_across_workers_and_skew() {
    for case in [
        PrefetchFixture::ManyTiny,
        PrefetchFixture::Balanced,
        PrefetchFixture::OversizedSingletonAmongNormalSources,
    ] {
        let (_temp, index, source_count, record_count) = prefetch_fixture(case);
        if matches!(case, PrefetchFixture::OversizedSingletonAmongNormalSources) {
            let whale_digest = source("prefetch-maximum-encoded.jsonl").identity().digest();
            let mut source_digests = core_source_states(index.manifest())
                .unwrap()
                .into_iter()
                .map(|state| state.source.identity().digest())
                .collect::<Vec<_>>();
            source_digests.sort_unstable();
            assert_eq!(
                source_digests
                    .iter()
                    .position(|digest| digest == &whale_digest),
                Some(4),
                "oversized singleton must be ordered between normal sources"
            );
        }
        let (expected_bytes, expected_digest, sequential) = sync_prefetch_fixture(&index, 1);
        assert_eq!(sequential.configured_parallelism, 1);
        assert_eq!(sequential.workers_launched, 0);
        assert_eq!(sequential.encoded_credit_final_bytes, 0);
        assert!(sequential.encoded_credit_high_water_bytes <= CORE_PREFETCH_ENCODED_BYTE_BUDGET);
        assert_eq!(sequential.planned_pages, sequential.materialized_pages);
        assert_eq!(sequential.decoded_records, record_count);
        assert_eq!(
            sequential.record_payload_sha256_traversals,
            sequential.decoded_records
        );
        assert_eq!(
            sequential.record_payload_sha256_bytes,
            sequential.decoded_record_bytes
        );

        for parallelism in [2, 4, 8] {
            let (actual_bytes, actual_digest, prefetch) =
                sync_prefetch_fixture(&index, parallelism);
            assert_eq!(
                actual_bytes, expected_bytes,
                "case={case:?} workers={parallelism}"
            );
            assert_eq!(
                actual_digest, expected_digest,
                "case={case:?} workers={parallelism}"
            );
            assert_eq!(prefetch.configured_parallelism, parallelism);
            assert_eq!(prefetch.encoded_credit_final_bytes, 0);
            assert!(
                prefetch.encoded_credit_high_water_bytes <= CORE_PREFETCH_ENCODED_BYTE_BUDGET,
                "case={case:?} workers={parallelism} high_water={}",
                prefetch.encoded_credit_high_water_bytes
            );
            assert_eq!(prefetch.planned_pages, prefetch.materialized_pages);
            assert_eq!(
                prefetch.record_payload_sha256_traversals, prefetch.decoded_records,
                "case={case:?} workers={parallelism}"
            );
            assert_eq!(
                prefetch.record_payload_sha256_bytes, prefetch.decoded_record_bytes,
                "case={case:?} workers={parallelism}"
            );
            match case {
                PrefetchFixture::OversizedSingletonAmongNormalSources => {
                    assert_eq!(prefetch.workers_launched, parallelism);
                    assert_eq!(
                        prefetch.encoded_credit_high_water_bytes,
                        CORE_PREFETCH_ENCODED_BYTE_BUDGET
                    );
                    assert!(prefetch.decoded_records >= record_count);
                    assert!(prefetch.decoded_records < record_count + prefetch.workers_launched);
                    assert!(prefetch.maximum_active_workers <= prefetch.workers_launched);
                }
                PrefetchFixture::ManyTiny | PrefetchFixture::Balanced => {
                    assert_eq!(prefetch.decoded_records, record_count);
                    assert_eq!(prefetch.workers_launched, parallelism.min(source_count));
                    assert!(prefetch.maximum_active_workers <= prefetch.workers_launched);
                }
            }
        }
    }
}

#[test]
fn later_maximum_singleton_cannot_block_an_earlier_oversized_source_successor() {
    let (temp, index) = oversized_nonterminal_before_maximum_singleton_fixture();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let _temp = temp;
        let mut consumer = Consumer::new();
        let result = sync_core_feed_with_options(
            &index,
            None,
            &mut consumer,
            CoreFeedExecutionOptions {
                prefetch_parallelism: 2,
            },
        )
        .map(|report| {
            (
                report.event_delta_pages,
                report.event_mutations,
                report.prefetch,
            )
        });
        sender.send(result).unwrap();
    });
    let (pages, mutations, prefetch) = receiver
        .recv_timeout(Duration::from_secs(120))
        .expect("ordered oversized prefetch deadlocked")
        .unwrap();
    worker.join().unwrap();

    assert_eq!(pages, 2);
    assert_eq!(mutations, 3);
    assert_eq!(prefetch.workers_launched, 2);
    assert!(prefetch.maximum_active_workers <= 2);
    assert_eq!(prefetch.planned_pages, 3);
    assert_eq!(prefetch.materialized_pages, 3);
    assert_eq!(prefetch.decoded_records, 3);
    assert_eq!(prefetch.encoded_credit_final_bytes, 0);
    assert_eq!(
        prefetch.encoded_credit_high_water_bytes,
        CORE_PREFETCH_ENCODED_BYTE_BUDGET
    );
}

#[test]
fn ordered_prefetch_early_helper_error_cancels_all_workers_without_deadlock() {
    let (_temp, index, _, _) = prefetch_fixture(PrefetchFixture::ManyTiny);
    let mut reconciliations = core_source_states(index.manifest())
        .unwrap()
        .into_iter()
        .map(|source| CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(source),
        })
        .collect::<Vec<_>>();
    reconciliations.sort_by_key(|item| item.delta.source().identity().digest());
    for (materialize_index, reconciliation) in reconciliations.iter_mut().enumerate() {
        reconciliation.materialize_index = u32::try_from(materialize_index).unwrap();
    }

    for parallelism in [1, 2, 4, 8] {
        let credits = Arc::new(EncodedPageCredits::new(CORE_PREFETCH_ENCODED_BYTE_BUDGET));
        let instrumentation = Arc::new(CorePrefetchInstrumentation::default());
        let mut consumer = Consumer::new();
        consumer.source_feed_terminal = true;
        consumer.event_state_error_after = Some(1);
        let error = reconcile_ordered_source_events(
            &index,
            TEST_MATERIALIZATION_ID,
            reconciliations.clone(),
            &mut consumer,
            OrderedReconciliationOptions {
                prefetch_parallelism: parallelism,
                exchange_mode: EventDeltaExchangeMode::Normal,
            },
            &credits,
            &instrumentation,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "synthetic_event_state_error: ordered coordinator failure"
        );
        assert_eq!(consumer.state_exchanges, 1);
        assert!(consumer.event_pages.is_empty());
        let (final_bytes, high_water) = credits.snapshot().unwrap();
        assert_eq!(final_bytes, 0);
        assert!(high_water <= CORE_PREFETCH_ENCODED_BYTE_BUDGET);
    }
}

#[test]
fn prefetched_later_source_error_preserves_the_first_source_helper_error() {
    let (_temp, index, _, _) = prefetch_fixture(PrefetchFixture::ManyTiny);
    let mut reconciliations = core_source_states(index.manifest())
        .unwrap()
        .into_iter()
        .map(|source| CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(source),
        })
        .collect::<Vec<_>>();
    reconciliations.sort_by_key(|item| item.delta.source().identity().digest());
    for (materialize_index, reconciliation) in reconciliations.iter_mut().enumerate() {
        reconciliation.materialize_index = u32::try_from(materialize_index).unwrap();
    }
    let first = reconciliations.remove(0);
    let first_digest = first.delta.source().identity().digest();
    let unknown = (0_u32..)
        .map(|index| source(&format!("unknown-prefetch-source-{index}.jsonl")))
        .find(|source| source.identity().digest() > first_digest)
        .unwrap();
    let CoreSourceDelta::Present(mut invalid_state) = first.delta.clone() else {
        unreachable!()
    };
    invalid_state.source = unknown;
    let reconciliations = vec![
        first,
        CoreSourceReconciliation {
            materialize_index: 1,
            delta: CoreSourceDelta::Present(invalid_state),
        },
    ];

    for parallelism in [2, 4, 8] {
        let credits = Arc::new(EncodedPageCredits::new(CORE_PREFETCH_ENCODED_BYTE_BUDGET));
        let instrumentation = Arc::new(CorePrefetchInstrumentation::default());
        let mut consumer = Consumer::new();
        consumer.source_feed_terminal = true;
        consumer.event_state_error_after = Some(1);
        let error = reconcile_ordered_source_events(
            &index,
            TEST_MATERIALIZATION_ID,
            reconciliations.clone(),
            &mut consumer,
            OrderedReconciliationOptions {
                prefetch_parallelism: parallelism,
                exchange_mode: EventDeltaExchangeMode::Normal,
            },
            &credits,
            &instrumentation,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "synthetic_event_state_error: ordered coordinator failure"
        );
        assert_eq!(consumer.state_exchanges, 1);
        assert_eq!(credits.snapshot().unwrap().0, 0);
        assert_eq!(
            instrumentation.snapshot(0, 0).workers_launched,
            parallelism.min(reconciliations.len())
        );
    }
}

#[test]
fn launch_product_budget_preserves_helper_and_control_headroom() {
    assert_eq!(
        core_launch_product_budget(32),
        CoreLaunchProductBudget {
            helper_preparation_workers: 16,
            host_prefetch_workers: 8,
            control_writer_headroom: 8,
        }
    );
    for available in 1..=128 {
        let budget = core_launch_product_budget(available);
        assert!(budget.host_prefetch_workers <= MAX_CORE_PREFETCH_WORKERS);
        assert!(budget.helper_preparation_workers <= MAX_HELPER_PREPARATION_WORKERS);
        assert!(
            budget.helper_preparation_workers + budget.control_writer_headroom <= available.max(1)
        );
        assert!(budget.host_prefetch_workers + budget.control_writer_headroom <= available.max(1));
    }
}

#[test]
fn worker_budget_selection_is_exact_and_peak_validated() {
    let unset = worker_selection_for_test(32, None);
    assert_eq!(
        unset.budget,
        CoreLaunchProductBudget {
            helper_preparation_workers: 16,
            host_prefetch_workers: 8,
            control_writer_headroom: 8,
        }
    );

    for (worker_count, helper, host, headroom) in [
        (1, 1, 1, 0),
        (2, 1, 1, 0),
        (4, 2, 1, 1),
        (8, 4, 2, 2),
        (16, 8, 4, 4),
        (32, 16, 8, 8),
    ] {
        let selection = worker_selection_for_test(64, Some(worker_count));
        let observed_peak = u16::try_from(helper).unwrap();
        selection
            .validate_observed_helper_peak(observed_peak)
            .unwrap();
        assert_eq!(selection.budget.helper_preparation_workers, helper);
        assert_eq!(selection.budget.host_prefetch_workers, host);
        assert_eq!(selection.budget.control_writer_headroom, headroom);
        let error = selection
            .validate_observed_helper_peak(observed_peak.saturating_add(1))
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("exceeded the requested helper limit"));
    }
}

#[test]
#[ignore = "focused ordered host-prefetch benchmark"]
fn ordered_core_host_prefetch_microbenchmark() {
    fn run(index: &VerifiedIndex, parallelism: usize) -> (Duration, [u8; 32]) {
        let mut consumer = Consumer::new();
        let started = Instant::now();
        let report = sync_core_feed_with_options(
            index,
            None,
            &mut consumer,
            CoreFeedExecutionOptions {
                prefetch_parallelism: parallelism,
            },
        )
        .unwrap();
        let elapsed = started.elapsed();
        let output = serde_json::to_vec(&(
            &report.receipt,
            report.event_delta_pages,
            report.event_mutations,
            &consumer.event_exchange_page_ids,
            &consumer.event_pages,
        ))
        .unwrap();
        (elapsed, Sha256::digest(output).into())
    }

    let (_temp, index, _, _) = prefetch_fixture(PrefetchFixture::Balanced);
    for parallelism in [1, 8] {
        black_box(run(&index, parallelism));
    }
    let mut samples = BTreeMap::<usize, Vec<Duration>>::new();
    let mut expected_digest = None;
    for _ in 0..5 {
        for parallelism in [1, 2, 4, 8] {
            let (elapsed, digest) = run(&index, parallelism);
            assert_eq!(*expected_digest.get_or_insert(digest), digest);
            samples.entry(parallelism).or_default().push(elapsed);
        }
    }
    for values in samples.values_mut() {
        values.sort_unstable();
    }
    let sequential = samples[&1][samples[&1].len() / 2];
    for parallelism in [1, 2, 4, 8] {
        let median = samples[&parallelism][samples[&parallelism].len() / 2];
        eprintln!(
            "ordered_core_host_prefetch workers={parallelism} median_ms={:.3} speedup={:.3}x samples_ms={:?}",
            median.as_secs_f64() * 1_000.0,
            sequential.as_secs_f64() / median.as_secs_f64(),
            samples[&parallelism]
                .iter()
                .map(|duration| duration.as_secs_f64() * 1_000.0)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
#[ignore = "focused Core-feed record SHA traversal microbenchmark"]
fn core_feed_record_sha_traversal_microbenchmark() {
    const RECORDS: usize = MAX_CORE_EVENT_DELTA_PAGE_ITEMS;
    const BODY_BYTES: usize = CORE_PREFETCH_PAGE_ENCODED_BYTE_BUDGET / RECORDS;
    const SAMPLES: usize = 7;

    #[derive(Clone, Copy)]
    enum Mode {
        LegacyCombined,
        RecordOnly,
    }

    fn run(
        records: &[(ctx_history_core::CoreRecord, Vec<u8>)],
        mode: Mode,
    ) -> (Duration, CorePrefetchInstrumentationSnapshot, u8) {
        let instrumentation = CorePrefetchInstrumentation::default();
        let mut checksum = 0_u8;
        let started = Instant::now();
        for (record, encoded) in records {
            let digest = match mode {
                Mode::LegacyCombined => {
                    // The discarded leaf and retained record digest each traverse
                    // the complete encoded payload in the former feed path.
                    instrumentation.record_payload_sha256_traversed(encoded.len());
                    instrumentation.record_payload_sha256_traversed(encoded.len());
                    ctx_pro_host_protocol::core_record_digests_from_encoded(record, encoded)
                        .unwrap()
                        .core_record_sha256
                }
                Mode::RecordOnly => {
                    instrumentation.record_payload_sha256_traversed(encoded.len());
                    core_record_sha256_from_encoded(encoded)
                }
            };
            checksum ^= digest.as_bytes()[0];
            black_box(&digest);
        }
        (started.elapsed(), instrumentation.snapshot(0, 0), checksum)
    }

    let source = source("record-sha-traversal-benchmark.jsonl");
    let records = (0..RECORDS)
        .map(|index| {
            let record = record(
                &source,
                u64::try_from(index + 1).unwrap(),
                "h".repeat(BODY_BYTES),
            );
            let encoded = record.encode_stored().unwrap();
            (record, encoded)
        })
        .collect::<Vec<_>>();
    let encoded_bytes = records
        .iter()
        .map(|(_, encoded)| encoded.len())
        .sum::<usize>();

    black_box(run(&records, Mode::LegacyCombined));
    black_box(run(&records, Mode::RecordOnly));

    let mut legacy_samples = Vec::with_capacity(SAMPLES);
    let mut record_only_samples = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        let modes = if sample % 2 == 0 {
            [Mode::LegacyCombined, Mode::RecordOnly]
        } else {
            [Mode::RecordOnly, Mode::LegacyCombined]
        };
        for mode in modes {
            let (elapsed, snapshot, checksum) = run(&records, mode);
            match mode {
                Mode::LegacyCombined => {
                    assert_eq!(snapshot.record_payload_sha256_traversals, RECORDS * 2);
                    assert_eq!(snapshot.record_payload_sha256_bytes, encoded_bytes * 2);
                    legacy_samples.push((elapsed, checksum));
                }
                Mode::RecordOnly => {
                    assert_eq!(snapshot.record_payload_sha256_traversals, RECORDS);
                    assert_eq!(snapshot.record_payload_sha256_bytes, encoded_bytes);
                    record_only_samples.push((elapsed, checksum));
                }
            }
        }
    }
    assert!(legacy_samples
        .iter()
        .zip(&record_only_samples)
        .all(|((_, legacy), (_, record_only))| legacy == record_only));
    legacy_samples.sort_unstable_by_key(|sample| sample.0);
    record_only_samples.sort_unstable_by_key(|sample| sample.0);
    let legacy_median = legacy_samples[SAMPLES / 2].0;
    let record_only_median = record_only_samples[SAMPLES / 2].0;
    eprintln!(
        "core_feed_record_sha records={RECORDS} encoded_bytes={encoded_bytes} legacy_traversals={} legacy_payload_bytes={} legacy_median_ms={:.3} record_only_traversals={} record_only_payload_bytes={} record_only_median_ms={:.3} legacy_samples_ms={:?} record_only_samples_ms={:?}",
        RECORDS * 2,
        encoded_bytes * 2,
        legacy_median.as_secs_f64() * 1_000.0,
        RECORDS,
        encoded_bytes,
        record_only_median.as_secs_f64() * 1_000.0,
        legacy_samples
            .iter()
            .map(|sample| sample.0.as_secs_f64() * 1_000.0)
            .collect::<Vec<_>>(),
        record_only_samples
            .iter()
            .map(|sample| sample.0.as_secs_f64() * 1_000.0)
            .collect::<Vec<_>>()
    );
}
