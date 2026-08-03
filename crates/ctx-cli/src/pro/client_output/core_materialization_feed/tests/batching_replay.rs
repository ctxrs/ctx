use super::*;

#[test]
fn event_delta_batches_use_exact_one_sixteen_and_seventeen_page_boundaries() {
    for (page_count, expected_batch_sizes) in [(1, vec![1]), (16, vec![16]), (17, vec![16, 1])] {
        let source = source(&format!("batch-count-{page_count}.jsonl"));
        let batches = event_page_batches(single_delta_event_pages(&source, page_count, 1));
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            expected_batch_sizes
        );
        let page_ids = batches
            .iter()
            .flatten()
            .map(|page| page.page_index)
            .collect::<Vec<_>>();
        assert_eq!(
            page_ids,
            (0..u32::try_from(page_count).unwrap()).collect::<Vec<_>>()
        );
        for batch in batches {
            ApplyCoreEventDeltaPagesRequest { pages: batch }
                .validate()
                .unwrap();
        }
    }
}

#[test]
fn event_delta_batch_splits_before_the_exact_aggregate_byte_ceiling() {
    let source = source("batch-byte-ceiling.jsonl");
    let body_bytes = MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES / 5 + 1;
    assert!(body_bytes < MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES);
    let pages = single_delta_event_pages(&source, 5, body_bytes);
    let empty_request_bytes =
        encoded_json_len(&ApplyCoreEventDeltaPagesRequest { pages: Vec::new() }).unwrap();
    let prospective_bytes = |count: usize| {
        empty_request_bytes
            + pages
                .iter()
                .take(count)
                .map(|page| encoded_json_len(page).unwrap())
                .sum::<usize>()
            + count.saturating_sub(1)
    };
    assert!(prospective_bytes(4) <= MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES);
    assert!(prospective_bytes(5) > MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES);
    let batches = event_page_batches(pages);
    assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), vec![4, 1]);
    for batch in batches {
        let request = ApplyCoreEventDeltaPagesRequest { pages: batch };
        assert!(
            encoded_json_len(&request).unwrap() <= MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES
        );
        request.validate().unwrap();
    }
}

#[test]
fn plural_bounds_retries_are_deterministically_bisected_in_order() {
    let source = source("batch-bisection.jsonl");
    let pages = single_delta_event_pages(&source, 5, 1);
    let mut attempts = Vec::<Vec<u32>>::new();
    let mut applied = Vec::<u32>::new();
    apply_batched_event_delta_pages_with(pages, &mut |message, _remaining| {
        let HostMessage::ApplyCoreEventDeltaPages(request) = message else {
            panic!("batch mode emitted a singular request")
        };
        attempts.push(request.pages.iter().map(|page| page.page_index).collect());
        if request.pages.len() > 2 {
            return Ok(HelperMessage::Error(
                ctx_pro_host_protocol::ProtocolError::new(
                    ErrorClass::Bounds,
                    "synthetic pre-mutation bound",
                ),
            ));
        }
        applied.extend(request.pages.iter().map(|page| page.page_index));
        Ok(successful_plural_response(message))
    })
    .unwrap();
    assert_eq!(
        attempts,
        vec![
            vec![0, 1, 2, 3, 4],
            vec![0, 1],
            vec![2, 3, 4],
            vec![2],
            vec![3, 4]
        ]
    );
    assert_eq!(applied, vec![0, 1, 2, 3, 4]);
}

#[test]
fn plural_bounds_fake_clock_exhausts_one_aggregate_deadline() {
    let source = source("batch-aggregate-deadline.jsonl");
    let pages = single_delta_event_pages(&source, 5, 1);
    let started = Instant::now();
    let elapsed = Rc::new(Cell::new(Duration::ZERO));
    let clock_elapsed = Rc::clone(&elapsed);
    let exchange_elapsed = Rc::clone(&elapsed);
    let mut remaining = Vec::new();

    let error = apply_batched_event_delta_pages_with_budget(
        pages,
        &mut |message, timeout| {
            remaining.push(timeout);
            exchange_elapsed.set(exchange_elapsed.get() + Duration::from_millis(25));
            let HostMessage::ApplyCoreEventDeltaPages(request) = message else {
                unreachable!()
            };
            if request.pages.len() > 2 {
                Ok(HelperMessage::Error(
                    ctx_pro_host_protocol::ProtocolError::new(
                        ErrorClass::Bounds,
                        "synthetic pre-mutation bound",
                    ),
                ))
            } else {
                Ok(successful_plural_response(message))
            }
        },
        started + Duration::from_millis(100),
        move || started + clock_elapsed.get(),
        || false,
    )
    .unwrap_err();

    assert_eq!(remaining, [100, 75, 50, 25].map(Duration::from_millis));
    assert_eq!(
        error.to_string(),
        "helper_timeout: Core event delta batch operation exceeded its aggregate deadline"
    );
}

#[test]
fn plural_bounds_cancellation_after_left_subtree_skips_right_subtree() {
    let source = source("batch-left-cancellation.jsonl");
    let pages = single_delta_event_pages(&source, 4, 1);
    let started = Instant::now();
    let cancelled = Rc::new(Cell::new(false));
    let exchange_cancelled = Rc::clone(&cancelled);
    let budget_cancelled = Rc::clone(&cancelled);
    let mut attempts = Vec::new();

    let error = apply_batched_event_delta_pages_with_budget(
        pages,
        &mut |message, _remaining| {
            let HostMessage::ApplyCoreEventDeltaPages(request) = message else {
                unreachable!()
            };
            let page_ids = request
                .pages
                .iter()
                .map(|page| page.page_index)
                .collect::<Vec<_>>();
            attempts.push(page_ids.clone());
            if page_ids == [0, 1] {
                exchange_cancelled.set(true);
                Ok(successful_plural_response(message))
            } else {
                Ok(HelperMessage::Error(
                    ctx_pro_host_protocol::ProtocolError::new(
                        ErrorClass::Bounds,
                        "synthetic pre-mutation bound",
                    ),
                ))
            }
        },
        started + BATCH_TIMEOUT,
        Instant::now,
        move || budget_cancelled.get(),
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "helper_cancelled: Core event delta batch operation cancelled"
    );
    assert_eq!(attempts, vec![vec![0, 1, 2, 3], vec![0, 1]]);
}

#[test]
fn plural_bounds_full_sixteen_page_tree_uses_exact_structural_attempt_bound() {
    let source = source("batch-structural-bound.jsonl");
    let pages = single_delta_event_pages(&source, MAX_CORE_EVENT_DELTA_PAGES, 1);
    let mut attempts = 0;
    apply_batched_event_delta_pages_with(pages, &mut |message, _remaining| {
        attempts += 1;
        let HostMessage::ApplyCoreEventDeltaPages(request) = message else {
            unreachable!()
        };
        if request.pages.len() == 1 {
            Ok(successful_plural_response(message))
        } else {
            Ok(HelperMessage::Error(
                ctx_pro_host_protocol::ProtocolError::new(
                    ErrorClass::Bounds,
                    "synthetic pre-mutation bound",
                ),
            ))
        }
    })
    .unwrap();
    assert_eq!(attempts, MAX_CORE_EVENT_DELTA_BATCH_EXCHANGES);
}

#[test]
fn plural_singleton_bounds_is_fatal_without_identity_change() {
    let source = source("batch-singleton-bounds.jsonl");
    let pages = single_delta_event_pages(&source, 1, 1);
    let expected_page = pages[0].clone();
    let mut observed = None;
    let error = apply_batched_event_delta_pages_with(pages, &mut |message, _remaining| {
        let HostMessage::ApplyCoreEventDeltaPages(request) = message else {
            panic!("batch mode emitted a singular request")
        };
        observed = Some(request.pages[0].clone());
        Ok(HelperMessage::Error(
            ctx_pro_host_protocol::ProtocolError::new(
                ErrorClass::Bounds,
                "synthetic singleton bound",
            ),
        ))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "invalid_request");
    assert_eq!(observed.unwrap(), expected_page);
}

#[test]
fn plural_acknowledgement_order_and_session_message_kind_fail_closed() {
    let source = source("batch-ack-order.jsonl");
    let pages = single_delta_event_pages(&source, 2, 1);
    let order_error =
        apply_batched_event_delta_pages_with(pages.clone(), &mut |message, _remaining| {
            let HelperMessage::CoreEventDeltaPagesApplied(mut response) =
                successful_plural_response(message)
            else {
                unreachable!()
            };
            response.pages.reverse();
            Ok(HelperMessage::CoreEventDeltaPagesApplied(response))
        })
        .unwrap_err();
    assert!(order_error.to_string().starts_with("invalid_response:"));

    let mode_error = apply_batched_event_delta_pages_with(pages, &mut |message, _remaining| {
        let HostMessage::ApplyCoreEventDeltaPages(request) = message else {
            unreachable!()
        };
        Ok(HelperMessage::CoreEventDeltaPageApplied(applied_page(
            &request.pages[0],
        )))
    })
    .unwrap_err();
    assert_eq!(
        mode_error.to_string(),
        "invalid_response: helper returned a non-Core-event-delta-batch response"
    );
}

#[test]
#[ignore = "focused host exchange-count microbenchmark"]
fn host_event_delta_exchange_microbenchmark() {
    #[derive(serde::Serialize)]
    struct SingularRequestRef<'a> {
        page: &'a CoreEventDeltaPage,
    }

    let source = source("host-exchange-benchmark.jsonl");
    let pages = single_delta_event_pages(&source, 64, 1024);
    let singular_request_bytes = pages
        .iter()
        .map(|page| encoded_json_len(&SingularRequestRef { page }).unwrap())
        .sum::<usize>();
    let batches = event_page_batches(pages);
    let batched_exchanges = batches.len();
    let batched_request_bytes = batches
        .into_iter()
        .map(|pages| encoded_json_len(&ApplyCoreEventDeltaPagesRequest { pages }).unwrap())
        .sum::<usize>();
    eprintln!(
        "host_event_delta_exchange_benchmark pages=64 singular_exchanges=64 batched_exchanges={} singular_request_bytes={} batched_request_bytes={}",
        batched_exchanges,
        singular_request_bytes,
        batched_request_bytes,
    );
    assert_eq!(batched_exchanges, 4);
    assert!(batched_request_bytes < singular_request_bytes);
}

pub(super) fn benchmark_page_builder(
    name: &str,
    reconciliation: &CoreSourceReconciliation,
    deltas: Vec<CoreEventDelta>,
    builder: fn(&CoreSourceReconciliation, Vec<CoreEventDelta>) -> Result<Vec<CoreEventDeltaPage>>,
) -> Duration {
    let started = Instant::now();
    let pages = builder(reconciliation, black_box(deltas)).unwrap();
    black_box(pages);
    let elapsed = started.elapsed();
    eprintln!(
        "event_page_builder implementation={name} elapsed_ms={:.3}",
        elapsed.as_secs_f64() * 1_000.0
    );
    elapsed
}

#[test]
fn source_page_admission_uses_exact_wire_index_at_decimal_transitions() {
    let materialization_id = "d".repeat(64);
    let generation_id = "a".repeat(64);

    for transition in [9_u32, 99] {
        let count = usize::try_from(transition).unwrap() * 2 + 5;
        let deltas = ordered_source_deltas(count, 32);
        let pair_start = usize::try_from(transition).unwrap() * 2;
        let exact_boundary_page = CoreSourceDeltaPage::new(
            &materialization_id,
            &generation_id,
            transition,
            false,
            deltas[pair_start..pair_start + 2].to_vec(),
        )
        .unwrap();
        let maximum_wire_bytes = serde_json::to_vec(&exact_boundary_page).unwrap().len();

        let pages = build_delta_pages_with_wire_bound(
            &materialization_id,
            &generation_id,
            deltas.clone(),
            maximum_wire_bytes,
        )
        .unwrap();
        let repeated = build_delta_pages_with_wire_bound(
            &materialization_id,
            &generation_id,
            deltas,
            maximum_wire_bytes,
        )
        .unwrap();

        assert_eq!(pages, repeated);
        assert_eq!(
            pages[usize::try_from(transition).unwrap()],
            exact_boundary_page
        );
        assert_eq!(
            pages[usize::try_from(transition + 1).unwrap()].deltas.len(),
            1
        );
        assert_eq!(
            pages[usize::try_from(transition + 2).unwrap()].deltas.len(),
            2
        );
        assert_eq!(
            serde_json::to_vec(&pages[usize::try_from(transition + 2).unwrap()])
                .unwrap()
                .len(),
            maximum_wire_bytes
        );
        assert_eq!(
            pages.last().map(|page| page.page_index),
            Some(transition + 2)
        );
        for (expected_index, page) in pages.iter().enumerate() {
            assert_eq!(page.page_index, u32::try_from(expected_index).unwrap());
            assert_eq!(page.terminal, expected_index + 1 == pages.len());
            assert!(serde_json::to_vec(page).unwrap().len() <= maximum_wire_bytes);
            page.validate().unwrap();
        }
    }
}

#[test]
fn source_page_admission_enforces_the_protocol_wire_bound() {
    let materialization_id = "d".repeat(64);
    let generation_id = "a".repeat(64);
    let deltas = ordered_source_deltas(140, 60 * 1024);

    assert_eq!(MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES, 4_194_304);
    let pages = build_delta_pages(&materialization_id, &generation_id, deltas).unwrap();

    assert!(pages.len() > 1);
    for (index, page) in pages.iter().enumerate() {
        let wire_bytes = serde_json::to_vec(page).unwrap().len();
        assert!(wire_bytes <= MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES);
        assert_eq!(page.page_index, u32::try_from(index).unwrap());
        assert_eq!(page.terminal, index + 1 == pages.len());
        page.validate().unwrap();
        if let Some(next) = pages.get(index + 1) {
            let next_delta_bytes = serde_json::to_vec(&next.deltas[0]).unwrap().len();
            assert!(
                page.deltas.len() == MAX_CORE_SOURCE_DELTA_PAGE_ITEMS
                    || wire_bytes + 1 + next_delta_bytes > MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES
            );
        }
    }
}

#[test]
fn source_page_admission_rejects_an_oversized_singleton_with_typed_error() {
    let materialization_id = "d".repeat(64);
    let generation_id = "a".repeat(64);
    let delta = source_delta(0, 32, 1);
    let exact_singleton_bytes = serde_json::to_vec(
        &CoreSourceDeltaPage::new(
            &materialization_id,
            &generation_id,
            0,
            true,
            vec![delta.clone()],
        )
        .unwrap(),
    )
    .unwrap()
    .len();

    let error = build_delta_pages_with_wire_bound(
        &materialization_id,
        &generation_id,
        vec![delta],
        exact_singleton_bytes - 1,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        CoreSourceDeltaPageBuildError::OversizedSingleton
    ));
    assert_eq!(
        error.to_string(),
        "invalid_request: one Core source delta exceeds its wire bound"
    );
}

#[test]
fn same_certificate_and_count_with_changed_core_record_is_reconciled() {
    let temp = tempdir().unwrap();
    let source = source("changed.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(
        &mut writer,
        &source,
        1,
        vec!["one".to_owned(), "two".to_owned(), "three".to_owned()],
    );
    writer.commit(|_| true).unwrap();
    let first = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let first_certificate = first.manifest().sources[0].clone();
    let first_accumulator = first.manifest().core_record_aggregates[0]
        .core_record_accumulator()
        .to_owned();
    let prior = receipt_for(&first, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    sync_core_feed(&first, None, &mut consumer).unwrap();
    drop(first);
    consumer.event_pages.clear();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(
        &mut writer,
        &source,
        1,
        vec![
            "one".to_owned(),
            "two revised".to_owned(),
            "three".to_owned(),
        ],
    );
    writer.commit(|_| true).unwrap();
    let second = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(second.manifest().sources[0], first_certificate);
    assert_ne!(
        second.manifest().core_record_aggregates[0].core_record_accumulator(),
        first_accumulator
    );
    let second_sources = core_source_states(second.manifest()).unwrap();
    let second_head = core_generation_head(&second, &second_sources).unwrap();
    assert_ne!(
        prior.source_snapshot_sha256,
        second_head.source_snapshot_sha256
    );
    let report = sync_core_feed(&second, Some(&prior), &mut consumer).unwrap();

    assert_eq!(report.changed_sources, 1);
    assert_eq!(report.event_mutations, 1);
    assert_eq!(consumer.event_pages.len(), 1);
    assert!(consumer.event_pages[0].terminal);
    assert!(matches!(
        consumer.event_pages[0].deltas.as_slice(),
        [CoreEventDelta::Replaced(replacement)]
            if replacement.record.content.normalized_body.as_deref() == Some("two revised")
    ));
}

#[test]
fn unchanged_large_source_is_not_resent_when_another_source_changes() {
    let temp = tempdir().unwrap();
    let large = source("large.jsonl");
    let changed = source("changed.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(&mut writer, &large, 1, vec!["L".repeat(32 * 1024)]);
    add_source(&mut writer, &changed, 2, vec!["changed body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let states = core_source_states(index.manifest()).unwrap();
    let mut consumer = Consumer::new();
    let large_state = states
        .iter()
        .find(|state| state.source.exact_descriptor_eq(&large))
        .unwrap();
    consumer.known_accumulators.insert(
        large.identity().digest(),
        large_state.core_record_accumulator.clone(),
    );
    consumer
        .known_accumulators
        .insert(changed.identity().digest(), "0".repeat(64));

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    assert_eq!(report.changed_sources, 1);
    assert_eq!(report.event_mutations, 1);
    assert!(consumer.event_pages.iter().all(|page| {
        page.reconciliation
            .delta
            .source()
            .exact_descriptor_eq(&changed)
            && page.deltas.iter().all(|delta| match delta {
                CoreEventDelta::Added(record) => {
                    record.content.normalized_body.as_deref() == Some("changed body")
                }
                _ => false,
            })
    }));
}

#[test]
fn current_replay_is_a_no_op_with_no_delta_or_event_pages() {
    let temp = tempdir().unwrap();
    let source = source("replay.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let prior = receipt_for(&index, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    consumer.replay_begin = true;
    consumer.replay_status_currentness = Some(CoreProjectionCurrentness::Current);
    consumer.last_receipt = Some(prior.clone());

    let report = sync_core_feed(&index, Some(&prior), &mut consumer).unwrap();
    assert!(report.replayed);
    assert!(consumer.delta_pages.is_empty());
    assert!(consumer.event_pages.is_empty());
    let finish = consumer.finish.unwrap();
    assert_eq!(finish.changed_sources, 0);
    assert_eq!(finish.removed_sources, 0);
    assert_eq!(finish.event_delta_pages, 0);
    assert_eq!(finish.event_mutations, 0);
    assert_eq!(consumer.status_exchanges, 2);
}

#[test]
fn missing_route_grace_rollover_finishes_without_source_or_event_mutations() {
    const DELETE_AFTER: u32 = 3;

    let temp = tempdir().unwrap();
    let source = source("missing-route-grace.jsonl");
    let route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(&mut writer, &source, 1, vec!["unchanged body".to_owned()]);
    writer
        .set_present_source_routes(vec![SourceRouteSnapshot::present(
            route.clone(),
            vec![source.clone()],
        )
        .unwrap()])
        .unwrap();
    writer.commit(|_| true).unwrap();
    let initial = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let prior = receipt_for(&initial, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    sync_core_feed(&initial, None, &mut consumer).unwrap();
    drop(initial);

    let prior_accumulators = consumer.known_accumulators.clone();
    let prior_sources = consumer.known_sources.clone();
    let prior_events = consumer.known_events.clone();
    consumer.event_pages.clear();
    consumer.finish = None;

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.set_present_source_routes(Vec::new()).unwrap();
    let grace = writer
        .observe_certified_missing_route(route.clone(), 100, DELETE_AFTER, || true)
        .unwrap();
    assert!(!grace.deleted());
    assert_eq!(grace.retained_sources(), std::slice::from_ref(&source));
    writer.commit(|_| true).unwrap();
    let rollover = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_ne!(rollover.generation_id(), prior.core_generation_id);
    assert_eq!(rollover.manifest().indexed_documents, prior.event_count);
    assert_eq!(
        u32::try_from(rollover.manifest().sources.len()).unwrap(),
        prior.source_count
    );
    assert_eq!(
        rollover
            .manifest()
            .source_route(&route)
            .unwrap()
            .missing_state()
            .unwrap()
            .consecutive_missing()
            .get(),
        1
    );

    let report = sync_core_feed(&rollover, Some(&prior), &mut consumer).unwrap();
    assert!(!report.replayed);
    assert_eq!(report.changed_sources, 0);
    assert_eq!(report.removed_sources, 0);
    assert_eq!(report.event_delta_pages, 0);
    assert_eq!(report.event_mutations, 0);
    assert!(consumer.event_pages.is_empty());
    assert_eq!(consumer.known_accumulators, prior_accumulators);
    assert_eq!(consumer.known_sources, prior_sources);
    assert_eq!(consumer.known_events, prior_events);

    let expected = receipt_for(&rollover, "test-core-materializer-v1");
    assert_eq!(report.receipt, expected);
    assert_eq!(
        report.receipt.source_snapshot_sha256,
        prior.source_snapshot_sha256
    );
    assert_ne!(report.receipt.core_generation_id, prior.core_generation_id);
    let finish = consumer.finish.as_ref().unwrap();
    assert_eq!(finish.head.core_generation_id, rollover.generation_id());
    assert_eq!(finish.changed_sources, 0);
    assert_eq!(finish.removed_sources, 0);
    assert_eq!(finish.event_delta_pages, 0);
    assert_eq!(finish.event_mutations, 0);
}

#[test]
fn partial_materialization_resumes_from_source_page_zero_with_complete_finish_counts() {
    let (_temp, index) = single_source_index("partial-after-source.jsonl", vec!["body".to_owned()]);
    let mut consumer = Consumer::new();
    consumer.source_response_loss_after = Some(1);

    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert_eq!(
        error.to_string(),
        "synthetic_source_response_lost: committed Core source page"
    );
    assert!(consumer.finish.is_none());
    let committed_page = consumer.delta_pages[0].clone();

    consumer.source_response_loss_after = None;
    consumer.replay_begin = true;
    consumer.replay_status_currentness = Some(CoreProjectionCurrentness::Partial);
    let report = sync_core_feed(&index, None, &mut consumer).unwrap();

    assert_eq!(consumer.delta_pages.len(), 1);
    assert_eq!(consumer.delta_pages[0], committed_page);
    assert_eq!(consumer.source_page_applications, 1);
    assert_eq!(
        consumer.source_acknowledgement_requests,
        vec![(0, 0), (0, 0)]
    );
    let acknowledgement = &consumer.source_acknowledgements[&0][0];
    assert_eq!(acknowledgement.page_index, 0);
    assert_eq!(acknowledgement.acknowledgement_page_index, 0);
    assert!(acknowledgement.acknowledgement_terminal);
    assert_eq!(acknowledgement.reconcile_sources.len(), 1);
    assert_eq!(acknowledgement.reconcile_sources[0].materialize_index, 0);
    assert_eq!(consumer.next_materialize_index, 1);
    assert_eq!(report.changed_sources, 1);
    assert_eq!(report.event_delta_pages, 1);
    assert_eq!(report.event_mutations, 1);
    let finish = consumer.finish.as_ref().unwrap();
    assert_eq!(finish.source_delta_pages, 1);
    assert_eq!(finish.changed_sources, 1);
    assert_eq!(finish.event_delta_pages, 1);
    assert_eq!(finish.event_mutations, 1);
}

#[test]
fn partial_materialization_resumes_after_exact_state_page_replay() {
    let (_temp, index) = single_source_index("partial-after-state.jsonl", vec!["body".to_owned()]);
    let mut consumer = Consumer::new();
    consumer.event_state_response_loss_after = Some(1);

    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert_eq!(
        error.to_string(),
        "synthetic_state_response_lost: committed Core event state page"
    );
    assert!(consumer.finish.is_none());
    let committed_request = consumer.state_requests[0].clone();

    consumer.event_state_response_loss_after = None;
    consumer.replay_begin = true;
    consumer.replay_status_currentness = Some(CoreProjectionCurrentness::Partial);
    let report = sync_core_feed(&index, None, &mut consumer).unwrap();

    assert_eq!(consumer.state_requests[1], committed_request);
    assert_eq!(consumer.state_requests[1].page_index, 0);
    assert_eq!(report.changed_sources, 1);
    assert_eq!(report.event_delta_pages, 1);
    assert_eq!(report.event_mutations, 1);
    assert_eq!(consumer.finish.as_ref().unwrap().event_mutations, 1);
}

#[test]
fn partial_materialization_replays_a_committed_event_batch_one_page_per_exchange() {
    let event_count = MAX_CORE_EVENT_DELTA_PAGE_ITEMS + 1;
    let (_temp, index) = single_source_index(
        "partial-after-event-batch.jsonl",
        (0..event_count)
            .map(|event| format!("body {event}"))
            .collect(),
    );
    let mut consumer = Consumer::new();
    consumer.event_response_loss_after = Some(1);

    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert_eq!(
        error.to_string(),
        "synthetic_event_response_lost: committed Core event delta exchange"
    );
    assert_eq!(consumer.event_exchange_page_ids[0].len(), 2);
    assert!(consumer.finish.is_none());

    consumer.event_response_loss_after = None;
    consumer.replay_begin = true;
    consumer.replay_status_currentness = Some(CoreProjectionCurrentness::Partial);
    let report = sync_core_feed(&index, None, &mut consumer).unwrap();

    assert_eq!(report.event_delta_pages, 2);
    assert_eq!(report.event_mutations, u64::try_from(event_count).unwrap());
    assert_eq!(
        consumer.event_exchange_page_ids[1..]
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![1, 1]
    );
    assert_eq!(consumer.finish.as_ref().unwrap().event_delta_pages, 2);
    assert_eq!(
        consumer.finish.as_ref().unwrap().event_mutations,
        u64::try_from(event_count).unwrap()
    );
}

#[test]
fn lost_finish_response_resumes_as_current_without_replaying_pages() {
    let (_temp, index) = single_source_index("lost-finish-response.jsonl", vec!["body".to_owned()]);
    let mut consumer = Consumer::new();
    consumer.lose_finish_response = true;

    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert_eq!(
        error.to_string(),
        "synthetic_finish_response_lost: committed Core finish"
    );
    let committed_receipt = consumer.last_receipt.clone().unwrap();
    let source_calls = consumer.delta_pages.len();
    let state_calls = consumer.state_requests.len();
    let event_calls = consumer.event_pages.len();
    assert_eq!(consumer.finish_requests.len(), 1);
    assert_eq!(consumer.finish_requests[0].changed_sources, 1);
    assert_eq!(consumer.finish_requests[0].event_mutations, 1);

    consumer.replay_begin = true;
    consumer.replay_status_currentness = Some(CoreProjectionCurrentness::Current);
    let report = sync_core_feed(&index, Some(&committed_receipt), &mut consumer).unwrap();

    assert!(report.replayed);
    assert_eq!(consumer.delta_pages.len(), source_calls);
    assert_eq!(consumer.state_requests.len(), state_calls);
    assert_eq!(consumer.event_pages.len(), event_calls);
    assert_eq!(consumer.finish_requests.len(), 2);
    let replay_finish = &consumer.finish_requests[1];
    assert_eq!(replay_finish.source_delta_pages, 0);
    assert_eq!(replay_finish.changed_sources, 0);
    assert_eq!(replay_finish.removed_sources, 0);
    assert_eq!(replay_finish.event_delta_pages, 0);
    assert_eq!(replay_finish.event_mutations, 0);
}

#[test]
fn partial_mixed_committed_prefix_and_new_suffix_is_singleton_bounded() {
    let event_count = MAX_CORE_EVENT_DELTA_PAGE_ITEMS * MAX_CORE_EVENT_DELTA_PAGES + 1;
    let (_temp, index) = single_source_index(
        "partial-mixed-prefix-suffix.jsonl",
        (0..event_count)
            .map(|event| format!("body {event}"))
            .collect(),
    );
    let mut consumer = Consumer::new();
    consumer.event_response_loss_after = Some(1);

    let error = sync_core_feed_with_options(
        &index,
        None,
        &mut consumer,
        CoreFeedExecutionOptions {
            prefetch_parallelism: 4,
        },
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "synthetic_event_response_lost: committed Core event delta exchange"
    );
    assert_eq!(
        consumer.event_exchange_page_ids[0].len(),
        MAX_CORE_EVENT_DELTA_PAGES
    );

    consumer.event_response_loss_after = None;
    consumer.replay_begin = true;
    consumer.replay_status_currentness = Some(CoreProjectionCurrentness::Partial);
    let report = sync_core_feed_with_options(
        &index,
        None,
        &mut consumer,
        CoreFeedExecutionOptions {
            prefetch_parallelism: 4,
        },
    )
    .unwrap();

    let expected_pages = MAX_CORE_EVENT_DELTA_PAGES + 1;
    assert_eq!(
        usize::try_from(report.event_delta_pages).unwrap(),
        expected_pages
    );
    assert_eq!(report.event_mutations, u64::try_from(event_count).unwrap());
    assert_eq!(consumer.event_exchange_page_ids[1..].len(), expected_pages);
    assert!(consumer.event_exchange_page_ids[1..]
        .iter()
        .all(|exchange| exchange.len() == 1));
    assert!(report.prefetch.encoded_credit_high_water_bytes <= CORE_PREFETCH_ENCODED_BYTE_BUDGET);
    assert_eq!(report.prefetch.encoded_credit_final_bytes, 0);
}

#[test]
fn journaled_source_state_and_event_retries_are_exact_and_divergence_fails_closed() {
    let (_temp, index) =
        single_source_index("exact-duplicate-retry.jsonl", vec!["body".to_owned()]);
    let mut consumer = Consumer::new();
    sync_core_feed(&index, None, &mut consumer).unwrap();

    let source_page = consumer.delta_pages[0].clone();
    let source_response = consumer
        .apply_source_delta(ApplyCoreSourceDeltaPageRequest {
            page: source_page.clone(),
            acknowledgement_page_index: 0,
        })
        .unwrap();
    assert!(source_response.replayed);
    let mut divergent_source = source_page;
    divergent_source.terminal = !divergent_source.terminal;
    let error = consumer
        .apply_source_delta(ApplyCoreSourceDeltaPageRequest {
            page: divergent_source,
            acknowledgement_page_index: 0,
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid_request: divergent duplicate Core source delta page"
    );

    let state_request = consumer.state_requests[0].clone();
    let state_response = consumer.event_states(state_request.clone()).unwrap();
    assert!(state_response.replayed);
    let mut divergent_state = state_request;
    divergent_state.maximum_items = divergent_state.maximum_items.saturating_sub(1);
    let error = consumer.event_states(divergent_state).unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid_request: divergent duplicate Core event state page"
    );

    let event_page = consumer.event_pages[0].clone();
    consumer
        .apply_event_delta_pages(vec![event_page.clone()])
        .unwrap();
    let mut divergent_event = event_page;
    let CoreEventDelta::Added(record) = &mut divergent_event.deltas[0] else {
        panic!("expected added fixture event")
    };
    record.content.normalized_body = Some("divergent body".to_owned());
    record.validate_contract().unwrap();
    let error = consumer
        .apply_event_delta_pages(vec![divergent_event])
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid_request: divergent duplicate Core event delta page"
    );
    assert_eq!(consumer.event_journal.len(), 1);
    assert_eq!(
        consumer
            .known_events
            .values()
            .map(BTreeMap::len)
            .sum::<usize>(),
        1
    );
}

#[test]
fn event_item_boundary_batches_pages_without_changing_finish_counts_or_page_ids() {
    let temp = tempdir().unwrap();
    let source = source("item-boundary.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(
        &mut writer,
        &source,
        1,
        (0..=MAX_CORE_EVENT_DELTA_PAGE_ITEMS)
            .map(|index| format!("body {index}"))
            .collect(),
    );
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    assert_eq!(
        report.event_mutations,
        u64::try_from(MAX_CORE_EVENT_DELTA_PAGE_ITEMS + 1).unwrap()
    );
    assert_eq!(report.event_delta_pages, 2);
    assert_eq!(consumer.event_exchanges, 1);
    assert_eq!(
        consumer.event_pages[0].deltas.len(),
        MAX_CORE_EVENT_DELTA_PAGE_ITEMS
    );
    assert_eq!(consumer.event_pages[1].deltas.len(), 1);
    assert!(!consumer.event_pages[0].terminal);
    assert!(consumer.event_pages[1].terminal);
    assert_eq!(
        consumer
            .event_pages
            .iter()
            .map(|page| page.page_index)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(consumer.finish.as_ref().unwrap().event_delta_pages, 2);
    assert_eq!(
        consumer.finish.as_ref().unwrap().event_mutations,
        u64::try_from(MAX_CORE_EVENT_DELTA_PAGE_ITEMS + 1).unwrap()
    );
}

#[test]
fn tiny_sources_scale_event_envelopes_as_ceiling_total_pages_over_sixteen() {
    const SOURCE_COUNT: usize = 1_024;
    let temp = tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    for index in 0..SOURCE_COUNT {
        let source = source(&format!("tiny-source-{index:04}.jsonl"));
        add_source(&mut writer, &source, 1, vec![format!("body {index}")]);
    }
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();

    let report = sync_core_feed_with_options(
        &index,
        None,
        &mut consumer,
        CoreFeedExecutionOptions {
            prefetch_parallelism: 4,
        },
    )
    .unwrap();

    let expected_envelopes = SOURCE_COUNT.div_ceil(MAX_CORE_EVENT_DELTA_PAGES);
    assert_eq!(
        usize::try_from(report.event_delta_pages).unwrap(),
        SOURCE_COUNT
    );
    assert_eq!(
        usize::try_from(report.event_mutations).unwrap(),
        SOURCE_COUNT
    );
    assert_eq!(
        usize::try_from(consumer.event_exchanges).unwrap(),
        expected_envelopes
    );
    assert_eq!(consumer.event_exchange_page_ids.len(), expected_envelopes);
    assert!(consumer
        .event_exchange_page_ids
        .iter()
        .all(|pages| pages.len() == MAX_CORE_EVENT_DELTA_PAGES));
    assert!(consumer
        .event_pages
        .iter()
        .all(|page| page.page_index == 0 && page.terminal));
    assert!(report.prefetch.encoded_credit_high_water_bytes <= CORE_PREFETCH_ENCODED_BYTE_BUDGET);
    assert_eq!(report.prefetch.encoded_credit_final_bytes, 0);
}

#[test]
fn mixed_multi_page_and_tiny_sources_share_globally_bounded_envelopes() {
    const TINY_SOURCES: usize = 31;
    let temp = tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    let large = source("mixed-large-source.jsonl");
    add_source(
        &mut writer,
        &large,
        1,
        (0..=MAX_CORE_EVENT_DELTA_PAGE_ITEMS)
            .map(|index| format!("large body {index}"))
            .collect(),
    );
    for index in 0..TINY_SOURCES {
        let source = source(&format!("mixed-tiny-source-{index:02}.jsonl"));
        add_source(&mut writer, &source, 1, vec![format!("tiny body {index}")]);
    }
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();

    let total_pages = TINY_SOURCES + 2;
    assert_eq!(
        usize::try_from(report.event_delta_pages).unwrap(),
        total_pages
    );
    assert_eq!(
        consumer
            .event_exchange_page_ids
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![16, 16, 1]
    );
    assert!(consumer
        .event_exchange_page_ids
        .iter()
        .any(|pages| { pages.windows(2).any(|pair| pair[0].0 != pair[1].0) }));
    let large_pages = consumer
        .event_pages
        .iter()
        .filter(|page| {
            page.reconciliation
                .delta
                .source()
                .exact_descriptor_eq(&large)
        })
        .collect::<Vec<_>>();
    assert_eq!(large_pages.len(), 2);
    assert_eq!(large_pages[0].page_index, 0);
    assert!(!large_pages[0].terminal);
    assert_eq!(large_pages[1].page_index, 1);
    assert!(large_pages[1].terminal);
    assert!(report.prefetch.encoded_credit_high_water_bytes <= CORE_PREFETCH_ENCODED_BYTE_BUDGET);
    assert_eq!(report.prefetch.encoded_credit_final_bytes, 0);
}
