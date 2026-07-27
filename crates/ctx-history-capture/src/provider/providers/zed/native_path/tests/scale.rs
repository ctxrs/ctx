use super::*;

pub(super) fn local_scale_scan_is_bounded_and_never_materializes_result_surfaces() {
    const THREADS: usize = 80;
    const MESSAGES_PER_THREAD: usize = 100;

    let directory = tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = new_database(&path);
    let transaction = connection.transaction().unwrap();
    for thread_index in 0..THREADS {
        let id = format!("thread-{thread_index:04}");
        let mut messages = Vec::with_capacity(MESSAGES_PER_THREAD);
        for message_index in 0..MESSAGES_PER_THREAD {
            let sequence = thread_index * MESSAGES_PER_THREAD + message_index;
            if message_index % 10 == 9 {
                messages.push(output_message(
                    &format!("call-{sequence:08}"),
                    &format!("src/scale-{sequence:08}.rs"),
                    &format!("CTX-ZED-SCALE-OUTPUT-{sequence:08} /result-only/{sequence:08}.txt"),
                    false,
                ));
            } else if message_index % 2 == 0 {
                messages.push(user(
                    &format!("user-{sequence:08}"),
                    &format!("user message {sequence:08}"),
                ));
            } else {
                messages.push(assistant(&format!("assistant message {sequence:08}")));
            }
        }
        insert_thread(
            &transaction,
            &id,
            (thread_index > 0).then_some("thread-0000"),
            if thread_index % 2 == 0 {
                "json"
            } else {
                "zstd"
            },
            &thread(messages),
        );
    }
    transaction.commit().unwrap();
    drop(connection);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.native_thread_rows, THREADS as u64);
    assert_eq!(
        authority.counters.retained_events,
        (THREADS * MESSAGES_PER_THREAD) as u64
    );
    assert_eq!(
        authority.counters.output.native_results_observed,
        (THREADS * (MESSAGES_PER_THREAD / 10)) as u64
    );
    assert_eq!(authority.counters.output.result_events_created, 0);
    assert_eq!(authority.counters.output.result_hashes_created, 0);
    assert_eq!(authority.counters.output.result_previews_created, 0);
    assert_eq!(authority.counters.output.result_file_touches_created, 0);
    assert!(sink.pages.len() > 1);
    assert!(sink.pages.iter().all(|page| {
        page.publication_units() <= ZED_NATIVE_PAGE_MAX_UNITS
            && page.estimated_bytes <= ZED_NATIVE_PAGE_MAX_BYTES
            && serde_json::to_vec(page).unwrap().len() <= page.estimated_bytes
    }));
    assert!(sink.events().iter().all(|event| {
        !event.body.contains("CTX-ZED-SCALE-OUTPUT")
            && !event.preview.contains("CTX-ZED-SCALE-OUTPUT")
            && event
                .safe_file_touches
                .iter()
                .all(|path| !path.contains("result-only"))
    }));
}
