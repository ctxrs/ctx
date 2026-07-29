use std::{
    fs::{self, File},
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

fn scan(
    leaf: ClaudeSourceBackedLeaf,
    previous: Option<&CertifiedSource>,
) -> (ClaudeSourceBackedScan, Vec<LexicalDocument>) {
    let mut scanner = ClaudeSourceBackedScanner::new(leaf, previous).unwrap();
    let mut documents = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        documents.extend(page.documents);
    }
    (scanner.finish().unwrap(), documents)
}

#[test]
fn source_backed_cold_and_noop_extract_stable_bounded_documents_and_frontiers() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let primary = session_path(&projects, "-project", "session-1");
    let subagent = projects.join("-project/session-1/subagents/agent-review.jsonl");
    let full_body = format!("claude-full-{}-claude-tail-sentinel", "c".repeat(8_192));
    write_lines(
        &primary,
        &[
            message("session-1", "message-1", &full_body),
            json!({
                "sessionId": "session-1",
                "type": "assistant",
                "uuid": "tool-1",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call-1",
                        "name": "Read",
                        "input": {"file_path": "src/lib.rs"}
                    }]
                }
            }),
        ],
    );
    write_lines(
        &subagent,
        &[message("session-1", "subagent-message", "subagent body")],
    );

    let inventory = discover_claude_source_backed(&projects).unwrap();
    assert_eq!(inventory.leaves().len(), 2);
    let certified_inventory = inventory.certify().unwrap();
    assert_eq!(certified_inventory.observed_sources(), 2);

    let leaf = inventory
        .leaves()
        .iter()
        .find(|leaf| leaf.source.key.provider_session_id() == "session-1")
        .unwrap()
        .clone();
    let (cold, cold_documents) = scan(leaf.clone(), None);
    assert_eq!(cold_documents.len(), 2);
    assert_eq!(cold_documents[0].body, full_body);
    assert!(cold_documents[0].body.ends_with("claude-tail-sentinel"));
    assert_eq!(cold_documents[0].session_id, leaf.session_id);
    assert_eq!(cold_documents[0].parent_session_id, None);
    assert_eq!(cold_documents[0].root_session_id, leaf.session_id);
    assert_eq!(&cold_documents[0].source, leaf.source_key());
    assert_eq!(
        cold_documents[0].provider_session_id.as_deref(),
        Some("session-1")
    );
    assert_eq!(cold_documents[0].branch.as_deref(), Some("main"));
    assert_eq!(cold_documents[0].source_path.as_deref(), primary.to_str());
    assert_eq!(cold_documents[0].agent_type, "primary");
    assert!(cold_documents[0].is_primary);
    assert_eq!(cold_documents[0].cwd.as_deref(), Some("/workspace/project"));
    assert_eq!(cold_documents[1].touched_files, ["src/lib.rs"]);
    let unsupported =
        hydrate_claude_source_record(&projects, &cold_documents[1].locator).unwrap_err();
    assert!(matches!(
        unsupported,
        ClaudeSourceBackedError::ExactDisplayUnavailable
    ));
    assert!(cold
        .source
        .frontier()
        .is_some_and(|frontier| frontier.certified_prefix_bytes() > 0));

    let (noop, noop_documents) = scan(leaf.clone(), Some(&cold.source));
    assert!(noop_documents.is_empty());
    assert_eq!(noop.source, cold.source);

    let subagent_leaf = inventory
        .leaves()
        .iter()
        .find(|leaf| {
            leaf.source
                .key
                .provider_session_id()
                .contains("/subagents/")
        })
        .unwrap()
        .clone();
    let (_, subagent_documents) = scan(subagent_leaf, None);
    assert_eq!(subagent_documents.len(), 1);
    assert_eq!(
        subagent_documents[0].parent_session_id,
        Some(leaf.session_id)
    );
    assert_eq!(subagent_documents[0].root_session_id, leaf.session_id);
    assert_eq!(subagent_documents[0].agent_type, "subagent");
    assert!(!subagent_documents[0].is_primary);
}

#[test]
fn exact_jsonl_locator_reopens_full_message_and_fails_closed_after_rewrite() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session-1");
    let full_text = format!("exact locator {} claude-exact-tail", "content ".repeat(700));
    write_lines(&path, &[message("session-1", "message-1", &full_text)]);

    let inventory = discover_claude_source_backed(&projects).unwrap();
    let leaf = inventory.leaves()[0].clone();
    let (_, documents) = scan(leaf, None);
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].body, full_text);
    assert!(documents[0].body.ends_with("claude-exact-tail"));
    let hydrated = hydrate_claude_source_record(&projects, &documents[0].locator).unwrap();
    assert_eq!(
        hydrated.decoded_display_text.as_deref(),
        Some(full_text.as_str())
    );
    assert_eq!(hydrated.provider_bytes, documents[0].body.as_bytes());

    let replacement = full_text.replace("exact locator", "stale locator");
    assert_eq!(replacement.len(), full_text.len());
    write_lines(&path, &[message("session-1", "message-1", &replacement)]);
    let error = hydrate_claude_source_record(&projects, &documents[0].locator).unwrap_err();
    assert!(matches!(
        error,
        ClaudeSourceBackedError::LocatorRecordChanged
    ));
}

#[test]
fn source_backed_claude_adapter_has_no_preview_or_store_body_fallback() {
    let source = include_str!("../source_backed.rs");
    assert!(!source.contains("MAX_BODY_PREVIEW_CHARS"));
    assert!(!source.contains("ctx_history_store"));
}

#[test]
fn explicit_leaf_cannot_claim_authoritative_tree_deletions() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session-1");
    write_lines(&path, &[message("session-1", "message-1", "body")]);

    let error = discover_claude_source_backed(&path).unwrap_err();
    assert!(matches!(
        error,
        ClaudeSourceBackedError::NonAuthoritativeRoot
    ));
}
