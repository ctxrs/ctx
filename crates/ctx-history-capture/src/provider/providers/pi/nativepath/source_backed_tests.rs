use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{
    EventHydrationRequest, LocatorRevisionPolicy, NativeRecordCoordinate, TypedKey,
};

use crate::{
    provider::importer::provider_path_identity, test_support_paths::tempdir, ProviderAdapterContext,
};

use super::*;

fn context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "pi-source-backed-test".to_owned(),
        source_path: None,
        source_root: Some(root.to_path_buf()),
        imported_at: "2026-07-28T12:00:00Z".parse().unwrap(),
    }
}

fn header(session_id: &str, parent_session_id: Option<&str>) -> String {
    let mut value = serde_json::json!({
        "type": "session",
        "id": session_id,
        "version": 3,
        "timestamp": "2026-07-28T12:00:00Z",
        "cwd": "/workspace/pi-source-backed",
    });
    if let Some(parent_session_id) = parent_session_id {
        value["parentSession"] = serde_json::json!(parent_session_id);
    }
    value.to_string()
}

fn message(id: &str, content: &str, second: u64) -> String {
    serde_json::json!({
        "type": "message",
        "id": id,
        "parentId": null,
        "timestamp": format!("2026-07-28T12:00:{second:02}Z"),
        "message": {
            "role": "user",
            "content": content,
        },
    })
    .to_string()
}

fn write_session(
    path: &Path,
    session_id: &str,
    parent_session_id: Option<&str>,
    messages: &[String],
) {
    let mut records = vec![header(session_id, parent_session_id)];
    records.extend_from_slice(messages);
    fs::write(path, format!("{}\n", records.join("\n"))).unwrap();
}

#[test]
fn cold_projection_certifies_lineage_and_old_locator_survives_append() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let parent_path = root.join("00-parent.jsonl");
    let child_path = root.join("01-child.jsonl");
    let parent_session = "pi-parent-session";
    let child_session = "pi-child-session";
    let long_message = format!(
        "pi exact-content sentinel {} complete-tail",
        "界".repeat(2_176)
    );
    write_session(
        &parent_path,
        parent_session,
        None,
        &[message("parent-message", "parent lexical sentinel", 1)],
    );
    write_session(
        &child_path,
        child_session,
        Some(parent_session),
        &[message("child-message", &long_message, 2)],
    );

    let winning = PiSourceBackedRoot::winning(&root).unwrap();
    let mut cold_pages = Vec::new();
    let cold =
        project_pi_source_backed_root_cold(&winning, context(&root), |page| cold_pages.push(page))
            .unwrap();
    assert_eq!(cold.inventory.observed_sources(), 2);
    assert_eq!(cold.sources.len(), 2);
    assert!(cold
        .sources
        .iter()
        .all(|source| source.lifecycle == PiSourceLifecycle::Fresh));

    let cold_documents = cold_pages
        .into_iter()
        .flat_map(|page| {
            assert!(page
                .documents
                .iter()
                .all(|document| document.source == page.source));
            page.documents
        })
        .collect::<Vec<_>>();
    assert_eq!(cold_documents.len(), 2);
    let parent_document = cold_documents
        .iter()
        .find(|document| document.provider_session_id.as_deref() == Some(parent_session))
        .unwrap();
    let child_document = cold_documents
        .iter()
        .find(|document| document.provider_session_id.as_deref() == Some(child_session))
        .unwrap()
        .clone();

    assert_eq!(parent_document.parent_session_id, None);
    assert_eq!(parent_document.root_session_id, parent_document.session_id);
    assert_eq!(parent_document.agent_type, "primary");
    assert!(parent_document.is_primary);
    assert_eq!(
        child_document.parent_session_id,
        Some(parent_document.session_id)
    );
    assert_eq!(child_document.root_session_id, parent_document.session_id);
    assert_eq!(child_document.agent_type, "subagent");
    assert!(!child_document.is_primary);
    assert_eq!(child_document.branch, None);
    assert_eq!(
        child_document.source_path.as_deref(),
        Some(provider_path_identity(&child_path).unwrap().as_str())
    );
    assert_eq!(child_document.body, long_message);
    assert!(child_document.body.ends_with("complete-tail"));
    assert_eq!(
        child_document.locator.revision_policy(),
        LocatorRevisionPolicy::StableRecordEvidence
    );
    assert!(child_document
        .locator
        .certified_source_revision_digest()
        .is_none());
    let NativeRecordCoordinate::Jsonl {
        physical_ordinal,
        native_session_key,
        native_event_key,
        ..
    } = child_document.locator.coordinate()
    else {
        panic!("expected Pi JSONL locator");
    };
    assert_eq!(*physical_ordinal, 1);
    assert_eq!(
        native_session_key,
        &Some(TypedKey::utf8(child_session).unwrap())
    );
    assert_eq!(
        native_event_key,
        &Some(TypedKey::utf8("child-message").unwrap())
    );

    let child_cold = cold
        .sources
        .iter()
        .find(|source| source.route.path == child_path)
        .unwrap()
        .clone();
    assert_eq!(child_cold.certificate.counts().complete_records, 2);
    assert_eq!(child_cold.certificate.counts().retained_records, 1);
    assert_eq!(child_cold.certificate.counts().ignored_records, 1);
    assert_eq!(child_cold.certificate.counts().indexed_documents, 1);
    assert_eq!(
        child_cold.certificate.frontier().unwrap().checkpoint_kind(),
        "pi-nativepath-checkpoint-v1"
    );

    let mut replay_pages = Vec::new();
    let replay = project_pi_source_backed_root_cold(&winning, context(&root), |page| {
        replay_pages.push(page)
    })
    .unwrap();
    let replay_child = replay_pages
        .into_iter()
        .flat_map(|page| page.documents)
        .find(|document| document.provider_session_id.as_deref() == Some(child_session))
        .unwrap();
    assert_eq!(replay_child.event_id, child_document.event_id);
    assert_eq!(replay_child.session_id, child_document.session_id);
    assert_eq!(replay_child.source, child_document.source);
    assert_eq!(replay_child.locator, child_document.locator);
    assert_eq!(
        replay.inventory.inventory_digest(),
        cold.inventory.inventory_digest()
    );

    OpenOptions::new()
        .append(true)
        .open(&child_path)
        .unwrap()
        .write_all(format!("{}\n", message("appended-message", "append sentinel", 3)).as_bytes())
        .unwrap();

    let mut append_scanner = PiSourceBackedScanner::open(
        &child_path,
        context(&root),
        Some(child_cold.certificate.clone()),
    )
    .unwrap();
    let mut appended_documents = Vec::new();
    while let Some(page) = append_scanner.next_page().unwrap() {
        appended_documents.extend(page.documents);
    }
    let appended = append_scanner.finish().unwrap();
    assert_eq!(appended.lifecycle, PiSourceLifecycle::Append);
    assert_eq!(appended_documents.len(), 1);
    assert_eq!(appended.certificate.counts().complete_records, 3);
    assert_eq!(appended.certificate.counts().retained_records, 2);
    assert_eq!(appended.certificate.counts().ignored_records, 1);
    assert_eq!(appended.certificate.counts().indexed_documents, 2);
    assert_eq!(appended.checkpoint.next_ordinal, 3);

    let resolver = PiSourceBackedResolver::new([appended.route]).unwrap();
    let request =
        EventHydrationRequest::new(child_document.event_id, child_document.locator).unwrap();
    assert_eq!(
        resolver.hydrate_message(&request).unwrap().as_deref(),
        Some(long_message.as_str())
    );
}

#[test]
fn historical_omp_root_is_explicit_only() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".omp").join("agent").join("sessions");
    fs::create_dir_all(&root).unwrap();
    write_session(
        &root.join("explicit.jsonl"),
        "explicit-omp-session",
        None,
        &[message("explicit-message", "explicit sentinel", 1)],
    );

    assert!(matches!(
        PiSourceBackedRoot::winning(&root),
        Err(PiSourceBackedError::HistoricalRootRequiresExplicit(path)) if path == root
    ));
    let explicit = PiSourceBackedRoot::explicit(&root);
    let mut documents = Vec::new();
    let projection = project_pi_source_backed_root_cold(&explicit, context(&root), |page| {
        documents.extend(page.documents)
    })
    .unwrap();
    assert_eq!(projection.inventory.observed_sources(), 1);
    assert_eq!(documents.len(), 1);
}
