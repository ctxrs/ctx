#[test]
fn bounded_session_coordinate_queries_ignore_pathological_nonselected_cardinality() {
    const SEGMENTS: u64 = 4;
    const EVENTS_PER_SEGMENT: u64 = 2_500;
    const EVENT_COUNT: u64 = SEGMENTS * EVENTS_PER_SEGMENT;
    const SELECTED_SEQUENCE: u64 = EVENT_COUNT / 2;

    let temp = tempdir().unwrap();
    let source = source("bounded-session-coordinates.jsonl");
    let first = document(&source, 1, "first");
    let session_id = first.session_id.as_uuid();
    let mut selected_event_id = None;
    let mut last_event_id = None;
    for segment_index in 0..SEGMENTS {
        let revision = (segment_index + 1) as u8;
        let retained_events = (segment_index + 1) * EVENTS_PER_SEGMENT;
        let retained_bytes = retained_events * 10;
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer
            .writer_mut()
            .unwrap()
            .set_merge_policy(Box::<NoMergePolicy>::default());
        let append_base = if segment_index == 0 {
            writer.begin_source(source.clone()).unwrap();
            None
        } else {
            Some(writer.begin_source_append(source.clone()).unwrap().clone())
        };
        for sequence in (0..EVENTS_PER_SEGMENT)
            .rev()
            .map(|event_index| segment_index + 1 + event_index * SEGMENTS)
        {
            let event = document(&source, sequence, "small body");
            if sequence == SELECTED_SEQUENCE {
                selected_event_id = Some(event.event_id.as_uuid());
            }
            if sequence == EVENT_COUNT {
                last_event_id = Some(event.event_id.as_uuid());
            }
            writer.add_core_record(event).unwrap();
        }
        let certified = appendable_certificate(&source, revision, retained_events, retained_bytes);
        if let Some(base) = append_base {
            writer
                .certify_source_append(
                    CertifiedSourceAppend::certify(
                        &base,
                        certified,
                        retained_bytes - EVENTS_PER_SEGMENT * 10,
                        [revision - 1; 32],
                    )
                    .unwrap(),
                )
                .unwrap();
        } else {
            writer.certify_source(certified).unwrap();
        }
        writer.commit(|_| true).unwrap();
    }

    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert!(index.searcher.segment_readers().len() >= SEGMENTS as usize);
    crate::query::reset_stored_event_record_materializations();
    crate::query::reset_stored_core_event_record_materializations();
    crate::query::reset_session_event_order_term_visits();
    let prefix = index
        .session_event_coordinate_prefix(session_id, 1)
        .unwrap();
    assert_eq!(
        prefix
            .iter()
            .map(|coordinate| coordinate.event_sequence)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(crate::query::stored_event_record_materializations(), 1);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);
    assert_eq!(crate::query::session_event_order_term_visits(), 1);
    assert_eq!(
        crate::query::session_event_order_visited_sequences(),
        vec![1]
    );

    let selected_event_id = selected_event_id.unwrap();
    crate::query::reset_stored_event_record_materializations();
    crate::query::reset_stored_core_event_record_materializations();
    crate::query::reset_session_event_order_term_visits();
    let window = index
        .session_event_coordinate_window(session_id, selected_event_id, 50, 50)
        .unwrap()
        .unwrap();
    assert_eq!(window.len(), MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS);
    assert_eq!(window.first().unwrap().event_sequence, 4_950);
    assert_eq!(window[50].event_id, selected_event_id);
    assert_eq!(window.last().unwrap().event_sequence, 5_050);
    assert_eq!(crate::query::stored_event_record_materializations(), 1);
    assert_eq!(crate::query::stored_core_event_record_materializations(), 0);
    assert_eq!(crate::query::session_event_order_term_visits(), 100);
    assert_eq!(
        crate::query::session_event_order_visited_sequences(),
        (4_950..5_000)
            .rev()
            .chain(5_001..=5_050)
            .collect::<Vec<_>>()
    );

    let old_generation = index.generation_id().to_owned();
    let mut appending = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    appending
        .writer_mut()
        .unwrap()
        .set_merge_policy(Box::<NoMergePolicy>::default());
    let base = appending
        .begin_source_append(source.clone())
        .unwrap()
        .clone();
    appending
        .add_core_record(document(&source, EVENT_COUNT + 1, "new generation tail"))
        .unwrap();
    appending
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(
                    &source,
                    (SEGMENTS + 1) as u8,
                    EVENT_COUNT + 1,
                    (EVENT_COUNT + 1) * 10,
                ),
                EVENT_COUNT * 10,
                [SEGMENTS as u8; 32],
            )
            .unwrap(),
        )
        .unwrap();
    appending.commit(|_| true).unwrap();
    let appended = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_ne!(old_generation, appended.generation_id());
    let last_event_id = last_event_id.unwrap();
    assert_eq!(
        index
            .session_event_coordinate_window(session_id, last_event_id, 1, 1)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|coordinate| coordinate.event_sequence)
            .collect::<Vec<_>>(),
        vec![EVENT_COUNT - 1, EVENT_COUNT]
    );
    assert_eq!(
        appended
            .session_event_coordinate_window(session_id, last_event_id, 1, 1)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|coordinate| coordinate.event_sequence)
            .collect::<Vec<_>>(),
        vec![EVENT_COUNT - 1, EVENT_COUNT, EVENT_COUNT + 1]
    );

    assert!(matches!(
        index.session_event_coordinate_prefix(
            session_id,
            MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS + 1,
        ),
        Err(IndexError::InvalidSessionEventCoordinateLimit { .. })
    ));
    assert!(matches!(
        index.session_event_coordinate_window(session_id, selected_event_id, 51, 50),
        Err(IndexError::InvalidSessionEventCoordinateLimit { .. })
    ));
}
