use std::io::Write;

use ctx_history_core::{ContentSourceResolver, EventHydrationRequest};
use serde_json::json;

use crate::test_support_paths::tempdir;

use super::*;

fn fixture(prompt: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".kimi-code");
    let session = root.join("sessions/work/session-1");
    let agent = session.join("agents/main");
    fs::create_dir_all(&agent).unwrap();
    fs::write(
        root.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": "session-1",
                "sessionDir": session,
                "workDir": "/workspace/kimi"
            })
        ),
    )
    .unwrap();
    fs::write(
        session.join("state.json"),
        json!({
            "createdAt": "2026-07-17T12:00:00Z",
            "title": "initial",
            "agents": {"main": {"type": "main"}}
        })
        .to_string(),
    )
    .unwrap();
    let wire = agent.join("wire.jsonl");
    let mut file = File::create(&wire).unwrap();
    for record in [
        json!({"type": "metadata", "created_at": 1_784_289_600_000_i64}),
        json!({
            "type": "turn.prompt",
            "time": 1_784_289_600_001_i64,
            "input": prompt
        }),
        json!({
            "type": "context.append_loop_event",
            "time": 1_784_289_600_002_i64,
            "event": {
                "type": "tool.result",
                "toolName": "bash",
                "exit_code": 0,
                "output": "SUCCESS_BODY_MUST_NOT_BE_STORED"
            }
        }),
        json!({
            "type": "context.append_loop_event",
            "time": 1_784_289_600_003_i64,
            "event": {
                "type": "tool.result",
                "toolName": "bash",
                "exit_code": 7,
                "output": "bounded failure"
            }
        }),
    ] {
        writeln!(file, "{record}").unwrap();
    }
    (temp, root, wire)
}

#[test]
fn kimi_source_backed_compound_cold_scan_and_exact_hydration() {
    let (_temp, root, _wire) = fixture("cold exact message");
    let catalog = KimiSourceBackedCatalog::discover(&root).unwrap();
    assert_eq!(catalog.inventory().observed_sources(), 1);
    assert!(catalog.revalidate_inventory().unwrap());
    let source = catalog.source_keys().next().unwrap().clone();
    let mut documents = Vec::new();
    let certificate = catalog
        .scan_source(&source, |document| {
            documents.push(document);
            Ok(())
        })
        .unwrap();
    assert_eq!(certificate.counts().complete_records, 4);
    assert_eq!(certificate.counts().indexed_documents, 2);
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0].body, "cold exact message");
    assert_eq!(documents[1].body, "bounded failure");
    assert_eq!(documents[0].root_session_id, documents[0].session_id);
    assert_eq!(documents[0].parent_session_id, None);
    assert_eq!(
        documents[0].provider_session_id.as_deref(),
        Some("session-1")
    );
    assert_eq!(documents[0].branch, None);
    assert!(documents[0].source_path.is_some());
    assert_eq!(documents[0].agent_type, AgentType::Primary.as_str());
    assert!(documents[0].is_primary);
    assert_eq!(documents[0].workspace.as_deref(), Some("/workspace/kimi"));
    assert!(documents
        .iter()
        .all(|document| !document.body.contains("SUCCESS_BODY_MUST_NOT_BE_STORED")));
    assert!(documents.iter().all(|document| matches!(
        document.locator.coordinate(),
        NativeRecordCoordinate::TreeRecord { .. }
    )));
    assert!(documents.iter().all(|document| {
        document.locator.revision_policy() == LocatorRevisionPolicy::ExactSourceRevision
    }));
    assert!(catalog.revalidate_source(&certificate).unwrap());

    let request =
        EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone()).unwrap();
    let resolver = KimiSourceBackedResolver::new(catalog);
    let hydrated = resolver.hydrate_event(&request).unwrap();
    assert_eq!(hydrated.event_id, documents[0].event_id);
    assert_eq!(hydrated.provider_bytes, b"cold exact message");
}

#[test]
fn kimi_source_backed_indexes_and_hydrates_the_full_policy_body() {
    let prompt = format!("kimi-head-{}-kimi-tail", "x".repeat(3_000));
    let (_temp, root, _wire) = fixture(&prompt);
    let catalog = KimiSourceBackedCatalog::discover(&root).unwrap();
    let source = catalog.source_keys().next().unwrap().clone();
    let mut documents = Vec::new();
    catalog
        .scan_source(&source, |document| {
            documents.push(document);
            Ok(())
        })
        .unwrap();
    assert_eq!(documents[0].body, prompt);
    assert!(documents[0].body.ends_with("kimi-tail"));

    let request =
        EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone()).unwrap();
    let hydrated = KimiSourceBackedResolver::new(catalog)
        .hydrate_event(&request)
        .unwrap();
    assert_eq!(hydrated.provider_bytes, prompt.as_bytes());
}

#[test]
fn kimi_source_backed_auxiliary_mutation_invalidates_exact_revision_not_identity() {
    let (_temp, root, _wire) = fixture("cold exact message");
    let initial = KimiSourceBackedCatalog::discover(&root).unwrap();
    let source = initial.source_keys().next().unwrap().clone();
    let mut initial_documents = Vec::new();
    let initial_certificate = initial
        .scan_source(&source, |document| {
            initial_documents.push(document);
            Ok(())
        })
        .unwrap();
    let stale_request = EventHydrationRequest::new(
        initial_documents[0].event_id,
        initial_documents[0].locator.clone(),
    )
    .unwrap();

    let state = root.join("sessions/work/session-1/state.json");
    fs::write(
        &state,
        json!({
            "createdAt": "2026-07-17T12:00:00Z",
            "title": "mutated auxiliary authority",
            "agents": {"main": {"type": "main"}}
        })
        .to_string(),
    )
    .unwrap();
    let stale = KimiSourceBackedResolver::new(initial)
        .hydrate_event(&stale_request)
        .unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleSourceEvidence);

    let refreshed = KimiSourceBackedCatalog::discover(&root).unwrap();
    let refreshed_source = refreshed.source_keys().next().unwrap().clone();
    let mut refreshed_documents = Vec::new();
    let refreshed_certificate = refreshed
        .scan_source(&refreshed_source, |document| {
            refreshed_documents.push(document);
            Ok(())
        })
        .unwrap();
    assert_eq!(source, refreshed_source);
    assert_eq!(
        initial_documents
            .iter()
            .map(|document| (document.session_id, document.event_id))
            .collect::<Vec<_>>(),
        refreshed_documents
            .iter()
            .map(|document| (document.session_id, document.event_id))
            .collect::<Vec<_>>()
    );
    assert_ne!(
        initial_certificate.observation().revision(),
        refreshed_certificate.observation().revision()
    );
    let refreshed_request = EventHydrationRequest::new(
        refreshed_documents[0].event_id,
        refreshed_documents[0].locator.clone(),
    )
    .unwrap();
    assert!(KimiSourceBackedResolver::new(refreshed)
        .hydrate_event(&refreshed_request)
        .is_ok());
}

#[cfg(unix)]
#[test]
fn compound_authority_kimi_rejects_missing_auxiliary_sibling_and_ancestor_swaps() {
    let (_temp, root, _wire) = fixture("cold exact message");
    let state = root.join("sessions/work/session-1/state.json");
    fs::remove_file(&state).unwrap();
    let missing = KimiSourceBackedCatalog::discover(&root).unwrap();
    fs::write(&state, r#"{"title":"appeared"}"#).unwrap();
    assert!(!missing.revalidate_inventory().unwrap());

    let catalog = KimiSourceBackedCatalog::discover(&root).unwrap();
    let source = catalog.source_keys().next().unwrap().clone();
    let mut documents = Vec::new();
    catalog
        .scan_source(&source, |document| {
            documents.push(document);
            Ok(())
        })
        .unwrap();
    let request =
        EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone()).unwrap();

    let state_bytes = fs::read(&state).unwrap();
    fs::rename(&state, state.with_extension("retired")).unwrap();
    fs::write(&state, state_bytes).unwrap();
    assert!(KimiSourceBackedResolver::new(catalog.clone())
        .hydrate_event(&request)
        .is_err());

    let retired_root = root.with_extension("retired");
    fs::rename(&root, &retired_root).unwrap();
    fs::create_dir_all(root.join("sessions/work/session-1/agents/main")).unwrap();
    fs::copy(
        retired_root.join("session_index.jsonl"),
        root.join("session_index.jsonl"),
    )
    .unwrap();
    fs::copy(
        retired_root.join("sessions/work/session-1/state.json"),
        root.join("sessions/work/session-1/state.json"),
    )
    .unwrap();
    fs::copy(
        retired_root.join("sessions/work/session-1/agents/main/wire.jsonl"),
        root.join("sessions/work/session-1/agents/main/wire.jsonl"),
    )
    .unwrap();
    assert!(KimiSourceBackedResolver::new(catalog)
        .hydrate_event(&request)
        .is_err());
}
