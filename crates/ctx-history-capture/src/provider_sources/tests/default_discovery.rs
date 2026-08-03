use ctx_history_core::CaptureProvider;

use super::super::{
    discover_provider_sources, discover_provider_sources_for_provider,
    discover_provider_sources_for_provider_report, discover_provider_sources_report,
    provider_source_for_path, DiscoveryIssueKind, ProviderImportSupport, ProviderSourceKind,
    ProviderSourceStatus,
};
use super::support::{assert_source_status, tempdir, EnvGuard, ENV_LOCK};

#[cfg(target_os = "windows")]
#[test]
fn windows_candidate_list_accepts_ordinary_absolute_codex_file() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _codex_home = EnvGuard::remove("CODEX_HOME");
    let path = temp.path().join(".codex/history.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{}\n").unwrap();

    assert!(path.is_absolute());
    let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Codex)
        .into_iter()
        .find(|source| source.path == path)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.unsupported_reason, None);
}

#[test]
fn gemini_default_source_is_empty_until_chat_transcripts_exist() {
    let temp = tempdir();
    let gemini = temp.path().join(".gemini");
    std::fs::create_dir_all(&gemini).unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Gemini)
        .unwrap();
    assert!(source.exists);
    assert_eq!(source.status, ProviderSourceStatus::Empty);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert!(source
        .unsupported_reason
        .unwrap()
        .contains("no Gemini CLI chat JSONL transcripts"));

    let chats = gemini.join("tmp/project/chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(chats.join("session.jsonl"), "{}\n").unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Gemini)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.unsupported_reason, None);
}

#[test]
fn tabnine_default_source_is_empty_until_chat_transcripts_exist() {
    let temp = tempdir();
    let tabnine = temp.path().join(".tabnine/agent");
    std::fs::create_dir_all(&tabnine).unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Tabnine)
        .unwrap();
    assert!(source.exists);
    assert_eq!(source.status, ProviderSourceStatus::Empty);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
    assert!(source
        .unsupported_reason
        .unwrap()
        .contains("no Tabnine CLI chat JSONL transcripts"));

    let chats = tabnine.join("tmp/project/chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(
        chats.join("session-2026-07-05T12-00-00000000.jsonl"),
        "{}\n",
    )
    .unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Tabnine)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.unsupported_reason, None);
}

#[test]
fn codebuddy_default_source_accepts_cli_project_jsonl() {
    let temp = tempdir();
    let codebuddy = temp.path().join(".codebuddy");
    std::fs::create_dir_all(&codebuddy).unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::CodeBuddy)
        .unwrap();
    assert!(source.exists);
    assert_eq!(source.status, ProviderSourceStatus::Empty);
    assert_eq!(source.import_support, ProviderImportSupport::Native);

    let project = codebuddy.join("projects/sanitized-workspace");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("codebuddy-cli-native.jsonl"),
        r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}],"sessionId":"codebuddy-cli-native"}"#,
    )
    .unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::CodeBuddy)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.unsupported_reason, None);
}

#[test]
fn codebuddy_cli_projects_probe_precedes_unrelated_root_entries() {
    let temp = tempdir();
    let codebuddy = temp.path().join(".codebuddy");
    let project = codebuddy.join("projects/sanitized-workspace");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("codebuddy-cli-native.jsonl"),
        r#"{"type":"message","role":"user","content":"hello"}"#,
    )
    .unwrap();
    let unrelated = codebuddy.join("unrelated");
    std::fs::create_dir_all(&unrelated).unwrap();
    for index in 0..10_001 {
        std::fs::write(unrelated.join(format!("entry-{index:05}.txt")), b"").unwrap();
    }

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::CodeBuddy)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.unsupported_reason, None);
}

#[test]
fn junie_default_source_accepts_unindexed_session_sibling() {
    let temp = tempdir();
    let sessions = temp.path().join(".junie/sessions");
    let unindexed = sessions.join("session-260709-212620-18se");
    std::fs::create_dir_all(&unindexed).unwrap();
    std::fs::write(
        sessions.join("index.jsonl"),
        r#"{"sessionId":"session-stale-without-events"}"#,
    )
    .unwrap();
    std::fs::write(
        unindexed.join("events.jsonl"),
        r#"{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":"AgentFailureEvent","message":"failure oracle"}}}"#,
    )
    .unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Junie)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.unsupported_reason, None);
}

#[test]
fn junie_default_source_stops_at_index_entry_budget() {
    let temp = tempdir();
    let sessions = temp.path().join(".junie/sessions");
    let target = sessions.join("session-after-budget");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("events.jsonl"), "{}\n").unwrap();
    let mut index = "{}\n".repeat(10_000);
    index.push_str(r#"{"sessionId":"session-after-budget"}"#);
    index.push('\n');
    std::fs::write(sessions.join("index.jsonl"), index).unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Junie)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Unknown);
    assert!(source.unsupported_reason.unwrap().contains("scan budget"));
}

#[cfg(unix)]
#[test]
fn junie_default_source_does_not_follow_symlinked_index() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let sessions = temp.path().join(".junie/sessions");
    let target = sessions.join("session-from-linked-index");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("events.jsonl"), "{}\n").unwrap();
    let outside_index = temp.path().join("outside-index.jsonl");
    std::fs::write(
        &outside_index,
        r#"{"sessionId":"session-from-linked-index"}"#,
    )
    .unwrap();
    symlink(outside_index, sessions.join("index.jsonl")).unwrap();

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Junie)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Empty);
}

#[test]
fn codex_selected_source_is_empty_until_jsonl_sessions_exist() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let codex_home = temp.path().join(".codex");
    let sessions = codex_home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let _codex_home = EnvGuard::set("CODEX_HOME", &codex_home);

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| {
            source.provider == CaptureProvider::Codex
                && source.source_format == "codex_session_jsonl_tree"
                && source.path == sessions
        })
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Empty);

    std::fs::write(sessions.join("session.jsonl"), "{}\n").unwrap();
    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| {
            source.provider == CaptureProvider::Codex
                && source.source_format == "codex_session_jsonl_tree"
                && source.path == sessions
        })
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
}

#[test]
fn native_provider_default_discovery_uses_importer_specific_file_predicates() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _xdg_config_home = EnvGuard::set("XDG_CONFIG_HOME", temp.path().join(".config"));

    let pi = temp.path().join(".pi/agent/sessions");
    std::fs::create_dir_all(pi.join("--workspace--")).unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::Pi,
        ProviderSourceStatus::Empty,
    );
    std::fs::write(pi.join("--workspace--/session.jsonl"), "{}\n").unwrap();
    let pi_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Pi)
        .unwrap();
    assert_eq!(pi_source.status, ProviderSourceStatus::Available);
    assert_eq!(pi_source.path, temp.path().join(".pi/agent/sessions"));

    let omp = temp.path().join(".omp/agent/sessions");
    std::fs::create_dir_all(omp.join("--workspace--")).unwrap();
    assert!(!discover_provider_sources(temp.path())
        .iter()
        .any(|source| source.provider == CaptureProvider::Pi && source.path == omp));
    std::fs::write(omp.join("--workspace--/session.jsonl"), "{}\n").unwrap();
    assert!(!discover_provider_sources(temp.path())
        .iter()
        .any(|source| source.provider == CaptureProvider::Pi && source.path == omp));

    let antigravity = temp.path().join(".gemini/antigravity-cli/brain");
    std::fs::create_dir_all(antigravity.join("session/.system_generated/logs")).unwrap();
    std::fs::write(
        antigravity.join("session/.system_generated/logs/not-a-transcript.jsonl"),
        "{}\n",
    )
    .unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::Antigravity,
        ProviderSourceStatus::Empty,
    );
    std::fs::write(
        antigravity.join("session/.system_generated/logs/transcript_full.jsonl"),
        "{}\n",
    )
    .unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::Antigravity,
        ProviderSourceStatus::Empty,
    );
    std::fs::write(
        antigravity.join("session/.system_generated/logs/transcript.jsonl"),
        "{}\n",
    )
    .unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::Antigravity,
        ProviderSourceStatus::Available,
    );

    let antigravity_ide = temp.path().join(".gemini/antigravity-ide/brain");
    std::fs::create_dir_all(antigravity_ide.join("ide-session/.system_generated/logs")).unwrap();
    std::fs::write(
        antigravity_ide.join("ide-session/.system_generated/logs/transcript.jsonl"),
        "{}\n",
    )
    .unwrap();
    let ide_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| {
            source.provider == CaptureProvider::Antigravity && source.path == antigravity_ide
        })
        .unwrap();
    assert_eq!(ide_source.status, ProviderSourceStatus::Available);
    assert_eq!(
        ide_source.source_format,
        "antigravity_cli_transcript_jsonl_tree"
    );

    let cursor = temp.path().join(".cursor/projects");
    std::fs::create_dir_all(cursor.join("project")).unwrap();
    std::fs::write(cursor.join("project/session.jsonl"), "{}\n").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::Cursor,
        ProviderSourceStatus::Empty,
    );
    std::fs::create_dir_all(cursor.join("project/agent-transcripts/session")).unwrap();
    std::fs::write(
        cursor.join("project/agent-transcripts/session/events.jsonl"),
        "{}\n",
    )
    .unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::Cursor,
        ProviderSourceStatus::Empty,
    );
    std::fs::write(
        cursor.join("project/agent-transcripts/session/session.jsonl"),
        "{}\n",
    )
    .unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::Cursor,
        ProviderSourceStatus::Available,
    );

    let copilot = temp.path().join(".copilot/session-state");
    std::fs::create_dir_all(copilot.join("session")).unwrap();
    std::fs::write(copilot.join("session/session.jsonl"), "{}\n").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::CopilotCli,
        ProviderSourceStatus::Empty,
    );
    std::fs::write(copilot.join("session/events.jsonl"), "{}\n").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::CopilotCli,
        ProviderSourceStatus::Available,
    );

    let qwen = temp.path().join(".qwen/projects/project/chats");
    std::fs::create_dir_all(&qwen).unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::QwenCode,
        ProviderSourceStatus::Empty,
    );
    std::fs::write(qwen.join("session.jsonl"), "{}\n").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::QwenCode,
        ProviderSourceStatus::Available,
    );

    let rovodev = temp.path().join(".rovodev/sessions/rovo-session");
    std::fs::create_dir_all(&rovodev).unwrap();
    std::fs::write(rovodev.join("metadata.json"), "{}\n").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::RovoDev,
        ProviderSourceStatus::Empty,
    );
    std::fs::write(rovodev.join("session_context.json"), "{}\n").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::RovoDev,
        ProviderSourceStatus::Available,
    );

    let kimi = temp
        .path()
        .join(".kimi-code/sessions/wd_project_abc123/kimi-session/agents/main");
    std::fs::create_dir_all(&kimi).unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::KimiCodeCli,
        ProviderSourceStatus::Empty,
    );
    std::fs::write(kimi.join("wire.jsonl"), "{}\n").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::KimiCodeCli,
        ProviderSourceStatus::Available,
    );

    let codebuddy = temp.path().join(".codebuddy");
    std::fs::create_dir_all(&codebuddy).unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::CodeBuddy,
        ProviderSourceStatus::Empty,
    );
    let codebuddy_session = codebuddy.join(
        "Data/VSCode/default/history/11112222333344445555666677778888/session-alpha/messages",
    );
    std::fs::create_dir_all(&codebuddy_session).unwrap();
    std::fs::write(
        codebuddy_session.parent().unwrap().join("index.json"),
        r#"{"messages":[{"id":"msg-1","role":"user"}]}"#,
    )
    .unwrap();
    std::fs::write(
        codebuddy_session.join("msg-1.json"),
        r#"{"message":"hello"}"#,
    )
    .unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::CodeBuddy,
        ProviderSourceStatus::Available,
    );

    let openclaw = temp.path().join(".openclaw/agents/personal/sessions");
    std::fs::create_dir_all(&openclaw).unwrap();
    std::fs::write(openclaw.join("session.jsonl"), "{}\n").unwrap();
    assert!(
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::OpenClaw).is_empty()
    );

    let hermes = temp.path().join(".hermes");
    std::fs::create_dir_all(&hermes).unwrap();
    std::fs::write(hermes.join("state.db"), b"sqlite fixture marker").unwrap();
    let hermes_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Hermes)
        .unwrap();
    assert_eq!(hermes_source.status, ProviderSourceStatus::Available);
    assert_eq!(hermes_source.import_support, ProviderImportSupport::Native);

    let astrbot = temp.path().join(".astrbot/data");
    std::fs::create_dir_all(&astrbot).unwrap();
    std::fs::write(astrbot.join("data_v4.db"), b"sqlite fixture marker").unwrap();
    let astrbot_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::AstrBot)
        .unwrap();
    assert_eq!(astrbot_source.status, ProviderSourceStatus::Available);
    assert_eq!(astrbot_source.import_support, ProviderImportSupport::Native);
    assert!(astrbot_source.import_support.is_importable());
    assert!(astrbot_source.import_support.is_auto_importable());

    let shelley = temp.path().join(".config/shelley");
    std::fs::create_dir_all(&shelley).unwrap();
    std::fs::write(shelley.join("shelley.db"), b"sqlite fixture marker").unwrap();
    let shelley_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Shelley)
        .unwrap();
    assert_eq!(
        shelley_source.path,
        std::env::current_dir().unwrap().join("shelley.db")
    );
    assert_ne!(shelley_source.path, shelley.join("shelley.db"));
    assert_eq!(shelley_source.status, ProviderSourceStatus::Missing);
    assert_eq!(shelley_source.import_support, ProviderImportSupport::Native);
    assert!(shelley_source.import_support.is_auto_importable());

    let continue_sessions = temp.path().join(".continue/sessions");
    std::fs::create_dir_all(&continue_sessions).unwrap();
    std::fs::write(continue_sessions.join("sessions.json"), "[]\n").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::Continue,
        ProviderSourceStatus::Empty,
    );
    std::fs::write(continue_sessions.join("session.json"), "{}\n").unwrap();
    let continue_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Continue)
        .unwrap();
    assert_eq!(continue_source.status, ProviderSourceStatus::Available);
    assert_eq!(continue_source.source_format, "continue_cli_sessions_json");
    assert_eq!(
        continue_source.import_support,
        ProviderImportSupport::Native
    );
    assert!(continue_source.import_support.is_auto_importable());

    let openhands = temp.path().join(".openhands/local-user");
    std::fs::create_dir_all(&openhands).unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::OpenHands,
        ProviderSourceStatus::Empty,
    );
    let openhands_events = openhands.join("v1_conversations/12345678123456781234567812345678");
    std::fs::create_dir_all(&openhands_events).unwrap();
    std::fs::write(
        openhands_events.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"),
        "{}\n",
    )
    .unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::OpenHands,
        ProviderSourceStatus::Available,
    );

    let cline = temp.path().join(".cline/data/tasks/cline-discovery");
    std::fs::create_dir_all(&cline).unwrap();
    std::fs::write(cline.join("api_conversation_history.json"), "[]").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::Cline,
        ProviderSourceStatus::Available,
    );

    let roo_root = temp.path().join(".vscode-mock/global-storage");
    let roo = roo_root.join("tasks/roo-discovery");
    std::fs::create_dir_all(&roo).unwrap();
    std::fs::write(roo.join("history_item.json"), "{}").unwrap();
    let roo_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::RooCode && source.path == roo_root)
        .unwrap();
    assert_eq!(roo_source.status, ProviderSourceStatus::Available);
}

#[test]
fn legacy_vector_apis_are_exact_report_source_projections() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _codex_home = EnvGuard::remove("CODEX_HOME");
    let all_report = discover_provider_sources_report(temp.path());
    assert_eq!(discover_provider_sources(temp.path()), all_report.sources);

    let provider_report =
        discover_provider_sources_for_provider_report(temp.path(), CaptureProvider::Codex);
    assert_eq!(
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::Codex),
        provider_report.sources
    );
    assert!(all_report.issues.iter().any(|issue| {
        issue.provider == CaptureProvider::Firebender
            && issue.kind == DiscoveryIssueKind::InsufficientOfficialEvidence
    }));
    assert!(all_report.sources.iter().any(|source| {
        source.provider == CaptureProvider::FactoryAiDroid
            && source.path == temp.path().join(".factory/sessions")
            && source.status == ProviderSourceStatus::Missing
    }));
    assert!(provider_report.issues.is_empty());
}

#[test]
fn exact_current_incompatible_explicit_paths_are_detection_only_unsupported() {
    let temp = tempdir();
    let cases = [
        (
            CaptureProvider::Codex,
            temp.path().join(".codex/sessions/session.jsonl.zst"),
            "compressed .jsonl.zst",
        ),
        (
            CaptureProvider::OpenClaw,
            temp.path()
                .join(".openclaw/agents/main/agent/openclaw-agent.sqlite"),
            "openclaw-agent.sqlite",
        ),
        (
            CaptureProvider::OpenHands,
            temp.path()
                .join(".openhands/conversations/conversation/events/event-1.json"),
            "events/event-*.json",
        ),
        (
            CaptureProvider::Mux,
            temp.path().join(".mux/sessions/session/chat-archive.jsonl"),
            "chat-archive.jsonl",
        ),
        (
            CaptureProvider::Cline,
            temp.path().join(".cline/data/db/sessions.db"),
            "current Cline SDK",
        ),
    ];
    for (_, path, _) in &cases {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"current format marker").unwrap();
    }

    let kiro = temp.path().join(".kiro/sessions");
    std::fs::create_dir_all(kiro.join("cli")).unwrap();
    std::fs::write(kiro.join("cli/session.json"), b"{}").unwrap();
    std::fs::write(kiro.join("cli/session.jsonl"), b"{}\n").unwrap();
    let qoder = temp.path().join(".qoder/projects/bucket/session.jsonl");
    std::fs::create_dir_all(qoder.parent().unwrap()).unwrap();
    std::fs::write(&qoder, b"{}\n").unwrap();

    for (provider, path, reason_fragment) in cases.into_iter().chain([
        (CaptureProvider::KiroCli, kiro, "ACP/v3"),
        (CaptureProvider::Qoder, qoder, "direct SDK JSONL"),
    ]) {
        let source = provider_source_for_path(provider, path);
        assert_eq!(source.status, ProviderSourceStatus::Unsupported);
        assert_eq!(source.import_support, ProviderImportSupport::Unsupported);
        assert_eq!(source.source_kind, ProviderSourceKind::DetectionOnly);
        assert_eq!(source.source_format, "unsupported");
        assert!(source
            .unsupported_reason
            .is_some_and(|reason| reason.contains(reason_fragment)));
    }
}

#[test]
fn explicit_unsupported_detection_preserves_supported_mixed_trees() {
    let temp = tempdir();
    let cases = [
        (
            CaptureProvider::Qoder,
            temp.path().join("qoder/projects"),
            "bucket/transcript/legacy.jsonl",
            "bucket/current.jsonl",
        ),
        (
            CaptureProvider::OpenClaw,
            temp.path().join("openclaw"),
            "agents/main/sessions/legacy.jsonl",
            "agents/main/agent/openclaw-agent.sqlite",
        ),
        (
            CaptureProvider::OpenHands,
            temp.path().join("openhands"),
            "v1_conversations/legacy/event.json",
            "conversations/current/events/event-1.json",
        ),
        (
            CaptureProvider::Mux,
            temp.path().join("mux/sessions"),
            "session/chat.jsonl",
            "session/chat-archive.jsonl",
        ),
        (
            CaptureProvider::Cline,
            temp.path().join("cline/data"),
            "tasks/legacy/api_conversation_history.json",
            "db/sessions.db",
        ),
    ];
    for (provider, root, supported, unsupported) in cases {
        for relative in [supported, unsupported] {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"{}\n").unwrap();
        }
        let source = provider_source_for_path(provider, root);
        assert_eq!(
            source.status,
            ProviderSourceStatus::Available,
            "{provider:?}"
        );
        assert!(source.import_support.is_importable(), "{provider:?}");
    }
}

#[test]
fn explicit_current_detection_requires_exact_kiro_and_qoder_shapes() {
    let temp = tempdir();
    let unrelated_sessions = temp.path().join("unrelated/sessions");
    std::fs::create_dir_all(&unrelated_sessions).unwrap();
    std::fs::write(unrelated_sessions.join("notes.jsonl"), b"{}\n").unwrap();
    let kiro = provider_source_for_path(CaptureProvider::KiroCli, unrelated_sessions);
    assert_eq!(kiro.status, ProviderSourceStatus::Available);

    let deep_qoder = temp
        .path()
        .join("qoder/projects/bucket/deeper/current.jsonl");
    std::fs::create_dir_all(deep_qoder.parent().unwrap()).unwrap();
    std::fs::write(&deep_qoder, b"{}\n").unwrap();
    let qoder = provider_source_for_path(CaptureProvider::Qoder, deep_qoder);
    assert_eq!(qoder.status, ProviderSourceStatus::Available);
}

#[test]
fn supported_explicit_shapes_and_missing_textual_paths_keep_pinned_mapping() {
    let temp = tempdir();
    let supported = [
        (CaptureProvider::KiroCli, temp.path().join("data.sqlite3")),
        (
            CaptureProvider::Qoder,
            temp.path().join("projects/bucket/transcript/session.jsonl"),
        ),
        (
            CaptureProvider::OpenClaw,
            temp.path().join("legacy-openclaw-sessions"),
        ),
        (
            CaptureProvider::OpenHands,
            temp.path().join("v1_conversations/conversation/event.json"),
        ),
        (CaptureProvider::Mux, temp.path().join("session/chat.jsonl")),
        (
            CaptureProvider::Cline,
            temp.path().join("legacy-cline-root"),
        ),
    ];
    for (_, path) in &supported {
        if path.extension().is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"supported marker").unwrap();
        } else {
            std::fs::create_dir_all(path).unwrap();
        }
    }
    for (provider, path) in supported {
        let source = provider_source_for_path(provider, path);
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert!(source.import_support.is_importable());
    }

    let missing_archive = temp.path().join("missing/chat-archive.jsonl");
    let source = provider_source_for_path(CaptureProvider::Mux, missing_archive.clone());
    assert_eq!(source.path, missing_archive);
    assert_eq!(source.status, ProviderSourceStatus::Missing);
    assert_eq!(source.source_format, "mux_session_jsonl");
}

#[test]
fn explicit_codex_files_use_bounded_schema_admission_not_filenames() {
    let temp = tempdir();
    let prompt_named_rollout = temp.path().join("rollout-renamed.jsonl");
    std::fs::write(
        &prompt_named_rollout,
        r#"{"session_id":"prompt-session","ts":1782259200,"text":"prompt"}"#,
    )
    .unwrap();
    let rollout_named_history = temp.path().join("history.jsonl");
    std::fs::write(
        &rollout_named_history,
        r#"{"timestamp":"2026-06-24T10:00:00Z","type":"session_meta","payload":{"id":"rollout-session"}}"#,
    )
    .unwrap();

    let prompt = provider_source_for_path(CaptureProvider::Codex, prompt_named_rollout);
    assert_eq!(prompt.status, ProviderSourceStatus::Available);
    assert_eq!(prompt.source_format, "codex_history_jsonl");

    let rollout = provider_source_for_path(CaptureProvider::Codex, rollout_named_history);
    assert_eq!(rollout.status, ProviderSourceStatus::Available);
    assert_eq!(rollout.source_format, "codex_session_jsonl");
}

#[test]
fn explicit_codex_admission_rejects_ambiguous_files_and_keeps_trees_typed() {
    let temp = tempdir();
    let ambiguous = temp.path().join("ambiguous.jsonl");
    std::fs::write(
        &ambiguous,
        r#"{"session_id":"both","ts":1782259200,"text":"both","timestamp":"2026-06-24T10:00:00Z","type":"session_meta","payload":{}}"#,
    )
    .unwrap();
    let ambiguous = provider_source_for_path(CaptureProvider::Codex, ambiguous);
    assert_eq!(ambiguous.status, ProviderSourceStatus::Unsupported);
    assert_eq!(ambiguous.source_format, "unsupported");
    assert!(ambiguous
        .unsupported_reason
        .is_some_and(|reason| reason.contains("schema is ambiguous")));

    let tree = temp.path().join("renamed-tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(
        tree.join("session.jsonl"),
        r#"{"timestamp":"2026-06-24T10:00:00Z","type":"session_meta","payload":{"id":"tree-session"}}"#,
    )
    .unwrap();
    let tree = provider_source_for_path(CaptureProvider::Codex, tree);
    assert_eq!(tree.status, ProviderSourceStatus::Available);
    assert_eq!(tree.source_format, "codex_session_jsonl_tree");
}
