use super::batching_replay::benchmark_page_builder;
use super::*;

#[test]
fn manifest_preflight_rebuild_required_finish_retries_the_generation_once() {
    let (_temp, index) = single_source_index("rebuild-required-once.jsonl", Vec::new());
    let mut consumer = Consumer::new();
    consumer.rebuild_required_finishes = 1;

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();

    assert_eq!(report.receipt.core_generation_id, index.generation_id());
    assert_eq!(consumer.finish_requests.len(), 2);
    assert!(consumer.finish.is_some());
}

#[test]
fn manifest_preflight_rebuild_required_finish_retry_is_bounded() {
    let (_temp, index) = single_source_index("rebuild-required-bounded.jsonl", Vec::new());
    let mut consumer = Consumer::new();
    consumer.rebuild_required_finishes = 2;

    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();

    assert_eq!(crate::pro::stable_error_code(&error), Some("needs_rebuild"));
    assert_eq!(consumer.finish_requests.len(), 2);
    assert!(consumer.finish.is_none());
}

#[test]
fn incremental_builder_matches_legacy_item_boundaries() {
    for count in [
        0,
        1,
        MAX_CORE_EVENT_DELTA_PAGE_ITEMS - 1,
        MAX_CORE_EVENT_DELTA_PAGE_ITEMS,
        MAX_CORE_EVENT_DELTA_PAGE_ITEMS + 1,
    ] {
        let source = source(&format!("item-edge-{count}.jsonl"));
        let deltas = sorted_additions(&source, vec!["x".to_owned(); count]);
        assert_exact_page_boundary_equivalence(&reconciliation(&source), deltas);
    }
}

#[test]
fn incremental_accounts_exact_protocol_bytes_across_page_index_digits() {
    let source = source("page-index-digits.jsonl");
    let reconciliation = reconciliation(&source);
    let added = record(&source, 1, "plain".to_owned());
    let mut replacement = record(&source, 2, "escaped \0 body".to_owned());
    replacement.content.structured_content = Some(serde_json::json!({
        "arguments": ["one", "two"],
        "control": "\0"
    }));
    replacement.validate_contract().unwrap();
    let tombstone_record = record(&source, 3, "removed".to_owned());
    let mut deltas = vec![
        CoreEventDelta::Added(added),
        CoreEventDelta::Replaced(CoreEventReplacement {
            prior_core_record_sha256: "4".repeat(64),
            record: replacement,
        }),
        CoreEventDelta::Tombstoned(CoreEventTombstone {
            event_id: tombstone_record.event_id,
            prior_core_record_sha256: "5".repeat(64),
        }),
    ];
    deltas.sort_by_key(|delta| delta.event_id().digest());

    for page_index in [0, 9, 10, 99, 100, 999, 1_000, u32::MAX] {
        for terminal in [false, true] {
            let mut builder = EventDeltaPageBuilder::new(
                TEST_MATERIALIZATION_ID,
                TEST_GENERATION_ID,
                &reconciliation,
                page_index,
            )
            .unwrap();
            for delta in deltas.clone() {
                assert!(builder
                    .try_push(PreparedEventDelta::from_typed(delta).unwrap())
                    .unwrap()
                    .is_none());
            }
            let expected_content_bytes = builder.content_bytes;
            let (carried_deltas, carried_wire_bytes) =
                builder.into_deltas_with_wire_bytes(terminal).unwrap();
            let carried_deltas = carried_deltas
                .into_iter()
                .map(PreparedEventDelta::into_typed)
                .collect();
            let page = unvalidated_event_delta_page(
                TEST_MATERIALIZATION_ID,
                TEST_GENERATION_ID,
                &reconciliation,
                page_index,
                terminal,
                carried_deltas,
            );
            assert_eq!(expected_content_bytes, page.content_bytes().unwrap());
            assert_eq!(carried_wire_bytes, serde_json::to_vec(&page).unwrap().len());
            page.validate().unwrap();
        }
    }
}

#[test]
fn prepared_delta_content_bytes_include_mcp_exchange() {
    let source = source("mcp-exchange-content-bytes.jsonl");
    let mut record = record(&source, 1, "body".to_owned());
    record.content.mcp_exchange = Some(McpExchangeContent {
        provider_call_id: "materialization-call".to_owned(),
        invocation: None,
        response: Some(McpTerminalResponseContent {
            status: McpTerminalStatus::Succeeded,
            failure_kind: None,
            duration_ns: Some(42),
            text: McpTextCapture::Absent,
            payload: McpJsonCapture::Present {
                value: serde_json::json!({
                    "result": ["complete", null, {"unicode": "雪"}],
                }),
            },
        }),
    });
    record.validate_contract().unwrap();
    let expected = record.content.encoded_content_bytes().unwrap();

    let prepared = PreparedEventDelta::from_typed(CoreEventDelta::Added(record)).unwrap();

    assert_eq!(prepared.content_bytes(), expected);
    assert!(prepared.content_bytes() > "body".len());
}

#[test]
fn batch_builder_carried_lengths_match_exact_request_encoding_after_every_page() {
    let source = source("batch-carried-lengths.jsonl");
    let pages = single_delta_event_pages(&source, MAX_CORE_EVENT_DELTA_PAGES, 17);
    let mut builder = EventDeltaPageBatchBuilder::new().unwrap();
    let mut exact_pages = Vec::new();

    for page in pages {
        exact_pages.push(page.clone());
        assert!(builder
            .try_push(prepared_event_delta_page_from_typed(page).unwrap())
            .unwrap()
            .is_none());
        let exact = encoded_json_len(&ApplyCoreEventDeltaPagesRequest {
            pages: exact_pages.clone(),
        })
        .unwrap();
        assert_eq!(builder.wire_bytes, exact);
    }
}

#[test]
fn prepared_request_reuses_stored_core_json_and_matches_legacy_frames_byte_for_byte() {
    let fixture_name = "prepared-frame-parity.jsonl";
    let (_temp, index) = single_source_index(
        fixture_name,
        vec![
            "added \0 \"body\"".to_owned(),
            "replacement ☃".to_owned(),
            "removed".to_owned(),
        ],
    );
    let source = source(fixture_name);
    let reconciliation = reconciliation(&source);
    let stored_page = index
        .stored_core_source_event_page_with_budget(
            &source,
            None,
            3,
            ctx_history_index::DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )
        .unwrap();
    let mut items = stored_page.items.into_iter();
    let added = items.next().unwrap();
    let replacement = items.next().unwrap();
    let tombstoned = items.next().unwrap();
    assert!(items.next().is_none());
    let expected_stored_bytes = added.stored_json.encoded_core_record().unwrap().len()
        + replacement.stored_json.encoded_core_record().unwrap().len();
    let added_record = added.core_record.clone();
    let replacement_record = replacement.core_record.clone();
    let tombstoned_event_id = tombstoned.core_record.event_id;
    let added_sha256 =
        core_record_sha256_from_encoded(added.stored_json.encoded_core_record().unwrap());
    let replacement_sha256 =
        core_record_sha256_from_encoded(replacement.stored_json.encoded_core_record().unwrap());
    let prepared_deltas = vec![
        PreparedEventDelta::added(PreparedCurrentRecord {
            record: added.core_record,
            stored_json: PreparedCoreRecordJson::Stored(added.stored_json),
            core_record_sha256: added_sha256,
        }),
        PreparedEventDelta::replaced(
            "4".repeat(64),
            PreparedCurrentRecord {
                record: replacement.core_record,
                stored_json: PreparedCoreRecordJson::Stored(replacement.stored_json),
                core_record_sha256: replacement_sha256,
            },
        ),
        PreparedEventDelta::tombstoned(CoreEventTombstone {
            event_id: tombstoned_event_id,
            prior_core_record_sha256: "5".repeat(64),
        }),
    ];
    let expected_deltas = vec![
        CoreEventDelta::Added(added_record),
        CoreEventDelta::Replaced(CoreEventReplacement {
            prior_core_record_sha256: "4".repeat(64),
            record: replacement_record,
        }),
        CoreEventDelta::Tombstoned(CoreEventTombstone {
            event_id: tombstoned_event_id,
            prior_core_record_sha256: "5".repeat(64),
        }),
    ];
    let page = event_delta_page(
        TEST_MATERIALIZATION_ID,
        TEST_GENERATION_ID,
        &reconciliation,
        0,
        true,
        expected_deltas,
    )
    .unwrap();
    let expected_request = ApplyCoreEventDeltaPagesRequest {
        pages: vec![page.clone()],
    };
    expected_request.validate().unwrap();

    reset_prepared_record_body_writes();
    let mut builder = EventDeltaPageBatchBuilder::new().unwrap();
    let mut page_builder = EventDeltaPageBuilder::new(
        TEST_MATERIALIZATION_ID,
        TEST_GENERATION_ID,
        &reconciliation,
        0,
    )
    .unwrap();
    for delta in prepared_deltas {
        assert!(page_builder.try_push(delta).unwrap().is_none());
    }
    let (prepared_deltas, wire_bytes) = page_builder.into_deltas_with_wire_bytes(true).unwrap();
    builder
        .push_empty_overflow(
            prepared_event_delta_page(
                TEST_MATERIALIZATION_ID,
                TEST_GENERATION_ID,
                &reconciliation,
                0,
                true,
                prepared_deltas,
                wire_bytes,
            )
            .unwrap(),
        )
        .unwrap();
    let prepared = builder.take_request().unwrap();
    assert_eq!(prepared_record_body_writes(), 0);
    let canonical_request = serde_json::to_vec(&expected_request).unwrap();
    let mut manual_request = Vec::new();
    prepared.write_request_json(&mut manual_request).unwrap();
    assert_eq!(prepared_record_body_writes(), 2);
    assert_eq!(manual_request, canonical_request);
    assert_eq!(prepared.encoded_request_bytes(), canonical_request.len());
    assert_eq!(
        serde_json::from_slice::<ApplyCoreEventDeltaPagesRequest>(&manual_request).unwrap(),
        expected_request
    );
    assert_eq!(
        prepared.acknowledgement_identity().unwrap(),
        expected_request.acknowledgement_identity().unwrap()
    );
    assert_eq!(
        prepared.core_record_encoding_counters().unwrap(),
        CoreRecordEncodingCounters {
            canonical_serializations: 0,
            canonical_serialized_bytes: 0,
            stored_values_reused: 2,
            stored_bytes_reused: expected_stored_bytes,
        }
    );

    let request_id = uuid::Uuid::from_u128(0x1234);
    for sequence in [0, 9, 10, 99, 100, 999, 1_000, u64::MAX] {
        let expected_envelope = ctx_pro_host_protocol::HostEnvelope {
            sequence,
            request_id,
            message: HostMessage::ApplyCoreEventDeltaPages(expected_request.clone()),
        };
        let mut canonical_frame = Vec::new();
        ctx_pro_host_protocol::write_frame(&mut canonical_frame, &expected_envelope).unwrap();
        reset_prepared_record_body_writes();
        let mut prepared_frame = Vec::new();
        prepared
            .write_frame(&mut prepared_frame, sequence, request_id)
            .unwrap();
        assert_eq!(prepared_record_body_writes(), 2);
        assert_eq!(prepared_frame, canonical_frame);
        assert_eq!(
            prepared_event_delta_pages_frame_payload_bytes(
                sequence,
                prepared.encoded_request_bytes()
            )
            .unwrap(),
            canonical_frame.len() - ctx_pro_host_protocol::FRAME_HEADER_BYTES
        );
        let decoded: ctx_pro_host_protocol::HostEnvelope =
            ctx_pro_host_protocol::read_frame(&mut prepared_frame.as_slice()).unwrap();
        assert_eq!(decoded, expected_envelope);
        let HostMessage::ApplyCoreEventDeltaPages(request) = &decoded.message else {
            panic!("prepared frame decoded as the wrong request kind")
        };
        request.validate().unwrap();
    }
}

#[test]
fn prepared_multi_page_split_retry_uses_carried_lengths_and_single_pass_frames() {
    let source = source("prepared-split-retry.jsonl");
    let pages = single_delta_event_pages(&source, 3, 17);
    reset_prepared_record_body_writes();
    let mut builder = EventDeltaPageBatchBuilder::new().unwrap();
    for page in pages {
        assert!(builder
            .try_push(prepared_event_delta_page_from_typed(page).unwrap())
            .unwrap()
            .is_none());
    }
    let request = builder.take_request().unwrap();
    assert_eq!(prepared_record_body_writes(), 0);

    let mut attempts = Vec::new();
    let request_id = uuid::Uuid::from_u128(0x5678);
    reset_prepared_record_body_writes();
    apply_prepared_batched_event_delta_pages_with(request, &mut |request, _remaining| {
        let sequence = u64::try_from(attempts.len()).unwrap();
        let mut frame = Vec::new();
        request
            .write_frame(&mut frame, sequence, request_id)
            .unwrap();
        let envelope: ctx_pro_host_protocol::HostEnvelope =
            ctx_pro_host_protocol::read_frame(&mut frame.as_slice()).unwrap();
        let HostMessage::ApplyCoreEventDeltaPages(typed) = &envelope.message else {
            panic!("prepared frame decoded as the wrong request kind")
        };
        attempts.push(
            typed
                .pages
                .iter()
                .map(|page| page.page_index)
                .collect::<Vec<_>>(),
        );
        let mut canonical = Vec::new();
        ctx_pro_host_protocol::write_frame(&mut canonical, &envelope).unwrap();
        assert_eq!(frame, canonical);
        if typed.pages.len() > 1 {
            Ok(HelperMessage::Error(
                ctx_pro_host_protocol::ProtocolError::new(
                    ErrorClass::Bounds,
                    "synthetic pre-mutation bound",
                ),
            ))
        } else {
            Ok(successful_plural_response(&envelope.message))
        }
    })
    .unwrap();

    assert_eq!(
        attempts,
        vec![vec![0, 1, 2], vec![0], vec![1, 2], vec![1], vec![2]]
    );
    assert_eq!(prepared_record_body_writes(), 8);
}

#[test]
fn carried_length_overflow_paths_remain_typed_and_bounded() {
    let frame_error =
        prepared_event_delta_pages_frame_payload_bytes(u64::MAX, usize::MAX).unwrap_err();
    assert_eq!(
        frame_error.to_string(),
        "invalid_request: Core event frame length overflowed"
    );

    let source = source("carried-length-overflow.jsonl");
    let reconciliation = reconciliation(&source);
    let mut builder = EventDeltaPageBuilder::new(
        TEST_MATERIALIZATION_ID,
        TEST_GENERATION_ID,
        &reconciliation,
        0,
    )
    .unwrap();
    builder.wire_bytes = usize::MAX;
    let delta = PreparedEventDelta::from_typed(CoreEventDelta::Added(record(
        &source,
        1,
        "body".to_owned(),
    )))
    .unwrap();
    assert!(builder.try_push(delta).unwrap().is_some());
    assert!(builder.is_empty());
}

#[test]
fn incremental_builder_matches_legacy_content_boundaries() {
    let source = source("content-edges.jsonl");
    let half = MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES / 2;
    for sizes in [
        vec![MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES],
        vec![half, MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES - half],
        vec![half, MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES - half + 1],
        vec![MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES - 1, 1, 1],
    ] {
        let deltas = sorted_additions(
            &source,
            sizes.into_iter().map(|bytes| "x".repeat(bytes)).collect(),
        );
        assert_exact_page_boundary_equivalence(&reconciliation(&source), deltas);
    }
}

#[test]
fn incremental_builder_matches_legacy_exact_wire_boundary() {
    let source = source("wire-edge.jsonl");
    let reconciliation = reconciliation(&source);
    let first_body_bytes = 6 * 1024 * 1024;
    let first = CoreEventDelta::Added(record(&source, 1, "\0".repeat(first_body_bytes)));
    let second_unit = CoreEventDelta::Added(record(&source, 2, "\0".to_owned()));
    let empty_page = unvalidated_event_delta_page(
        TEST_MATERIALIZATION_ID,
        TEST_GENERATION_ID,
        &reconciliation,
        0,
        false,
        Vec::new(),
    );
    let fixed_wire_bytes = serde_json::to_vec(&empty_page).unwrap().len()
        + serde_json::to_vec(&first).unwrap().len()
        + 1;
    let second_unit_wire_bytes = serde_json::to_vec(&second_unit).unwrap().len();
    let available = MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES
        .checked_sub(fixed_wire_bytes)
        .unwrap();
    let fitting_second_body_bytes = 1 + available.checked_sub(second_unit_wire_bytes).unwrap() / 6;
    assert!(first_body_bytes + fitting_second_body_bytes < MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES);

    let fitting_second =
        CoreEventDelta::Added(record(&source, 2, "\0".repeat(fitting_second_body_bytes)));
    let mut fitting = vec![first.clone(), fitting_second];
    fitting.sort_by_key(|delta| delta.event_id().digest());
    let fitting_page = event_delta_page(
        TEST_MATERIALIZATION_ID,
        TEST_GENERATION_ID,
        &reconciliation,
        0,
        false,
        fitting.clone(),
    )
    .unwrap();
    let fitting_wire_bytes = serde_json::to_vec(&fitting_page).unwrap().len();
    assert!(fitting_wire_bytes <= MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES);
    assert!(MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES - fitting_wire_bytes < 6);

    let overflowing_second = CoreEventDelta::Added(record(
        &source,
        2,
        "\0".repeat(fitting_second_body_bytes + 1),
    ));
    let mut overflowing = vec![first, overflowing_second];
    overflowing.sort_by_key(|delta| delta.event_id().digest());
    assert!(event_delta_page(
        TEST_MATERIALIZATION_ID,
        TEST_GENERATION_ID,
        &reconciliation,
        0,
        false,
        overflowing.clone(),
    )
    .unwrap_err()
    .to_string()
    .contains("wire bound"));
    assert_exact_page_boundary_equivalence(&reconciliation, overflowing);
}

#[test]
#[ignore = "focused page-builder microbenchmark"]
fn event_page_builder_worst_case_microbenchmark() {
    let source = source("builder-benchmark.jsonl");
    let reconciliation = reconciliation(&source);
    let body_bytes = MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES / MAX_CORE_EVENT_DELTA_PAGE_ITEMS;
    let deltas = sorted_additions(
        &source,
        vec!["x".repeat(body_bytes); MAX_CORE_EVENT_DELTA_PAGE_ITEMS],
    );
    let implementation =
        std::env::var("CTX_EVENT_PAGE_BUILDER_BENCH").unwrap_or_else(|_| "compare".to_owned());
    let legacy = matches!(implementation.as_str(), "legacy" | "compare").then(|| {
        benchmark_page_builder(
            "legacy",
            &reconciliation,
            deltas.clone(),
            legacy_event_delta_pages,
        )
    });
    let incremental = matches!(implementation.as_str(), "incremental" | "compare").then(|| {
        benchmark_page_builder(
            "incremental",
            &reconciliation,
            deltas,
            incremental_event_delta_pages,
        )
    });
    if let (Some(legacy), Some(incremental)) = (legacy, incremental) {
        eprintln!(
            "event_page_builder speedup={:.2}x",
            legacy.as_secs_f64() / incremental.as_secs_f64()
        );
    }
}

#[test]
fn source_deletion_is_resumable_tombstone_pages() {
    let temp = tempdir().unwrap();
    let retained = source("retained.jsonl");
    let removed = source("removed.jsonl");
    let removed_count = MAX_CORE_EVENT_DELTA_PAGE_ITEMS + 1;
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(&mut writer, &retained, 1, vec!["retained".to_owned()]);
    add_source(
        &mut writer,
        &removed,
        1,
        (0..removed_count)
            .map(|index| format!("removed {index}"))
            .collect(),
    );
    writer.commit(|_| true).unwrap();
    let prior_index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let prior = receipt_for(&prior_index, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    sync_core_feed(&prior_index, None, &mut consumer).unwrap();
    drop(prior_index);
    consumer.event_pages.clear();

    let observation = SourceInventoryObservation::new(
        removed.provider(),
        "fixture-root",
        TypedKey::utf8("fixture-authority").unwrap(),
        "inventory-v1",
        vec![2],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "discovery-v1",
        vec![retained.clone()],
    )
    .unwrap();
    let deletion = CertifiedSourceDeletion::from_inventory(removed.clone(), &inventory).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.delete_source(deletion, inventory).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let report = sync_core_feed(&index, Some(&prior), &mut consumer).unwrap();
    assert_eq!(report.changed_sources, 0);
    assert_eq!(report.removed_sources, 1);
    assert_eq!(
        report.event_mutations,
        u64::try_from(removed_count).unwrap()
    );
    assert_eq!(report.event_delta_pages, 2);
    assert!(consumer.event_pages.iter().all(|page| {
        page.reconciliation
            .delta
            .source()
            .exact_descriptor_eq(&removed)
            && page
                .deltas
                .iter()
                .all(|delta| matches!(delta, CoreEventDelta::Tombstoned(_)))
    }));
    assert!(!consumer
        .known_events
        .contains_key(&removed.identity().digest()));
}

#[test]
fn changed_current_then_lower_removed_source_flushes_before_source_order_reversal() {
    let temp = tempdir().unwrap();
    let mut candidates = (0..32)
        .map(|index| source(&format!("mixed-removal-order-{index:02}.jsonl")))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|source| source.identity().digest());
    let removed = candidates.first().unwrap().clone();
    let current = candidates.last().unwrap().clone();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(&mut writer, &removed, 1, vec!["removed prior".to_owned()]);
    add_source(&mut writer, &current, 1, vec!["current prior".to_owned()]);
    writer.commit(|_| true).unwrap();
    let prior_index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let prior = receipt_for(&prior_index, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    sync_core_feed(&prior_index, None, &mut consumer).unwrap();
    drop(prior_index);
    consumer.event_pages.clear();
    consumer.event_exchange_page_ids.clear();

    let observation = SourceInventoryObservation::new(
        removed.provider(),
        "fixture-root",
        TypedKey::utf8("fixture-authority").unwrap(),
        "inventory-v1",
        vec![2],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "discovery-v1",
        vec![current.clone()],
    )
    .unwrap();
    let deletion = CertifiedSourceDeletion::from_inventory(removed.clone(), &inventory).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(
        &mut writer,
        &current,
        2,
        vec!["current replacement".to_owned()],
    );
    writer.delete_source(deletion, inventory).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let report = sync_core_feed(&index, Some(&prior), &mut consumer).unwrap();
    assert_eq!(report.changed_sources, 1);
    assert_eq!(report.removed_sources, 1);
    assert_eq!(report.event_mutations, 2);
    assert_eq!(consumer.event_exchange_page_ids.len(), 2);
    assert_eq!(
        consumer.event_exchange_page_ids[0][0].0,
        current.identity().digest()
    );
    assert_eq!(
        consumer.event_exchange_page_ids[1][0].0,
        removed.identity().digest()
    );
}

#[test]
fn large_native_key_removals_without_receipt_drive_all_acknowledgements_idempotently() {
    const REMOVAL_COUNT: usize = 2_048;
    let temp = tempdir().unwrap();
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();
    for source_index in 0..REMOVAL_COUNT {
        let removed = large_native_key_source(source_index);
        consumer
            .known_sources
            .insert(removed.identity().digest(), removed);
    }

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    let expected_acknowledgement_pages = REMOVAL_COUNT.div_ceil(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS);
    assert_eq!(report.changed_sources, 0);
    assert_eq!(
        report.removed_sources,
        u64::try_from(REMOVAL_COUNT).unwrap()
    );
    assert_eq!(consumer.source_page_applications, 1);
    assert_eq!(consumer.delta_pages.len(), 1);
    assert!(consumer.delta_pages[0].terminal);
    assert!(consumer.delta_pages[0].deltas.is_empty());
    assert_eq!(
        consumer.finish.as_ref().unwrap().removed_sources,
        u32::try_from(REMOVAL_COUNT).unwrap()
    );
    assert_eq!(
        consumer.source_exchanges,
        u64::try_from(expected_acknowledgement_pages).unwrap()
    );
    assert_eq!(
        consumer.source_acknowledgement_requests,
        (0..expected_acknowledgement_pages)
            .map(|index| (0, u32::try_from(index).unwrap()))
            .collect::<Vec<_>>()
    );
    let responses = consumer.source_acknowledgements.get(&0).unwrap();
    assert_eq!(responses.len(), expected_acknowledgement_pages);
    assert_eq!(
        responses
            .iter()
            .map(|response| usize::try_from(response.removed_sources).unwrap())
            .sum::<usize>(),
        REMOVAL_COUNT
    );
    for (index, response) in responses.iter().enumerate() {
        assert_eq!(
            response.acknowledgement_page_index,
            u32::try_from(index).unwrap()
        );
        assert_eq!(
            response.acknowledgement_terminal,
            index + 1 == expected_acknowledgement_pages
        );
        assert_eq!(
            response.reconcile_sources.len(),
            MAX_CORE_SOURCE_DELTA_PAGE_ITEMS
        );
        for (item_index, reconciliation) in response.reconcile_sources.iter().enumerate() {
            let materialize_index = index
                .saturating_mul(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS)
                .saturating_add(item_index);
            assert_eq!(
                reconciliation.materialize_index,
                u32::try_from(materialize_index).unwrap(),
                "acknowledgement page {index} item {item_index}"
            );
            assert!(matches!(reconciliation.delta, CoreSourceDelta::Removed(_)));
        }
        assert!(serde_json::to_vec(response).unwrap().len() <= MAX_CORE_CONTROL_WIRE_BYTES);
        let mut frame = Vec::new();
        write_frame(
            &mut frame,
            &HelperEnvelope {
                sequence: u64::try_from(index).unwrap(),
                request_id: Uuid::from_u128(1),
                message: HelperMessage::CoreSourceDeltaPageApplied(response.clone()),
            },
        )
        .unwrap();
        assert!(frame.len() - FRAME_HEADER_BYTES <= MAX_FRAME_PAYLOAD_BYTES);
    }

    let source_page = consumer.delta_pages[0].clone();
    assert!(source_page.terminal);
    assert!(source_page.deltas.is_empty());
    let original_responses = responses.clone();
    for (index, expected) in original_responses.into_iter().enumerate() {
        let replayed = consumer
            .apply_source_delta(ApplyCoreSourceDeltaPageRequest {
                page: source_page.clone(),
                acknowledgement_page_index: u32::try_from(index).unwrap(),
            })
            .unwrap();
        assert_eq!(replayed.materialization_id, expected.materialization_id);
        assert_eq!(replayed.core_generation_id, expected.core_generation_id);
        assert_eq!(replayed.page_index, expected.page_index);
        assert_eq!(
            replayed.acknowledgement_page_index,
            expected.acknowledgement_page_index
        );
        assert_eq!(
            replayed.acknowledgement_terminal,
            expected.acknowledgement_terminal
        );
        assert_eq!(replayed.changed_sources, expected.changed_sources);
        assert_eq!(replayed.removed_sources, expected.removed_sources);
        assert!(!expected.replayed);
        assert!(replayed.replayed);
        assert_eq!(
            replayed.reconcile_sources.len(),
            expected.reconcile_sources.len()
        );
        for (item_index, (actual, expected)) in replayed
            .reconcile_sources
            .iter()
            .zip(&expected.reconcile_sources)
            .enumerate()
        {
            assert_eq!(
                actual.materialize_index, expected.materialize_index,
                "acknowledgement page {index} item {item_index}"
            );
            assert_eq!(
                actual.delta.source().identity().digest(),
                expected.delta.source().identity().digest(),
                "acknowledgement page {index} item {item_index}"
            );
            assert!(matches!(actual.delta, CoreSourceDelta::Removed(_)));
            assert!(matches!(expected.delta, CoreSourceDelta::Removed(_)));
        }
    }
    assert_eq!(consumer.source_page_applications, 1);
}

#[test]
fn event_page_cas_mismatch_fails_closed_after_one_exchange() {
    let temp = tempdir().unwrap();
    let source = source("event-cas-mismatch.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();
    consumer.wrong_event_page_index = true;

    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert!(error.to_string().contains("acknowledgement"));
    assert_eq!(consumer.event_exchanges, 1);
}

#[test]
fn event_page_is_authoritatively_validated_before_exchange() {
    let source = source("invalid-page.jsonl");
    let page = CoreEventDeltaPage {
        materialization_id: "d".repeat(64),
        core_generation_id: "a".repeat(64),
        reconciliation: CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(CoreSourceState {
                source,
                core_record_accumulator: "0".repeat(64),
                event_count: 0,
            }),
        },
        page_index: 0,
        terminal: false,
        deltas: Vec::new(),
    };
    let exchanges = Cell::new(0_u64);
    let error = apply_batched_event_delta_pages_with(vec![page], &mut |_, _| {
        exchanges.set(exchanges.get().saturating_add(1));
        unreachable!("invalid Core event page must fail before exchange")
    })
    .unwrap_err();
    assert!(error.to_string().contains("invalid_request"));
    assert_eq!(exchanges.get(), 0);
}

#[test]
fn generation_mismatched_delta_ack_fails_closed() {
    let temp = tempdir().unwrap();
    let source = source("mismatch.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();
    consumer.wrong_delta_generation = true;
    assert!(sync_core_feed(&index, None, &mut consumer)
        .unwrap_err()
        .to_string()
        .contains("acknowledgement"));
}

#[test]
fn source_acknowledgement_sequence_and_global_identity_fail_closed() {
    let temp = tempdir().unwrap();
    let source = source("invalid-source-ack.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    for (mutation, expected) in [
        (
            SourceResponseMutation::DuplicateSource,
            "repeat a stable source identity",
        ),
        (SourceResponseMutation::StalePresent, "stale current source"),
        (
            SourceResponseMutation::RemoveCurrent,
            "stored-minus-snapshot removals are valid only on the terminal source page",
        ),
        (
            SourceResponseMutation::SkipMaterializeIndex,
            "indices are not contiguous",
        ),
    ] {
        let mut consumer = Consumer::new();
        consumer.source_response_mutation = Some(mutation);
        let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        assert_eq!(consumer.source_exchanges, 1);
        assert_eq!(consumer.state_exchanges, 0);
        assert_eq!(consumer.event_exchanges, 0);
    }

    let mut consumer = Consumer::new();
    consumer.wrong_acknowledgement_page_index = true;
    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert!(error.to_string().contains("page CAS"), "{error:#}");
    assert_eq!(consumer.source_exchanges, 1);
    assert_eq!(consumer.state_exchanges, 0);
    assert_eq!(consumer.event_exchanges, 0);
}

#[test]
fn producer_reads_only_pinned_core_records() {
    let source = format!(
        "{}{}",
        include_str!("../../core_materialization_feed.rs"),
        include_str!("../ordered_prefetch.rs")
    );
    for forbidden in ["ctx_history_capture", "reread_source"] {
        assert!(!source.contains(forbidden), "producer contains {forbidden}");
    }
    assert!(source.contains("plan_core_source_event_page_with_budget"));
    assert!(source.contains("materialize_stored_core_source_event_page"));
    assert!(source.contains("core_record_sha256_from_encoded"));
    assert!(!source.contains("core_record_digests_from_encoded"));
    assert!(!source.contains("MaterializeCoreRecordPage"));
}
