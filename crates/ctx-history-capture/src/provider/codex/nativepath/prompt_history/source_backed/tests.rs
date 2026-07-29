use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{
    ContentSourceResolver, EventHydrationRequest, HydrationFailureKind, NativeRecordCoordinate,
};
use ctx_history_index::LexicalDocument;
use serde_json::json;

use super::*;
use crate::test_support_paths::tempdir;

fn prompt_line(session_id: &str, ts: i64, text: &str) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&json!({
        "session_id": session_id,
        "ts": ts,
        "text": text,
    }))
    .unwrap();
    bytes.push(b'\n');
    bytes
}

fn write_lines(path: &Path, lines: &[Vec<u8>]) {
    fs::write(path, lines.concat()).unwrap();
}

fn append(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

fn collect(
    input: &CodexPromptHistorySourceBackedInputV0,
    prior: Option<&CertifiedSource>,
) -> (
    CodexPromptHistorySourceBackedScanV0,
    Vec<LexicalDocument>,
    Vec<(usize, usize)>,
) {
    let mut documents = Vec::new();
    let mut pages = Vec::new();
    let scan = scan_codex_prompt_history_source_backed_explicit_v0(input, prior, |page| {
        assert_eq!(page.source, input.source_key().unwrap());
        pages.push((page.documents.len(), page.retained_bytes));
        documents.extend(page.documents);
        Ok(())
    })
    .unwrap();
    (scan, documents, pages)
}

#[test]
fn cold_noop_append_and_exact_hydration_keep_stable_bounded_identity() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    let long = format!(
        "bounded-prompt-{}",
        "x".repeat(MAX_BODY_PREVIEW_CHARS.saturating_add(128))
    );
    let mut lines = (0..70)
        .map(|index| {
            prompt_line(
                "session-a",
                1_785_139_200 + i64::from(index),
                if index == 0 { &long } else { "ordinary prompt" },
            )
        })
        .collect::<Vec<_>>();
    write_lines(&path, &lines);
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&path, [41; 32]);

    let (cold, cold_documents, pages) = collect(&input, None);
    assert!(matches!(
        cold.disposition,
        CodexPromptHistorySourceBackedDispositionV0::Cold
    ));
    assert_eq!(cold_documents.len(), 70);
    assert_eq!(cold.emitted_documents, 70);
    assert!(cold.terminal);
    assert!(pages.len() >= 2);
    assert!(pages.iter().all(|(documents, bytes)| {
        *documents <= PAGE_MAX_DOCUMENTS && *bytes <= PAGE_MAX_RETAINED_BYTES
    }));
    assert_eq!(
        cold_documents[0].body.chars().count(),
        MAX_BODY_PREVIEW_CHARS
    );
    assert_eq!(
        cold_documents[0].root_session_id,
        cold_documents[0].session_id
    );
    assert_eq!(cold_documents[0].event_sequence, 0);
    let NativeRecordCoordinate::Jsonl {
        physical_ordinal,
        native_session_key: Some(TypedKey::Utf8(session)),
        native_event_key: Some(TypedKey::U64(event_ordinal)),
        ..
    } = cold_documents[0].locator.coordinate()
    else {
        panic!("prompt history must emit a typed JSONL locator");
    };
    assert_eq!(*physical_ordinal, 0);
    assert_eq!(session, "session-a");
    assert_eq!(*event_ordinal, 0);

    let resolver = CodexPromptHistorySourceBackedResolverV0::new([cold.source.clone()]).unwrap();
    let request = EventHydrationRequest::new(
        cold_documents[0].event_id,
        cold_documents[0].locator.clone(),
    )
    .unwrap();
    assert_eq!(
        resolver.hydrate_event(&request).unwrap().provider_bytes,
        lines[0]
    );

    let (noop, noop_documents, noop_pages) = collect(&input, Some(&cold.certificate));
    assert!(matches!(
        noop.disposition,
        CodexPromptHistorySourceBackedDispositionV0::Unchanged
    ));
    assert!(noop_documents.is_empty());
    assert!(noop_pages.is_empty());

    let appended = prompt_line("session-a", 1_785_139_270, "appended prompt");
    append(&path, &appended);
    lines.push(appended);
    let (appended_scan, appended_documents, _) = collect(&input, Some(&noop.certificate));
    assert!(matches!(
        appended_scan.disposition,
        CodexPromptHistorySourceBackedDispositionV0::Append { .. }
    ));
    assert_eq!(appended_documents.len(), 1);
    assert_eq!(appended_documents[0].event_sequence, 70);
    assert_eq!(appended_documents[0].body, "appended prompt");
    assert_eq!(
        appended_documents[0].session_id,
        cold_documents[0].session_id
    );
    assert!(
        revalidate_codex_prompt_history_source_backed_v0(&input, &appended_scan.certificate)
            .unwrap()
    );

    let (_, rebuilt, _) = collect(&input, None);
    assert_eq!(rebuilt[0].event_id, cold_documents[0].event_id);
    assert_eq!(
        rebuilt.last().unwrap().event_id,
        appended_documents[0].event_id
    );
}

#[test]
fn malformed_incomplete_tail_is_not_certified_until_append_completes_it() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    let first = prompt_line("tail-session", 1_785_139_200, "complete");
    let incomplete = br#"{"session_id":"tail-session","ts":1785139201,"text":"tail"#;
    let mut bytes = first.clone();
    bytes.extend_from_slice(incomplete);
    fs::write(&path, bytes).unwrap();
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&path, [42; 32]);

    let (before, documents, _) = collect(&input, None);
    assert_eq!(documents.len(), 1);
    assert!(!before.terminal);
    assert_eq!(before.certificate.counts().complete_records, 1);
    assert_eq!(
        before.certificate.counts().certified_bytes,
        u64::try_from(first.len()).unwrap()
    );

    append(&path, b"\"}\n");
    let (after, appended_documents, _) = collect(&input, Some(&before.certificate));
    assert!(matches!(
        after.disposition,
        CodexPromptHistorySourceBackedDispositionV0::Append { .. }
    ));
    assert!(after.terminal);
    assert_eq!(after.certificate.counts().complete_records, 2);
    assert_eq!(appended_documents.len(), 1);
    assert_eq!(appended_documents[0].event_sequence, 1);
    assert_eq!(appended_documents[0].body, "tail");
}

#[cfg(unix)]
#[test]
fn retained_resolver_rejects_same_path_leaf_replacement() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    let line = prompt_line("leaf-session", 1_785_139_200, "leaf");
    fs::write(&path, &line).unwrap();
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&path, [43; 32]);
    let (scan, documents, _) = collect(&input, None);
    let resolver = CodexPromptHistorySourceBackedResolverV0::new([scan.source.clone()]).unwrap();
    let request =
        EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone()).unwrap();

    fs::rename(&path, temp.path().join("old-history.jsonl")).unwrap();
    fs::write(&path, &line).unwrap();

    let error = resolver.hydrate_event(&request).unwrap_err();
    assert_eq!(error.kind, HydrationFailureKind::StaleRecordEvidence);
}

#[cfg(unix)]
#[test]
fn retained_resolver_rejects_same_path_root_replacement() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("codex");
    fs::create_dir(&root).unwrap();
    let path = root.join("history.jsonl");
    let line = prompt_line("root-session", 1_785_139_200, "root");
    fs::write(&path, &line).unwrap();
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&path, [44; 32]);
    let (scan, documents, _) = collect(&input, None);
    let resolver = CodexPromptHistorySourceBackedResolverV0::new([scan.source.clone()]).unwrap();
    let request =
        EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone()).unwrap();

    fs::rename(&root, temp.path().join("old-codex")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(&path, &line).unwrap();

    let error = resolver.hydrate_event(&request).unwrap_err();
    assert_eq!(error.kind, HydrationFailureKind::StaleRecordEvidence);
}
