use std::{collections::BTreeSet, fs, path::PathBuf, process::Command};

use ctx_history_core::{RepositoryAbstentionReason, RepositoryFileInvocationKind};
use serde_json::json;
use tempfile::TempDir;

use super::super::{
    dto::GeminiToolCall,
    file_invocation::{
        checked_normalized_text_range, extract_gemini_file_invocations,
        normalize_gemini_tool_calls, GeminiFileInvocationOverflow,
        MAX_GEMINI_FILE_INVOCATIONS_PER_EVENT,
    },
    parser::MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT,
    source_backed::{apply_gemini_file_invocation_extraction, project_gemini_test_events},
};
use super::{fixture_root, rediscover, scan_collect, write_transcript};

fn call(name: Option<&str>, args: serde_json::Value) -> GeminiToolCall {
    GeminiToolCall {
        id: None,
        name: name.map(str::to_owned),
        args: Some(args),
    }
}

#[test]
fn gemini_file_invocations_preserve_per_call_membership_and_exact_tool_names() {
    let calls = vec![
        call(Some("read_file"), json!({"file_path": "src/read.rs"})),
        call(
            Some("write_file"),
            json!({"file_path": "src/write.rs", "content": "fn write() {}"}),
        ),
        call(
            Some("replace"),
            json!({
                "file_path": "src/edit.rs",
                "old_string": "old",
                "new_string": "new"
            }),
        ),
    ];
    let normalized = normalize_gemini_tool_calls(&calls).text;
    let extraction = extract_gemini_file_invocations(&calls, &normalized).unwrap();
    let evidence = extraction.evidence;

    assert_eq!(evidence.len(), 3);
    assert_eq!(evidence[0].path, "src/read.rs");
    assert_eq!(evidence[0].kind, RepositoryFileInvocationKind::Read);
    assert_eq!(evidence[0].operation_ordinal, 0);
    assert_eq!(evidence[0].tool_name.as_deref(), Some("read_file"));
    assert_eq!(evidence[1].path, "src/write.rs");
    assert_eq!(evidence[1].kind, RepositoryFileInvocationKind::Write);
    assert_eq!(evidence[1].operation_ordinal, 1);
    assert_eq!(evidence[1].tool_name.as_deref(), Some("write_file"));
    assert_eq!(evidence[2].path, "src/edit.rs");
    assert_eq!(evidence[2].kind, RepositoryFileInvocationKind::Modify);
    assert_eq!(evidence[2].operation_ordinal, 2);
    assert_eq!(evidence[2].tool_name.as_deref(), Some("replace"));

    for (index, expected_path) in ["src/read.rs", "src/write.rs", "src/edit.rs"]
        .into_iter()
        .enumerate()
    {
        let range = evidence[index].normalized_text_range.unwrap();
        let unit = &normalized[range.start as usize..range.end as usize];
        assert!(unit.starts_with('{') && unit.ends_with('}'));
        assert!(unit.contains(expected_path));
        for other_path in ["src/read.rs", "src/write.rs", "src/edit.rs"] {
            assert_eq!(unit.contains(other_path), other_path == expected_path);
        }
    }
}

#[test]
fn gemini_file_invocations_abstain_for_unproven_or_ambiguous_schemas() {
    let calls = vec![
        call(Some("custom_writer"), json!({"file_path": "custom.rs"})),
        call(Some("write_file"), json!({"path": "wrong-key.rs"})),
        call(
            Some("write_file"),
            json!({"file_path": "first.rs", "path": "conflict.rs"}),
        ),
        call(Some("WRITE_FILE"), json!({"file_path": "wrong-name.rs"})),
        call(Some("read_file"), json!({"file_path": ["not-a-string"]})),
        call(None, json!({"file_path": "missing-name.rs"})),
        call(Some("write_file"), json!({"file_path": "   "})),
    ];
    let normalized = normalize_gemini_tool_calls(&calls).text;

    let extraction = extract_gemini_file_invocations(&calls, &normalized).unwrap();
    assert!(extraction.evidence.is_empty());
    assert!(extraction.abstained_target_bearing_calls);
}

#[test]
fn gemini_file_invocation_overflow_rejects_the_complete_set() {
    let calls = (0..=MAX_GEMINI_FILE_INVOCATIONS_PER_EVENT)
        .map(|index| {
            call(
                Some("write_file"),
                json!({"file_path": format!("src/{index}.rs")}),
            )
        })
        .collect::<Vec<_>>();
    let normalized = normalize_gemini_tool_calls(&calls).text;

    assert_eq!(
        extract_gemini_file_invocations(&calls, &normalized),
        Err(GeminiFileInvocationOverflow::Count {
            limit: MAX_GEMINI_FILE_INVOCATIONS_PER_EVENT,
        })
    );
}

#[test]
fn gemini_file_invocation_byte_overflow_rejects_the_complete_set() {
    let calls = (0..5)
        .map(|index| {
            let path = format!("{index}{}", "x".repeat((16 * 1024) - 1));
            call(Some("read_file"), json!({"file_path": path}))
        })
        .collect::<Vec<_>>();
    let normalized = normalize_gemini_tool_calls(&calls).text;

    assert_eq!(
        extract_gemini_file_invocations(&calls, &normalized),
        Err(GeminiFileInvocationOverflow::Bytes {
            limit: MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT,
        })
    );
}

#[test]
fn gemini_file_invocation_range_overflow_is_typed() {
    let Ok(start) = usize::try_from(u64::from(u32::MAX) + 1) else {
        return;
    };
    assert_eq!(
        checked_normalized_text_range(&(start..start + 1)),
        Err(GeminiFileInvocationOverflow::NormalizedTextRange)
    );
}

#[test]
fn gemini_strict_overflows_preserve_ordinary_input_and_block_session_fallback() {
    use crate::repository_attribution::{
        AttributionInput, RepositoryAttributor, UnscopedFileObservation,
    };
    use ctx_history_core::{RepositoryEvidenceKind, RepositoryFileObservationKind};

    let temp = TempDir::new().unwrap();
    let repo = native_repository(&temp);
    let ordinary = UnscopedFileObservation {
        path: "src/shared.rs".to_owned(),
        prior_path: None,
        kind: RepositoryFileObservationKind::Unknown,
    };
    let overflows = [
        GeminiFileInvocationOverflow::Count {
            limit: MAX_GEMINI_FILE_INVOCATIONS_PER_EVENT,
        },
        GeminiFileInvocationOverflow::Bytes {
            limit: MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT,
        },
        GeminiFileInvocationOverflow::NormalizedTextRange,
    ];

    for overflow in overflows {
        let mut input = AttributionInput {
            session_cwd: Some(repo.to_string_lossy().into_owned()),
            file_observations: vec![ordinary.clone()],
            ..AttributionInput::default()
        };
        let mut abstentions = Vec::new();
        apply_gemini_file_invocation_extraction(&mut input, &mut abstentions, Err(overflow));

        assert_eq!(input.file_observations, vec![ordinary.clone()]);
        assert!(input.repository_file_invocation_evidence.is_empty());
        assert!(input.provider_native_context_ambiguous);
        assert_eq!(
            abstentions,
            vec![(
                RepositoryEvidenceKind::FileActivity,
                RepositoryAbstentionReason::CandidateLimitExceeded,
                "gemini_file_invocation_evidence_overflow",
            )]
        );

        let annotation = RepositoryAttributor::default().attribute(input);
        assert_eq!(annotation.repository_file_observations.len(), 1);

        let mut fallback_input = AttributionInput {
            session_cwd: Some(repo.to_string_lossy().into_owned()),
            ..AttributionInput::default()
        };
        let mut fallback_abstentions = Vec::new();
        apply_gemini_file_invocation_extraction(
            &mut fallback_input,
            &mut fallback_abstentions,
            Err(overflow),
        );
        let fallback_annotation = RepositoryAttributor::default().attribute(fallback_input);
        assert!(fallback_annotation.repository_bindings.is_empty());
    }
}

#[test]
fn gemini_normalized_range_requires_the_exact_complete_body_layout() {
    let calls = vec![call(
        Some("read_file"),
        json!({"file_path": "src/naive-ï.rs", "offset": 3}),
    )];
    let normalized = normalize_gemini_tool_calls(&calls).text;
    let exact = extract_gemini_file_invocations(&calls, &normalized).unwrap();
    let exact_range = exact.evidence[0].normalized_text_range.unwrap();
    assert_eq!(
        &normalized[exact_range.start as usize..exact_range.end as usize],
        serde_json::to_string(calls[0].args.as_ref().unwrap()).unwrap()
    );

    let mismatched = format!("prefix\n{normalized}");
    let without_range = extract_gemini_file_invocations(&calls, &mismatched).unwrap();
    assert_eq!(without_range.evidence.len(), 1);
    assert_eq!(without_range.evidence[0].path, "src/naive-ï.rs");
    assert_eq!(without_range.evidence[0].normalized_text_range, None);
}

#[test]
fn gemini_file_invocation_projection_keeps_membership_after_global_touch_dedup() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let repo = native_repository(&temp);
    let path = write_transcript(
        &root,
        &[
            json!({
                "sessionId": "gemini-file-invocation-membership",
                "startTime": "2026-08-02T12:00:00Z",
                "kind": "main",
                "directories": [repo]
            }),
            json!({
                "id": "multi-call-record",
                "timestamp": "2026-08-02T12:00:01Z",
                "type": "gemini",
                "toolCalls": [
                    {
                        "id": "read-shared",
                        "name": "read_file",
                        "args": {"file_path": "src/shared.rs"}
                    },
                    {
                        "id": "write-shared",
                        "name": "write_file",
                        "args": {"file_path": "src/shared.rs", "content": "new"}
                    },
                    {
                        "id": "replace-other",
                        "name": "replace",
                        "args": {
                            "file_path": "src/other.rs",
                            "old_string": "old",
                            "new_string": "new"
                        }
                    },
                    {
                        "id": "custom-call",
                        "name": "custom_writer",
                        "args": {"file_path": "src/not-proven.rs"}
                    }
                ]
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, None);
    assert_eq!(rows[0].safe_file_touches.len(), 3);
    let records = project_gemini_test_events(&source, rows).unwrap();
    let [record] = records.as_slice() else {
        panic!("expected one Gemini tool-call record");
    };

    let evidence = &record.repository_file_invocation_evidence;
    assert_eq!(evidence.len(), 3);
    assert_eq!(
        evidence
            .iter()
            .map(|item| (
                item.operation_ordinal,
                item.relative_path.as_str(),
                item.kind,
                item.tool_name.as_deref(),
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                0,
                "src/shared.rs",
                RepositoryFileInvocationKind::Read,
                Some("read_file"),
            ),
            (
                1,
                "src/shared.rs",
                RepositoryFileInvocationKind::Write,
                Some("write_file"),
            ),
            (
                2,
                "src/other.rs",
                RepositoryFileInvocationKind::Modify,
                Some("replace"),
            ),
        ]
    );
    assert_eq!(
        record
            .repository_file_observations
            .iter()
            .map(|observation| observation.relative_path.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["src/not-proven.rs", "src/other.rs", "src/shared.rs"])
    );
    assert!(record.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::Unsupported
            && abstention.detail.as_deref() == Some("gemini_file_invocation_schema_not_proven")
    }));

    let normalized = record.content.normalized_body.as_deref().unwrap();
    for (item, expected_path) in
        evidence
            .iter()
            .zip(["src/shared.rs", "src/shared.rs", "src/other.rs"])
    {
        let range = item.normalized_text_range.unwrap();
        let unit = &normalized[range.start as usize..range.end as usize];
        assert!(unit.starts_with('{') && unit.ends_with('}'));
        assert!(unit.contains(expected_path));
    }
}

#[test]
fn gemini_file_invocation_projection_all_abstains_on_per_call_overflow() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let repo = native_repository(&temp);
    let calls = (0..=MAX_GEMINI_FILE_INVOCATIONS_PER_EVENT)
        .map(|index| {
            json!({
                "id": format!("write-{index}"),
                "name": "write_file",
                "args": {"file_path": "src/shared.rs", "content": index.to_string()}
            })
        })
        .collect::<Vec<_>>();
    let path = write_transcript(
        &root,
        &[
            json!({
                "sessionId": "gemini-file-invocation-overflow",
                "startTime": "2026-08-02T12:00:00Z",
                "kind": "main",
                "directories": [repo]
            }),
            json!({
                "id": "overflow-record",
                "timestamp": "2026-08-02T12:00:01Z",
                "type": "gemini",
                "toolCalls": calls
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (_, rows) = scan_collect(&source, None);
    assert_eq!(rows[0].safe_file_touches, vec!["src/shared.rs"]);
    let records = project_gemini_test_events(&source, rows).unwrap();
    let [record] = records.as_slice() else {
        panic!("expected one Gemini tool-call record");
    };

    assert!(record.repository_file_invocation_evidence.is_empty());
    assert_eq!(record.repository_file_observations.len(), 1);
    assert_eq!(
        record.repository_file_observations[0].relative_path,
        "src/shared.rs"
    );
    assert!(record.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::CandidateLimitExceeded
            && abstention.detail.as_deref() == Some("gemini_file_invocation_evidence_overflow")
    }));
}

fn native_repository(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("native-repo");
    fs::create_dir(&path).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&path)
        .status()
        .unwrap()
        .success());
    fs::create_dir(path.join("src")).unwrap();
    fs::write(path.join("src/shared.rs"), "old\n").unwrap();
    fs::write(path.join("src/other.rs"), "old\n").unwrap();
    path
}
