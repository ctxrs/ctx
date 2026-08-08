use super::*;

const LONG_SOURCE_RECORDS: usize = MAX_CORE_EVENT_DELTA_PAGE_ITEMS * 2 + 1;
const LONG_BODY_BYTES: usize = 1_024;

fn long_bodies(label: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("{label}-{index:04}-{}", "x".repeat(LONG_BODY_BYTES)))
        .collect()
}

fn encoded_record_bytes(source: &SourceKey, bodies: &[String]) -> usize {
    encoded_record_bytes_from(source, bodies, 0)
}

fn encoded_record_bytes_from(
    source: &SourceKey,
    bodies: &[String],
    sequence_offset: usize,
) -> usize {
    bodies
        .iter()
        .enumerate()
        .map(|(index, body)| {
            record(
                source,
                u64::try_from(sequence_offset + index + 1).unwrap(),
                body.clone(),
            )
            .encode_stored()
            .unwrap()
            .len()
        })
        .sum()
}

fn publish_source(
    root: &Path,
    source: &SourceKey,
    revision: u8,
    bodies: &[String],
) -> VerifiedIndex {
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    add_source(&mut writer, source, revision, bodies.to_vec());
    writer.commit(|_| true).unwrap();
    VerifiedIndex::open_pinned(root).unwrap()
}

fn sync_serial(
    index: &VerifiedIndex,
    prior: Option<&CoreMaterializationReceipt>,
    consumer: &mut Consumer,
) -> CoreMaterializationSyncReport {
    sync_core_feed_with_options(
        index,
        prior,
        consumer,
        CoreFeedExecutionOptions {
            prefetch_parallelism: 1,
        },
    )
    .unwrap()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SourceRequestMetrics {
    data_item_visits: u64,
    request_bytes: u64,
}

fn source_request_metrics(
    pages: &[CoreSourceDeltaPage],
    terminal_acknowledgement_pages: usize,
) -> SourceRequestMetrics {
    let mut metrics = SourceRequestMetrics::default();
    for (page_position, page) in pages.iter().enumerate() {
        let request_count = if page_position + 1 == pages.len() {
            terminal_acknowledgement_pages
        } else {
            1
        };
        for acknowledgement_page_index in 0..request_count {
            let request = ApplyCoreSourceDeltaPageRequest {
                page: page.clone(),
                acknowledgement_page_index: u32::try_from(acknowledgement_page_index).unwrap(),
            };
            request.validate().unwrap();
            metrics.data_item_visits = metrics
                .data_item_visits
                .checked_add(u64::try_from(page.deltas.len()).unwrap())
                .unwrap();
            metrics.request_bytes = metrics
                .request_bytes
                .checked_add(u64::try_from(serde_json::to_vec(&request).unwrap().len()).unwrap())
                .unwrap();
        }
    }
    metrics
}

#[test]
fn source_page_feed_is_linear_for_5916_current_sources_and_maximum_prior_removals() {
    const CURRENT_SOURCE_COUNT: usize = 5_916;
    const PRIOR_REMOVAL_COUNT: usize = MAX_CORE_SOURCE_STATES;

    let materialization_id = "d".repeat(64);
    let generation_id = "a".repeat(64);
    let deltas = ordered_source_deltas(CURRENT_SOURCE_COUNT, 32);
    let pages = build_delta_pages(&materialization_id, &generation_id, deltas).unwrap();
    let (terminal_page, data_pages) = pages.split_last().unwrap();
    assert_eq!(
        data_pages.len(),
        CURRENT_SOURCE_COUNT.div_ceil(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS)
    );
    assert!(data_pages
        .iter()
        .all(|page| !page.terminal && !page.deltas.is_empty()));
    assert!(terminal_page.terminal);
    assert!(terminal_page.deltas.is_empty());

    let mut consumer = Consumer::new();
    for source_index in 0..PRIOR_REMOVAL_COUNT {
        let removed = source(&format!("prior-removal-{source_index:05}.jsonl"));
        assert!(consumer
            .known_sources
            .insert(removed.identity().digest(), removed)
            .is_none());
    }

    let mut changed_sources = 0_u64;
    let mut removed_sources = 0_u64;
    for page in pages.clone() {
        let mut acknowledgement_page_index = 0_u32;
        loop {
            let request = ApplyCoreSourceDeltaPageRequest {
                page: page.clone(),
                acknowledgement_page_index,
            };
            let identity = request.acknowledgement_identity();
            let applied = consumer.apply_source_delta(request).unwrap();
            applied.validate_for_identity(&identity).unwrap();
            changed_sources = changed_sources
                .checked_add(u64::from(applied.changed_sources))
                .unwrap();
            removed_sources = removed_sources
                .checked_add(u64::from(applied.removed_sources))
                .unwrap();
            if applied.acknowledgement_terminal {
                break;
            }
            acknowledgement_page_index = acknowledgement_page_index.checked_add(1).unwrap();
        }
    }

    let removal_acknowledgement_pages =
        PRIOR_REMOVAL_COUNT.div_ceil(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS);
    let mut expected_requests = data_pages
        .iter()
        .map(|page| (page.page_index, 0))
        .collect::<Vec<_>>();
    expected_requests.extend(
        (0..removal_acknowledgement_pages)
            .map(|index| (terminal_page.page_index, u32::try_from(index).unwrap())),
    );

    assert_eq!(
        changed_sources,
        u64::try_from(CURRENT_SOURCE_COUNT).unwrap()
    );
    assert_eq!(removed_sources, u64::try_from(PRIOR_REMOVAL_COUNT).unwrap());
    assert_eq!(consumer.source_acknowledgement_requests, expected_requests);
    assert_eq!(consumer.delta_pages, pages);
    assert_eq!(
        consumer.source_page_applications,
        u64::try_from(pages.len()).unwrap()
    );
    assert_eq!(
        consumer.source_request_data_item_visits,
        u64::try_from(CURRENT_SOURCE_COUNT).unwrap()
    );

    // Measurement-only reconstruction of the prior public layout. It is not a
    // compatibility path: the final populated page carried `terminal: true`
    // and was resent for every acknowledgement page containing its changed
    // sources plus all prior removals.
    let mut old_layout_pages = data_pages.to_vec();
    let old_terminal_page = old_layout_pages.last_mut().unwrap();
    old_terminal_page.terminal = true;
    old_terminal_page.validate().unwrap();
    let old_terminal_acknowledgement_pages = PRIOR_REMOVAL_COUNT
        .checked_add(old_terminal_page.deltas.len())
        .unwrap()
        .div_ceil(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS);
    let before = source_request_metrics(&old_layout_pages, old_terminal_acknowledgement_pages);
    let after = SourceRequestMetrics {
        data_item_visits: consumer.source_request_data_item_visits,
        request_bytes: consumer.source_request_bytes,
    };

    assert_eq!(before.data_item_visits, 7_708);
    assert_eq!(after.data_item_visits, 5_916);
    assert!(after.request_bytes < before.request_bytes);
    eprintln!(
        "source_page_proportionality current_sources={CURRENT_SOURCE_COUNT} prior_removals={PRIOR_REMOVAL_COUNT} before_data_item_visits={} after_data_item_visits={} before_request_bytes={} after_request_bytes={} old_terminal_acknowledgements={old_terminal_acknowledgement_pages} new_terminal_acknowledgements={removal_acknowledgement_pages}",
        before.data_item_visits,
        after.data_item_visits,
        before.request_bytes,
        after.request_bytes,
    );
}

#[test]
fn changed_logical_source_replay_work_is_measured_for_jsonl_and_sqlite() {
    for (source_format, source_name) in [
        ("codex_session_jsonl", "long-session.jsonl"),
        ("shelley_sqlite", "logical-session-42"),
    ] {
        let temp = tempdir().unwrap();
        let source = source_with_format(source_format, source_name);
        let initial_bodies = long_bodies(source_format, LONG_SOURCE_RECORDS);
        let initial = publish_source(temp.path(), &source, 1, &initial_bodies);
        let initial_generation = initial.generation_id().to_owned();
        let mut consumer = Consumer::new();
        let cold = sync_serial(&initial, None, &mut consumer);
        assert_eq!(cold.prefetch.decoded_records, LONG_SOURCE_RECORDS);
        assert_eq!(
            cold.prefetch.decoded_record_bytes,
            encoded_record_bytes(&source, &initial_bodies)
        );
        let initial_receipt = cold.receipt;
        drop(initial);

        let mut appended_bodies = initial_bodies.clone();
        appended_bodies.push(format!("{source_format}-appended-one-event"));
        let appended = publish_source(temp.path(), &source, 2, &appended_bodies);
        assert_ne!(appended.generation_id(), initial_generation);
        assert_eq!(
            appended.manifest().indexed_documents,
            u64::try_from(LONG_SOURCE_RECORDS).unwrap() + 1
        );
        let appended_report = sync_serial(&appended, Some(&initial_receipt), &mut consumer);
        let appended_bytes = encoded_record_bytes(&source, &appended_bodies);
        let appended_event_bytes = encoded_record_bytes_from(
            &source,
            &appended_bodies[LONG_SOURCE_RECORDS..],
            LONG_SOURCE_RECORDS,
        );
        assert_eq!(appended_report.changed_sources, 1);
        assert_eq!(appended_report.event_mutations, 1);
        assert_eq!(
            appended_report.prefetch.decoded_records,
            LONG_SOURCE_RECORDS + 1
        );
        assert_eq!(
            appended_report.prefetch.decoded_record_bytes,
            appended_bytes
        );
        assert!(appended_bytes / appended_event_bytes >= 128);
        eprintln!(
            "pro format={source_format} transition=one_event_append changed_events=1 decoded_records={} decoded_bytes={appended_bytes} changed_record_bytes={appended_event_bytes} byte_amplification={}x",
            appended_report.prefetch.decoded_records,
            appended_bytes / appended_event_bytes
        );

        consumer.replay_begin = true;
        consumer.replay_status_currentness = Some(CoreProjectionCurrentness::Current);
        let replayed = sync_serial(&appended, Some(&appended_report.receipt), &mut consumer);
        assert!(replayed.replayed);
        assert_eq!(replayed.prefetch.decoded_records, 0);
        assert_eq!(replayed.prefetch.decoded_record_bytes, 0);
        eprintln!(
            "pro format={source_format} transition=completed_restart decoded_records=0 decoded_bytes=0"
        );
        consumer.replay_begin = false;
        consumer.replay_status_currentness = None;
        let appended_receipt = appended_report.receipt;
        drop(appended);

        let mut replacement_bodies = appended_bodies.clone();
        replacement_bodies[LONG_SOURCE_RECORDS / 2] =
            format!("{source_format}-one-record-replacement");
        let replacement = publish_source(temp.path(), &source, 3, &replacement_bodies);
        let replacement_report = sync_serial(&replacement, Some(&appended_receipt), &mut consumer);
        let replacement_bytes = encoded_record_bytes(&source, &replacement_bodies);
        let replaced_record_bytes = encoded_record_bytes_from(
            &source,
            &replacement_bodies[LONG_SOURCE_RECORDS / 2..LONG_SOURCE_RECORDS / 2 + 1],
            LONG_SOURCE_RECORDS / 2,
        );
        assert_eq!(replacement_report.changed_sources, 1);
        assert_eq!(replacement_report.event_mutations, 1);
        assert_eq!(
            replacement_report.prefetch.decoded_records,
            LONG_SOURCE_RECORDS + 1
        );
        assert_eq!(
            replacement_report.prefetch.decoded_record_bytes,
            replacement_bytes
        );
        assert!(replacement_bytes / replaced_record_bytes >= 128);
        eprintln!(
            "pro format={source_format} transition=one_record_replacement changed_events=1 decoded_records={} decoded_bytes={replacement_bytes} changed_record_bytes={replaced_record_bytes} byte_amplification={}x",
            replacement_report.prefetch.decoded_records,
            replacement_bytes / replaced_record_bytes
        );
        let replacement_receipt = replacement_report.receipt;
        drop(replacement);

        let observation = SourceInventoryObservation::new(
            source.provider(),
            "fixture-root",
            TypedKey::utf8("fixture-authority").unwrap(),
            "inventory-v1",
            vec![4],
        )
        .unwrap();
        let inventory = CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            "discovery-v1",
            Vec::new(),
        )
        .unwrap();
        let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.delete_source(deletion, inventory).unwrap();
        writer.commit(|_| true).unwrap();
        let removed = VerifiedIndex::open_pinned(temp.path()).unwrap();
        let removal_report = sync_serial(&removed, Some(&replacement_receipt), &mut consumer);
        assert_eq!(removal_report.changed_sources, 0);
        assert_eq!(removal_report.removed_sources, 1);
        assert_eq!(
            removal_report.event_mutations,
            u64::try_from(LONG_SOURCE_RECORDS).unwrap() + 1
        );
        assert_eq!(removal_report.prefetch.decoded_records, 0);
        assert_eq!(removal_report.prefetch.decoded_record_bytes, 0);
        eprintln!(
            "pro format={source_format} transition=source_removal removed_events={} decoded_records=0 decoded_bytes=0",
            LONG_SOURCE_RECORDS + 1
        );
    }
}

#[test]
fn partial_restart_redecodes_the_changed_logical_source_for_jsonl_and_sqlite() {
    for (source_format, source_name) in [
        ("codex_session_jsonl", "resume-session.jsonl"),
        ("shelley_sqlite", "logical-resume-session-42"),
    ] {
        let temp = tempdir().unwrap();
        let source = source_with_format(source_format, source_name);
        let bodies = long_bodies(source_format, LONG_SOURCE_RECORDS);
        let index = publish_source(temp.path(), &source, 1, &bodies);
        let expected_bytes = encoded_record_bytes(&source, &bodies);
        let mut consumer = Consumer::new();
        consumer.event_response_loss_after = Some(1);

        let error = sync_core_feed_with_options(
            &index,
            None,
            &mut consumer,
            CoreFeedExecutionOptions {
                prefetch_parallelism: 1,
            },
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "synthetic_event_response_lost: committed Core event delta exchange"
        );

        consumer.event_response_loss_after = None;
        consumer.replay_begin = true;
        consumer.replay_status_currentness = Some(CoreProjectionCurrentness::Partial);
        let resumed = sync_serial(&index, None, &mut consumer);
        assert_eq!(resumed.prefetch.decoded_records, LONG_SOURCE_RECORDS);
        assert_eq!(resumed.prefetch.decoded_record_bytes, expected_bytes);
        eprintln!(
            "pro format={source_format} transition=partial_restart decoded_records={} decoded_bytes={expected_bytes}",
            resumed.prefetch.decoded_records
        );
    }
}
