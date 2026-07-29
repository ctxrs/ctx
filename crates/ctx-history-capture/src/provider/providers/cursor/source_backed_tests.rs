use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{LocatorRevisionPolicy, NativeRecordCoordinate, StableEntityId};
use serde_json::json;
use tempfile::TempDir;

use super::{
    extract_cursor_source_backed_cold, hydrate_cursor_source_backed_message,
    CursorSourceBackedPage, CursorSourceBackedRecord, CursorSourceBackedSink,
    CursorSourceBackedSourcePlan, CursorSourceBackedSummary, CursorSourceBackedTerminal,
    CURSOR_PUBLICATION_PAGE_MAX_BYTES, CURSOR_PUBLICATION_PAGE_MAX_ROWS,
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
    assert_eq!(first_summary.indexed_documents, 4);
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
        4
    );
    assert_eq!(first.aborted, 0);
    assert!(first.pages.iter().all(
        |page| page.records.len() <= CURSOR_PUBLICATION_PAGE_MAX_ROWS
            && page.estimated_bytes <= CURSOR_PUBLICATION_PAGE_MAX_BYTES
    ));
    assert!(first.records().all(|record| {
        record.locator.revision_policy() == LocatorRevisionPolicy::ExactSourceRevision
            && record.locator.certified_source_revision_digest().is_some()
            && matches!(
                record.locator.coordinate(),
                NativeRecordCoordinate::Jsonl { byte_length, .. } if *byte_length > 0
            )
    }));
    assert!(first.records().all(|record| {
        let document = record
            .lexical_document()
            .expect("fixture events all have lexical projections");
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
    assert!(first
        .plans
        .iter()
        .all(|plan| plan.native_session_id != "decoy-session"));
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
