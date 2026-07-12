use super::support::*;

#[test]

fn provider_fixture_replay_supports_claude_cursor_metadata() {
    let temp = tempdir();
    let fixture = provider_fixture("claude.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Claude, "claude-session-1");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events[1].event_type, EventType::Summary);
    assert_eq!(
        events[1].sync.metadata["cursor"].as_str(),
        Some("claude-cursor-1")
    );
    assert_eq!(events[1].payload["provider_event_index"].as_u64(), Some(1));
}

#[test]
fn provider_fixture_replay_supports_opencode_fixture() {
    let temp = tempdir();
    let fixture = provider_fixture("opencode.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_provider_fixture_jsonl(&fixture, &mut store, fixed_import_options(fixture.clone()))
            .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 3);
    assert_eq!(summary.imported_edges, 1);
    let parent_id =
        stored_provider_session_id(&store, CaptureProvider::OpenCode, "opencode-session-1");
    let child_id = stored_provider_session_id(
        &store,
        CaptureProvider::OpenCode,
        "opencode-session-1-scout",
    );
    let parent = store.get_session(parent_id).unwrap();
    let child = store.get_session(child_id).unwrap();
    assert_eq!(parent.provider, CaptureProvider::OpenCode);
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    assert_eq!(store.events_for_session(parent_id).unwrap().len(), 2);
    assert_eq!(store.events_for_session(child_id).unwrap().len(), 1);
}

#[test]
fn native_file_tree_imports_do_not_mutate_provider_sources() {
    let temp = tempdir();

    let codex_sessions = provider_history_fixture("codex-sessions");
    assert_native_source_clean_import_preserves_source(
        "Codex sessions",
        &codex_sessions,
        |store| {
            import_codex_session_tree(
                &codex_sessions,
                store,
                CodexSessionImportOptions {
                    source_path: Some(codex_sessions.clone()),
                    allow_partial_failures: true,
                    ..CodexSessionImportOptions::default()
                },
            )
            .unwrap()
        },
    );

    let codex_history = provider_history_fixture("codex-history.jsonl");
    assert_native_source_clean_import_preserves_source("Codex history", &codex_history, |store| {
        import_codex_history_jsonl(
            &codex_history,
            store,
            CodexHistoryImportOptions {
                source_path: Some(codex_history.clone()),
                ..CodexHistoryImportOptions::default()
            },
        )
        .unwrap()
    });

    let pi = provider_history_fixture("pi-session.jsonl");
    assert_native_source_clean_import_preserves_source("Pi", &pi, |store| {
        import_pi_session_jsonl(
            &pi,
            store,
            PiSessionImportOptions {
                source_path: Some(pi.clone()),
                allow_partial_failures: true,
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap()
    });

    let claude = write_claude_smoke_fixture(&temp);
    assert_native_source_clean_import_preserves_source("Claude", &claude, |store| {
        import_claude_projects_jsonl_tree(
            &claude,
            store,
            ClaudeProjectsImportOptions {
                source_path: Some(claude.clone()),
                allow_partial_failures: true,
                ..ClaudeProjectsImportOptions::default()
            },
        )
        .unwrap()
    });

    let qoder = provider_history_fixture("qoder/projects");
    assert_native_source_clean_import_preserves_source("Qoder", &qoder, |store| {
        import_qoder_history(
            &qoder,
            store,
            QoderImportOptions {
                source_path: Some(qoder.clone()),
                allow_partial_failures: true,
                ..QoderImportOptions::default()
            },
        )
        .unwrap()
    });

    let codebuddy = provider_history_fixture("codebuddy/Data");
    assert_native_source_clean_import_preserves_source("CodeBuddy", &codebuddy, |store| {
        import_codebuddy_history(
            &codebuddy,
            store,
            CodeBuddyImportOptions {
                source_path: Some(codebuddy.clone()),
                allow_partial_failures: true,
                ..CodeBuddyImportOptions::default()
            },
        )
        .unwrap()
    });

    let continue_root = temp.path().join("continue-sessions-readonly");
    fs::create_dir_all(&continue_root).unwrap();
    fs::write(
        continue_root.join("continue-readonly.json"),
        json!({
            "sessionId": "continue-readonly",
            "title": "Continue readonly",
            "createdAt": "2026-07-04T16:00:00Z",
            "history": [
                {"message": {"role": "user", "content": "continue readonly oracle"}},
                {"message": {"role": "assistant", "content": "continue readonly answer"}}
            ]
        })
        .to_string(),
    )
    .unwrap();
    assert_native_source_clean_import_preserves_source("Continue", &continue_root, |store| {
        import_continue_cli_sessions(
            &continue_root,
            store,
            ContinueCliImportOptions {
                source_path: Some(continue_root.clone()),
                allow_partial_failures: true,
                ..ContinueCliImportOptions::default()
            },
        )
        .unwrap()
    });

    let openclaw_root = temp.path().join("openclaw-readonly");
    fs::create_dir_all(openclaw_root.join("sessions")).unwrap();
    fs::write(
        openclaw_root.join("sessions/session-1.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "id": "openclaw-readonly",
                "timestamp": "2026-07-04T16:00:00Z"
            }),
            json!({
                "type": "message",
                "id": "openclaw-readonly-user",
                "timestamp": "2026-07-04T16:00:01Z",
                "message": {"role": "user", "content": "openclaw readonly oracle"}
            })
        ),
    )
    .unwrap();
    assert_native_source_clean_import_preserves_source("OpenClaw", &openclaw_root, |store| {
        import_openclaw_history(
            &openclaw_root,
            store,
            OpenClawImportOptions {
                source_path: Some(openclaw_root.clone()),
                allow_partial_failures: true,
                ..OpenClawImportOptions::default()
            },
        )
        .unwrap()
    });

    let trae = provider_history_fixture("trae/User/workspaceStorage");
    assert_native_source_clean_import_preserves_source("Trae", &trae, |store| {
        import_trae_history(
            &trae,
            store,
            TraeImportOptions {
                source_path: Some(trae.clone()),
                allow_partial_failures: true,
                ..TraeImportOptions::default()
            },
        )
        .unwrap()
    });

    let antigravity = provider_history_fixture("antigravity/v1/brain");
    assert_native_source_clean_import_preserves_source("Antigravity", &antigravity, |store| {
        import_antigravity_cli_history(
            &antigravity,
            store,
            AntigravityCliImportOptions {
                source_path: Some(antigravity.clone()),
                allow_partial_failures: true,
                ..AntigravityCliImportOptions::default()
            },
        )
        .unwrap()
    });

    let gemini = write_gemini_smoke_fixture(&temp);
    assert_native_source_clean_import_preserves_source("Gemini", &gemini, |store| {
        import_gemini_cli_history(
            &gemini,
            store,
            GeminiCliImportOptions {
                source_path: Some(gemini.clone()),
                allow_partial_failures: true,
                ..GeminiCliImportOptions::default()
            },
        )
        .unwrap()
    });

    let tabnine = provider_history_fixture("tabnine-cli/.tabnine/agent");
    assert_native_source_clean_import_preserves_source("Tabnine", &tabnine, |store| {
        import_tabnine_cli_history(
            &tabnine,
            store,
            TabnineCliImportOptions {
                source_path: Some(tabnine.clone()),
                allow_partial_failures: true,
                ..TabnineCliImportOptions::default()
            },
        )
        .unwrap()
    });

    let cursor = provider_history_fixture("cursor/2026.06.24");
    assert_native_source_clean_import_preserves_source("Cursor", &cursor, |store| {
        import_cursor_native_history(
            &cursor,
            store,
            CursorNativeImportOptions {
                source_path: Some(cursor.clone()),
                allow_partial_failures: true,
                ..CursorNativeImportOptions::default()
            },
        )
        .unwrap()
    });

    let windsurf = provider_history_fixture("windsurf/transcripts");
    assert_native_source_clean_import_preserves_source("Windsurf", &windsurf, |store| {
        import_windsurf_cascade_hook_transcripts(
            &windsurf,
            store,
            WindsurfCascadeHookImportOptions {
                source_path: Some(windsurf.clone()),
                allow_partial_failures: true,
                ..WindsurfCascadeHookImportOptions::default()
            },
        )
        .unwrap()
    });

    let droid = write_droid_smoke_fixture(&temp);
    assert_native_source_clean_import_preserves_source("Factory Droid", &droid, |store| {
        import_factory_ai_droid_sessions(
            &droid,
            store,
            FactoryAiDroidImportOptions {
                source_path: Some(droid.clone()),
                allow_partial_failures: true,
                ..FactoryAiDroidImportOptions::default()
            },
        )
        .unwrap()
    });

    let copilot = write_copilot_smoke_fixture(&temp);
    assert_native_source_clean_import_preserves_source("Copilot CLI", &copilot, |store| {
        import_copilot_cli_session_events(
            &copilot,
            store,
            CopilotCliImportOptions {
                source_path: Some(copilot.clone()),
                allow_partial_failures: true,
                ..CopilotCliImportOptions::default()
            },
        )
        .unwrap()
    });

    let qwen = provider_history_fixture("qwen-code/.qwen/projects");
    assert_native_source_clean_import_preserves_source("Qwen Code", &qwen, |store| {
        import_qwen_code_history(
            &qwen,
            store,
            QwenCodeImportOptions {
                source_path: Some(qwen.clone()),
                allow_partial_failures: true,
                ..QwenCodeImportOptions::default()
            },
        )
        .unwrap()
    });

    let kimi = provider_history_fixture("kimi-code-cli/.kimi-code");
    assert_native_source_clean_import_preserves_source("Kimi Code CLI", &kimi, |store| {
        import_kimi_code_cli_history(
            &kimi,
            store,
            KimiCodeCliImportOptions {
                source_path: Some(kimi.clone()),
                allow_partial_failures: true,
                ..KimiCodeCliImportOptions::default()
            },
        )
        .unwrap()
    });

    let auggie = provider_history_fixture("auggie/v0.32.0/sessions");
    assert_native_source_clean_import_preserves_source("Auggie", &auggie, |store| {
        import_auggie_history(
            &auggie,
            store,
            AuggieImportOptions {
                source_path: Some(auggie.clone()),
                allow_partial_failures: true,
                ..AuggieImportOptions::default()
            },
        )
        .unwrap()
    });

    let junie = provider_history_fixture("junie/sessions");
    assert_native_source_clean_import_preserves_source("Junie", &junie, |store| {
        import_junie_history(
            &junie,
            store,
            JunieImportOptions {
                source_path: Some(junie.clone()),
                allow_partial_failures: true,
                ..JunieImportOptions::default()
            },
        )
        .unwrap()
    });

    let nanoclaw = write_nanoclaw_smoke_project(&temp, "nanoclaw readonly oracle");
    assert_native_source_clean_import_preserves_source("NanoClaw", &nanoclaw, |store| {
        import_nanoclaw_project(
            &nanoclaw,
            store,
            NanoClawImportOptions {
                source_path: Some(nanoclaw.clone()),
                allow_partial_failures: true,
                ..NanoClawImportOptions::default()
            },
        )
        .unwrap()
    });

    let mistral = provider_history_fixture("mistral-vibe/v1/logs/session");
    assert_native_source_clean_import_preserves_source("Mistral Vibe", &mistral, |store| {
        import_mistral_vibe_history(
            &mistral,
            store,
            MistralVibeImportOptions {
                source_path: Some(mistral.clone()),
                allow_partial_failures: true,
                ..MistralVibeImportOptions::default()
            },
        )
        .unwrap()
    });

    let mux = provider_history_fixture("mux/v0.27.0/sessions");
    assert_native_source_clean_import_preserves_source("Mux", &mux, |store| {
        import_mux_history(
            &mux,
            store,
            MuxImportOptions {
                source_path: Some(mux.clone()),
                allow_partial_failures: true,
                ..MuxImportOptions::default()
            },
        )
        .unwrap()
    });

    let rovodev = provider_history_fixture("rovodev/v1/sessions");
    assert_native_source_clean_import_preserves_source("Rovo Dev", &rovodev, |store| {
        import_rovodev_history(
            &rovodev,
            store,
            RovoDevImportOptions {
                source_path: Some(rovodev.clone()),
                allow_partial_failures: true,
                ..RovoDevImportOptions::default()
            },
        )
        .unwrap()
    });

    let cline = provider_history_fixture("cline/data");
    assert_native_source_clean_import_preserves_source("Cline", &cline, |store| {
        import_cline_task_json_history(
            &cline,
            store,
            ClineTaskJsonImportOptions {
                source_path: Some(cline.clone()),
                allow_partial_failures: true,
                ..ClineTaskJsonImportOptions::default()
            },
        )
        .unwrap()
    });

    let roo = provider_history_fixture("roo/storage");
    assert_native_source_clean_import_preserves_source("Roo Code", &roo, |store| {
        import_roo_task_json_history(
            &roo,
            store,
            RooTaskJsonImportOptions {
                source_path: Some(roo.clone()),
                allow_partial_failures: true,
                ..RooTaskJsonImportOptions::default()
            },
        )
        .unwrap()
    });
}

fn assert_native_source_clean_import_preserves_source(
    label: &str,
    source: &Path,
    run_import: impl FnOnce(&mut Store) -> ProviderImportSummary,
) {
    let summary = assert_provider_source_unchanged(source, run_import);
    assert!(
        summary.imported_sessions > 0,
        "{label}: expected imported sessions, got {summary:?}"
    );
    assert!(
        summary.imported_events > 0,
        "{label}: expected imported events, got {summary:?}"
    );
}

#[test]
fn continue_cli_empty_history_rejects_metadata_only_session() {
    let temp = tempdir();
    let root = temp.path().join("continue-sessions");
    fs::create_dir_all(&root).unwrap();
    let fixture = root.join("empty-session.json");
    fs::write(
        &fixture,
        json!({
            "sessionId": "continue-empty-session",
            "title": "Empty Continue session",
            "createdAt": "2026-07-04T16:00:00Z",
            "history": []
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_continue_cli_sessions(
        &root,
        &mut store,
        ContinueCliImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T16:00:00Z".parse().unwrap(),
            ..ContinueCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.skipped_sessions, 1);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation messages"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn continue_cli_tool_call_redacts_raw_outputs_and_reimports_file_touches() {
    let temp = tempdir();
    let root = temp.path().join("continue-sessions");
    fs::create_dir_all(&root).unwrap();
    let raw_output = "CONTINUE_RAW_TOOL_OUTPUT_NEEDLE";
    let raw_old = "CONTINUE_RAW_DIFF_OLD_NEEDLE";
    let raw_new = "CONTINUE_RAW_DIFF_NEW_NEEDLE";
    let patch = format!(
        "*** Begin Patch\n*** Update File: src/continue_policy.rs\n- {raw_old}\n+ {raw_new}\n*** End Patch\n"
    );
    fs::write(
        root.join("continue-tool-boundary.json"),
        json!({
            "sessionId": "continue-tool-boundary",
            "title": "Continue tool policy",
            "createdAt": "2026-07-04T16:00:00Z",
            "history": [
                {
                    "id": "continue-user-1",
                    "timestamp": "2026-07-04T16:00:00Z",
                    "message": {
                        "role": "user",
                        "content": "continue tool policy oracle prompt"
                    }
                },
                {
                    "id": "continue-tool-1",
                    "timestamp": "2026-07-04T16:00:01Z",
                    "message": {
                        "role": "assistant",
                        "content": ""
                    },
                    "toolCallStates": [
                        {
                            "status": "done",
                            "toolCall": {
                                "function": {
                                    "name": "apply_patch",
                                    "arguments": patch
                                }
                            },
                            "output": raw_output
                        }
                    ]
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_continue_cli_sessions(
        &root,
        &mut store,
        ContinueCliImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T16:05:00Z".parse().unwrap(),
            allow_partial_failures: true,
            ..ContinueCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Continue, "continue-tool-boundary");
    let events = store.events_for_session(session_id).unwrap();
    let tool = events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .expect("tool call metadata event imported");
    assert_eq!(
        tool.payload["body"]["content_retention"].as_str(),
        Some("metadata")
    );
    let rendered_tool = serde_json::to_string(tool).unwrap();
    assert!(rendered_tool.contains("apply_patch"));
    assert!(!rendered_tool.contains(raw_output));
    assert!(!rendered_tool.contains(raw_old));
    assert!(!rendered_tool.contains(raw_new));
    assert!(store
        .search_event_hits("continue tool policy oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Continue)));
    assert!(store.search_event_hits(raw_output, 10).unwrap().is_empty());
    assert!(store.search_event_hits(raw_old, 10).unwrap().is_empty());
    assert!(store.search_event_hits(raw_new, 10).unwrap().is_empty());
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| {
            file.sync.metadata["provider"].as_str() == Some(CaptureProvider::Continue.as_str())
                && file.path == "src/continue_policy.rs"
                && file.confidence == Confidence::Explicit
        }));

    let second = import_continue_cli_sessions(
        &root,
        &mut store,
        ContinueCliImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T16:06:00Z".parse().unwrap(),
            allow_partial_failures: true,
            ..ContinueCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}

#[test]
fn native_pi_fixture_imports_event_types_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-session.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Pi, fixture.clone());
    assert_eq!(source.source_format, "pi_session_jsonl");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T16:10:00Z".parse().unwrap(),
            allow_partial_failures: true,
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 6);

    let session_id = stored_provider_session_id(&store, CaptureProvider::Pi, "pi-session-docs-1");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 6);
    assert_event_type_count(&events, EventType::Message, 2);
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    assert_event_type_count(&events, EventType::CommandOutput, 1);
    assert_event_type_count(&events, EventType::Summary, 1);
    assert_event_with_role(&events, EventType::ToolOutput, EventRole::Tool);
    assert_event_with_role(&events, EventType::CommandOutput, EventRole::Tool);
    assert_events_have_provider_citations(&events);

    assert_search_hits_provider(
        &store,
        "Inspect the provider metadata rows",
        CaptureProvider::Pi,
    );
    assert_search_hits_provider(
        &store,
        "Provider metadata import fixture",
        CaptureProvider::Pi,
    );
    assert_search_misses(&store, "tests passed");
    assert_search_misses(&store, "ok token=fixture-secret");

    let second = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T16:15:00Z".parse().unwrap(),
            allow_partial_failures: true,
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 6);
}

#[test]
fn native_pi_malformed_file_is_atomic_without_partial_failures() {
    let temp = tempdir();
    let fixture = provider_history_fixture("pi-malformed-partial.jsonl");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_pi_session_jsonl(
        &fixture,
        &mut store,
        PiSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..PiSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 2, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store
        .search_event_hits("after malformed line", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn native_claude_projects_imports_jsonl_tree() {
    let temp = tempdir();
    let fixture = write_claude_smoke_fixture(&temp);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_claude_projects_jsonl_tree(
        &fixture,
        &mut store,
        ClaudeProjectsImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 5);
    assert_eq!(summary.imported_edges, 1);
    let parent_id =
        stored_provider_session_id(&store, CaptureProvider::Claude, "claude-native-parent");
    let child_id = stored_provider_session_id(
        &store,
        CaptureProvider::Claude,
        "claude-native-parent/subagents/agent-scout",
    );
    let child = store.get_session(child_id).unwrap();
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    let events = store.events_for_session(parent_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
}

#[test]
fn native_claude_empty_project_jsonl_rejects_no_real_message() {
    let temp = tempdir();
    let root = temp.path().join("claude/projects/-workspace");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("empty.jsonl"), "").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_claude_projects_jsonl_tree(
        temp.path().join("claude/projects"),
        &mut store,
        ClaudeProjectsImportOptions {
            allow_partial_failures: true,
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation messages"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_claude_projects_skips_oversized_jsonl_record() {
    let temp = tempdir();
    let root = temp.path().join("claude/projects/-workspace");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("oversized-claude.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "sessionId": "claude-oversized",
            "timestamp": "2026-07-04T14:00:00Z",
            "cwd": "/workspace",
            "version": "test",
            "type": "user",
            "message": {"role": "user", "content": "before oversized claude"},
            "uuid": "claude-oversized-before"
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    bytes.extend_from_slice(
        jsonl_line(json!({
            "sessionId": "claude-oversized",
            "timestamp": "2026-07-04T14:00:01Z",
            "cwd": "/workspace",
            "version": "test",
            "type": "assistant",
            "message": {"role": "assistant", "content": "after oversized claude"},
            "uuid": "claude-oversized-after"
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_claude_projects_jsonl_tree(
        temp.path().join("claude/projects"),
        &mut store,
        ClaudeProjectsImportOptions {
            source_path: Some(temp.path().join("claude/projects")),
            imported_at: "2026-07-04T14:30:00Z".parse().unwrap(),
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Claude, "claude-oversized");
    let rendered = serde_json::to_string(&store.events_for_session(session_id).unwrap()).unwrap();
    assert!(rendered.contains("before oversized claude"));
    assert!(rendered.contains("after oversized claude"));
}

#[test]
fn antigravity_native_history_imports_transcripts_and_preserves_previews() {
    let temp = tempdir();
    let fixture = provider_history_fixture("antigravity/v1/brain");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_antigravity_cli_history(
        &fixture,
        &mut store,
        AntigravityCliImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
            ..AntigravityCliImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.failures[0].line, 3);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
    assert_eq!(summary.imported_sessions, 3);
    assert_eq!(summary.imported_events, 9);

    let success_session =
        stored_provider_session_id(&store, CaptureProvider::Antigravity, "agy-success");
    let success = store.events_for_session(success_session).unwrap();
    assert_eq!(success.len(), 3);
    let tool = success
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .unwrap();
    assert!(tool.payload["body"]["tool_calls"].is_array());
    assert!(tool.payload["body"]["tool_calls"][0]["args"].is_object());
    assert_eq!(
        tool.payload["body"]["tool_calls"][0]["args"]["CodeContent"].as_str(),
        Some("# Demo\n\nThis is a sanitized Antigravity fixture.\n")
    );
    let archive = store.export_archive().unwrap();
    assert!(archive.files_touched.iter().any(|file| {
        file.path == "/workspace/demo/README.md" && file.confidence == Confidence::High
    }));
    assert_eq!(
        tool.sync.metadata["metadata"]["source_format"].as_str(),
        Some(ANTIGRAVITY_CLI_SOURCE_FORMAT)
    );
    let source_paths: Vec<String> = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .filter_map(|source| source.descriptor.raw_source_path)
        .collect();
    assert!(source_paths
        .iter()
        .any(|path| path.contains("transcript_full.jsonl")));

    assert!(store
        .sessions_by_external_session_limited(CaptureProvider::Antigravity, "agy-future", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn native_windsurf_fixture_imports_searches_reimports_and_file_touches() {
    let temp = tempdir();
    let fixture = provider_history_fixture("windsurf/transcripts");
    let transcript = fixture.join("windsurf-hook-trajectory-1.jsonl");
    let transcript_text = fs::read_to_string(&transcript).unwrap().replace(
        "windsurf unknown typed payload oracle",
        "windsurf-unknown-payload-sentinel",
    );
    fs::write(&transcript, transcript_text).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Windsurf, fixture.clone());
    assert_eq!(
        source.source_format,
        "windsurf_cascade_hook_transcript_jsonl_tree"
    );
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert!(source.import_support.is_auto_importable());
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
            ..WindsurfCascadeHookImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{first:?}");
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 5);
    assert!(store
        .search_event_hits("windsurf cascade hook oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Windsurf)));
    assert!(store
        .search_event_hits("windsurf-unknown-payload-sentinel", 10)
        .unwrap()
        .is_empty());

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Windsurf,
        "windsurf-hook-trajectory-1",
    );
    let events = store.events_for_session(session_id).unwrap();
    let code_action = events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .unwrap();
    let code_action_payload = code_action.payload.to_string();
    assert!(code_action_payload.contains("src/windsurf_hook_oracle.py"));
    assert!(!code_action_payload.contains("print('windsurf cascade hook oracle')"));

    let archive = store.export_archive().unwrap();
    assert!(archive.files_touched.iter().any(|file| {
        file.path == "src/windsurf_hook_oracle.py" && file.confidence == Confidence::High
    }));

    let second = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:05:00Z".parse().unwrap(),
            ..WindsurfCascadeHookImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{second:?}");
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 5);
}

#[test]
fn native_qoder_fixture_imports_documented_transcript_jsonl() {
    let temp = tempdir();
    let fixture = provider_history_fixture("qoder/projects");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Qoder, fixture.clone());
    assert_eq!(source.source_format, "qoder_transcript_jsonl_tree");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_qoder_history(
        &fixture,
        &mut store,
        QoderImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-07-01T12:00:00Z".parse().unwrap(),
            ..QoderImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{first:?}");
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 7);
    assert!(store
        .search_event_hits("qoder jsonl oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Qoder)));
    assert!(store
        .search_event_hits("qoder native import ok", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Qoder)));

    let session_id = stored_provider_session_id(&store, CaptureProvider::Qoder, "qoder-session-1");
    let events = store.events_for_session(session_id).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == EventType::Message
                && event.role == Some(EventRole::User))
    );
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall
            && event.role == Some(EventRole::Assistant)));
    let tool_output = events
        .iter()
        .find(|event| {
            event.event_type == EventType::ToolOutput && event.role == Some(EventRole::User)
        })
        .expect("tool output metadata event imported");
    assert!(!tool_output.payload.to_string().contains("qoder import ok"));

    let second = import_qoder_history(
        &fixture,
        &mut store,
        QoderImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-07-01T12:05:00Z".parse().unwrap(),
            ..QoderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{second:?}");
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 7);
}

#[test]
fn native_cursor_fixture_imports_searches_reports_malformed_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("cursor/2026.06.24");
    let transcript = fixture.join(
        "projects/sanitized-workspace/agent-transcripts/cursor-native-session-1/cursor-native-session-1.jsonl",
    );
    let transcript_text = fs::read_to_string(&transcript)
        .unwrap()
        .replace("cursor native fixture proof", "cursor-tool-input-sentinel")
        .replace(
            "wrote cursor-native-cli-oracle.txt",
            "cursor-tool-output-sentinel",
        );
    fs::write(&transcript, transcript_text).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Cursor, fixture.clone());
    assert_eq!(source.source_format, "cursor_agent_transcript_jsonl_tree");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_cursor_native_history(
        &fixture,
        &mut store,
        CursorNativeImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T12:20:00Z".parse().unwrap(),
            ..CursorNativeImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 1, "{first:?}");
    assert_eq!(first.failures[0].line, 2);
    assert!(first.failures[0].error.contains("malformed JSONL"));
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 6);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Cursor, "cursor-native-session-1");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 5);
    assert_event_type_count(&events, EventType::Message, 2);
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    assert_event_type_count(&events, EventType::Summary, 1);
    assert_event_with_role(&events, EventType::ToolOutput, EventRole::User);
    assert_events_have_provider_citations(&events);

    let partial_id =
        stored_provider_session_id(&store, CaptureProvider::Cursor, "cursor-malformed-session");
    let partial_events = store.events_for_session(partial_id).unwrap();
    assert_eq!(partial_events.len(), 1);
    assert_event_type_count(&partial_events, EventType::Message, 1);
    assert_events_have_provider_citations(&partial_events);

    assert_search_hits_provider(
        &store,
        "Create cursor-native-cli-oracle",
        CaptureProvider::Cursor,
    );
    assert_search_hits_provider(
        &store,
        "This valid line should import",
        CaptureProvider::Cursor,
    );
    assert_search_misses(&store, "cursor-tool-output-sentinel");
    assert_search_misses(&store, "cursor-tool-input-sentinel");

    let archive = store.export_archive().unwrap();
    assert!(archive
        .files_touched
        .iter()
        .any(|file| file.path == "cursor-native-cli-oracle.txt"));

    let second = import_cursor_native_history(
        &fixture,
        &mut store,
        CursorNativeImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-24T12:25:00Z".parse().unwrap(),
            ..CursorNativeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 1, "{second:?}");
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 6);
}

#[test]
fn native_windsurf_reports_malformed_jsonl_partially() {
    let temp = tempdir();
    let fixture = provider_history_fixture("windsurf/malformed/transcripts");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_windsurf_cascade_hook_transcripts(
        &fixture,
        &mut store,
        WindsurfCascadeHookImportOptions {
            allow_partial_failures: true,
            imported_at: "2026-06-24T14:00:00Z".parse().unwrap(),
            ..WindsurfCascadeHookImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{summary:?}");
    assert_eq!(summary.failures[0].line, 2);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert!(store
        .search_event_hits("windsurf malformed after bad oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Windsurf)));
}

#[test]
fn native_claude_projects_reports_malformed_jsonl() {
    let temp = tempdir();
    let fixture = temp.path().join("claude-malformed/projects/-workspace");
    fs::create_dir_all(&fixture).unwrap();
    fs::write(
            fixture.join("claude-malformed.jsonl"),
            concat!(
                "{\"sessionId\":\"claude-malformed\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"valid\"}}\n",
                "{\"sessionId\":\"claude-malformed\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"partial\"}]\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_claude_projects_jsonl_tree(
        &fixture,
        &mut store,
        ClaudeProjectsImportOptions {
            allow_partial_failures: true,
            ..ClaudeProjectsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert!(summary.failures[0].error.contains("malformed JSONL"));
    assert!(summary.failures[0].error.contains("claude-malformed.jsonl"));
}

#[test]
fn native_task_json_imports_cline_and_roo_task_directories() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let cline = provider_history_fixture("cline/data");
    let cline_first = import_cline_task_json_history(
        &cline,
        &mut store,
        ClineTaskJsonImportOptions {
            source_path: Some(cline.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-30T12:10:00Z".parse().unwrap(),
            ..ClineTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(cline_first.failed, 0, "{:?}", cline_first.failures);
    assert_eq!(cline_first.imported_sessions, 1);
    assert_eq!(cline_first.imported_events, 4);

    let cline_session = stored_provider_session_id(&store, CaptureProvider::Cline, "cline-task-1");
    let cline_events = store.events_for_session(cline_session).unwrap();
    assert_eq!(cline_events.len(), 4);
    assert_event_type_count(&cline_events, EventType::ToolCall, 1);
    assert_event_type_count(&cline_events, EventType::ToolOutput, 1);
    assert_event_with_role(&cline_events, EventType::ToolOutput, EventRole::User);
    assert_events_have_provider_citations(&cline_events);
    assert_search_hits_provider(
        &store,
        "Write a short parser note for Cline task JSON support.",
        CaptureProvider::Cline,
    );
    assert_search_misses(&store, "CLINE_RAW_TOOL_RESULT_NEEDLE");
    assert!(!serde_json::to_string(&cline_events)
        .unwrap()
        .contains("CLINE_RAW_TOOL_RESULT_NEEDLE"));
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "docs/cline-task-json.md"));

    let cline_second = import_cline_task_json_history(
        &cline,
        &mut store,
        ClineTaskJsonImportOptions {
            source_path: Some(cline.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-30T12:10:00Z".parse().unwrap(),
            ..ClineTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(cline_second.imported_sessions, 0);
    assert_eq!(cline_second.imported_events, 0);
    assert_eq!(cline_second.skipped_sessions, 1);
    assert_eq!(cline_second.skipped_events, 4);

    let roo = provider_history_fixture("roo/storage");
    let roo_first = import_roo_task_json_history(
        &roo,
        &mut store,
        RooTaskJsonImportOptions {
            source_path: Some(roo.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-30T12:10:00Z".parse().unwrap(),
            ..RooTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(roo_first.failed, 0, "{:?}", roo_first.failures);
    assert_eq!(roo_first.imported_sessions, 2);
    assert_eq!(roo_first.imported_events, 6);

    let roo_session = stored_provider_session_id(&store, CaptureProvider::RooCode, "roo-task-1");
    let roo_events = store.events_for_session(roo_session).unwrap();
    assert_eq!(roo_events.len(), 4);
    assert_event_type_count(&roo_events, EventType::ToolCall, 1);
    assert_event_type_count(&roo_events, EventType::ToolOutput, 1);
    assert_event_with_role(&roo_events, EventType::ToolOutput, EventRole::User);
    assert_events_have_provider_citations(&roo_events);
    assert_search_hits_provider(
        &store,
        "Add a Roo Code task JSON import smoke test.",
        CaptureProvider::RooCode,
    );
    assert_search_misses(&store, "ROO_RAW_TOOL_RESULT_NEEDLE");
    assert!(!serde_json::to_string(&roo_events)
        .unwrap()
        .contains("ROO_RAW_TOOL_RESULT_NEEDLE"));
    let fallback =
        stored_provider_session_id(&store, CaptureProvider::RooCode, "roo-fallback-task");
    assert_eq!(store.events_for_session(fallback).unwrap().len(), 2);
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| file.path == "tests/roo-task-json.txt"));

    let roo_second = import_roo_task_json_history(
        &roo,
        &mut store,
        RooTaskJsonImportOptions {
            source_path: Some(roo.clone()),
            allow_partial_failures: true,
            imported_at: "2026-06-30T12:15:00Z".parse().unwrap(),
            ..RooTaskJsonImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(roo_second.imported_sessions, 0);
    assert_eq!(roo_second.imported_events, 0);
    assert_eq!(roo_second.skipped_sessions, 2);
    assert_eq!(roo_second.skipped_events, 6);
}
#[test]
fn native_task_json_malformed_file_is_atomic_without_partial_failures() {
    let temp = tempdir();
    let task = temp.path().join("cline-data/tasks/cline-bad");
    fs::create_dir_all(&task).unwrap();
    fs::write(
        task.join("task_metadata.json"),
        r#"{"taskId":"cline-bad","createdAt":"2026-06-30T12:00:00Z"}"#,
    )
    .unwrap();
    fs::write(
        task.join("api_conversation_history.json"),
        "[{\"role\":\"user\"",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_cline_task_json_history(
        temp.path().join("cline-data"),
        &mut store,
        ClineTaskJsonImportOptions::default(),
    )
    .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("api_conversation_history.json"));
    let session_id = provider_session_uuid(CaptureProvider::Cline, "cline-bad");
    assert!(store.get_session(session_id).is_err());
}

#[test]
fn native_task_json_metadata_only_task_rejects_no_real_message() {
    let temp = tempdir();
    let task = temp.path().join("cline-data/tasks/cline-metadata-only");
    fs::create_dir_all(&task).unwrap();
    fs::write(
        task.join("task_metadata.json"),
        json!({
            "taskId": "cline-metadata-only",
            "createdAt": "2026-06-30T12:00:00Z",
            "task": "metadata only should not import"
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_cline_task_json_history(
        temp.path().join("cline-data"),
        &mut store,
        ClineTaskJsonImportOptions {
            allow_partial_failures: true,
            ..ClineTaskJsonImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation message"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_roo_non_array_message_history_rejects_no_real_message() {
    let temp = tempdir();
    let task = temp.path().join("roo-storage/tasks/roo-non-array");
    fs::create_dir_all(&task).unwrap();
    fs::write(
        task.join("api_conversation_history.json"),
        json!({
            "messages": {
                "role": "user",
                "content": "roo non-array history should not import"
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_roo_task_json_history(
        temp.path().join("roo-storage"),
        &mut store,
        RooTaskJsonImportOptions {
            allow_partial_failures: true,
            ..RooTaskJsonImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation message"));
    assert!(store
        .search_event_hits("roo non-array history should not import", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_codebuddy_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codebuddy/Data");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codebuddy_history(
        &fixture,
        &mut store,
        CodeBuddyImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T16:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 3);

    let alpha = stored_provider_session_id(
        &store,
        CaptureProvider::CodeBuddy,
        "11112222333344445555666677778888/session-alpha",
    );
    let events = store.events_for_session(alpha).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("codebuddy oracle prompt update"));
    assert!(rendered.contains("src/codebuddy_fixture.rs"));
    assert!(!events[0]
        .payload
        .pointer("/body/text")
        .and_then(Value::as_str)
        .unwrap()
        .contains("project_context"));
    assert!(store
        .search_event_hits("codebuddy oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::CodeBuddy)));
    assert!(store
        .search_event_hits("project_context", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("plain fallback codebuddy beta oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::CodeBuddy)));

    let source = provider_source_for_path(CaptureProvider::CodeBuddy, fixture.clone());
    assert_eq!(source.source_format, CODEBUDDY_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let second = import_codebuddy_history(
        &fixture,
        &mut store,
        CodeBuddyImportOptions {
            allow_partial_failures: true,
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 3);
}

#[test]
fn native_codebuddy_empty_messages_rejects_no_real_message() {
    let temp = tempdir();
    let session_dir = temp.path().join("codebuddy/project/session-empty");
    fs::create_dir_all(session_dir.join("messages")).unwrap();
    fs::write(
        session_dir.join("index.json"),
        json!({"messages": []}).to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codebuddy_history(
        temp.path().join("codebuddy/project"),
        &mut store,
        CodeBuddyImportOptions {
            allow_partial_failures: true,
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation messages"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_codebuddy_non_array_messages_rejects_orphan_message_file() {
    let temp = tempdir();
    let session_dir = temp.path().join("codebuddy/project/session-non-array");
    fs::create_dir_all(session_dir.join("messages")).unwrap();
    fs::write(
        session_dir.join("index.json"),
        json!({"messages": {"id": "message-1", "role": "user"}}).to_string(),
    )
    .unwrap();
    fs::write(
        session_dir.join("messages/message-1.json"),
        json!({"content": "codebuddy orphan message should not import"}).to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codebuddy_history(
        temp.path().join("codebuddy/project"),
        &mut store,
        CodeBuddyImportOptions {
            allow_partial_failures: true,
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation messages"));
    assert!(store
        .search_event_hits("codebuddy orphan message should not import", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn native_codebuddy_symlinked_messages_dir_is_not_imported() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let project = temp.path().join("codebuddy/project");
    let session_dir = project.join("session-linked");
    let real_messages = temp.path().join("real-messages");
    fs::create_dir_all(&session_dir).unwrap();
    fs::create_dir_all(&real_messages).unwrap();
    fs::write(
        session_dir.join("index.json"),
        json!({"messages": [{"id": "message-1", "role": "user"}]}).to_string(),
    )
    .unwrap();
    fs::write(
        real_messages.join("message-1.json"),
        json!({"content": "symlinked CodeBuddy content must not import"}).to_string(),
    )
    .unwrap();
    symlink(&real_messages, session_dir.join("messages")).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_codebuddy_history(
        &project,
        &mut store,
        CodeBuddyImportOptions {
            allow_partial_failures: true,
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("project")
                && reason.contains("no CodeBuddy history sessions")
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn native_codebuddy_symlinked_message_file_is_not_imported() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let project = temp.path().join("codebuddy/project");
    let session_dir = project.join("session-linked-message");
    let messages_dir = session_dir.join("messages");
    let outside_message = temp.path().join("outside-message.json");
    fs::create_dir_all(&messages_dir).unwrap();
    fs::write(
        session_dir.join("index.json"),
        json!({"messages": [{"id": "message-1", "role": "user"}]}).to_string(),
    )
    .unwrap();
    fs::write(
        &outside_message,
        json!({"content": "symlinked CodeBuddy message file must not import"}).to_string(),
    )
    .unwrap();
    symlink(&outside_message, messages_dir.join("message-1.json")).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codebuddy_history(
        &project,
        &mut store,
        CodeBuddyImportOptions {
            allow_partial_failures: true,
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("symlinked provider transcript files are rejected"));
    assert!(store
        .search_event_hits("symlinked CodeBuddy message file must not import", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_openhands_file_events_redact_outputs_cite_source_and_leave_tree_readonly() {
    let temp = tempdir();
    let root = temp.path().join("openhands");
    let conversation = root
        .join("user-a")
        .join("v1_conversations")
        .join("conversation-1");
    fs::create_dir_all(&conversation).unwrap();
    let raw_output = "OPENHANDS_RAW_COMMAND_OUTPUT_NEEDLE";
    let raw_old = "OPENHANDS_RAW_DIFF_OLD_NEEDLE";
    let raw_new = "OPENHANDS_RAW_DIFF_NEW_NEEDLE";
    fs::write(
        conversation.join("0001-message.json"),
        json!({
            "id": "openhands-message-1",
            "timestamp": "2026-07-04T17:00:00Z",
            "source": "user",
            "llm_message": {
                "role": "user",
                "content": "openhands file event oracle prompt"
            }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        conversation.join("0002-action.json"),
        json!({
            "id": "openhands-action-1",
            "timestamp": "2026-07-04T17:00:01Z",
            "source": "agent",
            "action": {
                "kind": "FileEditorAction",
                "command": "write",
                "path": "src/openhands_policy.py",
                "diff": format!(
                    "diff --git a/src/openhands_policy.py b/src/openhands_policy.py\n@@\n- {raw_old}\n+ {raw_new}\n"
                )
            }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        conversation.join("0003-output.json"),
        json!({
            "id": "openhands-output-1",
            "timestamp": "2026-07-04T17:00:02Z",
            "source": "environment",
            "observation": {
                "kind": "ExecuteBashObservation",
                "output": raw_output,
                "exit_code": 0
            }
        })
        .to_string(),
    )
    .unwrap();
    let before_tree = provider_source_snapshot(&root);
    let source = provider_source_for_path(CaptureProvider::OpenHands, root.clone());
    assert_eq!(source.source_format, "openhands_file_events");
    assert_eq!(source.status, ProviderSourceStatus::Available);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_openhands_file_events(
        &root,
        &mut store,
        OpenHandsImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T17:05:00Z".parse().unwrap(),
            allow_partial_failures: true,
            ..OpenHandsImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(provider_source_snapshot(&root), before_tree);
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::OpenHands, "conversation-1");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 3);
    let action = events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .expect("file editor action imported");
    let rendered_action = serde_json::to_string(action).unwrap();
    assert!(rendered_action.contains("src/openhands_policy.py"));
    assert!(!rendered_action.contains(raw_old));
    assert!(!rendered_action.contains(raw_new));
    let output = events
        .iter()
        .find(|event| event.event_type == EventType::CommandOutput)
        .expect("successful command output metadata imported");
    assert_eq!(
        output.payload["content_retention"]
            .as_str()
            .or_else(|| output.payload["body"]["content_retention"].as_str())
            .or_else(|| output.payload["body"]["body"]["content_retention"].as_str()),
        Some("metadata_only")
    );
    let rendered_output = serde_json::to_string(output).unwrap();
    assert!(!rendered_output.contains(raw_output));
    assert!(store
        .search_event_hits("openhands file event oracle prompt", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::OpenHands)));
    assert!(store.search_event_hits(raw_output, 10).unwrap().is_empty());
    assert!(store.search_event_hits(raw_old, 10).unwrap().is_empty());
    assert!(store.search_event_hits(raw_new, 10).unwrap().is_empty());
    assert!(store
        .export_archive()
        .unwrap()
        .files_touched
        .iter()
        .any(|file| {
            file.sync.metadata["provider"].as_str() == Some(CaptureProvider::OpenHands.as_str())
                && file.path == "src/openhands_policy.py"
                && file.confidence == Confidence::High
        }));
    let source = store
        .capture_source_by_external_session(CaptureProvider::OpenHands, "conversation-1")
        .unwrap()
        .unwrap();
    assert!(source
        .descriptor
        .raw_source_path
        .as_deref()
        .unwrap()
        .ends_with("v1_conversations/conversation-1"));

    let second = import_openhands_file_events(
        &root,
        &mut store,
        OpenHandsImportOptions {
            source_path: Some(root.clone()),
            imported_at: "2026-07-04T17:06:00Z".parse().unwrap(),
            allow_partial_failures: true,
            ..OpenHandsImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
}

#[test]
fn native_openclaw_empty_session_jsonl_rejects_no_real_message() {
    let temp = tempdir();
    let root = temp.path().join("openclaw/sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("empty.jsonl"), "").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_openclaw_history(
        temp.path().join("openclaw"),
        &mut store,
        OpenClawImportOptions {
            allow_partial_failures: true,
            ..OpenClawImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation messages"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_openclaw_contentless_message_does_not_fabricate_search_text() {
    let temp = tempdir();
    let root = temp.path().join("openclaw/sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("contentless.jsonl"),
        json!({
            "type": "message",
            "id": "openclaw-contentless",
            "role": "assistant"
        })
        .to_string()
            + "\n",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_openclaw_history(
        temp.path().join("openclaw"),
        &mut store,
        OpenClawImportOptions {
            allow_partial_failures: true,
            ..OpenClawImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation message"));
    assert!(store
        .search_event_hits("OpenClaw message", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_openclaw_tool_output_is_metadata_only_and_not_searchable() {
    let temp = tempdir();
    let root = temp.path().join("openclaw/sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("openclaw-tool-output.jsonl"),
        [
            json!({
                "type": "session",
                "id": "openclaw-tool-output",
                "timestamp": "2026-07-04T12:00:00Z",
                "cwd": "/workspace/openclaw"
            })
            .to_string(),
            json!({
                "type": "message",
                "id": "openclaw-tool-user",
                "timestamp": "2026-07-04T12:00:01Z",
                "message": {
                    "role": "user",
                    "content": "openclaw tool output policy oracle"
                }
            })
            .to_string(),
            json!({
                "type": "message",
                "id": "openclaw-tool-result",
                "timestamp": "2026-07-04T12:00:02Z",
                "message": {
                    "role": "tool",
                    "content": "OPENCLAW_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH"
                }
            })
            .to_string(),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_openclaw_history(
        temp.path().join("openclaw"),
        &mut store,
        OpenClawImportOptions {
            allow_partial_failures: true,
            ..OpenClawImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::OpenClaw, "openclaw-tool-output");
    let events = store.events_for_session(session_id).unwrap();
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    assert_event_with_role(&events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&events);
    assert_search_hits_provider(
        &store,
        "openclaw tool output policy oracle",
        CaptureProvider::OpenClaw,
    );
    assert_search_misses(&store, "OPENCLAW_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains("OPENCLAW_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH"));
}

#[test]
fn native_trae_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("trae/User/workspaceStorage");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_trae_history(
        &fixture,
        &mut store,
        TraeImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T21:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);

    let source = provider_source_for_path(CaptureProvider::Trae, fixture.clone());
    assert_eq!(source.source_format, TRAE_STATE_VSCDB_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Trae,
        "trae-workspace-1/trae-fixture-session",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(session.provider, CaptureProvider::Trae);
    assert_eq!(
        session.sync.metadata["metadata"]["workspace_folder"].as_str(),
        Some("/workspace/trae-fixture")
    );
    let session_metadata = session.sync.metadata["metadata"].to_string();
    assert!(!session_metadata.contains("\"messages\""));
    assert!(!session_metadata.contains("trae oracle answer from state vscdb"));

    let events = store.events_for_session(session_id).unwrap();
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("trae oracle prompt from state vscdb"));
    assert!(rendered.contains("trae oracle answer from state vscdb"));
    assert!(store
        .search_event_hits("trae oracle answer", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));

    let second = import_trae_history(
        &fixture,
        &mut store,
        TraeImportOptions {
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}

#[test]
fn native_trae_chatstore_entries_schema_drift_imports() {
    let temp = tempdir();
    let workspace = temp.path().join("User/workspaceStorage/schema-drift");
    fs::create_dir_all(&workspace).unwrap();
    let db_path = workspace.join("state.vscdb");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    let value = json!({
        "entries": {
            "drift-session": {
                "id": "drift-session",
                "name": "Drift session",
                "messages": [
                    {
                        "id": "drift-user",
                        "role": "user",
                        "content": [{"type": "text", "text": "trae drift prompt"}],
                        "createdAt": "2026-07-05T12:00:00Z"
                    },
                    {
                        "id": "drift-assistant",
                        "role": "assistant",
                        "content": {"summary": "trae drift answer"},
                        "createdAt": "2026-07-05T12:01:00Z"
                    }
                ]
            }
        }
    })
    .to_string();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES ('ChatStore', ?1)",
        [value],
    )
    .unwrap();
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_trae_history(
        temp.path().join("User/workspaceStorage"),
        &mut store,
        TraeImportOptions {
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    assert!(store
        .search_event_hits("trae drift answer", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));
}

#[test]
fn native_trae_cn_input_history_key_imports_user_messages() {
    let temp = tempdir();
    let workspace = temp
        .path()
        .join("Trae CN/User/workspaceStorage/cn-workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("workspace.json"),
        r#"{"folder":"file:///workspace/trae-cn-fixture"}"#,
    )
    .unwrap();
    let db_path = workspace.join("state.vscdb");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
        rusqlite::params![
            TRAE_CN_INPUT_HISTORY_KEY,
            json!([
                {
                    "id": "cn-input-1",
                    "inputText": "TRAE_CN_INPUT_HISTORY_ORACLE alpha",
                    "createdAt": "2026-07-05T13:00:00Z"
                },
                {
                    "id": "cn-input-2",
                    "text": "TRAE_CN_INPUT_HISTORY_ORACLE beta",
                    "createdAt": "2026-07-05T13:01:00Z"
                }
            ])
            .to_string()
        ],
    )
    .unwrap();
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_trae_history(
        temp.path().join("Trae CN/User/workspaceStorage"),
        &mut store,
        TraeImportOptions {
            allow_partial_failures: true,
            ..TraeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Trae,
        "cn-workspace/trae-cn-input-history",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.sync.metadata["metadata"]["workspace_folder"].as_str(),
        Some("/workspace/trae-cn-fixture")
    );
    let events = store.events_for_session(session_id).unwrap();
    assert!(events
        .iter()
        .all(|event| event.role == Some(EventRole::User)));
    assert!(store
        .search_event_hits("TRAE_CN_INPUT_HISTORY_ORACLE", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Trae)));
}
#[test]
fn native_auggie_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("auggie/v0.32.0/sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Auggie, fixture.clone());
    assert_eq!(source.source_format, AUGGIE_SESSION_JSON_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_auggie_history(
        &fixture,
        &mut store,
        AuggieImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T20:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);

    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Auggie,
        "01K0AUGGIESESSION0000000000",
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("auggie session json oracle prompt"));
    assert!(rendered.contains("Auggie session import finished"));
    assert!(rendered.contains("auggie node text oracle prompt"));
    assert!(store
        .search_event_hits("Auggie node response imported", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Auggie)));

    let source = store
        .capture_source_by_external_session(CaptureProvider::Auggie, "01K0AUGGIESESSION0000000000")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_metadata"]["upstream_schema_anchor"]["package"].as_str(),
        Some("@augmentcode/auggie@0.32.0")
    );

    let second = import_auggie_history(
        &fixture,
        &mut store,
        AuggieImportOptions {
            allow_partial_failures: true,
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 4);
}

#[test]
fn native_auggie_tool_only_nodes_reject_no_real_message() {
    let temp = tempdir();
    let root = temp.path().join("auggie/sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("auggie-tool-only.json"),
        json!({
            "sessionId": "auggie-tool-only",
            "created": "2026-07-04T20:00:00Z",
            "chatHistory": [
                {
                    "exchange": {
                        "request_id": "req-tool-only",
                        "request_nodes": [
                            {
                                "type": "tool_call",
                                "name": "read_file",
                                "args": {
                                    "path": "src/auggie_tool_only.rs"
                                }
                            }
                        ],
                        "response_nodes": [
                            {
                                "type": "tool_result",
                                "content": "AUGGIE_RAW_TOOL_OUTPUT_NEEDLE"
                            },
                            {
                                "type": "tool_result",
                                "text_node": {
                                    "content": "AUGGIE_RAW_TEXT_NODE_TOOL_OUTPUT_NEEDLE"
                                }
                            }
                        ]
                    },
                    "finishedAt": "2026-07-04T20:00:01Z"
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_auggie_history(
        &root,
        &mut store,
        AuggieImportOptions {
            source_path: Some(root.clone()),
            allow_partial_failures: true,
            imported_at: "2026-07-04T20:05:00Z".parse().unwrap(),
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("no real conversation message"));
    assert!(store.search_event_hits("read_file", 10).unwrap().is_empty());
    assert!(store
        .search_event_hits("AUGGIE_RAW_TOOL_OUTPUT_NEEDLE", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("AUGGIE_RAW_TEXT_NODE_TOOL_OUTPUT_NEEDLE", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_auggie_mixed_tool_nodes_do_not_store_raw_tool_output() {
    let temp = tempdir();
    let root = temp.path().join("auggie/sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("auggie-mixed-tool.json"),
        json!({
            "sessionId": "auggie-mixed-tool",
            "created": "2026-07-04T20:10:00Z",
            "chatHistory": [
                {
                    "exchange": {
                        "request_id": "req-mixed-tool",
                        "request_message": "Auggie mixed request oracle",
                        "response_nodes": [
                            {
                                "text_node": {
                                    "content": "Auggie mixed response oracle"
                                }
                            },
                            {
                                "type": "tool_result",
                                "content": "AUGGIE_MIXED_RAW_TOOL_OUTPUT_NEEDLE"
                            }
                        ]
                    },
                    "finishedAt": "2026-07-04T20:10:01Z"
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_auggie_history(
        &root,
        &mut store,
        AuggieImportOptions {
            source_path: Some(root.clone()),
            allow_partial_failures: true,
            imported_at: "2026-07-04T20:15:00Z".parse().unwrap(),
            ..AuggieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);
    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Auggie, "auggie-mixed-tool");
    let events = store.events_for_session(session_id).unwrap();
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("Auggie mixed request oracle"));
    assert!(rendered.contains("Auggie mixed response oracle"));
    assert!(rendered.contains("tool_node_count"));
    assert!(!rendered.contains("AUGGIE_MIXED_RAW_TOOL_OUTPUT_NEEDLE"));
    assert!(store
        .search_event_hits("AUGGIE_MIXED_RAW_TOOL_OUTPUT_NEEDLE", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn native_rovodev_non_array_message_history_rejects_no_real_message() {
    let temp = tempdir();
    let session_dir = temp.path().join("rovodev/sessions/rovodev-non-array");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("session_context.json"),
        json!({
            "session_id": "rovodev-non-array",
            "message_history": {
                "role": "user",
                "content": "rovodev non-array history should not import"
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_rovodev_history(
        temp.path().join("rovodev/sessions"),
        &mut store,
        RovoDevImportOptions {
            source_path: Some(temp.path().join("rovodev/sessions")),
            allow_partial_failures: true,
            imported_at: "2026-07-04T20:10:00Z".parse().unwrap(),
            ..RovoDevImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(summary.failures[0]
        .error
        .contains("missing message_history array"));
    assert!(store
        .search_event_hits("rovodev non-array history should not import", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_firebender_fixture_imports_project_root_db_and_reimports() {
    let temp = tempdir();
    let source_project = provider_history_fixture("firebender/v1");
    let project_root = temp.path().join("firebender-project");
    let fixture = project_root
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    fs::create_dir_all(fixture.parent().unwrap()).unwrap();
    fs::copy(
        source_project
            .join(".idea")
            .join("firebender")
            .join("chat_history.db"),
        &fixture,
    )
    .unwrap();
    {
        let conn = rusqlite::Connection::open(&fixture).unwrap();
        let messages_json: String = conn
            .query_row(
                "select messages_json from chat_sessions where id = 'firebender-fixture-session'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut messages: Vec<Value> = serde_json::from_str(&messages_json).unwrap();
        messages.push(json!({
            "id": "firebender-tool-result",
            "role": "tool",
            "tool_call_id": "call-firebender-1",
            "content": {
                "type": "text",
                "text": "FIREBENDER_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH"
            }
        }));
        conn.execute(
            "update chat_sessions set messages_json = ?1 where id = 'firebender-fixture-session'",
            [serde_json::to_string(&messages).unwrap()],
        )
        .unwrap();
    }
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let root_source = provider_source_for_path(CaptureProvider::Firebender, project_root.clone());
    assert_eq!(root_source.source_format, FIREBENDER_SQLITE_SOURCE_FORMAT);
    assert_eq!(root_source.status, ProviderSourceStatus::Available);
    let db_source = provider_source_for_path(CaptureProvider::Firebender, fixture.clone());
    assert_eq!(db_source.source_format, FIREBENDER_SQLITE_SOURCE_FORMAT);
    assert_eq!(db_source.status, ProviderSourceStatus::Available);

    let first = import_firebender_sqlite(
        &project_root,
        &mut store,
        FirebenderSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(project_root.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T20:10:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..FirebenderSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 4);
    let session_id = stored_provider_session_id(
        &store,
        CaptureProvider::Firebender,
        "firebender-fixture-session",
    );
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::User)));
    assert!(events
        .iter()
        .any(|event| event.role == Some(EventRole::Assistant)));
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    assert_event_with_role(&events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&events);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("firebender fixture oracle prompt"));
    assert!(rendered.contains("Firebender fixture oracle response"));
    assert!(!rendered.contains("FIREBENDER_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH"));
    assert!(store
        .search_event_hits("firebender fixture oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Firebender)));
    assert_search_misses(&store, "FIREBENDER_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH");

    let source = store
        .capture_source_by_external_session(
            CaptureProvider::Firebender,
            "firebender-fixture-session",
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        source.sync.metadata["source_metadata"]["storage"].as_str(),
        Some(".idea/firebender/chat_history.db")
    );

    let second = import_firebender_sqlite(
        &fixture,
        &mut store,
        FirebenderSqliteImportOptions {
            source_path: Some(fixture.clone()),
            allow_partial_failures: true,
            ..FirebenderSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 4);
}
#[test]
fn provider_sources_discovers_auggie_default_sessions() {
    let temp = tempdir();
    let fixture = provider_history_fixture("auggie/v0.32.0/sessions");
    let sessions = temp.path().join(".augment").join("sessions");
    copy_dir_all(&fixture, &sessions);

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Auggie);
    let source = sources
        .iter()
        .find(|source| source.source_format == AUGGIE_SESSION_JSON_SOURCE_FORMAT)
        .unwrap_or_else(|| panic!("missing Auggie source in {sources:#?}"));
    assert_eq!(source.provider, CaptureProvider::Auggie);
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert_eq!(source.path, sessions);
}

#[test]
fn native_lingma_fixture_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("lingma/v1/local.db");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let source = provider_source_for_path(CaptureProvider::Lingma, fixture.clone());
    assert_eq!(source.source_format, LINGMA_SQLITE_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_lingma_sqlite(
        &fixture,
        &mut store,
        LingmaSqliteImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(fixture.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T16:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            allow_partial_failures: true,
            ..LingmaSqliteImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 6);

    let alpha = stored_provider_session_id(&store, CaptureProvider::Lingma, "lingma-session-1");
    let events = store.events_for_session(alpha).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    assert_eq!(events[1].sync.fidelity, Fidelity::SummaryOnly);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("lingma oracle prompt update"));
    assert!(rendered.contains("src/lingma_fixture.rs"));
    assert!(rendered.contains("Lingma summary oracle answer"));
    assert!(rendered.contains("summary_only"));
    assert!(rendered.contains("assistant_content_caveat"));
    assert!(store
        .search_event_hits("Lingma summary oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Lingma)));
    assert!(store
        .search_event_hits("lingma oracle prompt update", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Lingma)));

    let error_session =
        stored_provider_session_id(&store, CaptureProvider::Lingma, "lingma-session-2");
    let error_events = store.events_for_session(error_session).unwrap();
    assert_eq!(error_events.len(), 2);
    assert_eq!(error_events[1].event_type, EventType::Notice);
    assert!(serde_json::to_string(&error_events)
        .unwrap()
        .contains("sanitized Lingma error"));

    let second = import_lingma_sqlite(
        &fixture,
        &mut store,
        LingmaSqliteImportOptions {
            allow_partial_failures: true,
            ..LingmaSqliteImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 6);
}

#[test]
fn native_lingma_import_reports_corrupt_sqlite() {
    let temp = tempdir();
    let db = temp.path().join("corrupt-lingma.db");
    fs::write(&db, b"not sqlite").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let err = import_lingma_sqlite(&db, &mut store, LingmaSqliteImportOptions::default())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("not a database") || err.contains("sqlite"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn native_lingma_normalizer_rejects_symlinked_sqlite() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = provider_history_fixture("lingma/v1/local.db");
    let link = temp.path().join("linked-lingma.db");
    symlink(&fixture, &link).unwrap();

    let err = normalize_lingma_sqlite(&link, &ProviderAdapterContext::default()).unwrap_err();
    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-lingma.db")
                && reason == "symlinked provider transcript files are rejected"
    ));
}

#[test]
fn native_factory_ai_droid_supports_new_session_format() {
    // Use project temp dir to avoid macOS /var symlink issues
    let temp_dir = std::env::current_dir().unwrap().join("target/test-temp");
    fs::create_dir_all(&temp_dir).unwrap();
    let temp = tempfile::Builder::new().tempdir_in(&temp_dir).unwrap();
    let root = temp.path().join("droid/sessions/project");
    fs::create_dir_all(&root).unwrap();

    // Test the new format: "id" instead of "sessionId", nested message.content/role
    let new_content = concat!(
        "{\"type\":\"session_start\",\"id\":\"droid-new\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\"}\n",
        "{\"type\":\"message\",\"id\":\"msg-1\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"droid new format prompt\"}]}}\n",
        "{\"type\":\"message\",\"id\":\"msg-2\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"droid_worker\"}]}}\n",
        "{\"type\":\"message\",\"id\":\"msg-3\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"message\":{\"role\":\"tool\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"is_error\":false,\"content\":\"result\"}]}}\n",
    );
    fs::write(root.join("droid-new-format.jsonl"), new_content).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_factory_ai_droid_sessions(
        &root,
        &mut store,
        FactoryAiDroidImportOptions {
            source_path: Some(root.clone()),
            allow_partial_failures: true,
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 4);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::FactoryAiDroid, "droid-new");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_event_type_count(&events, EventType::Message, 1);
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    assert_event_with_role(&events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&events);

    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("droid new format prompt"));
    assert!(rendered.contains("droid_worker"));
    assert!(!rendered.contains("DROID_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH"));
}

#[test]
fn native_factory_ai_droid_supports_legacy_session_format() {
    // Use project temp dir to avoid macOS /var symlink issues
    let temp_dir = std::env::current_dir().unwrap().join("target/test-temp");
    fs::create_dir_all(&temp_dir).unwrap();
    let temp = tempfile::Builder::new().tempdir_in(&temp_dir).unwrap();
    let root = temp.path().join("droid/sessions/project");
    fs::create_dir_all(&root).unwrap();

    // Test the legacy format: "sessionId" at root, "content" and "role" at message root
    let legacy_content = concat!(
        "{\"type\":\"session_start\",\"sessionId\":\"droid-legacy\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\"}\n",
        "{\"type\":\"message\",\"id\":\"msg-1\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"droid legacy prompt\"}]}\n",
        "{\"type\":\"message\",\"id\":\"msg-2\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"droid_worker\"}]}\n",
        "{\"type\":\"message\",\"id\":\"msg-3\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"role\":\"tool\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"legacy result\"}]}\n",
    );
    fs::write(root.join("droid-legacy.jsonl"), legacy_content).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_factory_ai_droid_sessions(
        &root,
        &mut store,
        FactoryAiDroidImportOptions {
            source_path: Some(root.clone()),
            allow_partial_failures: true,
            ..FactoryAiDroidImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 4);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::FactoryAiDroid, "droid-legacy");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 4);
    assert_event_type_count(&events, EventType::Message, 1);
    assert_event_type_count(&events, EventType::ToolCall, 1);
    assert_event_type_count(&events, EventType::ToolOutput, 1);
    assert_event_with_role(&events, EventType::ToolOutput, EventRole::Tool);
    assert_events_have_provider_citations(&events);

    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("droid legacy prompt"));
    assert!(rendered.contains("droid_worker"));
    assert!(!rendered.contains("DROID_RAW_TOOL_OUTPUT_SHOULD_NOT_SEARCH"));
}
