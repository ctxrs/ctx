use crate::tests::support::fixtures::jsonl::{
    write_claude_smoke_fixture, write_copilot_smoke_fixture, write_droid_smoke_fixture,
    write_gemini_smoke_fixture, write_nanoclaw_smoke_project,
};
use crate::tests::support::fixtures::sqlite::{
    write_hermes_smoke_db, write_opencode_message_part_db, write_opencode_smoke_db,
    write_shelley_smoke_db,
};
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::source_snapshot::{
    assert_provider_source_unchanged, assert_sqlite_source_file_unchanged,
};
use crate::{
    import_antigravity_cli_history, import_auggie_history,
    import_claude_projects_jsonl_tree, import_cline_task_json_history, import_codebuddy_history,
    import_codex_history_jsonl, import_codex_session_tree, import_continue_cli_sessions,
    import_copilot_cli_session_events, import_crush_sqlite, import_cursor_native_history,
    import_deepagents_sqlite, import_factory_ai_droid_sessions, import_firebender_sqlite,
    import_forgecode_sqlite, import_gemini_cli_history, import_goose_sessions_sqlite,
    import_hermes_sqlite, import_junie_history, import_kilo_sqlite, import_kimi_code_cli_history,
    import_kiro_sqlite, import_lingma_sqlite, import_mimocode_sqlite, import_mistral_vibe_history,
    import_mux_history, import_nanoclaw_project, import_openclaw_history, import_opencode_sqlite,
    import_pi_session_jsonl, import_qoder_history, import_qwen_code_history,
    import_roo_task_json_history, import_rovodev_history, import_shelley_sqlite,
    import_tabnine_cli_history, import_trae_history, import_warp_sqlite,
    import_windsurf_cascade_hook_transcripts, import_zed_threads_sqlite,
    AntigravityCliImportOptions, AuggieImportOptions,
    ClaudeProjectsImportOptions, ClineTaskJsonImportOptions, CodeBuddyImportOptions,
    CodexHistoryImportOptions, CodexSessionImportOptions, ContinueCliImportOptions,
    CopilotCliImportOptions, CrushSqliteImportOptions, CursorNativeImportOptions,
    DeepAgentsSqliteImportOptions, FactoryAiDroidImportOptions, FirebenderSqliteImportOptions,
    ForgeCodeSqliteImportOptions, GeminiCliImportOptions, GooseSessionsSqliteImportOptions,
    HermesSqliteImportOptions, JunieImportOptions, KiloSqliteImportOptions,
    KimiCodeCliImportOptions, KiroSqliteImportOptions, LingmaSqliteImportOptions,
    MiMoCodeSqliteImportOptions, MistralVibeImportOptions, MuxImportOptions, NanoClawImportOptions,
    OpenClawImportOptions, OpenCodeSqliteImportOptions, PiSessionImportOptions,
    ProviderImportSummary, QoderImportOptions, QwenCodeImportOptions, RooTaskJsonImportOptions,
    RovoDevImportOptions, ShelleySqliteImportOptions, TabnineCliImportOptions, TraeImportOptions,
    WarpSqliteImportOptions, WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions,
};
use ctx_history_store::Store;
use serde_json::json;
use std::fs;
use std::path::Path;

fn assert_sqlite_clean_import_preserves_file(
    label: &str,
    source_file: &Path,
    run_import: impl FnOnce(&mut Store) -> ProviderImportSummary,
) {
    let summary = assert_sqlite_source_file_unchanged(source_file, run_import);
    assert_eq!(summary.failed, 0, "{label}: {:?}", summary.failures);
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
                ..PiSessionImportOptions::default()
            },
        )
        .unwrap()
    });

    let claude_root = write_claude_smoke_fixture(&temp);
    let claude = claude_root.join("-workspace/claude-native-parent.jsonl");
    assert_native_source_clean_import_preserves_source("Claude", &claude, |store| {
        import_claude_projects_jsonl_tree(
            &claude,
            store,
            ClaudeProjectsImportOptions {
                source_path: Some(claude_root.clone()),
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
fn native_sqlite_imports_do_not_mutate_provider_databases() {
    let temp = tempdir();

    let opencode = write_opencode_smoke_db(&temp, false);
    assert_sqlite_clean_import_preserves_file("OpenCode", &opencode, |store| {
        import_opencode_sqlite(
            &opencode,
            store,
            OpenCodeSqliteImportOptions {
                ..OpenCodeSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let mimocode = write_opencode_message_part_db(
        &temp,
        "mimocode-read-only.db",
        "mimocode-read-only-root",
        "mimocode read-only oracle",
    );
    assert_sqlite_clean_import_preserves_file("MiMo Code", &mimocode, |store| {
        import_mimocode_sqlite(
            &mimocode,
            store,
            MiMoCodeSqliteImportOptions {
                ..MiMoCodeSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let kilo = provider_history_fixture("kilo/kilo.db");
    assert_sqlite_clean_import_preserves_file("Kilo", &kilo, |store| {
        import_kilo_sqlite(
            &kilo,
            store,
            KiloSqliteImportOptions {
                ..KiloSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let kiro = provider_history_fixture("kiro-cli/v2/data.sqlite3");
    assert_sqlite_clean_import_preserves_file("Kiro CLI", &kiro, |store| {
        import_kiro_sqlite(
            &kiro,
            store,
            KiroSqliteImportOptions {
                ..KiroSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let crush = provider_history_fixture("crush/v1/crush.db");
    assert_sqlite_clean_import_preserves_file("Crush", &crush, |store| {
        import_crush_sqlite(
            &crush,
            store,
            CrushSqliteImportOptions {
                ..CrushSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let goose = provider_history_fixture("goose/v14/sessions.db");
    assert_sqlite_clean_import_preserves_file("Goose", &goose, |store| {
        import_goose_sessions_sqlite(
            &goose,
            store,
            GooseSessionsSqliteImportOptions {
                ..GooseSessionsSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let lingma = provider_history_fixture("lingma/v1/local.db");
    assert_sqlite_clean_import_preserves_file("Lingma", &lingma, |store| {
        import_lingma_sqlite(
            &lingma,
            store,
            LingmaSqliteImportOptions {
                ..LingmaSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let warp = provider_history_fixture("warp/v1/warp.sqlite");
    assert_sqlite_clean_import_preserves_file("Warp", &warp, |store| {
        import_warp_sqlite(
            &warp,
            store,
            WarpSqliteImportOptions {
                ..WarpSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let zed = provider_history_fixture("zed/v1/threads.db");
    assert_sqlite_clean_import_preserves_file("Zed", &zed, |store| {
        import_zed_threads_sqlite(
            &zed,
            store,
            ZedThreadsSqliteImportOptions {
                ..ZedThreadsSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let hermes = write_hermes_smoke_db(&temp);
    assert_sqlite_clean_import_preserves_file("Hermes", &hermes, |store| {
        import_hermes_sqlite(
            &hermes,
            store,
            HermesSqliteImportOptions {
                ..HermesSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let shelley = write_shelley_smoke_db(&temp);
    assert_sqlite_clean_import_preserves_file("Shelley", &shelley, |store| {
        import_shelley_sqlite(
            &shelley,
            store,
            ShelleySqliteImportOptions {
                ..ShelleySqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let firebender_root = provider_history_fixture("firebender/v1");
    let firebender_db = firebender_root
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    assert_sqlite_clean_import_preserves_file("Firebender", &firebender_db, |store| {
        import_firebender_sqlite(
            &firebender_root,
            store,
            FirebenderSqliteImportOptions {
                ..FirebenderSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let forgecode = provider_history_fixture("forgecode/v1/forge.db");
    assert_sqlite_clean_import_preserves_file("ForgeCode", &forgecode, |store| {
        import_forgecode_sqlite(
            &forgecode,
            store,
            ForgeCodeSqliteImportOptions {
                ..ForgeCodeSqliteImportOptions::default()
            },
        )
        .unwrap()
    });

    let deepagents = provider_history_fixture("deepagents/v1/sessions.db");
    assert_sqlite_clean_import_preserves_file("DeepAgents", &deepagents, |store| {
        import_deepagents_sqlite(
            &deepagents,
            store,
            DeepAgentsSqliteImportOptions {
                ..DeepAgentsSqliteImportOptions::default()
            },
        )
        .unwrap()
    });
}
