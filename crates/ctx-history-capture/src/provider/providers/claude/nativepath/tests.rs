use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

fn projects_root(root: &Path) -> PathBuf {
    root.join(".claude/projects")
}

fn session_path(projects: &Path, project: &str, session: &str) -> PathBuf {
    projects.join(project).join(format!("{session}.jsonl"))
}

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(path).unwrap());
    for line in lines {
        writeln!(writer, "{line}").unwrap();
    }
    writer.flush().unwrap();
}

fn append_line(path: &Path, line: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}

fn message(session: &str, uuid: &str, text: &str) -> Value {
    json!({
        "sessionId": session,
        "type": "user",
        "uuid": uuid,
        "timestamp": "2026-01-01T00:00:00.000Z",
        "cwd": "/workspace/project",
        "version": "2.1.219",
        "gitBranch": "main",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn discover_session(projects: &Path, session: &str) -> DiscoveredClaudeSession {
    discover_projects(projects)
        .unwrap()
        .sessions
        .into_iter()
        .find(|source| source.key.root_session_id == session && source.key.agent_id.is_none())
        .unwrap()
}

fn parse_collect(
    source: &DiscoveredClaudeSession,
    previous: Option<&ParseCheckpoint>,
) -> (ParseOutput, Vec<ClaudeRetainedRow>, Vec<(usize, usize)>) {
    let mut rows = Vec::new();
    let mut pages = Vec::new();
    let output = parse_session(source, previous, |page| {
        pages.push((page.rows.len(), page.estimated_bytes));
        rows.extend(page.rows);
        Ok(())
    })
    .unwrap();
    (output, rows, pages)
}

fn parse_discard(
    source: &DiscoveredClaudeSession,
    previous: Option<&ParseCheckpoint>,
) -> ParseOutput {
    parse_session(source, previous, |_| Ok(())).unwrap()
}

fn scan_owned(
    source: &DiscoveredClaudeSession,
    previous: Option<&ParseCheckpoint>,
    profile: ClaudeNativeProfile,
) -> (
    ParseOutput,
    Vec<ClaudeNativePage>,
    Vec<ClaudeNativeProOutputPage>,
) {
    let mut scanner = ClaudeNativeScanner::new(source.clone(), previous, profile).unwrap();
    let mut core = Vec::new();
    let mut pro = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        match page {
            ClaudeNativeOwnedPage::Core(page) => core.push(*page),
            ClaudeNativeOwnedPage::Pro(page) => pro.push(*page),
        }
    }
    (scanner.finish().unwrap(), core, pro)
}

fn assert_core_pages_equal(left: &[ClaudeNativePage], right: &[ClaudeNativePage]) {
    assert_eq!(left.len(), right.len());
    for (left, right) in left.iter().zip(right) {
        assert_eq!(left.session, right.session);
        assert_eq!(left.expected_frontier, right.expected_frontier);
        assert_eq!(left.next_safe_frontier, right.next_safe_frontier);
        assert_eq!(left.rows, right.rows);
        assert_eq!(left.rejections, right.rejections);
        assert_eq!(left.rejected_records, right.rejected_records);
        assert_eq!(left.logical_units, right.logical_units);
        assert_eq!(left.serialized_bytes, right.serialized_bytes);
        assert_eq!(left.terminal, right.terminal);
        assert_eq!(left.certificate, right.certificate);
        assert_eq!(left.identity, right.identity);
    }
}

mod certification;
mod cursor_lifecycle;
mod discovery;
mod profile_output;
mod record_privacy;
