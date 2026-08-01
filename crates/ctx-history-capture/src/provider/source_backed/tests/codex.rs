use std::{fs::OpenOptions, io::Write};

use super::*;

fn prompt_line(session_id: &str, ts: i64, text: &str) -> Vec<u8> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "session_id": session_id,
        "ts": ts,
        "text": text,
    }))
    .unwrap();
    line.push(b'\n');
    line
}

fn core_records(index: &VerifiedIndex) -> Vec<CoreRecord> {
    let mut records = Vec::new();
    for source in &index.manifest().sources {
        let source_key = source.observation().source();
        let page = index.source_event_page(source_key, None, 256).unwrap();
        assert!(page.next_cursor.is_none());
        for item in page.items {
            records.push(
                index
                    .core_record_by_id(item.event_id.as_uuid())
                    .unwrap()
                    .unwrap(),
            );
        }
    }
    records.sort_by_key(|record| {
        (
            record.source.source_format().to_owned(),
            record.event_sequence,
        )
    });
    records
}

#[test]
fn codex_history_and_sessions_publish_self_contained_core_across_lifecycle() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let sessions = home.join(".codex/sessions");
    let history = home.join(".codex/history.jsonl");
    fs::create_dir_all(&sessions).unwrap();

    let native_session_id = "019faadb-b9f2-7413-9fab-edf59fd787a6";
    let session_path = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(
        &session_path,
        codex_rollout_bytes(native_session_id, &["complete session body"]),
    )
    .unwrap();
    let prompt_tail = "full-body-tail-marker";
    let prompt_body = format!("complete prompt {} {prompt_tail}", "x".repeat(8_192));
    fs::write(
        &history,
        prompt_line(native_session_id, 1_785_139_200, &prompt_body),
    )
    .unwrap();

    let context = DiscoveryContext::new(
        &home,
        temp.path().join("cwd"),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let routes = vec![
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_history_jsonl",
            ProviderImportSupport::Native,
            &history,
        ),
    ];
    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.path().join("ctx-data"),
        routes,
        Vec::new(),
    );
    assert_eq!(build.executable_route_count(), 2);
    assert!(build.issues.is_empty());

    let index_path = temp.path().join("index");
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let cold =
        refresh_source_backed_generation(&index_path, &build.registry, options.clone()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 2);
    let index = VerifiedIndex::open(&index_path).unwrap();
    let records = core_records(&index);
    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|record| record.validate_contract().is_ok()));
    let prompt = records
        .iter()
        .find(|record| record.source.source_format() == "codex_history_jsonl")
        .unwrap();
    assert_eq!(
        prompt.content.normalized_body.as_deref(),
        Some(prompt_body.as_str())
    );
    assert!(prompt
        .content
        .normalized_body
        .as_deref()
        .unwrap()
        .ends_with(prompt_tail));
    assert_eq!(
        prompt.provider_session_id.as_deref(),
        Some(native_session_id)
    );
    let prompt_first_id = prompt.event_id;
    let session = records
        .iter()
        .find(|record| record.source.source_format() == "codex_session_jsonl")
        .unwrap();
    assert_eq!(
        session.content.normalized_body.as_deref(),
        Some("complete session body")
    );
    assert_eq!(session.cwd.as_deref(), Some("/tmp/explicit-codex-source"));

    OpenOptions::new()
        .append(true)
        .open(&history)
        .unwrap()
        .write_all(&prompt_line(
            native_session_id,
            1_785_139_201,
            "appended prompt",
        ))
        .unwrap();
    let append_bytes = codex_rollout_bytes(native_session_id, &["discarded", "appended session"]);
    let appended_session_line = append_bytes
        .split_inclusive(|byte| *byte == b'\n')
        .nth(2)
        .unwrap();
    OpenOptions::new()
        .append(true)
        .open(&session_path)
        .unwrap()
        .write_all(appended_session_line)
        .unwrap();
    let appended =
        refresh_source_backed_generation(&index_path, &build.registry, options.clone()).unwrap();
    assert_eq!(appended.commit.indexed_documents, 4);
    let appended_generation = appended.commit.generation_id.clone();
    let index = VerifiedIndex::open(&index_path).unwrap();
    let appended_records = core_records(&index);
    assert!(appended_records
        .iter()
        .any(|record| { record.content.normalized_body.as_deref() == Some("appended prompt") }));
    assert!(appended_records
        .iter()
        .any(|record| { record.content.normalized_body.as_deref() == Some("appended session") }));

    let unchanged =
        refresh_source_backed_generation(&index_path, &build.registry, options.clone()).unwrap();
    assert_eq!(unchanged.commit.generation_id, appended_generation);

    fs::write(
        &history,
        [
            prompt_line(native_session_id, 1_785_139_200, "rewritten prompt"),
            prompt_line(native_session_id, 1_785_139_201, "appended prompt"),
        ]
        .concat(),
    )
    .unwrap();
    refresh_source_backed_generation(&index_path, &build.registry, options.clone()).unwrap();
    let index = VerifiedIndex::open(&index_path).unwrap();
    let rewritten = core_records(&index)
        .into_iter()
        .find(|record| {
            record.source.source_format() == "codex_history_jsonl" && record.event_sequence == 0
        })
        .unwrap();
    assert_eq!(rewritten.event_id, prompt_first_id);
    assert_eq!(
        rewritten.content.normalized_body.as_deref(),
        Some("rewritten prompt")
    );

    fs::remove_file(&session_path).unwrap();
    for expected_missing in 1..AUTOMATIC_SOURCE_DELETION_MISSING_INVENTORIES {
        let grace = refresh_source_backed_generation(&index_path, &build.registry, options.clone())
            .unwrap();
        assert!(grace.removals.is_empty());
        let missing = grace.commit.manifest().source_catalog().missing_sources();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].consecutive_missing().get(), expected_missing);
        assert_eq!(
            VerifiedIndex::open(&index_path).unwrap().document_count(),
            4
        );
    }
    refresh_source_backed_generation(&index_path, &build.registry, options).unwrap();
    let index = VerifiedIndex::open(&index_path).unwrap();
    assert_eq!(index.document_count(), 2);
    assert!(index.manifest().sources.iter().all(|source| source
        .observation()
        .source()
        .source_format()
        == "codex_history_jsonl"));
}
