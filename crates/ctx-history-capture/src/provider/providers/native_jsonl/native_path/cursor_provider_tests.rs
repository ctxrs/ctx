use std::fs;

use serde_json::json;
use tempfile::tempdir;

use super::*;
use crate::provider::{
    native_ingestion::{NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS},
    providers::cursor::CursorEventBody,
};
use crate::{
    import_cursor_native_history, CursorNativeImportOptions, ImportProfile,
    ProviderImportWorkResult,
};

const MACHINE: &str = "cursor-nativepath-two-tier-boundary";

#[test]
fn cursor_65_records_span_pages_and_commit_in_one_publication_group() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("projects");
    write_cursor_transcript(
        &root,
        "project",
        "unit-boundary",
        &(0..NATIVE_INGESTION_PAGE_MAX_UNITS + 1)
            .map(|index| cursor_user(&format!("message-{index}")))
            .collect::<Vec<_>>(),
    );
    let inventory = discover_cursor_transcripts(&root);
    assert_eq!(inventory.transcripts.len(), 1);
    let pending = cursor_pending_pages(&inventory.transcripts[0]);
    assert!(pending.len() >= 2);
    assert_eq!(
        pending
            .iter()
            .map(|pending| pending.page.events.len())
            .sum::<usize>(),
        NATIVE_INGESTION_PAGE_MAX_UNITS + 1
    );
    assert!(pending.iter().all(|pending| {
        pending.page.events.len() <= NATIVE_INGESTION_PAGE_MAX_UNITS
            && pending.page.serialized_bytes <= NATIVE_INGESTION_PAGE_MAX_BYTES
    }));

    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    let first = import_cursor(&root, &mut store, CaptureWorkLimit::OneSafeGroup);

    assert_eq!(first.imported_events, NATIVE_INGESTION_PAGE_MAX_UNITS + 1);
    assert!(first.work_remaining);
    let session = store
        .session_by_external_session(CaptureProvider::Cursor, "unit-boundary")
        .unwrap()
        .unwrap();
    assert_eq!(
        store.events_for_session(session.id).unwrap().len(),
        NATIVE_INGESTION_PAGE_MAX_UNITS + 1
    );

    let drained = import_cursor(&root, &mut store, CaptureWorkLimit::Drain);
    assert_eq!(drained.work_result(), ProviderImportWorkResult::NoOp);
    assert!(!drained.work_remaining);
}

#[test]
fn cursor_group_rotates_before_the_six_mib_retained_target() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("projects");
    write_cursor_transcript(&root, "a-project", "byte-a", &[cursor_user("first")]);
    write_cursor_transcript(&root, "b-project", "byte-b", &[cursor_user("second")]);
    let inventory = discover_cursor_transcripts(&root);
    assert_eq!(inventory.transcripts.len(), 2);
    let page_bytes = CURSOR_GROUP_MAX_BYTES / 2 + 1;
    let pending = inventory
        .transcripts
        .iter()
        .map(|transcript| {
            let mut pending = cursor_pending_pages(transcript).pop().unwrap();
            pending.page.serialized_bytes = page_bytes;
            pending
        })
        .collect::<Vec<_>>();
    assert!(page_bytes <= NATIVE_INGESTION_PAGE_MAX_BYTES);
    assert!(page_bytes.saturating_mul(pending.len()) > CURSOR_GROUP_MAX_BYTES);

    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    let (summary, stopped) = publish_one_safe_cursor_group(&root, &mut store, pending);

    assert!(stopped);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(
        store
            .list_sessions()
            .unwrap()
            .into_iter()
            .filter(|session| session.provider == CaptureProvider::Cursor)
            .count(),
        1
    );
}

#[test]
fn cursor_group_rotates_before_the_estimated_mutation_target() {
    const TOUCHES_PER_EVENT: usize = 24;

    let temp = tempdir().unwrap();
    let root = temp.path().join("projects");
    for (project, session) in [("a-project", "mutation-a"), ("b-project", "mutation-b")] {
        write_cursor_transcript(
            &root,
            project,
            session,
            &(0..NATIVE_INGESTION_PAGE_MAX_UNITS)
                .map(|index| cursor_user(&format!("{session}-{index}")))
                .collect::<Vec<_>>(),
        );
    }
    let inventory = discover_cursor_transcripts(&root);
    assert_eq!(inventory.transcripts.len(), 2);
    let pending = inventory
        .transcripts
        .iter()
        .map(|transcript| {
            let mut pending = cursor_pending_pages(transcript).pop().unwrap();
            assert_eq!(pending.page.events.len(), NATIVE_INGESTION_PAGE_MAX_UNITS);
            for event in &mut pending.page.events {
                event.body = CursorEventBody::ToolCall {
                    call_id: None,
                    tool_name: Some("write_file".to_owned()),
                    input_paths: (0..TOUCHES_PER_EVENT)
                        .map(|index| format!("path-{index}.txt"))
                        .collect(),
                };
            }
            pending.page.serialized_bytes = 1024 * 1024;
            pending
        })
        .collect::<Vec<_>>();
    let page_mutations = NATIVE_INGESTION_PAGE_MAX_UNITS
        .saturating_mul(1 + TOUCHES_PER_EVENT)
        .saturating_add(4);
    assert!(page_mutations <= CURSOR_GROUP_MAX_ESTIMATED_MUTATIONS);
    assert!(page_mutations.saturating_mul(pending.len()) > CURSOR_GROUP_MAX_ESTIMATED_MUTATIONS);

    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    let (summary, stopped) = publish_one_safe_cursor_group(&root, &mut store, pending);

    assert!(stopped);
    assert_eq!(summary.imported_events, NATIVE_INGESTION_PAGE_MAX_UNITS);
}

fn publish_one_safe_cursor_group(
    root: &Path,
    store: &mut Store,
    pending: Vec<CursorPendingPage>,
) -> (ProviderImportSummary, bool) {
    let committed_store = Store::open_read_only(store.path()).unwrap();
    let bulk_guard = store.begin_event_search_bulk_mode().unwrap();
    let context = CursorPublicationContext {
        machine_id: MACHINE,
        source_root: root,
        imported_at: "2026-07-27T12:00:00Z".parse().unwrap(),
        history_record_id: None,
    };
    let result = {
        let mut accumulator = CursorGroupAccumulator::new(
            store,
            &committed_store,
            &bulk_guard,
            context,
            CaptureWorkLimit::OneSafeGroup,
        );
        for page in pending {
            accumulator.push(page).unwrap();
        }
        let summary = accumulator.finish().unwrap();
        (summary, accumulator.stopped)
    };
    store.finish_event_search_bulk_mode(&bulk_guard).unwrap();
    result
}

fn cursor_pending_pages(transcript: &CursorTranscriptPath) -> Vec<CursorPendingPage> {
    let frozen = freeze_cursor_source(transcript).unwrap();
    let mut sink = CursorPageCollector::default();
    scan_cursor_source_into(&frozen, None, &mut sink).unwrap();
    sink.pages
        .into_iter()
        .map(|page| CursorPendingPage {
            transcript: frozen.transcript().clone(),
            observation: frozen.observation().clone(),
            retained_event_count: page.retained_event_count,
            page,
        })
        .collect()
}

#[derive(Default)]
struct CursorPageCollector {
    pages: Vec<CursorPublicationPage>,
}

impl CursorPublicationSink for CursorPageCollector {
    fn begin_cursor_publication(&mut self) -> Result<()> {
        Ok(())
    }

    fn stage_cursor_page(&mut self, page: CursorPublicationPage) -> Result<()> {
        self.pages.push(page);
        Ok(())
    }

    fn abort_cursor_publication(&mut self) {
        self.pages.clear();
    }

    fn commit_cursor_publication(&mut self) -> Result<()> {
        Ok(())
    }
}

fn import_cursor(
    root: &Path,
    store: &mut Store,
    capture_work_limit: CaptureWorkLimit,
) -> ProviderImportSummary {
    import_cursor_native_history(
        root,
        store,
        CursorNativeImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-27T12:00:00Z".parse().unwrap(),
            capture_work_limit,
            import_profile: ImportProfile::CoreOnly,
            ..CursorNativeImportOptions::default()
        },
    )
    .unwrap()
}

fn write_cursor_transcript(root: &Path, project: &str, session: &str, rows: &[serde_json::Value]) {
    let path = root
        .join(project)
        .join("agent-transcripts")
        .join(session)
        .join(format!("{session}.jsonl"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn cursor_user(text: &str) -> serde_json::Value {
    json!({
        "timestamp": "2026-07-27T12:00:00Z",
        "role": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    })
}
