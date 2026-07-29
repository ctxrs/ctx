use std::collections::BTreeMap;

use serde_json::json;

use crate::test_support_paths::tempdir;

use super::*;

const IMPORTED_AT: &str = "2026-07-28T12:00:00Z";

fn write_dual_store(root: &Path, cli_text: &str, extension_text: &str) {
    let cli = root.join("projects/shared-project/shared-session.jsonl");
    fs::create_dir_all(cli.parent().unwrap()).unwrap();
    fs::write(
        &cli,
        format!(
            "{}\n",
            serde_json::to_string(&json!({
                "id": "cli-message",
                "type": "message",
                "role": "user",
                "content": cli_text,
                "timestamp": IMPORTED_AT,
                "sessionId": "shared-session",
                "cwd": "/workspace/codebuddy-cli",
            }))
            .unwrap()
        ),
    )
    .unwrap();

    let project = root.join("history/shared-project");
    let session = project.join("shared-session");
    fs::create_dir_all(session.join("messages")).unwrap();
    fs::write(
        session.join("index.json"),
        serde_json::to_vec(&json!({
            "messages": [{
                "id": "extension-message",
                "type": "message",
                "role": "assistant",
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        project.join("index.json"),
        serde_json::to_vec(&json!({
            "conversations": [{
                "id": "shared-session",
                "name": "Shared native IDs",
                "projectPath": "/workspace/codebuddy-ide",
                "createdAt": IMPORTED_AT,
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        session.join("messages/extension-message.json"),
        serde_json::to_vec(&json!({
            "id": "extension-message",
            "role": "assistant",
            "content": extension_text,
            "createdAt": IMPORTED_AT,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn scan(root: &Path) -> Vec<CodeBuddySourceBackedScan> {
    scan_codebuddy_source_backed_root(root, IMPORTED_AT.parse().unwrap()).unwrap()
}

fn documents(scans: &[CodeBuddySourceBackedScan]) -> BTreeMap<String, &LexicalDocument> {
    scans
        .iter()
        .flat_map(|scan| scan.pages.iter().flat_map(|page| page.documents.iter()))
        .map(|document| (document.source.schema_variant().to_owned(), document))
        .collect()
}

#[test]
fn dual_format_cold_scan_emits_independent_full_body_exact_records() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("codebuddy");
    let cli_text = format!("cli exact head {} cli exact tail", "c".repeat(3_000));
    let extension_text = format!(
        "extension exact head {} extension exact tail",
        "e".repeat(3_000)
    );
    write_dual_store(&root, &cli_text, &extension_text);

    let scans = scan(&root);
    assert_eq!(scans.len(), 2);
    assert_ne!(
        scans[0].source.observation().source().identity(),
        scans[1].source.observation().source().identity(),
        "CLI and IDE stores with equal native project/session IDs must remain independent"
    );
    for scan in &scans {
        assert_eq!(scan.source.counts().complete_records, 1);
        assert_eq!(scan.source.counts().retained_records, 1);
        assert_eq!(scan.source.counts().indexed_documents, 1);
        assert!(scan.rejections.is_empty());
        assert!(scan.pages.iter().all(|page| page.documents.len() <= 64));
    }

    let documents = documents(&scans);
    let cli = documents.get(CODEBUDDY_CLI_SCHEMA_VARIANT).unwrap();
    assert_eq!(cli.body, cli_text);
    assert!(cli.body.ends_with("cli exact tail"));
    let NativeRecordCoordinate::Jsonl {
        native_event_key, ..
    } = cli.locator.coordinate()
    else {
        panic!("CLI record must use a JSONL range");
    };
    assert!(tagged_event_key_matches(
        native_event_key.as_ref(),
        CODEBUDDY_CLI_LOCATOR_TAG,
        "cli-message"
    ));
    let hydrated_cli = hydrate_codebuddy_source_backed_record(&root, &cli.locator).unwrap();
    assert_eq!(hydrated_cli.decoded_display_text, cli_text);
    assert_eq!(hydrated_cli.provider_bytes, cli_text.as_bytes());

    let extension = documents.get(CODEBUDDY_EXTENSION_SCHEMA_VARIANT).unwrap();
    assert_eq!(extension.body, extension_text);
    assert!(extension.body.ends_with("extension exact tail"));
    let (_, _, native_id) = structured_coordinate(extension.locator.coordinate()).unwrap();
    assert_eq!(native_id, "shared-project/shared-session:extension-message");
    let hydrated_extension =
        hydrate_codebuddy_source_backed_record(&root, &extension.locator).unwrap();
    assert_eq!(hydrated_extension.decoded_display_text, extension_text);
    assert_eq!(hydrated_extension.provider_bytes, extension_text.as_bytes());
}

#[test]
fn dual_format_replacement_preserves_stable_ids_and_rejects_stale_locators() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("codebuddy");
    write_dual_store(&root, "CLI before replacement", "IDE before replacement");
    let before = scan(&root);
    let before_documents = documents(&before);
    let before_state = before_documents
        .iter()
        .map(|(shape, document)| {
            (
                shape.clone(),
                (
                    document.source.identity(),
                    document.session_id,
                    document.event_id,
                    document.locator.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    drop(before_documents);

    let replacement = root.join("projects/shared-project/replacement.jsonl");
    fs::write(
        &replacement,
        format!(
            "{}\n",
            serde_json::to_string(&json!({
                "id": "cli-message",
                "type": "message",
                "role": "user",
                "content": "CLI after replacement with changed bytes",
                "timestamp": IMPORTED_AT,
                "sessionId": "shared-session",
                "cwd": "/workspace/codebuddy-cli",
            }))
            .unwrap()
        ),
    )
    .unwrap();
    fs::rename(
        &replacement,
        root.join("projects/shared-project/shared-session.jsonl"),
    )
    .unwrap();
    fs::write(
        root.join("history/shared-project/shared-session/messages/extension-message.json"),
        serde_json::to_vec(&json!({
            "id": "extension-message",
            "role": "assistant",
            "content": "IDE after replacement with changed bytes",
            "createdAt": IMPORTED_AT,
        }))
        .unwrap(),
    )
    .unwrap();

    let after = scan(&root);
    assert_eq!(after.len(), 2, "both installed stores must remain selected");
    let after_documents = documents(&after);
    for (shape, document) in &after_documents {
        let (source_id, session_id, event_id, stale_locator) = before_state.get(shape).unwrap();
        assert_eq!(document.source.identity(), *source_id);
        assert_eq!(document.session_id, *session_id);
        assert_eq!(document.event_id, *event_id);
        assert!(
            hydrate_codebuddy_source_backed_record(&root, stale_locator).is_err(),
            "{shape} stale locator unexpectedly hydrated after replacement"
        );
        let hydrated = hydrate_codebuddy_source_backed_record(&root, &document.locator).unwrap();
        assert!(hydrated.decoded_display_text.contains("after replacement"));
    }
    for scan in &after {
        let prior = before
            .iter()
            .find(|candidate| {
                candidate.source.observation().source().schema_variant()
                    == scan.source.observation().source().schema_variant()
            })
            .unwrap();
        assert_ne!(scan.source.content_digest(), prior.source.content_digest());
    }
}

#[test]
fn compound_authority_codebuddy_rejects_missing_auxiliary_and_sibling_swap() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("codebuddy");
    write_dual_store(&root, "cli", "extension");
    let project_index = root.join("history/shared-project/index.json");
    fs::remove_file(&project_index).unwrap();
    let mut inventory = discover_sources(&root, &ProviderImportOptions::default()).unwrap();
    let authority = codebuddy_authority(&root).unwrap();
    let extension = inventory
        .sources
        .iter_mut()
        .find(|source| source.shape == CodeBuddySourceShape::Extension)
        .unwrap();
    bind_codebuddy_capability(extension, &authority).unwrap();
    fs::write(&project_index, br#"{"conversations":[]}"#).unwrap();
    assert!(revalidate_codebuddy_capability(extension).is_err());

    let temp = tempdir().unwrap();
    let root = temp.path().join("codebuddy");
    write_dual_store(&root, "cli", "extension");
    let mut inventory = discover_sources(&root, &ProviderImportOptions::default()).unwrap();
    let authority = codebuddy_authority(&root).unwrap();
    let extension = inventory
        .sources
        .iter_mut()
        .find(|source| source.shape == CodeBuddySourceShape::Extension)
        .unwrap();
    bind_codebuddy_capability(extension, &authority).unwrap();
    let message =
        root.join("history/shared-project/shared-session/messages/extension-message.json");
    let bytes = fs::read(&message).unwrap();
    fs::rename(&message, message.with_extension("retired")).unwrap();
    fs::write(&message, bytes).unwrap();
    assert!(revalidate_codebuddy_capability(extension).is_err());
}

#[cfg(unix)]
#[test]
fn compound_authority_codebuddy_rejects_ancestor_swap_and_stale_locator() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("codebuddy");
    write_dual_store(&root, "cli before", "extension before");
    let scans = scan(&root);
    let stale_extension = documents(&scans)[CODEBUDDY_EXTENSION_SCHEMA_VARIANT]
        .locator
        .clone();

    let mut inventory = discover_sources(&root, &ProviderImportOptions::default()).unwrap();
    let authority = codebuddy_authority(&root).unwrap();
    for source in &mut inventory.sources {
        bind_codebuddy_capability(source, &authority).unwrap();
    }
    let retired = temp.path().join("retired-codebuddy");
    fs::rename(&root, &retired).unwrap();
    write_dual_store(&root, "cli after", "extension after");
    assert!(inventory
        .sources
        .iter()
        .all(|source| revalidate_codebuddy_capability(source).is_err()));
    assert!(hydrate_codebuddy_source_backed_record(&root, &stale_extension).is_err());
}
