#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn native_provider_default_discovery_uses_importer_specific_file_predicates() {
    let temp = tempfile::tempdir().unwrap();

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
    let omp_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Pi && source.path == omp)
        .unwrap();
    assert_eq!(omp_source.status, ProviderSourceStatus::Empty);
    assert_eq!(omp_source.source_format, "pi_session_jsonl");
    std::fs::write(omp.join("--workspace--/session.jsonl"), "{}\n").unwrap();
    let omp_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Pi && source.path == omp)
        .unwrap();
    assert_eq!(omp_source.status, ProviderSourceStatus::Available);

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
    assert_source_status(
        temp.path(),
        CaptureProvider::OpenClaw,
        ProviderSourceStatus::Empty,
    );
    std::fs::write(openclaw.join("session.jsonl"), "{}\n").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::OpenClaw,
        ProviderSourceStatus::Available,
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
    assert_eq!(shelley_source.status, ProviderSourceStatus::Available);
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

    let roo = temp
        .path()
        .join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/roo-discovery");
    std::fs::create_dir_all(&roo).unwrap();
    std::fs::write(roo.join("history_item.json"), "{}").unwrap();
    assert_source_status(
        temp.path(),
        CaptureProvider::RooCode,
        ProviderSourceStatus::Available,
    );
}

#[test]
pub(crate) fn pi_discovery_uses_env_session_dir() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let custom = temp.path().join("pi-env-sessions");
    write_pi_discovery_session(&custom);
    let _session_dir = EnvGuard::set("PI_CODING_AGENT_SESSION_DIR", custom.as_os_str());
    let _agent_dir = EnvGuard::remove("PI_CODING_AGENT_DIR");

    let sources = discover_provider_sources(temp.path());
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::Pi && source.path == custom)
        .unwrap();

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

#[test]
pub(crate) fn pi_discovery_uses_global_and_project_settings_session_dirs() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let _session_dir = EnvGuard::remove("PI_CODING_AGENT_SESSION_DIR");
    let _agent_dir = EnvGuard::remove("PI_CODING_AGENT_DIR");

    let global = temp.path().join("global-pi-sessions");
    write_pi_discovery_session(&global);
    std::fs::create_dir_all(temp.path().join(".pi/agent")).unwrap();
    std::fs::write(
        temp.path().join(".pi/agent/settings.json"),
        r#"{"sessionDir":"~/global-pi-sessions"}"#,
    )
    .unwrap();

    let project_sessions = project.path().join(".pi/custom-sessions");
    write_pi_discovery_session(&project_sessions);
    std::fs::write(
        project.path().join(".pi/settings.json"),
        r#"{"sessionDir":"custom-sessions"}"#,
    )
    .unwrap();

    let spec = provider_source_spec(CaptureProvider::Pi).unwrap();
    let project_settings_dirs = [
        project.path().join("subdir/.pi"),
        project.path().join(".pi"),
    ];
    let sources = discover_pi_custom_session_sources_with_project_settings(
        temp.path(),
        spec,
        &project_settings_dirs,
    );
    for path in [&global, &project_sessions] {
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::Pi && source.path == *path)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
    }
}

pub(crate) fn write_pi_discovery_session(root: &Path) {
    let project = root.join("--workspace--");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("2026-07-03T12-00-00-000Z_pi-discovery.jsonl"),
        "{}\n",
    )
    .unwrap();
}
