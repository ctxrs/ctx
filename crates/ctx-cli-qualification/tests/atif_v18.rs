#[path = "../../ctx-cli-contract-tests/tests/contracts/support/mod.rs"]
mod support;

use ctx_history_atif::{
    parse_atif_bytes, AudioMediaType, ContentPart, MessageContent, SchemaVersion,
};
use support::{
    ctx, daemon_test_root, fs, json, json_output, wait_for_test_lexical_projection, TempDir, Value,
};

fn wait_for_import_generation(temp: &TempDir, report: &Value) {
    if let Some(generation) = report["sources"][0]["published_generation"].as_str() {
        wait_for_test_lexical_projection(temp, generation);
    }
}

fn sample_full_atif_v18() -> Value {
    json!({
        "schema_version": "ATIF-v1.8",
        "session_id": "harbor-run-001",
        "trajectory_id": "root-traj-001",
        "agent": {
            "name": "harbor-terminus",
            "version": "2.5.0",
            "model_name": "gemini-4-neon",
            "tool_definitions": [
                {
                    "type": "function",
                    "function": {
                        "name": "run_shell",
                        "description": "Run a shell command",
                        "parameters": {
                            "type": "object",
                            "properties": { "command": { "type": "string" } },
                            "required": ["command"]
                        }
                    }
                }
            ],
            "extra": { "cwd": "/workspace/atif-e2e" }
        },
        "notes": "End-to-end ATIF v1.8 round-trip fixture",
        "extra": { "suite": "ctx-e2e" },
        "final_metrics": {
            "total_prompt_tokens": 520,
            "total_completion_tokens": 80,
            "total_cached_tokens": 200,
            "total_cost_usd": 0.00045,
            "total_steps": 4
        },
        "steps": [
            {
                "step_id": 1,
                "timestamp": "2026-06-24T12:00:00Z",
                "source": "system",
                "message": "Context compaction boundary applied",
                "is_copied_context": true,
                "observation": {
                    "results": [
                        {
                            "content": "Summary from compaction subagent",
                            "subagent_trajectory_ref": [
                                {
                                    "trajectory_id": "sub-compact-1",
                                    "session_id": "harbor-run-001",
                                    "trajectory_path": null
                                },
                                {
                                    "trajectory_id": "sub-audit-2",
                                    "session_id": "harbor-run-001"
                                }
                            ]
                        }
                    ]
                },
                "extra": {
                    "context_management": { "type": "compaction", "boundary": "replace" }
                }
            },
            {
                "step_id": 2,
                "timestamp": "2026-06-24T12:00:05Z",
                "source": "user",
                "message": [
                    { "type": "text", "text": "Diagnose the failing widget screenshot and audio clip." },
                    { "type": "image", "source": { "media_type": "image/png", "path": "images/widget.png" } },
                    { "type": "audio", "source": { "media_type": "audio/mp3", "path": "audio/prompt.mp3", "duration_sec": 4.5 } }
                ]
            },
            {
                "step_id": 3,
                "timestamp": "2026-06-24T12:00:09Z",
                "source": "agent",
                "model_name": "gemini-4-neon",
                "reasoning_effort": "high",
                "message": "Checking workspace widget status via shell.",
                "reasoning_content": "Inspect git status and build logs first.",
                "llm_call_count": 1,
                "tool_calls": [
                    {
                        "tool_call_id": "call_shell_1",
                        "function_name": "run_shell",
                        "arguments": { "command": "cargo check --workspace", "path": "src/widget.rs" },
                        "extra": { "timeout_sec": 60 }
                    }
                ],
                "observation": {
                    "results": [
                        {
                            "source_call_id": "call_shell_1",
                            "content": [
                                { "type": "text", "text": "widget compile clean" },
                                { "type": "image", "source": { "media_type": "image/webp", "path": "images/result.webp" } }
                            ],
                            "extra": { "exit_code": 0 }
                        }
                    ]
                },
                "metrics": {
                    "prompt_tokens": 520,
                    "completion_tokens": 80,
                    "cached_tokens": 200,
                    "cost_usd": 0.00045,
                    "prompt_token_ids": [10, 20, 30],
                    "completion_token_ids": [40, 50],
                    "logprobs": [-0.12, -0.05]
                }
            },
            {
                "step_id": 4,
                "timestamp": "2026-06-24T12:00:12Z",
                "source": "agent",
                "message": "",
                "llm_call_count": 0,
                "tool_calls": [
                    {
                        "tool_call_id": "call_hook_1",
                        "function_name": "post_check_hook",
                        "arguments": {}
                    }
                ],
                "observation": {
                    "results": [
                        { "source_call_id": "call_hook_1", "content": "deterministic hook passed" }
                    ]
                }
            }
        ],
        "subagent_trajectories": [
            {
                "schema_version": "ATIF-v1.8",
                "session_id": "harbor-run-001",
                "trajectory_id": "sub-compact-1",
                "agent": { "name": "compactor", "version": "1.0.0" },
                "steps": [
                    { "step_id": 1, "source": "user", "message": "Compress alpha-compaction-marker" },
                    { "step_id": 2, "source": "agent", "message": "Compacted prior conversation cleanly." }
                ]
            },
            {
                "schema_version": "ATIF-v1.8",
                "session_id": "harbor-run-001",
                "trajectory_id": "sub-audit-2",
                "agent": { "name": "auditor", "version": "1.0.0" },
                "steps": [
                    { "step_id": 1, "source": "user", "message": "Verify beta-audit-marker" },
                    { "step_id": 2, "source": "agent", "message": "Audit verified zero regressions." }
                ]
            }
        ]
    })
}

#[test]
fn import_atif_v18_single_file_is_searchable_idempotent_and_round_trips_losslessly() {
    let temp = daemon_test_root();
    let input_path = temp.path().join("full-v18.json");
    let raw = serde_json::to_vec_pretty(&sample_full_atif_v18()).unwrap();
    let expected_parsed = parse_atif_bytes(&raw).expect("fixture is valid ATIF v1.8");
    fs::write(&input_path, &raw).unwrap();

    let first = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "atif-v1",
        "--path",
        input_path.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["sources"][0]["provider"], "custom");
    assert_eq!(first["sources"][0]["source_format"], "atif_v1");
    assert_eq!(first["totals"]["current_rejected_records"], 0);
    assert!(
        first["totals"]["current_indexed_documents"]
            .as_u64()
            .unwrap()
            >= 8,
        "{first:#}"
    );
    wait_for_import_generation(&temp, &first);

    let second = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "atif-v1",
        "--path",
        input_path.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["change"], "no_op", "{second:#}");

    let root_search = json_output(ctx(&temp).args([
        "search",
        "failing widget screenshot",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        !root_search["results"].as_array().unwrap().is_empty(),
        "{root_search:#}"
    );
    let root_session_id = root_search["results"][0]["ctx_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(root_search["results"][0]["agent_scope"], "primary");

    let sub_search = json_output(ctx(&temp).args([
        "search",
        "alpha-compaction-marker",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        !sub_search["results"].as_array().unwrap().is_empty(),
        "{sub_search:#}"
    );
    assert_eq!(sub_search["results"][0]["agent_scope"], "subagent");

    let zero_events_err = ctx(&temp)
        .args([
            "show",
            "session",
            &root_session_id,
            "--format",
            "atif",
            "--max-events",
            "0",
        ])
        .output()
        .unwrap();
    assert!(!zero_events_err.status.success());

    let exported_path = temp.path().join("exported-v18.json");
    ctx(&temp)
        .args([
            "show",
            "session",
            &root_session_id,
            "--format",
            "atif",
            "--out",
            exported_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let exported_bytes = fs::read(&exported_path).unwrap();
    let exported_traj =
        parse_atif_bytes(&exported_bytes).expect("exported ATIF must satisfy strict v1.8 schema");
    assert_eq!(exported_traj.schema_version, SchemaVersion::V1_8);
    assert_eq!(exported_traj, expected_parsed);

    let MessageContent::Parts(parts) = &exported_traj.steps[1].message else {
        panic!("expected multimodal message parts");
    };
    assert!(matches!(
        &parts[2],
        ContentPart::Audio { source } if source.media_type == AudioMediaType::Mpeg
    ));

    // Import exported file into a completely fresh data root and verify search + show.
    let fresh = daemon_test_root();
    let reimport = json_output(ctx(&fresh).args([
        "import",
        "--input-format",
        "atif-v1",
        "--path",
        exported_path.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(reimport["totals"]["current_rejected_records"], 0);
    wait_for_import_generation(&fresh, &reimport);

    let fresh_search = json_output(ctx(&fresh).args([
        "search",
        "beta-audit-marker",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        !fresh_search["results"].as_array().unwrap().is_empty(),
        "re-imported subagent was not searchable: {fresh_search:#}"
    );
    assert_eq!(fresh_search["results"][0]["agent_scope"], "subagent");

    let fresh_root_search = json_output(ctx(&fresh).args([
        "search",
        "deterministic hook passed",
        "--refresh",
        "off",
        "--format=json",
    ]));
    let fresh_root_id = fresh_root_search["results"][0]["ctx_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let shown = json_output(ctx(&fresh).args([
        "show",
        "session",
        &fresh_root_id,
        "--mode",
        "log",
        "--format=json",
    ]));
    let events = shown["events"].as_array().unwrap();
    assert!(
        events.iter().any(|event| event["event_type"] == "tool_call"
            && event["text"].as_str().unwrap_or("").contains("run_shell")),
        "expected tool_call in re-imported session: {shown:#}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["event_type"] == "tool_output"
                && event["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("widget compile clean")),
        "expected tool_output in re-imported session: {shown:#}"
    );
}

#[test]
fn import_atif_directory_resolves_external_subagents_continuations_and_ignores_non_atif_json() {
    let temp = daemon_test_root();
    let dir = temp.path().join("atif-batch");
    fs::create_dir_all(dir.join("subagents")).unwrap();

    fs::write(
        dir.join("root.json"),
        json!({
            "schema_version": "ATIF-v1.5",
            "session_id": "dir-run-001",
            "continued_trajectory_ref": "part2.json",
            "agent": { "name": "dir-agent", "version": "1.0.0" },
            "steps": [
                {
                    "step_id": 1,
                    "timestamp": "2026-06-24T13:00:00Z",
                    "source": "agent",
                    "message": "Delegating root-dir-marker to external worker.",
                    "observation": {
                        "results": [
                            {
                                "content": "delegated",
                                "subagent_trajectory_ref": [
                                    {
                                        "session_id": "dir-run-001",
                                        "trajectory_path": "subagents/worker.json"
                                    }
                                ]
                            }
                        ]
                    }
                }
            ]
        })
        .to_string(),
    )
    .unwrap();

    fs::write(
        dir.join("subagents/worker.json"),
        json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "ext-worker-1",
            "agent": { "name": "worker-agent", "version": "1.0.0" },
            "steps": [
                {
                    "step_id": 1,
                    "timestamp": "2026-06-24T13:00:05Z",
                    "source": "user",
                    "message": "external-subagent-marker task"
                }
            ]
        })
        .to_string(),
    )
    .unwrap();

    fs::write(
        dir.join("part2.json"),
        json!({
            "schema_version": "ATIF-v1.8",
            "session_id": "dir-run-001",
            "trajectory_id": "root-part-2",
            "agent": { "name": "dir-agent", "version": "1.0.0" },
            "steps": [
                {
                    "step_id": 1,
                    "timestamp": "2026-06-24T13:01:00Z",
                    "source": "agent",
                    "message": "continuation-segment-marker complete"
                }
            ]
        })
        .to_string(),
    )
    .unwrap();

    fs::write(dir.join("config.json"), r#"{"unrelated": true}"#).unwrap();

    let report = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "atif-v1",
        "--path",
        dir.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(report["sources"][0]["source_format"], "atif_v1");
    assert_eq!(report["totals"]["current_rejected_records"], 0);
    assert!(
        report["totals"]["current_ignored_records"]
            .as_u64()
            .unwrap()
            >= 1,
        "expected non-ATIF config.json to be counted as ignored: {report:#}"
    );
    wait_for_import_generation(&temp, &report);

    let worker_search = json_output(ctx(&temp).args([
        "search",
        "external-subagent-marker",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(worker_search["results"][0]["agent_scope"], "subagent");

    let cont_search = json_output(ctx(&temp).args([
        "search",
        "continuation-segment-marker",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        !cont_search["results"].as_array().unwrap().is_empty(),
        "{cont_search:#}"
    );

    // Archive export embeds delegated subagents into their parent trajectory,
    // while keeping ResumedFrom continuation segments as separate files.
    let archive_embedded = temp.path().join("atif-dir-embedded");
    let embedded_receipt = json_output(ctx(&temp).args([
        "archive",
        "atif",
        "--out",
        archive_embedded.to_str().unwrap(),
        "--format=json",
    ]));
    assert_eq!(
        embedded_receipt["sessions_exported"], 2,
        "expected root + continuation (with worker embedded), got: {embedded_receipt:#}"
    );
    assert_eq!(embedded_receipt["subagents_embedded"], 1);

    // Verify embedded export rewrote root.json's external trajectory_path ref in place
    // instead of leaving a stale trajectory_path or appending a duplicate ObservationResult.
    let embedded_root_file = embedded_receipt["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["subagents"] == 1)
        .expect("root file with 1 embedded subagent");
    let embedded_root_traj =
        parse_atif_bytes(&fs::read(embedded_root_file["path"].as_str().unwrap()).unwrap())
            .expect("embedded root ATIF valid");
    let root_obs = embedded_root_traj.steps[0]
        .observation
        .as_ref()
        .expect("root observation present");
    assert_eq!(
        root_obs.results.len(),
        1,
        "must update external ref in place instead of appending duplicate ObservationResult"
    );
    let root_refs = root_obs.results[0]
        .subagent_trajectory_ref
        .as_ref()
        .expect("subagent_trajectory_ref present");
    assert_eq!(root_refs.len(), 1);
    assert_eq!(root_refs[0].trajectory_id.as_deref(), Some("ext-worker-1"));
    assert_eq!(root_refs[0].trajectory_path, None);

    let archive_flat = temp.path().join("atif-dir-flat");
    let flat_receipt = json_output(ctx(&temp).args([
        "archive",
        "atif",
        "--out",
        archive_flat.to_str().unwrap(),
        "--no-subagents",
        "--format=json",
    ]));
    assert_eq!(flat_receipt["sessions_exported"], 3);
    assert_eq!(flat_receipt["subagents_embedded"], 0);

    // Standalone worker export under --no-subagents populates inherited session_id.
    let flat_worker_file = flat_receipt["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["provider_session_id"] == "ext-worker-1")
        .expect("standalone worker file in flat export");
    let flat_worker_traj =
        parse_atif_bytes(&fs::read(flat_worker_file["path"].as_str().unwrap()).unwrap())
            .expect("flat worker ATIF valid");
    assert_eq!(flat_worker_traj.session_id.as_deref(), Some("dir-run-001"));
}

#[test]
fn import_and_archive_atif_handles_cyclic_external_trajectory_paths_without_dropping_sessions() {
    let temp = daemon_test_root();
    let dir = temp.path().join("atif-cycle");
    fs::create_dir_all(&dir).unwrap();

    fs::write(
        dir.join("a.json"),
        json!({
            "schema_version": "ATIF-v1.8",
            "session_id": "cycle-run-a",
            "trajectory_id": "cycle-traj-a",
            "agent": { "name": "cycle-agent-a", "version": "1.0.0" },
            "steps": [
                {
                    "step_id": 1,
                    "timestamp": "2026-06-24T15:00:00Z",
                    "source": "agent",
                    "message": "cyclic-external-alpha referencing b.json",
                    "observation": {
                        "results": [
                            {
                                "subagent_trajectory_ref": [
                                    { "trajectory_path": "b.json" }
                                ]
                            }
                        ]
                    }
                }
            ]
        })
        .to_string(),
    )
    .unwrap();

    fs::write(
        dir.join("b.json"),
        json!({
            "schema_version": "ATIF-v1.8",
            "session_id": "cycle-run-b",
            "trajectory_id": "cycle-traj-b",
            "agent": { "name": "cycle-agent-b", "version": "1.0.0" },
            "steps": [
                {
                    "step_id": 1,
                    "timestamp": "2026-06-24T15:00:05Z",
                    "source": "agent",
                    "message": "cyclic-external-beta referencing a.json",
                    "observation": {
                        "results": [
                            {
                                "subagent_trajectory_ref": [
                                    { "trajectory_path": "a.json" }
                                ]
                            }
                        ]
                    }
                }
            ]
        })
        .to_string(),
    )
    .unwrap();

    let report = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "atif-v1",
        "--path",
        dir.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(report["totals"]["current_rejected_records"], 0);
    wait_for_import_generation(&temp, &report);

    for needle in ["cyclic-external-alpha", "cyclic-external-beta"] {
        let search =
            json_output(ctx(&temp).args(["search", needle, "--refresh", "off", "--format=json"]));
        assert!(
            !search["results"].as_array().unwrap().is_empty(),
            "missing `{needle}`: {search:#}"
        );
        assert_eq!(
            search["results"][0]["agent_scope"], "primary",
            "cyclic files must not emit mutual Subagent links: {search:#}"
        );
    }

    let archive_out = temp.path().join("atif-cycle-out");
    let receipt = json_output(ctx(&temp).args([
        "archive",
        "atif",
        "--out",
        archive_out.to_str().unwrap(),
        "--format=json",
    ]));
    assert_eq!(
        receipt["sessions_exported"], 2,
        "cyclic external trajectory files must both export instead of vanishing: {receipt:#}"
    );
    assert_eq!(receipt["subagents_embedded"], 0);
}

#[test]
fn import_atif_rejects_invalid_v17_v18_documents() {
    let invalid_cases = [
        (
            "session_only_subagent_ref.json",
            json!({
                "schema_version": "ATIF-v1.8",
                "agent": { "name": "a", "version": "1" },
                "steps": [{
                    "step_id": 1,
                    "source": "system",
                    "message": "delegated",
                    "observation": {
                        "results": [{
                            "subagent_trajectory_ref": [{ "session_id": "run-1" }]
                        }]
                    }
                }]
            }),
        ),
        (
            "unknown_field.json",
            json!({
                "schema_version": "ATIF-v1.8",
                "rogue_key": true,
                "agent": { "name": "a", "version": "1" },
                "steps": [{ "step_id": 1, "source": "user", "message": "hi" }]
            }),
        ),
        (
            "user_with_tool_calls.json",
            json!({
                "schema_version": "ATIF-v1.8",
                "agent": { "name": "a", "version": "1" },
                "steps": [{
                    "step_id": 1,
                    "source": "user",
                    "message": "hi",
                    "tool_calls": [{ "tool_call_id": "c1", "function_name": "f", "arguments": {} }]
                }]
            }),
        ),
        (
            "llm_zero_with_metrics.json",
            json!({
                "schema_version": "ATIF-v1.8",
                "agent": { "name": "a", "version": "1" },
                "steps": [{
                    "step_id": 1,
                    "source": "agent",
                    "message": "",
                    "llm_call_count": 0,
                    "metrics": { "prompt_tokens": 5 }
                }]
            }),
        ),
        (
            "audio_image_mismatch.json",
            json!({
                "schema_version": "ATIF-v1.8",
                "agent": { "name": "a", "version": "1" },
                "steps": [{
                    "step_id": 1,
                    "source": "user",
                    "message": [{
                        "type": "audio",
                        "source": { "media_type": "image/png", "path": "a.png" }
                    }]
                }]
            }),
        ),
        (
            "unsupported_v2.json",
            json!({
                "schema_version": "ATIF-v2.0",
                "agent": { "name": "a", "version": "1" },
                "steps": [{ "step_id": 1, "source": "user", "message": "hi" }]
            }),
        ),
    ];

    for (name, payload) in invalid_cases {
        let temp = daemon_test_root();
        let file = temp.path().join(name);
        fs::write(&file, payload.to_string()).unwrap();
        let output = ctx(&temp)
            .args([
                "import",
                "--input-format",
                "atif-v1",
                "--path",
                file.to_str().unwrap(),
                "--format=json",
                "--progress",
                "none",
            ])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "expected invalid ATIF `{name}` to fail, got stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn native_codex_session_exports_via_show_and_archive_atif_then_round_trips_into_fresh_root() {
    let temp = daemon_test_root();
    let sessions_dir = temp.path().join("codex-sessions/2026/06/24");
    fs::create_dir_all(&sessions_dir).unwrap();
    let rollout_path = sessions_dir.join("rollout-atif-e2e.jsonl");
    fs::write(
        &rollout_path,
        concat!(
            r#"{"timestamp":"2026-06-24T14:00:00.000Z","type":"session_meta","payload":{"id":"codex-atif-root","timestamp":"2026-06-24T14:00:00.000Z","cwd":"/repo/atif-demo","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-24T14:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Check native-codex-export-marker"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-06-24T14:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Running diagnostic command now."}]}}"#,
            "\n",
            r#"{"timestamp":"2026-06-24T14:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo hello-from-tool\"}","call_id":"call_exec_1"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-24T14:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec_1","output":"hello-from-tool\n"}}"#,
            "\n"
        ),
    )
    .unwrap();

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        rollout_path.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    wait_for_import_generation(&temp, &imported);

    let search_a = json_output(ctx(&temp).args([
        "search",
        "native-codex-export-marker",
        "--refresh",
        "off",
        "--format=json",
    ]));
    let session_id = search_a["results"][0]["ctx_session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let show_stdout = ctx(&temp)
        .args(["show", "session", &session_id, "--format", "atif"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let single_traj = parse_atif_bytes(&show_stdout)
        .expect("show session --format atif must emit valid ATIF v1.8");
    assert_eq!(single_traj.schema_version, SchemaVersion::V1_8);
    assert_eq!(single_traj.agent.name, "codex");
    assert!(
        single_traj
            .steps
            .iter()
            .any(|step| step.tool_calls.as_ref().is_some_and(|calls| {
                calls
                    .iter()
                    .any(|call| call.function_name == "exec_command")
            })),
        "expected exec_command tool call in ATIF export: {single_traj:#?}"
    );
    assert!(
        single_traj.steps.iter().any(|step| {
            step.observation.as_ref().is_some_and(|obs| {
                obs.results.iter().any(|res| {
                    res.source_call_id.as_deref() == Some("call_exec_1")
                        && res
                            .content
                            .as_ref()
                            .is_some_and(|c| c.to_searchable_text().contains("hello-from-tool"))
                })
            })
        }),
        "expected correlated observation result in ATIF export: {single_traj:#?}"
    );

    let archive_out = temp.path().join("atif-archive-out");
    let receipt = json_output(ctx(&temp).args([
        "archive",
        "atif",
        "--out",
        archive_out.to_str().unwrap(),
        "--provider",
        "codex",
        "--workspace",
        "atif-demo",
        "--since",
        "2026-06-24T13:59:00Z",
        "--until",
        "2026-06-24T14:10:00Z",
        "--format=json",
    ]));
    assert_eq!(receipt["operation"], "archive_atif");
    assert_eq!(receipt["atif_schema_version"], "ATIF-v1.8");
    assert_eq!(receipt["sessions_exported"], 1);
    let exported_file = receipt["files"][0]["path"].as_str().unwrap();
    let batch_traj = parse_atif_bytes(&fs::read(exported_file).unwrap())
        .expect("archive atif output must satisfy ATIF v1.8 validation");
    assert_eq!(batch_traj.schema_version, SchemaVersion::V1_8);

    // Re-import exported batch directory into a completely fresh data root B.
    let fresh = daemon_test_root();
    let reimport = json_output(ctx(&fresh).args([
        "import",
        "--input-format",
        "atif-v1",
        "--path",
        archive_out.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(reimport["totals"]["current_rejected_records"], 0);
    wait_for_import_generation(&fresh, &reimport);

    let search_b = json_output(ctx(&fresh).args([
        "search",
        "native-codex-export-marker",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        !search_b["results"].as_array().unwrap().is_empty(),
        "{search_b:#}"
    );
    let session_b = search_b["results"][0]["ctx_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let show_b = json_output(ctx(&fresh).args([
        "show",
        "session",
        &session_b,
        "--mode",
        "log",
        "--format=json",
    ]));
    let events_b = show_b["events"].as_array().unwrap();
    assert!(
        events_b
            .iter()
            .any(|e| e["text"].as_str().unwrap_or("").contains("exec_command")),
        "expected exec_command in re-imported Codex session: {show_b:#}"
    );
    assert!(
        events_b
            .iter()
            .any(|e| e["text"].as_str().unwrap_or("").contains("hello-from-tool")),
        "expected hello-from-tool in re-imported Codex session: {show_b:#}"
    );
}

#[test]
fn cli_surface_rejects_show_event_atif_and_serves_embedded_atif_docs() {
    let temp = support::tempdir();

    let event_err = ctx(&temp)
        .env("FORCE_COLOR", "1")
        .args([
            "show",
            "event",
            "00000000-0000-0000-0000-000000000000",
            "--format=atif",
        ])
        .output()
        .unwrap();
    assert!(!event_err.status.success());
    let stderr = String::from_utf8_lossy(&event_err.stderr);
    assert!(
        stderr.contains("invalid value 'atif'"),
        "expected clap to reject --format=atif on show event, got: {stderr}"
    );
    assert!(
        !stderr.contains('\u{1b}'),
        "expected pre-parse --format=atif Clap error to stay uncolored in machine mode, got: {stderr:?}"
    );

    let docs = json_output(ctx(&temp).args(["docs", "show", "atif-format", "--format=json"]));
    assert_eq!(docs["id"], "atif-format");
    let body = docs["body"].as_str().unwrap();
    assert!(body.contains("ctx import --input-format atif-v1"));
    assert!(body.contains("ctx show session"));
    assert!(body.contains("ctx archive atif"));
}
