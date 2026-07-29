use std::fs;

use chrono::TimeZone;
use ctx_history_core::{ContentSourceResolver, EventHydrationRequest, SessionHydrationRequest};
use serde_json::json;

use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf, Vec<Vec<u8>>) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-alpha");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("meta.json"),
        serde_json::to_vec(&json!({
            "session_id": "session-alpha",
            "title": "metadata-only-sentinel-a",
            "start_time": "2026-07-28T12:00:00Z",
            "git_branch": "main",
            "environment": {
                "working_directory": "/workspace/project"
            },
            "agent_profile": {
                "name": "vibe"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let records = vec![
        serde_json::to_vec(&json!({
            "role": "user",
            "message_id": "message-user",
            "timestamp": "2026-07-28T12:00:01Z",
            "content": format!("cold exact sentinel {}", "x".repeat(4_096))
        }))
        .unwrap(),
        serde_json::to_vec(&json!({
            "role": "assistant",
            "message_id": "message-assistant",
            "timestamp": "2026-07-28T12:00:02Z",
            "content": "bounded assistant response"
        }))
        .unwrap(),
    ];
    let mut messages = Vec::new();
    for record in &records {
        messages.extend_from_slice(record);
        messages.push(b'\n');
    }
    fs::write(session.join("messages.jsonl"), messages).unwrap();
    (temp, root, records)
}

fn imported_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 28, 12, 30, 0)
        .single()
        .unwrap()
}

#[test]
fn cold_scan_emits_stable_full_documents_and_exact_grouped_hydration() {
    let (_temp, root, records) = fixture();
    let first = scan_mistral_vibe_source_backed(&root, imported_at()).unwrap();
    let second =
        scan_mistral_vibe_source_backed(&root, imported_at() + chrono::Duration::hours(1)).unwrap();

    assert_eq!(first.leaves.len(), 1);
    let leaf = &first.leaves[0];
    assert_eq!(leaf.source.counts().complete_records, 2);
    assert_eq!(leaf.source.counts().retained_records, 2);
    assert_eq!(leaf.source.counts().indexed_documents, 2);
    assert_eq!(leaf.documents.len(), 2);
    assert_eq!(
        leaf.documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>(),
        second.leaves[0]
            .documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        leaf.documents[0].session_id,
        second.leaves[0].documents[0].session_id
    );
    let expected_source_path = fs::canonicalize(root.join("session-alpha/messages.jsonl"))
        .unwrap()
        .display()
        .to_string();
    assert!(leaf.documents.iter().all(|document| {
        document.parent_session_id.is_none()
            && document.root_session_id == document.session_id
            && document.provider_session_id.as_deref() == Some("session-alpha")
            && document.branch.as_deref() == Some("main")
            && document.source_path.as_deref() == Some(expected_source_path.as_str())
            && document.agent_type == AgentType::Primary.as_str()
            && document.is_primary
    }));
    let first_record: Value = serde_json::from_slice(&records[0]).unwrap();
    let expected_first_body = first_record["content"].as_str().unwrap();
    assert_eq!(leaf.documents[0].body, expected_first_body);
    assert!(leaf.documents[0].body.ends_with(&"x".repeat(4_096)));
    assert!(leaf
        .documents
        .iter()
        .all(|document| !document.body.contains("metadata-only-sentinel")));
    assert!(leaf.documents.iter().all(|document| {
        document.locator.revision_policy() == LocatorRevisionPolicy::ExactSourceRevision
            && document
                .locator
                .certified_source_revision_digest()
                .is_some()
    }));

    let requests = leaf
        .documents
        .iter()
        .map(|document| {
            EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
        })
        .collect::<Vec<_>>();
    let session_request =
        SessionHydrationRequest::new(leaf.documents[0].session_id, requests).unwrap();
    let hydrated = first.resolver.hydrate_session(&session_request).unwrap();
    assert_eq!(hydrated.len(), records.len());
    for (hydrated, document) in hydrated.iter().zip(&leaf.documents) {
        assert_eq!(hydrated.provider_bytes, document.body.as_bytes());
    }
}

#[test]
fn lineage_and_filter_fields_follow_native_session_metadata() {
    let (_temp, root, _) = fixture();
    write_related_session(
        &root,
        "session-child",
        Some("session-alpha"),
        "feature/child",
    );
    write_related_session(
        &root,
        "session-grandchild",
        Some("session-child"),
        "feature/grandchild",
    );

    let scan = scan_mistral_vibe_source_backed(&root, imported_at()).unwrap();
    assert_eq!(scan.leaves.len(), 3);
    let document = |provider_session_id: &str| {
        scan.leaves
            .iter()
            .flat_map(|leaf| &leaf.documents)
            .find(|document| document.provider_session_id.as_deref() == Some(provider_session_id))
            .unwrap()
    };
    let root_document = document("session-alpha");
    let child_document = document("session-child");
    let grandchild_document = document("session-grandchild");

    assert_eq!(
        child_document.parent_session_id,
        Some(root_document.session_id)
    );
    assert_eq!(child_document.root_session_id, root_document.session_id);
    assert_eq!(
        grandchild_document.parent_session_id,
        Some(child_document.session_id)
    );
    assert_eq!(
        grandchild_document.root_session_id,
        root_document.session_id
    );
    for related in [child_document, grandchild_document] {
        assert_eq!(related.agent_type, AgentType::Subagent.as_str());
        assert!(!related.is_primary);
        assert!(related.source_path.as_deref().is_some_and(|path| {
            path.ends_with(&format!(
                "{}/messages.jsonl",
                related.provider_session_id.as_deref().unwrap()
            ))
        }));
    }
    assert_eq!(child_document.branch.as_deref(), Some("feature/child"));
    assert_eq!(
        grandchild_document.branch.as_deref(),
        Some("feature/grandchild")
    );
}

#[test]
fn metadata_mutation_invalidates_exact_hydration_without_changing_ids() {
    let (_temp, root, _) = fixture();
    let before = scan_mistral_vibe_source_backed(&root, imported_at()).unwrap();
    let document = &before.leaves[0].documents[0];
    let request = EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap();
    assert!(before.resolver.hydrate_event(&request).is_ok());

    let metadata_path = root.join("session-alpha/meta.json");
    let metadata = fs::read_to_string(&metadata_path)
        .unwrap()
        .replace("metadata-only-sentinel-a", "metadata-only-sentinel-b");
    fs::write(metadata_path, metadata).unwrap();

    let failure = before.resolver.hydrate_event(&request).unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);

    let after = scan_mistral_vibe_source_backed(&root, imported_at()).unwrap();
    assert_eq!(
        before.leaves[0]
            .documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>(),
        after.leaves[0]
            .documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>()
    );
    assert_ne!(
        before.leaves[0].documents[0]
            .locator
            .certified_source_revision_digest(),
        after.leaves[0].documents[0]
            .locator
            .certified_source_revision_digest()
    );
}

#[cfg(unix)]
#[test]
fn compound_authority_mistral_rejects_missing_sibling_and_ancestor_swaps() {
    let (_temp, root, _) = fixture();
    let scan = scan_mistral_vibe_source_backed(&root, imported_at()).unwrap();
    let document = &scan.leaves[0].documents[0];
    let request = EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap();
    let metadata = root.join("session-alpha/meta.json");
    let bytes = fs::read(&metadata).unwrap();

    fs::remove_file(&metadata).unwrap();
    assert!(scan.resolver.hydrate_event(&request).is_err());
    fs::write(&metadata, &bytes).unwrap();
    assert!(scan.resolver.hydrate_event(&request).is_err());

    let retired = root.with_extension("retired");
    fs::rename(&root, &retired).unwrap();
    fs::create_dir_all(root.join("session-alpha")).unwrap();
    fs::copy(
        retired.join("session-alpha/meta.json"),
        root.join("session-alpha/meta.json"),
    )
    .unwrap();
    fs::copy(
        retired.join("session-alpha/messages.jsonl"),
        root.join("session-alpha/messages.jsonl"),
    )
    .unwrap();
    assert!(scan.resolver.hydrate_event(&request).is_err());
}

fn write_related_session(
    root: &Path,
    session_id: &str,
    parent_session_id: Option<&str>,
    git_branch: &str,
) {
    let session = root.join(session_id);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("meta.json"),
        serde_json::to_vec(&json!({
            "session_id": session_id,
            "parent_session_id": parent_session_id,
            "start_time": "2026-07-28T12:00:00Z",
            "git_branch": git_branch,
            "environment": {
                "working_directory": "/workspace/project"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let record = serde_json::to_vec(&json!({
        "role": "user",
        "message_id": format!("{session_id}-message"),
        "timestamp": "2026-07-28T12:00:01Z",
        "content": format!("{session_id} body")
    }))
    .unwrap();
    let mut messages = record;
    messages.push(b'\n');
    fs::write(session.join("messages.jsonl"), messages).unwrap();
}
