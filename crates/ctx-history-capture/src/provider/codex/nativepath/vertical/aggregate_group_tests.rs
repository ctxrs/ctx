use super::super::tests::{discover_one, message, session_meta, write_source};
use super::*;

fn chunk(
    pages: usize,
    mutation_units: usize,
    serialized_bytes: usize,
) -> CodexNativeRootChunkAccounting {
    CodexNativeRootChunkAccounting {
        pages,
        mutation_units,
        serialized_bytes,
    }
}

fn first_chunk(store: &Store, source: CodexCatalogSource) -> CodexNativeRootChunk {
    let mut producer = prepare_codex_native_producer_task(
        store,
        source,
        CodexNativeStoreOptions {
            machine_id: "codex-two-tier-group-test".to_owned(),
            imported_at: "2026-07-27T12:00:00Z".parse().unwrap(),
            history_record_id: None,
        },
    )
    .unwrap()
    .open()
    .unwrap();
    match producer.next_window().unwrap() {
        CodexNativeProducerStep::Window { chunk, .. } => chunk,
        CodexNativeProducerStep::Noop(_) => panic!("fixture must produce a Core window"),
    }
}

#[test]
fn root_group_accepts_real_pages_with_more_than_64_total_units() {
    let fixture = |session_id: &str| {
        let mut contents = session_meta(session_id);
        for index in 0..63 {
            contents.push_str(&message("user", &format!("{session_id}-{index}")));
        }
        write_source(&contents)
    };
    let (first_temp, first_path) = fixture("00000000-0000-7000-8000-000000000064");
    let (second_temp, second_path) = fixture("00000000-0000-7000-8000-000000000065");
    let store_temp = tempfile::TempDir::new().unwrap();
    let store = Store::open(store_temp.path().join("history.sqlite")).unwrap();
    let first = first_chunk(
        &store,
        discover_one(&first_path, "00000000-0000-7000-8000-000000000064"),
    );
    let second = first_chunk(
        &store,
        discover_one(&second_path, "00000000-0000-7000-8000-000000000065"),
    );
    let page_units = first
        .pages
        .iter()
        .chain(&second.pages)
        .map(CodexNativePage::mutation_units)
        .sum::<usize>();

    let mut group = CodexNativeRootGroup::default();
    group.try_push(first).unwrap();
    group.try_push(second).unwrap();
    assert_eq!(group.chunks.len(), 2);
    assert!(page_units > 64);
    drop((first_temp, second_temp));
}

#[test]
fn producer_coalesces_source_pages_to_store_group_capacity() {
    let session_id = "00000000-0000-7000-8000-000000000066";
    let mut contents = session_meta(session_id);
    for index in 0..600 {
        contents.push_str(&message(
            "user",
            &format!("{session_id}-{index}-{}", "x".repeat(16 * 1024)),
        ));
    }
    let (source_temp, source_path) = write_source(&contents);
    let store_temp = tempfile::TempDir::new().unwrap();
    let store = Store::open(store_temp.path().join("history.sqlite")).unwrap();
    let mut producer = prepare_codex_native_producer_task(
        &store,
        discover_one(&source_path, session_id),
        CodexNativeStoreOptions {
            machine_id: "codex-byte-bound-test".to_owned(),
            imported_at: "2026-07-27T12:00:00Z".parse().unwrap(),
            history_record_id: None,
        },
    )
    .unwrap()
    .open()
    .unwrap();
    let first = producer.next_window().unwrap();
    let second = producer.next_window().unwrap();
    let (
        CodexNativeProducerStep::Window {
            chunk: first,
            source_done: false,
            ..
        },
        CodexNativeProducerStep::Window { chunk: second, .. },
    ) = (first, second)
    else {
        panic!("the real aggregate byte bound must retain one charged lookahead window");
    };

    assert!(first.pages.len() > 1);
    assert!(first.pages.len() <= NATIVE_PATH_MAX_GROUP_PAGES);
    assert!(first.mutation_units <= NATIVE_PATH_MAX_MUTATION_UNITS);
    assert!(first.serialized_bytes <= NATIVE_PATH_MAX_RETAINED_PAGE_BYTES);
    assert!(second.serialized_bytes <= NATIVE_PATH_MAX_RETAINED_PAGE_BYTES);
    assert!(
        first
            .serialized_bytes
            .saturating_add(second.serialized_bytes)
            > NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
        "real prepared bytes, rather than accounting-only fixtures, must reject the merge"
    );
    drop(source_temp);
}

#[test]
fn root_group_accumulator_splits_on_attempted_store_mutations() {
    let mut first = CodexNativeRootGroupAccounting::default();
    assert!(first.try_push(chunk(64, NATIVE_PATH_MAX_MUTATION_UNITS, 1)));
    assert!(!first.try_push(chunk(1, 1, 1)));

    let mut second = CodexNativeRootGroupAccounting::default();
    assert!(second.try_push(chunk(1, 1, 1)));
}

#[test]
fn root_group_accumulator_splits_on_aggregate_encoded_bytes() {
    let mut first = CodexNativeRootGroupAccounting::default();
    assert!(first.try_push(chunk(1, 1, NATIVE_PATH_MAX_RETAINED_PAGE_BYTES - 1)));
    assert!(!first.try_push(chunk(1, 1, 2)));

    let mut second = CodexNativeRootGroupAccounting::default();
    assert!(second.try_push(chunk(1, 1, 2)));
}
