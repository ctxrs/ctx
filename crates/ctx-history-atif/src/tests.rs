use std::path::Path;

use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, AgentScope,
    CaptureProvider, CoreActivity, CoreContent, CoreContentPolicyStatus, CoreRecord, EventRole,
    EventType, LiteralFactKind, NativeItemKey, NativeSessionKey, ProviderDeclaredFact,
    ProviderNativeSessionRelationship, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
    CORE_ACTIVITY_REVISION, CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION,
    CORE_RECORD_VERSION,
};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{
    export_session_to_trajectory, export_trajectory_json_pretty, import::AtifProjectionInput,
    parse_atif_bytes, parse_atif_str, project_trajectory_to_core_records, safe_trajectory_filename,
    AtifExportOptions, AtifExportSession, AtifImportError, AudioMediaType, ContentPart,
    ImageMediaType, MessageContent, SchemaVersion, StepSource, ATIF_SOURCE_FORMAT,
};

fn test_source_key() -> SourceKey {
    SourceKey::derive(
        CaptureProvider::Custom.as_str(),
        ATIF_SOURCE_FORMAT,
        "atif-v1-source-backed-v1",
        1,
        SourceAnchor::CatalogLineage([7_u8; 32]),
    )
    .expect("valid test source key")
}

#[test]
fn parses_all_supported_schema_versions_v1_0_through_v1_8() {
    for (version_str, expected) in [
        ("ATIF-v1.0", SchemaVersion::V1_0),
        ("ATIF-v1.1", SchemaVersion::V1_1),
        ("ATIF-v1.2", SchemaVersion::V1_2),
        ("ATIF-v1.3", SchemaVersion::V1_3),
        ("ATIF-v1.4", SchemaVersion::V1_4),
        ("ATIF-v1.5", SchemaVersion::V1_5),
        ("ATIF-v1.6", SchemaVersion::V1_6),
        ("ATIF-v1.7", SchemaVersion::V1_7),
        ("ATIF-v1.8", SchemaVersion::V1_8),
    ] {
        let doc = json!({
            "schema_version": version_str,
            "session_id": "run-001",
            "agent": { "name": "harbor-agent", "version": "1.0.0" },
            "steps": [
                { "step_id": 1, "source": "user", "message": "hello" }
            ]
        });
        let traj = parse_atif_str(&doc.to_string()).expect("valid ATIF trajectory");
        assert_eq!(traj.schema_version, expected);
        assert!(!traj.has_multimodal_content());
    }
}

#[test]
fn rejects_unsupported_schema_version() {
    let doc = json!({
        "schema_version": "ATIF-v2.0",
        "session_id": "run-001",
        "agent": { "name": "harbor-agent", "version": "1.0.0" },
        "steps": [{ "step_id": 1, "source": "user", "message": "hello" }]
    });
    let err = parse_atif_str(&doc.to_string()).unwrap_err();
    assert!(matches!(err, AtifImportError::UnsupportedSchemaVersion(v) if v == "ATIF-v2.0"));
}

#[test]
fn requires_session_id_on_v1_6_and_allows_omission_on_v1_7_and_v1_8() {
    let v1_6_missing = json!({
        "schema_version": "ATIF-v1.6",
        "agent": { "name": "harbor-agent", "version": "1.0.0" },
        "steps": [{ "step_id": 1, "source": "user", "message": "hello" }]
    });
    let err = parse_atif_str(&v1_6_missing.to_string()).unwrap_err();
    assert!(err.to_string().contains("session_id: required"));

    for version in ["ATIF-v1.7", "ATIF-v1.8"] {
        let doc = json!({
            "schema_version": version,
            "trajectory_id": "traj-001",
            "agent": { "name": "harbor-agent", "version": "1.0.0" },
            "steps": [{ "step_id": 1, "source": "user", "message": "hello" }]
        });
        let traj = parse_atif_str(&doc.to_string()).expect("v1.7+ allows omitting session_id");
        assert_eq!(traj.session_id, None);
        assert_eq!(traj.trajectory_id.as_deref(), Some("traj-001"));
    }
}

#[test]
fn accepts_full_v1_8_optional_fields_and_round_trips_losslessly() {
    let doc = json!({
        "schema_version": "ATIF-v1.8",
        "session_id": "run-full-001",
        "trajectory_id": "root-traj-001",
        "agent": {
            "name": "harbor-terminus",
            "version": "2.4.1",
            "model_name": "gemini-4-neon",
            "tool_definitions": [
                {
                    "type": "function",
                    "function": {
                        "name": "run_shell",
                        "description": "Execute a shell command",
                        "parameters": { "type": "object", "properties": { "command": { "type": "string" } } }
                    }
                }
            ],
            "extra": { "cwd": "/workspace/demo", "strategy": "react" }
        },
        "notes": "Full v1.8 feature exercise",
        "continued_trajectory_ref": "part2.json",
        "extra": { "benchmark": "terminal-bench" },
        "final_metrics": {
            "total_prompt_tokens": 1120,
            "total_completion_tokens": 124,
            "total_cached_tokens": 200,
            "total_cost_usd": 0.00078,
            "total_steps": 4,
            "extra": { "provider_tier": "standard" }
        },
        "steps": [
            {
                "step_id": 1,
                "timestamp": "2026-04-15T12:00:00Z",
                "source": "system",
                "message": "Context compaction performed",
                "is_copied_context": true,
                "observation": {
                    "results": [
                        {
                            "content": "Summary of earlier context",
                            "subagent_trajectory_ref": [
                                {
                                    "trajectory_id": "compact-sub-1",
                                    "session_id": "run-full-001",
                                    "trajectory_path": null,
                                    "extra": { "exit_status": "ok" }
                                }
                            ],
                            "extra": { "source_doc_id": "doc-1" }
                        }
                    ]
                },
                "extra": {
                    "context_management": { "type": "compaction", "boundary": "replace" }
                }
            },
            {
                "step_id": 2,
                "timestamp": "2026-04-15T12:00:05.500+00:00",
                "source": "user",
                "message": [
                    { "type": "text", "text": "Inspect the screenshot and voice note." },
                    { "type": "image", "source": { "media_type": "image/png", "path": "images/step2.png" } },
                    { "type": "audio", "source": { "media_type": " AUDIO/MP3 ", "path": "audio/step2.mp3", "duration_sec": 3.25 } }
                ]
            },
            {
                "step_id": 3,
                "timestamp": "2026-04-15T12:00:08Z",
                "source": "agent",
                "model_name": "gemini-4-neon",
                "reasoning_effort": 0.75,
                "message": "Running diagnostics.",
                "reasoning_content": "Check repo status before patching.",
                "llm_call_count": 1,
                "tool_calls": [
                    {
                        "tool_call_id": "call_1",
                        "function_name": "run_shell",
                        "arguments": { "command": "git status", "path": "src/main.rs" },
                        "extra": { "timeout_sec": 30 }
                    }
                ],
                "observation": {
                    "results": [
                        {
                            "source_call_id": "call_1",
                            "content": [
                                { "type": "text", "text": "clean tree" },
                                { "type": "image", "source": { "media_type": "image/webp", "path": "https://example.test/out.webp" } }
                            ],
                            "extra": { "retrieval_score": 0.99 }
                        }
                    ]
                },
                "metrics": {
                    "prompt_tokens": 520,
                    "completion_tokens": 80,
                    "cached_tokens": 200,
                    "cost_usd": 0.00045,
                    "prompt_token_ids": [1, 2, 3],
                    "completion_token_ids": [4, 5],
                    "logprobs": [-0.1, -0.2],
                    "extra": { "cache_creation_input_tokens": 50 }
                }
            },
            {
                "step_id": 4,
                "timestamp": "2026-04-15T12:00:10",
                "source": "agent",
                "message": "",
                "llm_call_count": 0,
                "tool_calls": [
                    {
                        "tool_call_id": "call_det_1",
                        "function_name": "deterministic_hook",
                        "arguments": {}
                    }
                ],
                "observation": {
                    "results": [
                        { "source_call_id": "call_det_1", "content": "hook ok" }
                    ]
                }
            }
        ],
        "subagent_trajectories": [
            {
                "schema_version": "ATIF-v1.8",
                "session_id": "run-full-001",
                "trajectory_id": "compact-sub-1",
                "agent": { "name": "summarizer", "version": "" },
                "steps": [
                    { "step_id": 1, "source": "user", "message": "Summarize prior turns" },
                    { "step_id": 2, "source": "agent", "message": "Summary complete." }
                ]
            }
        ]
    });

    let traj = parse_atif_str(&doc.to_string()).expect("full v1.8 trajectory parses");
    assert!(traj.has_multimodal_content());
    assert!(traj.find_embedded_subagent("compact-sub-1").is_some());

    // Audio alias ` AUDIO/MP3 ` normalized to canonical `audio/mpeg`.
    let MessageContent::Parts(parts) = &traj.steps[1].message else {
        panic!("expected multimodal parts");
    };
    assert!(matches!(
        &parts[2],
        ContentPart::Audio { source }
            if source.media_type == AudioMediaType::Mpeg
            && source.path == "audio/step2.mp3"
            && source.duration_sec == Some(3.25)
    ));

    // Project to CoreRecords and split root vs embedded child records.
    let source = test_source_key();
    let records = project_trajectory_to_core_records(&AtifProjectionInput {
        source: &source,
        document_key: "traj.json",
        trajectory: &traj,
        lineage: None,
    })
    .expect("projection succeeds");

    let root_session_id = records[0].session_id;
    let root_records: Vec<CoreRecord> = records
        .iter()
        .filter(|r| r.session_id == root_session_id)
        .cloned()
        .collect();
    let child_records: Vec<CoreRecord> = records
        .iter()
        .filter(|r| r.session_id != root_session_id)
        .cloned()
        .collect();
    assert!(!root_records.is_empty());
    assert_eq!(child_records.len(), 2);
    assert_eq!(child_records[0].parent_session_id, Some(root_session_id));
    assert_eq!(child_records[0].root_session_id, Some(root_session_id));
    assert_eq!(
        child_records[0].session_relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    assert_eq!(child_records[0].agent_scope, Some(AgentScope::Subagent));

    // Re-export losslessly back to ATIF v1.8 and verify equality with parsed trajectory.
    let exported = export_session_to_trajectory(
        &AtifExportSession {
            ctx_session_id: root_session_id.as_uuid(),
            provider: CaptureProvider::Custom,
            provider_session_id: root_records[0].provider_session_id.clone(),
            source_format: ATIF_SOURCE_FORMAT.to_owned(),
            records: root_records,
            children: vec![AtifExportSession {
                ctx_session_id: child_records[0].session_id.as_uuid(),
                provider: CaptureProvider::Custom,
                provider_session_id: child_records[0].provider_session_id.clone(),
                source_format: ATIF_SOURCE_FORMAT.to_owned(),
                records: child_records,
                children: Vec::new(),
            }],
        },
        AtifExportOptions::default(),
    )
    .expect("lossless export succeeds");

    assert_eq!(exported, traj);
    let json_out = export_trajectory_json_pretty(&exported).expect("serializes");
    let reparsed = parse_atif_bytes(json_out.as_bytes()).expect("reparses cleanly");
    assert_eq!(reparsed, traj);
}

#[test]
fn normalizes_all_eleven_audio_aliases_and_rejects_image_aliases() {
    let cases = [
        ("audio/mp3", AudioMediaType::Mpeg),
        ("audio/mpga", AudioMediaType::Mpeg),
        ("audio/x-mpeg", AudioMediaType::Mpeg),
        ("audio/x-wav", AudioMediaType::Wav),
        ("audio/wave", AudioMediaType::Wav),
        ("audio/vnd.wave", AudioMediaType::Wav),
        ("audio/x-m4a", AudioMediaType::Mp4),
        ("audio/m4a", AudioMediaType::Mp4),
        ("audio/x-aac", AudioMediaType::Aac),
        ("audio/x-flac", AudioMediaType::Flac),
        ("audio/x-aiff", AudioMediaType::Aiff),
        ("  AUDIO/X-M4A  ", AudioMediaType::Mp4),
    ];
    for (written, canonical) in cases {
        let doc = json!({
            "schema_version": "ATIF-v1.8",
            "agent": { "name": "a", "version": "1" },
            "steps": [{
                "step_id": 1,
                "source": "user",
                "message": [{
                    "type": "audio",
                    "source": { "media_type": written, "path": "audio/sample.bin" }
                }]
            }]
        });
        let traj = parse_atif_str(&doc.to_string()).unwrap_or_else(|e| panic!("{written}: {e}"));
        let MessageContent::Parts(parts) = &traj.steps[0].message else {
            panic!("expected parts");
        };
        assert!(matches!(
            &parts[0],
            ContentPart::Audio { source } if source.media_type == canonical
        ));
    }

    // Strict 4 canonical image MIME types: no aliases, no whitespace, no wrong case.
    for valid_img in ["image/jpeg", "image/png", "image/gif", "image/webp"] {
        let doc = json!({
            "schema_version": "ATIF-v1.8",
            "agent": { "name": "a", "version": "1" },
            "steps": [{
                "step_id": 1,
                "source": "user",
                "message": [{
                    "type": "image",
                    "source": { "media_type": valid_img, "path": "images/x.png" }
                }]
            }]
        });
        let traj = parse_atif_str(&doc.to_string()).expect("valid image");
        let MessageContent::Parts(parts) = &traj.steps[0].message else {
            panic!("expected parts");
        };
        assert!(matches!(
            &parts[0],
            ContentPart::Image { source }
                if matches!(
                    source.media_type,
                    ImageMediaType::Jpeg
                        | ImageMediaType::Png
                        | ImageMediaType::Gif
                        | ImageMediaType::Webp
                )
        ));
    }

    for invalid_img in ["image/jpg", "IMAGE/PNG", " image/png ", "audio/wav"] {
        let doc = json!({
            "schema_version": "ATIF-v1.8",
            "agent": { "name": "a", "version": "1" },
            "steps": [{
                "step_id": 1,
                "source": "user",
                "message": [{
                    "type": "image",
                    "source": { "media_type": invalid_img, "path": "images/x.png" }
                }]
            }]
        });
        assert!(
            parse_atif_str(&doc.to_string()).is_err(),
            "expected image media_type `{invalid_img}` to be rejected"
        );
    }
}

#[test]
fn rejects_mismatched_content_parts_and_negative_audio_duration() {
    let bad_parts = [
        json!({ "type": "text", "text": "hi", "source": { "media_type": "image/png", "path": "a.png" } }),
        json!({ "type": "image", "text": "nope", "source": { "media_type": "image/png", "path": "a.png" } }),
        json!({ "type": "image", "source": { "media_type": "image/png", "path": "a.png", "duration_sec": 1.0 } }),
        json!({ "type": "audio", "source": { "media_type": "image/png", "path": "a.png" } }),
        json!({ "type": "audio", "source": { "media_type": "audio/wav", "path": "a.wav", "duration_sec": -0.5 } }),
        json!({ "type": "video", "source": { "media_type": "video/mp4", "path": "a.mp4" } }),
    ];
    for part in bad_parts {
        let doc = json!({
            "schema_version": "ATIF-v1.8",
            "agent": { "name": "a", "version": "1" },
            "steps": [{ "step_id": 1, "source": "user", "message": [part] }]
        });
        assert!(parse_atif_str(&doc.to_string()).is_err());
    }
}

#[test]
fn rejects_v1_7_and_v1_8_semantic_violations() {
    // 1. Unknown top-level field (deny_unknown_fields).
    let unknown = json!({
        "schema_version": "ATIF-v1.8",
        "unexpected_field": 123,
        "agent": { "name": "a", "version": "1" },
        "steps": [{ "step_id": 1, "source": "user", "message": "hi" }]
    });
    assert!(parse_atif_str(&unknown.to_string()).is_err());

    // 2. Empty steps on root and embedded subagent.
    let empty_root = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": []
    });
    assert!(parse_atif_str(&empty_root.to_string()).is_err());

    let empty_sub = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": [{ "step_id": 1, "source": "user", "message": "hi" }],
        "subagent_trajectories": [{
            "schema_version": "ATIF-v1.8",
            "trajectory_id": "sub-1",
            "agent": { "name": "sub", "version": "1" },
            "steps": []
        }]
    });
    assert!(parse_atif_str(&empty_sub.to_string()).is_err());

    // 3. SubagentTrajectoryRef with only session_id (v1.7+ breaking rule).
    let session_only_ref = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": [{
            "step_id": 1,
            "source": "system",
            "message": "delegated",
            "observation": {
                "results": [{
                    "subagent_trajectory_ref": [{ "session_id": "shared-run" }]
                }]
            }
        }]
    });
    assert!(parse_atif_str(&session_only_ref.to_string()).is_err());

    // 4. Duplicate or missing embedded subagent trajectory_id.
    let missing_tid = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": [{ "step_id": 1, "source": "user", "message": "hi" }],
        "subagent_trajectories": [{
            "schema_version": "ATIF-v1.8",
            "session_id": "shared",
            "agent": { "name": "sub", "version": "1" },
            "steps": [{ "step_id": 1, "source": "user", "message": "x" }]
        }]
    });
    assert!(parse_atif_str(&missing_tid.to_string()).is_err());

    let dup_tid = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": [{ "step_id": 1, "source": "user", "message": "hi" }],
        "subagent_trajectories": [
            {
                "schema_version": "ATIF-v1.8",
                "trajectory_id": "dup",
                "agent": { "name": "sub", "version": "1" },
                "steps": [{ "step_id": 1, "source": "user", "message": "x" }]
            },
            {
                "schema_version": "ATIF-v1.8",
                "trajectory_id": "dup",
                "agent": { "name": "sub", "version": "1" },
                "steps": [{ "step_id": 1, "source": "user", "message": "y" }]
            }
        ]
    });
    assert!(parse_atif_str(&dup_tid.to_string()).is_err());

    // 5. llm_call_count == 0 with metrics or reasoning_content.
    let det_with_metrics = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": [{
            "step_id": 1,
            "source": "agent",
            "message": "",
            "llm_call_count": 0,
            "metrics": { "prompt_tokens": 10 }
        }]
    });
    assert!(parse_atif_str(&det_with_metrics.to_string()).is_err());

    let det_with_reasoning = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": [{
            "step_id": 1,
            "source": "agent",
            "message": "",
            "llm_call_count": 0,
            "reasoning_content": "should not be here"
        }]
    });
    assert!(parse_atif_str(&det_with_reasoning.to_string()).is_err());

    // 6. Agent-only fields on user/system step, and source_call_id on system/user step.
    let user_tool_calls = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": [{
            "step_id": 1,
            "source": "user",
            "message": "hi",
            "tool_calls": [{ "tool_call_id": "c1", "function_name": "f", "arguments": {} }]
        }]
    });
    assert!(parse_atif_str(&user_tool_calls.to_string()).is_err());

    let sys_source_call_id = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": [{
            "step_id": 1,
            "source": "system",
            "message": "sys",
            "observation": {
                "results": [{ "source_call_id": "c1", "content": "nope" }]
            }
        }]
    });
    let err = parse_atif_str(&sys_source_call_id.to_string()).unwrap_err();
    assert!(err.to_string().contains("source_call_id"));

    // 7. Non-sequential step_id.
    let bad_step_id = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": [
            { "step_id": 1, "source": "user", "message": "a" },
            { "step_id": 3, "source": "agent", "message": "b" }
        ]
    });
    assert!(parse_atif_str(&bad_step_id.to_string()).is_err());

    // 8. Unmatched source_call_id on agent step.
    let unmatched = json!({
        "schema_version": "ATIF-v1.8",
        "agent": { "name": "a", "version": "1" },
        "steps": [{
            "step_id": 1,
            "source": "agent",
            "message": "calling",
            "tool_calls": [{ "tool_call_id": "c1", "function_name": "f", "arguments": {} }],
            "observation": {
                "results": [{ "source_call_id": "c2", "content": "wrong id" }]
            }
        }]
    });
    assert!(parse_atif_str(&unmatched.to_string()).is_err());
}

#[allow(clippy::too_many_arguments)]
fn make_native_record(
    source: &SourceKey,
    session_key: &str,
    seq: u64,
    event_type: EventType,
    role: EventRole,
    body: &str,
    structured: Option<Value>,
    activity: Option<CoreActivity>,
) -> CoreRecord {
    let native_session =
        NativeSessionKey::composite("native.test", vec![TypedKey::utf8(session_key).unwrap()])
            .unwrap();
    let session_id = ctx_history_core::derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "native-session",
        native_session_key: &native_session,
    })
    .unwrap();
    let native_item = NativeItemKey::native_id("native.event", TypedKey::U64(seq)).unwrap();
    let event_id = ctx_history_core::derive_event_id(ctx_history_core::EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "native-event",
        native_item_key: &native_item,
        subrecord_selector: None,
    })
    .unwrap();
    CoreRecord {
        record_version: CORE_RECORD_VERSION,
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: None,
        session_relationship: Some(ProviderNativeSessionRelationship::Root),
        event_copy: None,
        source: source.clone(),
        provider_session_id: Some(session_key.to_owned()),
        native_event_id: Some(TypedKey::U64(seq)),
        event_sequence: seq,
        occurred_at_unix_ms: Some(1_760_000_000_000 + i64::try_from(seq).unwrap() * 1000),
        event_type: event_type.as_str().to_owned(),
        role: Some(role.as_str().to_owned()),
        agent_scope: Some(AgentScope::Primary),
        parser_revision: "test-native-v1".to_owned(),
        normalization_revision: CORE_NORMALIZATION_REVISION,
        content: CoreContent {
            policy_revision: CORE_CONTENT_POLICY_REVISION,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: Some(body.to_owned()),
            structured_content: structured,
            discovery_exclusion: None,
            activity,
        },
    }
}

#[test]
fn exports_native_session_with_tools_orphaned_output_and_subagent_to_valid_atif_v1_8() {
    let source = SourceKey::derive(
        CaptureProvider::Codex.as_str(),
        "codex_rollout_jsonl",
        "codex-v1",
        1,
        SourceAnchor::CatalogLineage([9_u8; 32]),
    )
    .unwrap();

    let mut args = Map::new();
    args.insert("command".to_owned(), json!("cargo test"));
    let call_activity = CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::Utf8("call_abc".to_owned())),
        invocation: Some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: "shell".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: Value::Object(args),
            },
            started_at_unix_ms: Some(1_760_000_002_000),
        }),
        result: None,
        facts: vec![ProviderDeclaredFact {
            kind: LiteralFactKind::SessionCwd,
            value: "/repo/ctx".to_owned(),
        }],
    };
    let res_activity = CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::Utf8("call_abc".to_owned())),
        invocation: None,
        result: Some(ActivityResult {
            status: None,
            completed_at_unix_ms: Some(1_760_000_003_000),
            duration_ns: None,
            text: ActivityTextCapture::NormalizedBody,
            structured_content: ActivityJsonCapture::Absent,
        }),
        facts: Vec::new(),
    };
    let orphan_activity = CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::Utf8("call_never_issued".to_owned())),
        invocation: None,
        result: Some(ActivityResult {
            status: None,
            completed_at_unix_ms: Some(1_760_000_004_000),
            duration_ns: None,
            text: ActivityTextCapture::NormalizedBody,
            structured_content: ActivityJsonCapture::Absent,
        }),
        facts: Vec::new(),
    };

    let records = vec![
        make_native_record(
            &source,
            "codex-root",
            0,
            EventType::Message,
            EventRole::User,
            "Run the test suite",
            None,
            None,
        ),
        make_native_record(
            &source,
            "codex-root",
            1,
            EventType::Message,
            EventRole::Assistant,
            "I will run cargo test now.",
            Some(json!({
                "model": "gpt-6.2-codex",
                "usage": { "prompt_tokens": 100, "completion_tokens": 25 }
            })),
            None,
        ),
        make_native_record(
            &source,
            "codex-root",
            2,
            EventType::ToolCall,
            EventRole::Assistant,
            "shell {\"command\":\"cargo test\"}",
            None,
            Some(call_activity),
        ),
        make_native_record(
            &source,
            "codex-root",
            3,
            EventType::ToolOutput,
            EventRole::Tool,
            "test result: ok",
            None,
            Some(res_activity),
        ),
        make_native_record(
            &source,
            "codex-root",
            4,
            EventType::ToolOutput,
            EventRole::Tool,
            "late unmatched output",
            None,
            Some(orphan_activity),
        ),
    ];

    let child_records = vec![make_native_record(
        &source,
        "codex-sub-1",
        0,
        EventType::Message,
        EventRole::User,
        "Check clippy warnings",
        None,
        None,
    )];

    let session = AtifExportSession {
        ctx_session_id: records[0].session_id.as_uuid(),
        provider: CaptureProvider::Codex,
        provider_session_id: Some("codex-root".to_owned()),
        source_format: "codex_rollout_jsonl".to_owned(),
        records,
        children: vec![AtifExportSession {
            ctx_session_id: child_records[0].session_id.as_uuid(),
            provider: CaptureProvider::Codex,
            provider_session_id: Some("codex-sub-1".to_owned()),
            source_format: "codex_rollout_jsonl".to_owned(),
            records: child_records,
            children: Vec::new(),
        }],
    };

    let traj = export_session_to_trajectory(&session, AtifExportOptions::default())
        .expect("native export produces valid ATIF v1.8");
    assert_eq!(traj.schema_version, SchemaVersion::V1_8);
    assert_eq!(traj.agent.name, "codex");
    assert_eq!(traj.agent.model_name.as_deref(), Some("gpt-6.2-codex"));
    assert_eq!(traj.steps.len(), 2);
    assert_eq!(traj.steps[0].source, StepSource::User);
    assert_eq!(traj.steps[1].source, StepSource::Agent);
    let obs = traj.steps[1]
        .observation
        .as_ref()
        .expect("observation present");
    assert_eq!(obs.results[0].source_call_id.as_deref(), Some("call_abc"));
    assert_eq!(obs.results[1].source_call_id, None);
    assert_eq!(
        obs.results[1]
            .extra
            .as_ref()
            .and_then(|e| e.get("unmatched_source_call_id"))
            .and_then(Value::as_str),
        Some("call_never_issued")
    );
    let subagents = traj
        .subagent_trajectories
        .as_ref()
        .expect("subagent embedded");
    assert_eq!(subagents.len(), 1);
    assert_eq!(subagents[0].trajectory_id.as_deref(), Some("codex-sub-1"));

    let encoded = export_trajectory_json_pretty(&traj).expect("valid JSON");
    parse_atif_str(&encoded).expect("exported JSON passes strict ATIF v1.8 validation");
}

#[test]
fn local_relative_path_resolver_blocks_traversal_urls_and_absolute_paths() {
    use crate::import::resolve_local_relative_path;

    assert_eq!(
        resolve_local_relative_path(Path::new("runs/root.json"), "sub/child.json"),
        Some(Path::new("runs/sub/child.json").to_path_buf())
    );
    assert_eq!(
        resolve_local_relative_path(Path::new("runs/a/root.json"), "../b/child.json"),
        Some(Path::new("runs/b/child.json").to_path_buf())
    );
    for unsafe_ref in [
        "../outside.json",
        "runs/../../outside.json",
        "/etc/passwd",
        "\\windows\\system32",
        "C:\\temp\\a.json",
        "s3://bucket/sub.json",
        "https://example.test/sub.json",
        "",
        "   ",
    ] {
        assert_eq!(
            resolve_local_relative_path(Path::new("root.json"), unsafe_ref),
            None,
            "expected `{unsafe_ref}` to be rejected"
        );
    }

    let id = Uuid::nil();
    assert_eq!(
        safe_trajectory_filename(CaptureProvider::Claude, id),
        format!("claude_{id}.atif.json")
    );
}

#[test]
fn preserves_embedded_subagent_order_and_nested_depth_two_round_trip() {
    let doc = json!({
        "schema_version": "ATIF-v1.8",
        "session_id": "nested-run",
        "trajectory_id": "root-1",
        "agent": { "name": "orchestrator", "version": "1.0.0" },
        "steps": [
            {
                "step_id": 1,
                "source": "agent",
                "message": "Spawned two subagents",
                "observation": {
                    "results": [
                        {
                            "subagent_trajectory_ref": [
                                { "trajectory_id": "sub-first" },
                                { "trajectory_id": "sub-second" }
                            ]
                        }
                    ]
                }
            }
        ],
        "subagent_trajectories": [
            {
                "schema_version": "ATIF-v1.8",
                "trajectory_id": "sub-first",
                "agent": { "name": "worker-a", "version": "1.0.0" },
                "steps": [
                    {
                        "step_id": 1,
                        "source": "agent",
                        "message": "Delegating grandchild",
                        "observation": {
                            "results": [
                                { "subagent_trajectory_ref": [{ "trajectory_id": "sub-first" }] }
                            ]
                        }
                    }
                ],
                "subagent_trajectories": [
                    {
                        "schema_version": "ATIF-v1.8",
                        "trajectory_id": "sub-first",
                        "agent": { "name": "grandchild", "version": "1.0.0" },
                        "steps": [
                            { "step_id": 1, "source": "user", "message": "Grandchild task" }
                        ]
                    }
                ]
            },
            {
                "schema_version": "ATIF-v1.8",
                "trajectory_id": "sub-second",
                "agent": { "name": "worker-b", "version": "1.0.0" },
                "steps": [
                    { "step_id": 1, "source": "user", "message": "Second task" }
                ]
            }
        ]
    });

    let traj = parse_atif_str(&doc.to_string()).expect("nested trajectory parses");
    let source = test_source_key();
    let records = project_trajectory_to_core_records(&AtifProjectionInput {
        source: &source,
        document_key: "nested.json",
        trajectory: &traj,
        lineage: None,
    })
    .expect("projection succeeds");

    let root_id = records[0].session_id;
    let root_records: Vec<CoreRecord> = records
        .iter()
        .filter(|r| r.session_id == root_id)
        .cloned()
        .collect();
    let first_id = records
        .iter()
        .find(|r| r.provider_session_id.as_deref() == Some("sub-first"))
        .unwrap()
        .session_id;
    let second_id = records
        .iter()
        .find(|r| r.provider_session_id.as_deref() == Some("sub-second"))
        .unwrap()
        .session_id;
    let grandchild_records: Vec<CoreRecord> = records
        .iter()
        .filter(|r| r.parent_session_id == Some(first_id))
        .cloned()
        .collect();
    assert_eq!(grandchild_records.len(), 1);

    let first_child = AtifExportSession {
        ctx_session_id: first_id.as_uuid(),
        provider: CaptureProvider::Custom,
        provider_session_id: Some("sub-first".to_owned()),
        source_format: ATIF_SOURCE_FORMAT.to_owned(),
        records: records
            .iter()
            .filter(|r| r.session_id == first_id)
            .cloned()
            .collect(),
        children: vec![AtifExportSession {
            ctx_session_id: grandchild_records[0].session_id.as_uuid(),
            provider: CaptureProvider::Custom,
            provider_session_id: grandchild_records[0].provider_session_id.clone(),
            source_format: ATIF_SOURCE_FORMAT.to_owned(),
            records: grandchild_records,
            children: Vec::new(),
        }],
    };
    let second_child = AtifExportSession {
        ctx_session_id: second_id.as_uuid(),
        provider: CaptureProvider::Custom,
        provider_session_id: Some("sub-second".to_owned()),
        source_format: ATIF_SOURCE_FORMAT.to_owned(),
        records: records
            .iter()
            .filter(|r| r.session_id == second_id)
            .cloned()
            .collect(),
        children: Vec::new(),
    };

    // Pass children in reversed order to verify embedded_subagent_index restores exact order.
    let exported = export_session_to_trajectory(
        &AtifExportSession {
            ctx_session_id: root_id.as_uuid(),
            provider: CaptureProvider::Custom,
            provider_session_id: root_records[0].provider_session_id.clone(),
            source_format: ATIF_SOURCE_FORMAT.to_owned(),
            records: root_records,
            children: vec![second_child, first_child],
        },
        AtifExportOptions::default(),
    )
    .expect("nested export succeeds");

    assert_eq!(exported, traj);
}

#[test]
fn native_export_normalizes_anthropic_cached_and_cache_creation_tokens() {
    let source = SourceKey::derive(
        CaptureProvider::Claude.as_str(),
        "claude_jsonl",
        "claude-v1",
        1,
        SourceAnchor::CatalogLineage([11_u8; 32]),
    )
    .unwrap();
    let records = vec![make_native_record(
        &source,
        "claude-sess-1",
        0,
        EventType::Message,
        EventRole::Assistant,
        "Done",
        Some(json!({
            "message": {
                "model": "claude-sonnet-4-5",
                "usage": {
                    "input_tokens": 120,
                    "cache_read_input_tokens": 800,
                    "cache_creation_input_tokens": 80,
                    "output_tokens": 45
                }
            }
        })),
        None,
    )];
    let session = AtifExportSession {
        ctx_session_id: records[0].session_id.as_uuid(),
        provider: CaptureProvider::Claude,
        provider_session_id: Some("claude-sess-1".to_owned()),
        source_format: "claude_jsonl".to_owned(),
        records,
        children: Vec::new(),
    };
    let traj = export_session_to_trajectory(&session, AtifExportOptions::default())
        .expect("anthropic native export succeeds");
    let metrics = traj.steps[0].metrics.as_ref().expect("step metrics");
    assert_eq!(metrics.prompt_tokens, Some(1000));
    assert_eq!(metrics.cached_tokens, Some(800));
    assert_eq!(metrics.completion_tokens, Some(45));
    assert_eq!(
        metrics
            .extra
            .as_ref()
            .and_then(|e| e.get("cache_creation_input_tokens"))
            .and_then(Value::as_u64),
        Some(80)
    );
    let final_metrics = traj.final_metrics.as_ref().expect("final metrics");
    assert_eq!(final_metrics.total_prompt_tokens, Some(1000));
    assert_eq!(final_metrics.total_cached_tokens, Some(800));
    assert_eq!(final_metrics.total_completion_tokens, Some(45));
}

#[test]
fn no_subagents_strips_embedded_only_refs_and_populates_inherited_session_id() {
    let doc = json!({
        "schema_version": "ATIF-v1.8",
        "session_id": "run-no-sub-001",
        "trajectory_id": "root-no-sub",
        "agent": { "name": "orchestrator", "version": "1.0.0" },
        "steps": [
            {
                "step_id": 1,
                "source": "agent",
                "message": "Spawned embedded and external workers",
                "observation": {
                    "results": [
                        {
                            "content": "Embedded worker summary",
                            "subagent_trajectory_ref": [
                                { "trajectory_id": "sub-embedded-1", "session_id": "run-no-sub-001" }
                            ]
                        },
                        {
                            "subagent_trajectory_ref": [
                                { "trajectory_id": "sub-embedded-only" }
                            ]
                        },
                        {
                            "subagent_trajectory_ref": [
                                { "trajectory_path": "subagents/external.json" }
                            ]
                        }
                    ]
                }
            },
            {
                "step_id": 2,
                "source": "system",
                "message": "Compaction step with embedded-only observation",
                "observation": {
                    "results": [
                        {
                            "subagent_trajectory_ref": [
                                { "trajectory_id": "sub-embedded-1" }
                            ]
                        }
                    ]
                }
            }
        ],
        "subagent_trajectories": [
            {
                "schema_version": "ATIF-v1.8",
                "trajectory_id": "sub-embedded-1",
                "agent": { "name": "worker", "version": "1.0.0" },
                "steps": [
                    { "step_id": 1, "source": "user", "message": "Subtask without own session_id" }
                ]
            }
        ]
    });

    let traj = parse_atif_str(&doc.to_string()).expect("fixture parses");
    let source = test_source_key();
    let records = project_trajectory_to_core_records(&AtifProjectionInput {
        source: &source,
        document_key: "parent.json",
        trajectory: &traj,
        lineage: None,
    })
    .expect("projection succeeds");

    let root_id = records[0].session_id;
    let root_records: Vec<CoreRecord> = records
        .iter()
        .filter(|r| r.session_id == root_id)
        .cloned()
        .collect();
    let child_records: Vec<CoreRecord> = records
        .iter()
        .filter(|r| r.session_id != root_id)
        .cloned()
        .collect();

    // 1. Exporting parent with include_subagents == false strips embedded-only refs,
    //    prunes empty ObservationResult / Observation, and preserves external trajectory_path refs.
    let parent_flat = export_session_to_trajectory(
        &AtifExportSession {
            ctx_session_id: root_id.as_uuid(),
            provider: CaptureProvider::Custom,
            provider_session_id: root_records[0].provider_session_id.clone(),
            source_format: ATIF_SOURCE_FORMAT.to_owned(),
            records: root_records,
            children: Vec::new(),
        },
        AtifExportOptions {
            include_subagents: false,
        },
    )
    .expect("flat parent export succeeds");

    assert!(parent_flat.subagent_trajectories.is_none());
    let step1_obs = parent_flat.steps[0]
        .observation
        .as_ref()
        .expect("step 1 keeps content + external ref");
    assert_eq!(step1_obs.results.len(), 2);
    assert!(step1_obs.results[0].content.is_some());
    assert!(step1_obs.results[0].subagent_trajectory_ref.is_none());
    let preserved_refs = step1_obs.results[1]
        .subagent_trajectory_ref
        .as_ref()
        .expect("external trajectory_path ref preserved");
    assert_eq!(preserved_refs.len(), 1);
    assert_eq!(
        preserved_refs[0].trajectory_path.as_deref(),
        Some("subagents/external.json")
    );
    assert!(
        parent_flat.steps[1].observation.is_none(),
        "step 2 observation with only embedded refs should be pruned"
    );

    // 2. Exporting the embedded subagent standalone under --no-subagents populates
    //    session_id from StoredAtifTrajectoryHeader.inherited_session_id.
    let child_standalone = export_session_to_trajectory(
        &AtifExportSession {
            ctx_session_id: child_records[0].session_id.as_uuid(),
            provider: CaptureProvider::Custom,
            provider_session_id: child_records[0].provider_session_id.clone(),
            source_format: ATIF_SOURCE_FORMAT.to_owned(),
            records: child_records,
            children: Vec::new(),
        },
        AtifExportOptions {
            include_subagents: false,
        },
    )
    .expect("standalone embedded subagent export succeeds");

    assert_eq!(
        child_standalone.session_id.as_deref(),
        Some("run-no-sub-001")
    );
    assert_eq!(
        child_standalone.trajectory_id.as_deref(),
        Some("sub-embedded-1")
    );
}

#[test]
fn external_trajectory_path_subagent_updates_parent_ref_in_place_when_embedded() {
    let source = test_source_key();
    let parent_doc = json!({
        "schema_version": "ATIF-v1.5",
        "session_id": "ext-embed-run",
        "agent": { "name": "parent-agent", "version": "1.0.0" },
        "steps": [
            {
                "step_id": 1,
                "source": "agent",
                "message": "Delegating to external worker",
                "observation": {
                    "results": [
                        {
                            "content": "delegated",
                            "subagent_trajectory_ref": [
                                {
                                    "session_id": "ext-embed-run",
                                    "trajectory_path": "subagents/worker.json"
                                }
                            ]
                        }
                    ]
                }
            }
        ]
    });
    let worker_doc = json!({
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "ext-worker-42",
        "agent": { "name": "worker-agent", "version": "1.0.0" },
        "steps": [
            { "step_id": 1, "source": "user", "message": "Run external task" }
        ]
    });

    let parent_traj = parse_atif_str(&parent_doc.to_string()).expect("parent parses");
    let worker_traj = parse_atif_str(&worker_doc.to_string()).expect("worker parses");

    let parent_records = project_trajectory_to_core_records(&AtifProjectionInput {
        source: &source,
        document_key: "runs/root.json",
        trajectory: &parent_traj,
        lineage: None,
    })
    .expect("parent projects");
    let parent_session_id = parent_records[0].session_id;
    let worker_records = project_trajectory_to_core_records(&AtifProjectionInput {
        source: &source,
        document_key: "runs/subagents/worker.json",
        trajectory: &worker_traj,
        lineage: Some(&crate::import::AtifLineageLink {
            parent_session_id,
            root_session_id: parent_session_id,
            relationship: ProviderNativeSessionRelationship::Delegated,
            agent_scope: AgentScope::Subagent,
            inherited_session_id: Some("ext-embed-run".to_owned()),
        }),
    })
    .expect("worker projects");

    let exported = export_session_to_trajectory(
        &AtifExportSession {
            ctx_session_id: parent_session_id.as_uuid(),
            provider: CaptureProvider::Custom,
            provider_session_id: parent_records[0].provider_session_id.clone(),
            source_format: ATIF_SOURCE_FORMAT.to_owned(),
            records: parent_records,
            children: vec![AtifExportSession {
                ctx_session_id: worker_records[0].session_id.as_uuid(),
                provider: CaptureProvider::Custom,
                provider_session_id: worker_records[0].provider_session_id.clone(),
                source_format: ATIF_SOURCE_FORMAT.to_owned(),
                records: worker_records,
                children: Vec::new(),
            }],
        },
        AtifExportOptions {
            include_subagents: true,
        },
    )
    .expect("external-to-embedded export succeeds");

    let obs = exported.steps[0]
        .observation
        .as_ref()
        .expect("observation present");
    assert_eq!(
        obs.results.len(),
        1,
        "must update existing ObservationResult in place instead of appending a duplicate: {obs:#?}"
    );
    let refs = obs.results[0]
        .subagent_trajectory_ref
        .as_ref()
        .expect("subagent_trajectory_ref present");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].trajectory_id.as_deref(), Some("ext-worker-42"));
    assert_eq!(
        refs[0].trajectory_path, None,
        "stale relative trajectory_path must be cleared when embedded"
    );
    assert_eq!(refs[0].session_id.as_deref(), Some("ext-embed-run"));

    let embedded = exported
        .subagent_trajectories
        .as_ref()
        .expect("worker embedded");
    assert_eq!(embedded.len(), 1);
    assert_eq!(embedded[0].trajectory_id.as_deref(), Some("ext-worker-42"));
}
