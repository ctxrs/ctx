use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    EventType, LocatorRevisionPolicy, NativeRecordCoordinate, SourceRecordLocator, StableEntityId,
};
use serde_json::json;
use tempfile::TempDir;

use super::source_backed::CursorSourceBackedSummary;
use super::{
    discover_cursor_transcripts, extract_cursor_source_backed_cold, freeze_cursor_source,
    hydrate_cursor_source_backed_message, CursorSourceBackedPage, CursorSourceBackedRecord,
    CursorSourceBackedSink, CursorSourceBackedSourcePlan, CursorSourceBackedTerminal,
    CURSOR_SOURCE_BACKED_PAGE_MAX_BYTES, CURSOR_SOURCE_BACKED_PAGE_MAX_ROWS,
};
use crate::{CaptureError, Result, PROVIDER_MAX_TEXT_CHARS};

fn tempdir() -> TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    tempfile::Builder::new()
        .prefix("cursor-source-backed-")
        .tempdir_in(temp_root)
        .unwrap()
}

fn transcript_path(root: &Path, project: &str, session: &str) -> PathBuf {
    root.join(project)
        .join("agent-transcripts")
        .join(session)
        .join(format!("{session}.jsonl"))
}

fn write_transcript(
    root: &Path,
    project: &str,
    session: &str,
    rows: impl IntoIterator<Item = serde_json::Value>,
) -> PathBuf {
    let path = transcript_path(root, project, session);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, &row).unwrap();
        bytes.push(b'\n');
    }
    fs::write(&path, bytes).unwrap();
    path
}

fn user(text: &str) -> serde_json::Value {
    json!({
        "timestamp": "2026-07-24T12:00:00Z",
        "role": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn multipart() -> serde_json::Value {
    json!({
        "timestamp": "2026-07-24T12:00:01Z",
        "role": "assistant",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "first"},
                {
                    "type": "tool_use",
                    "id": "call-1",
                    "name": "write_file",
                    "input": {"path": "src/main.rs"}
                },
                {"type": "text", "text": "second"}
            ]
        }
    })
}

fn tool_result(text: &str) -> serde_json::Value {
    json!({
        "timestamp": "2026-07-24T12:00:02Z",
        "role": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call-1",
                "content": text
            }]
        }
    })
}

#[derive(Default)]
struct CollectingSink {
    plans: Vec<CursorSourceBackedSourcePlan>,
    pages: Vec<CursorSourceBackedPage>,
    terminals: Vec<CursorSourceBackedTerminal>,
    aborted: usize,
}

impl CursorSourceBackedSink for CollectingSink {
    fn begin_cursor_source(&mut self, plan: &CursorSourceBackedSourcePlan) -> Result<()> {
        self.plans.push(plan.clone());
        Ok(())
    }

    fn stage_cursor_source_page(&mut self, page: CursorSourceBackedPage) -> Result<()> {
        self.pages.push(page);
        Ok(())
    }

    fn finish_cursor_source(&mut self, terminal: CursorSourceBackedTerminal) -> Result<()> {
        self.terminals.push(terminal);
        Ok(())
    }

    fn abort_cursor_source(&mut self) {
        self.aborted = self.aborted.saturating_add(1);
    }
}

impl CollectingSink {
    fn records(&self) -> impl Iterator<Item = &CursorSourceBackedRecord> {
        self.pages.iter().flat_map(|page| page.records.iter())
    }

    fn event_ids(&self) -> Vec<StableEntityId> {
        self.records().map(|record| record.event_id).collect()
    }
}

#[test]
fn cursor_source_backed_cold_extraction_preserves_winning_root_ids_and_bounds() {
    let temp = tempdir();
    let winning_data_dir = temp.path().join("winning-cursor-data");
    let winning_projects = winning_data_dir.join("projects");
    write_transcript(
        &winning_projects,
        "project",
        "session-a",
        [user("winning root"), multipart()],
    );
    let decoy_projects = temp.path().join("default-cursor-data/projects");
    write_transcript(
        &decoy_projects,
        "project",
        "decoy-session",
        [user("must not be discovered")],
    );

    let mut first = CollectingSink::default();
    let first_summary: CursorSourceBackedSummary =
        extract_cursor_source_backed_cold(&winning_data_dir, &mut first).unwrap();
    let mut replay = CollectingSink::default();
    let replay_summary = extract_cursor_source_backed_cold(&winning_data_dir, &mut replay).unwrap();

    assert_eq!(first_summary.projects_root, winning_projects);
    assert_eq!(first_summary.discovered_sources, 1);
    assert_eq!(first_summary.projected_records, 4);
    assert_eq!(first_summary.indexed_documents, 3);
    assert_eq!(first_summary, replay_summary);
    assert_eq!(first.event_ids(), replay.event_ids());
    assert_eq!(first.plans[0].native_session_id, "session-a");
    assert_eq!(
        first.plans[0].source_path,
        fs::canonicalize(transcript_path(&winning_projects, "project", "session-a")).unwrap()
    );
    assert_eq!(first.terminals.len(), 1);
    assert_eq!(first.terminals[0].projected_records, 4);
    assert_eq!(first.terminals[0].physical_records, 2);
    assert_eq!(
        first.terminals[0]
            .certified_source
            .counts()
            .indexed_documents,
        3
    );
    assert_eq!(first.aborted, 0);
    assert!(first.pages.iter().all(|page| page.records.len()
        <= CURSOR_SOURCE_BACKED_PAGE_MAX_ROWS
        && page.estimated_bytes <= CURSOR_SOURCE_BACKED_PAGE_MAX_BYTES));
    assert!(first.records().all(|record| {
        record.locator.revision_policy() == LocatorRevisionPolicy::ExactSourceRevision
            && record.locator.certified_source_revision_digest().is_some()
            && matches!(
                record.locator.coordinate(),
                NativeRecordCoordinate::Jsonl { byte_length, .. } if *byte_length > 0
            )
    }));
    assert!(first
        .records()
        .filter_map(|record| record.lexical_document())
        .all(|document| {
            let record = first
                .records()
                .find(|record| record.event_id == document.event_id)
                .unwrap();
            document.event_id == record.event_id
                && document.session_id == record.session_id
                && document.parent_session_id.is_none()
                && document.root_session_id == record.session_id
                && document.provider_session_id.as_deref() == Some("session-a")
                && document.branch.is_none()
                && document.source_path.as_deref() == Some(record.source_path.as_str())
                && document.agent_type == "primary"
                && document.is_primary
                && document.workspace.is_none()
                && document.cwd.is_none()
        }));
    assert!(first.records().all(|record| {
        (record.lexical_document().is_some())
            == (record.event_type == EventType::Message
                && record.verified_content_locator.is_some()
                && record.verified_content_indexed_text.is_some())
    }));
    let tool_call = first
        .records()
        .find(|record| record.event_type == EventType::ToolCall)
        .expect("fixture retains tool-call metadata");
    assert!(
        tool_call.lexical_document().is_none(),
        "non-display metadata must not bypass exact hydration"
    );
    assert!(first
        .plans
        .iter()
        .all(|plan| plan.native_session_id != "decoy-session"));
}

#[test]
fn cursor_source_backed_parser_excludes_output_bodies_without_replay() {
    let temp = tempdir();
    let data_dir = temp.path().join("cursor-data");
    let projects = data_dir.join("projects");
    let sentinel = "CURSOR_OUTPUT_BODY_MUST_NOT_ENTER_SOURCE_BACKED_CORE";
    write_transcript(
        &projects,
        "project",
        "output-session",
        [
            user("searchable message"),
            tool_result(&sentinel.repeat(512)),
        ],
    );

    let mut sink = CollectingSink::default();
    let summary = extract_cursor_source_backed_cold(&data_dir, &mut sink).unwrap();

    assert_eq!(summary.projected_records, 1);
    assert_eq!(summary.indexed_documents, 1);
    assert_eq!(sink.terminals[0].physical_records, 2);
    assert_eq!(sink.terminals[0].projected_records, 1);
    assert_eq!(
        sink.records()
            .next()
            .and_then(CursorSourceBackedRecord::lexical_document)
            .map(|document| document.body),
        Some("searchable message".to_owned())
    );
    assert!(sink.records().all(|record| !record
        .lexical_body
        .as_deref()
        .is_some_and(|body| body.contains(sentinel))));
}

struct MutatingSink {
    transcript: PathBuf,
    inner: CollectingSink,
    mutated: bool,
}

impl CursorSourceBackedSink for MutatingSink {
    fn begin_cursor_source(&mut self, plan: &CursorSourceBackedSourcePlan) -> Result<()> {
        self.inner.begin_cursor_source(plan)
    }

    fn stage_cursor_source_page(&mut self, page: CursorSourceBackedPage) -> Result<()> {
        self.inner.stage_cursor_source_page(page)?;
        if !self.mutated {
            let mut transcript = OpenOptions::new()
                .append(true)
                .open(&self.transcript)
                .unwrap();
            serde_json::to_writer(&mut transcript, &user("late mutation")).unwrap();
            transcript.write_all(b"\n").unwrap();
            transcript.flush().unwrap();
            self.mutated = true;
        }
        Ok(())
    }

    fn finish_cursor_source(&mut self, terminal: CursorSourceBackedTerminal) -> Result<()> {
        self.inner.finish_cursor_source(terminal)
    }

    fn abort_cursor_source(&mut self) {
        self.inner.abort_cursor_source();
    }
}

#[test]
fn cursor_source_backed_extraction_aborts_when_source_changes_during_projection() {
    let temp = tempdir();
    let data_dir = temp.path().join("cursor-data");
    let projects = data_dir.join("projects");
    let transcript = write_transcript(
        &projects,
        "project",
        "mutable-session",
        (0..=CURSOR_SOURCE_BACKED_PAGE_MAX_ROWS).map(|index| user(&format!("message-{index}"))),
    );
    let mut sink = MutatingSink {
        transcript,
        inner: CollectingSink::default(),
        mutated: false,
    };

    assert!(matches!(
        extract_cursor_source_backed_cold(&data_dir, &mut sink),
        Err(CaptureError::SourceChangedDuringCapture)
    ));
    assert!(sink.mutated);
    assert_eq!(sink.inner.aborted, 1);
    assert!(sink.inner.terminals.is_empty());
}

#[test]
fn cursor_source_backed_short_message_is_searchable_and_exactly_hydratable() {
    let temp = tempdir();
    let data_dir = temp.path().join("cursor-data");
    let projects = data_dir.join("projects");
    let short_text = "short Cursor hydration fixture";
    write_transcript(&projects, "project", "short-session", [user(short_text)]);

    let mut sink = CollectingSink::default();
    extract_cursor_source_backed_cold(&data_dir, &mut sink).unwrap();
    let record = sink.records().next().unwrap();

    assert_eq!(
        record.lexical_document().unwrap().body,
        short_text,
        "Core admission must use the exact-hydration eligibility contract"
    );
    assert_eq!(
        record.verified_content_indexed_text.as_deref(),
        Some(short_text)
    );
    let verified_locator = record
        .verified_content_locator
        .as_ref()
        .expect("every searchable Cursor message has an exact content address");
    assert!(verified_locator
        .content_ref()
        .verifies(short_text.as_bytes()));
    assert_eq!(
        hydrate_cursor_source_backed_message(&data_dir, record).unwrap(),
        short_text
    );
}

#[test]
fn cursor_source_backed_all_admitted_route_shapes_index_and_complete_show_exactly() {
    let temp = tempdir();
    let data_dir = temp.path().join("cursor-data");
    let projects = data_dir.join("projects");
    let complete_text = format!(
        "{}cursor-complete-show-tail{}",
        "prefix-".repeat(1_024),
        "suffix-".repeat(PROVIDER_MAX_TEXT_CHARS)
    );
    let transcript = write_transcript(
        &projects,
        "project",
        "route-session",
        [user(&complete_text)],
    );
    let session_dir = transcript.parent().unwrap().to_path_buf();
    let agent_transcripts = session_dir.parent().unwrap().to_path_buf();
    let project = agent_transcripts.parent().unwrap().to_path_buf();
    let admitted_roots = [
        data_dir,
        projects,
        project,
        agent_transcripts,
        session_dir,
        transcript,
    ];
    let mut expected_event_id = None;
    let mut expected_locator = None;

    for selected_root in admitted_roots {
        let mut sink = CollectingSink::default();
        let summary = extract_cursor_source_backed_cold(&selected_root, &mut sink).unwrap();
        assert_eq!(summary.discovered_sources, 1);
        assert_eq!(summary.indexed_documents, 1);
        let record = sink.records().next().unwrap();
        let document = record
            .lexical_document()
            .expect("every admitted Cursor message must have exact indexed content");
        assert_eq!(
            document.body.as_str(),
            record.verified_content_indexed_text.as_deref().unwrap()
        );
        assert!(
            document.body.len() < complete_text.len(),
            "the index must retain policy text, not a complete-body fallback"
        );
        assert_eq!(
            hydrate_cursor_source_backed_message(&selected_root, record).unwrap(),
            complete_text,
            "complete show must reopen the exact source for every admitted route shape"
        );

        if let Some(expected) = expected_event_id {
            assert_eq!(record.event_id, expected);
            assert_eq!(&record.locator, expected_locator.as_ref().unwrap());
        } else {
            expected_event_id = Some(record.event_id);
            expected_locator = Some(record.locator.clone());
        }
    }
}

#[test]
fn cursor_source_backed_exact_locator_hydrates_and_rejects_root_or_source_mutation() {
    let temp = tempdir();
    let data_dir = temp.path().join("cursor-data");
    let projects = data_dir.join("projects");
    let complete_text = format!(
        "{}cursor-tail-term{}",
        "x".repeat(3_000),
        "y".repeat(PROVIDER_MAX_TEXT_CHARS)
    );
    let transcript = write_transcript(&projects, "project", "long-session", [user(&complete_text)]);
    let mut sink = CollectingSink::default();
    extract_cursor_source_backed_cold(&data_dir, &mut sink).unwrap();
    let record = sink.records().next().unwrap().clone();

    assert_eq!(
        hydrate_cursor_source_backed_message(&data_dir, &record).unwrap(),
        complete_text
    );
    assert_eq!(
        record.lexical_body.as_deref().unwrap().chars().count(),
        PROVIDER_MAX_TEXT_CHARS
    );
    assert!(record
        .lexical_body
        .as_deref()
        .unwrap()
        .contains("cursor-tail-term"));
    assert_eq!(
        record.lexical_body.as_deref(),
        record.verified_content_indexed_text.as_deref()
    );
    let verified_locator = record
        .verified_content_locator
        .as_ref()
        .expect("truncated message has verified complete-content address");
    assert_eq!(
        verified_locator.kind(),
        crate::complete_content::jsonl::EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND
    );
    assert_eq!(
        record
            .verified_content_indexed_text
            .as_deref()
            .unwrap()
            .chars()
            .count(),
        PROVIDER_MAX_TEXT_CHARS
    );

    let mut altered_binding = verified_locator.source_locator().unwrap().value().to_vec();
    altered_binding[16] ^= 0x01;
    let mut altered_record = record.clone();
    altered_record.verified_content_locator = Some(
        crate::complete_content::VerifiedContentLocatorV1::new(
            verified_locator.role(),
            verified_locator.content_profile(),
            verified_locator.content_ref().clone(),
            verified_locator.family(),
            verified_locator.kind(),
            &altered_binding,
            verified_locator.native_record_id(),
            verified_locator.record_sha256().clone(),
        )
        .unwrap(),
    );
    assert!(matches!(
        hydrate_cursor_source_backed_message(&data_dir, &altered_record),
        Err(CaptureError::SourceChangedDuringCapture)
    ));
    let mut wrong_digest = *record.locator.record_digest();
    wrong_digest[0] ^= 1;
    let mut wrong_digest_record = record.clone();
    wrong_digest_record.locator = SourceRecordLocator::new(
        record.locator.source().clone(),
        record.locator.coordinate().clone(),
        record.locator.revision_policy(),
        record.locator.certified_source_revision_digest().copied(),
        wrong_digest,
    )
    .unwrap();
    assert!(matches!(
        hydrate_cursor_source_backed_message(&data_dir, &wrong_digest_record),
        Err(CaptureError::SourceChangedDuringCapture)
    ));

    let relocated_data_dir = temp.path().join("relocated-cursor-data");
    let relocated_projects = relocated_data_dir.join("projects");
    write_transcript(
        &relocated_projects,
        "project",
        "long-session",
        [user(&complete_text)],
    );
    assert!(matches!(
        hydrate_cursor_source_backed_message(&relocated_data_dir, &record),
        Err(CaptureError::SourceChangedDuringCapture)
    ));

    let mut mutated = fs::read(&transcript).unwrap();
    let position = mutated
        .iter()
        .position(|byte| *byte == b'x')
        .expect("fixture contains mutable body bytes");
    mutated[position] = b'y';
    fs::write(&transcript, mutated).unwrap();
    assert!(matches!(
        hydrate_cursor_source_backed_message(&data_dir, &record),
        Err(CaptureError::SourceChangedDuringCapture)
    ));
}

#[test]
fn cursor_catalog_descriptor_growth_is_bounded_at_provider_cap() {
    const TRANSCRIPTS: usize = 128;

    let temp = tempdir();
    let data_dir = temp.path().join("cursor-data");
    let projects = data_dir.join("projects");
    for index in 0..TRANSCRIPTS {
        write_transcript(
            &projects,
            "project",
            &format!("session-{index:03}"),
            [user("bounded catalog")],
        );
    }
    #[cfg(target_os = "linux")]
    let descriptors_before = fs::read_dir("/proc/self/fd").unwrap().count();

    let inventory = discover_cursor_transcripts(&data_dir);

    assert!(inventory.completed);
    assert_eq!(inventory.transcripts.len(), TRANSCRIPTS);
    #[cfg(target_os = "linux")]
    assert!(
        fs::read_dir("/proc/self/fd").unwrap().count() <= descriptors_before + 64,
        "the resident Cursor catalog must not retain one descriptor per transcript"
    );
    inventory.revalidate().unwrap();
}

#[test]
fn cursor_catalog_rejects_same_size_rewrite_with_restored_mtime() {
    use std::fs::FileTimes;

    let temp = tempdir();
    let data_dir = temp.path().join("cursor-data");
    let projects = data_dir.join("projects");
    let path = write_transcript(&projects, "project", "rewrite", [user("first")]);
    let source = discover_cursor_transcripts(&data_dir).transcripts.remove(0);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let mut bytes = fs::read(&path).unwrap();
    let index = bytes.iter().position(|byte| *byte == b'f').unwrap();
    bytes[index] = b'w';
    fs::write(&path, bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();

    assert!(matches!(
        freeze_cursor_source(&source),
        Err(CaptureError::SourceChangedDuringCapture)
    ));
}

#[test]
fn cursor_catalog_accepts_hardlink_aliases() {
    let temp = tempdir();
    let data_dir = temp.path().join("cursor-data");
    let projects = data_dir.join("projects");
    let first = write_transcript(&projects, "project", "first", [user("alias")]);
    let second = transcript_path(&projects, "project", "second");
    fs::create_dir_all(second.parent().unwrap()).unwrap();
    fs::hard_link(first, second).unwrap();

    let inventory = discover_cursor_transcripts(&data_dir);

    assert!(inventory.completed);
    assert_eq!(inventory.transcripts.len(), 2);
    for source in inventory.transcripts {
        freeze_cursor_source(&source).unwrap().revalidate().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn cursor_catalog_rejects_root_and_leaf_swaps() {
    let temp = tempdir();
    let data_dir = temp.path().join("cursor-data");
    let projects = data_dir.join("projects");
    write_transcript(&projects, "project", "root-swap", [user("original")]);
    let source = discover_cursor_transcripts(&data_dir).transcripts.remove(0);
    fs::rename(&data_dir, temp.path().join("cursor-data-displaced")).unwrap();
    write_transcript(
        &data_dir.join("projects"),
        "project",
        "root-swap",
        [user("replacement")],
    );
    assert!(freeze_cursor_source(&source)
        .and_then(|frozen| frozen.revalidate())
        .is_err());

    let leaf_data = temp.path().join("leaf-cursor-data");
    let leaf_projects = leaf_data.join("projects");
    let leaf = write_transcript(&leaf_projects, "project", "leaf-swap", [user("original")]);
    let leaf_source = discover_cursor_transcripts(&leaf_data)
        .transcripts
        .remove(0);
    fs::rename(&leaf, leaf.with_extension("displaced")).unwrap();
    write_transcript(
        &leaf_projects,
        "project",
        "leaf-swap",
        [user("replacement")],
    );
    assert!(freeze_cursor_source(&leaf_source).is_err());
}
