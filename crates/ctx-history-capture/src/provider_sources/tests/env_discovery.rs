use ctx_history_core::CaptureProvider;
use rusqlite::Connection;

use super::super::{
    discover_provider_sources, discover_provider_sources_for_provider, provider_source_spec,
    ProviderImportSupport, ProviderSourceStatus,
};
use super::super::{
    discovery::discover_pi_custom_session_sources_with_project_settings,
    probes::{has_trae_state_vscdb_chat_history, BoundedProbe},
    specs::TRAE_STATE_VSCDB_SOURCE_FORMAT,
};
use super::support::{
    shared_provider_history_fixture, tempdir, write_junie_discovery_session,
    write_kimi_discovery_wire, write_lingma_discovery_db, write_mistral_vibe_discovery_session,
    write_mux_discovery_session, write_pi_discovery_session, write_qwen_discovery_chat,
    write_task_json_discovery_task, write_trae_discovery_db, write_trae_non_chat_state_db,
    CwdGuard, EnvGuard, ENV_LOCK,
};

#[test]
fn continue_discovery_uses_global_dir_env_sessions_subdir() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let continue_home = temp.path().join("continue-home");
    let sessions = continue_home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("session.json"), "{}\n").unwrap();
    let _global_dir = EnvGuard::set("CONTINUE_GLOBAL_DIR", continue_home.as_os_str());

    let sources = discover_provider_sources(temp.path());
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::Continue && source.path == sessions)
        .unwrap();

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "continue_cli_sessions_json");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

#[test]
fn kilo_discovery_uses_xdg_kilo_db_env_override_and_channel_dbs() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _kilo_db = EnvGuard::remove("KILO_DB");
    let _xdg_data = EnvGuard::remove("XDG_DATA_HOME");
    let _config_dir = EnvGuard::remove("KILO_CONFIG_DIR");
    let _disable_channel = EnvGuard::remove("KILO_DISABLE_CHANNEL_DB");

    let data_dir = temp.path().join(".local/share/kilo");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("kilo.db"), b"sqlite fixture marker").unwrap();
    std::fs::write(data_dir.join("kilo-dev.db"), b"sqlite fixture marker").unwrap();
    std::fs::write(data_dir.join("opencode-dev.db"), b"ignored").unwrap();

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
    assert_eq!(
        sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>(),
        vec![data_dir.join("kilo.db"), data_dir.join("kilo-dev.db")]
    );
    assert!(sources
        .iter()
        .all(|source| source.status == ProviderSourceStatus::Available));

    let xdg_data = temp.path().join("xdg-data");
    let xdg_kilo = xdg_data.join("kilo");
    std::fs::create_dir_all(&xdg_kilo).unwrap();
    std::fs::write(xdg_kilo.join("kilo.db"), b"sqlite fixture marker").unwrap();
    let _xdg_data_set = EnvGuard::set("XDG_DATA_HOME", xdg_data.as_os_str());
    let _config_dir_set = EnvGuard::set("KILO_CONFIG_DIR", temp.path().join("config"));

    let xdg_sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
    assert_eq!(xdg_sources[0].path, xdg_kilo.join("kilo.db"));
    assert_ne!(
        xdg_sources[0].path,
        temp.path().join("config").join("kilo.db")
    );

    let _relative_db = EnvGuard::set("KILO_DB", "relative-kilo.db");
    std::fs::write(xdg_kilo.join("relative-kilo.db"), b"sqlite fixture marker").unwrap();
    let relative_sources =
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
    assert_eq!(relative_sources.len(), 1);
    assert_eq!(relative_sources[0].path, xdg_kilo.join("relative-kilo.db"));
    assert_eq!(relative_sources[0].status, ProviderSourceStatus::Available);

    let absolute_db = temp.path().join("absolute-kilo.db");
    std::fs::write(&absolute_db, b"sqlite fixture marker").unwrap();
    let _absolute_db = EnvGuard::set("KILO_DB", absolute_db.as_os_str());
    let absolute_sources =
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
    assert_eq!(absolute_sources.len(), 1);
    assert_eq!(absolute_sources[0].path, absolute_db);
    assert_eq!(absolute_sources[0].status, ProviderSourceStatus::Available);
}

#[test]
fn qwen_discovery_uses_runtime_and_home_env_overrides() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let runtime = temp.path().join("qwen-runtime");
    write_qwen_discovery_chat(&runtime.join("projects"));
    let qwen_home = temp.path().join("qwen-home");
    write_qwen_discovery_chat(&qwen_home.join("projects"));
    let _runtime = EnvGuard::set("QWEN_RUNTIME_DIR", runtime.as_os_str());
    let _home = EnvGuard::set("QWEN_HOME", qwen_home.as_os_str());

    let sources = discover_provider_sources(temp.path());
    for path in [runtime.join("projects"), qwen_home.join("projects")] {
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::QwenCode && source.path == path)
            .unwrap_or_else(|| panic!("missing Qwen Code source for {path:?}: {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }
}

#[test]
fn kimi_discovery_uses_home_env_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let kimi_home = temp.path().join("kimi-home");
    write_kimi_discovery_wire(&kimi_home);
    let _home = EnvGuard::set("KIMI_CODE_HOME", kimi_home.as_os_str());

    let sources = discover_provider_sources(temp.path());
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::KimiCodeCli && source.path == kimi_home)
        .unwrap_or_else(|| panic!("missing Kimi Code CLI source in {sources:#?}"));
    assert_eq!(source.status, ProviderSourceStatus::Available);
    let crush = temp.path().join(".local/share/crush");
    std::fs::create_dir_all(&crush).unwrap();
    std::fs::write(crush.join("crush.db"), b"sqlite fixture marker").unwrap();
    let crush_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Crush)
        .unwrap();
    assert_eq!(crush_source.status, ProviderSourceStatus::Available);
    assert_eq!(crush_source.source_format, "crush_sqlite");

    let goose = temp.path().join(".local/share/goose/sessions");
    std::fs::create_dir_all(&goose).unwrap();
    std::fs::write(goose.join("sessions.db"), b"sqlite fixture marker").unwrap();
    let goose_source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| source.provider == CaptureProvider::Goose)
        .unwrap();
    assert_eq!(goose_source.status, ProviderSourceStatus::Available);
    assert_eq!(goose_source.source_format, "goose_sessions_sqlite");
}

#[test]
fn codebuddy_discovery_uses_localappdata_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let local_app_data = temp.path().join("local-app-data");
    let codebuddy = local_app_data.join("CodeBuddyExtension");
    let session = codebuddy
        .join("CodeBuddyIDE/default/history/11112222333344445555666677778888/session-alpha");
    std::fs::create_dir_all(session.join("messages")).unwrap();
    std::fs::write(
        session.join("index.json"),
        r#"{"messages":[{"id":"msg-1","role":"user"}]}"#,
    )
    .unwrap();
    std::fs::write(
        session.join("messages/msg-1.json"),
        r#"{"message":"hello"}"#,
    )
    .unwrap();
    let _local_app_data = EnvGuard::set("LOCALAPPDATA", local_app_data.as_os_str());

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::CodeBuddy);
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::CodeBuddy && source.path == codebuddy)
        .unwrap_or_else(|| panic!("missing CodeBuddy LOCALAPPDATA source in {sources:#?}"));

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

#[test]
fn firebender_discovery_uses_current_project_chat_history_db() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let project = temp.path().join("project");
    let nested = project.join("src/module");
    let db = project.join(".idea/firebender/chat_history.db");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    Connection::open(&db)
        .unwrap()
        .execute_batch(
            r#"
            CREATE TABLE chat_sessions (
                id TEXT PRIMARY KEY,
                messages_json TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
    let _cwd = CwdGuard::set(&nested);

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Firebender);
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::Firebender && source.path == db)
        .unwrap_or_else(|| panic!("missing Firebender cwd source in {sources:#?}"));

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "firebender_chat_history_sqlite");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}
#[test]
fn junie_discovery_uses_default_sessions_and_env_overrides() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _sessions_dir = EnvGuard::remove("JUNIE_SESSIONS_DIR");
    let _junie_home = EnvGuard::remove("JUNIE_HOME");

    let default_sessions = temp.path().join(".junie/sessions");
    std::fs::create_dir_all(&default_sessions).unwrap();
    let empty_source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Junie)
        .into_iter()
        .find(|source| source.path == default_sessions)
        .unwrap();
    assert_eq!(empty_source.status, ProviderSourceStatus::Empty);
    assert_eq!(
        empty_source.source_format,
        "junie_session_events_jsonl_tree"
    );

    write_junie_discovery_session(&default_sessions, "session-260607-110000-default");
    let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Junie)
        .into_iter()
        .find(|source| source.path == default_sessions)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);

    let env_sessions = temp.path().join("junie-env-sessions");
    write_junie_discovery_session(&env_sessions, "session-260607-110001-env");
    let _sessions_dir = EnvGuard::set("JUNIE_SESSIONS_DIR", env_sessions.as_os_str());

    let junie_home = temp.path().join("junie-home");
    let home_sessions = junie_home.join("sessions");
    write_junie_discovery_session(&home_sessions, "session-260607-110002-home");
    let _junie_home = EnvGuard::set("JUNIE_HOME", junie_home.as_os_str());

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Junie);
    for path in [&env_sessions, &home_sessions] {
        let source = sources
            .iter()
            .find(|source| source.path == *path)
            .unwrap_or_else(|| panic!("missing Junie source {path:?} in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "junie_session_events_jsonl_tree");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }
}

#[test]
fn mistral_vibe_discovery_uses_default_and_home_env_sessions() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _home = EnvGuard::remove("VIBE_HOME");

    let default_sessions = temp.path().join(".vibe/logs/session");
    std::fs::create_dir_all(&default_sessions).unwrap();
    let empty_source =
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::MistralVibe)
            .into_iter()
            .find(|source| source.path == default_sessions)
            .unwrap();
    assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

    write_mistral_vibe_discovery_session(&default_sessions);
    let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::MistralVibe)
        .into_iter()
        .find(|source| source.path == default_sessions)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "mistral_vibe_session_jsonl_tree");
    assert_eq!(source.import_support, ProviderImportSupport::Native);

    let custom_home = temp.path().join("custom-vibe");
    let custom_sessions = custom_home.join("logs/session");
    write_mistral_vibe_discovery_session(&custom_sessions);
    let _home = EnvGuard::set("VIBE_HOME", custom_home.as_os_str());
    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::MistralVibe);
    assert!(sources.iter().any(|source| {
        source.path == custom_sessions && source.status == ProviderSourceStatus::Available
    }));
}

#[test]
fn mux_discovery_uses_default_and_mux_root_sessions() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _home = EnvGuard::remove("MUX_ROOT");

    let default_sessions = temp.path().join(".mux/sessions");
    std::fs::create_dir_all(&default_sessions).unwrap();
    let empty_source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Mux)
        .into_iter()
        .find(|source| source.path == default_sessions)
        .unwrap();
    assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

    write_mux_discovery_session(&default_sessions);
    let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Mux)
        .into_iter()
        .find(|source| source.path == default_sessions)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "mux_session_jsonl_tree");
    assert_eq!(source.import_support, ProviderImportSupport::Native);

    let custom_home = temp.path().join("custom-mux");
    let custom_sessions = custom_home.join("sessions");
    write_mux_discovery_session(&custom_sessions);
    let _home = EnvGuard::set("MUX_ROOT", custom_home.as_os_str());
    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Mux);
    assert!(sources.iter().any(|source| {
        source.path == custom_sessions && source.status == ProviderSourceStatus::Available
    }));
}

#[test]
fn deepagents_discovery_uses_default_sessions_db() {
    let temp = tempdir();
    let db = temp.path().join(".deepagents/.state/sessions.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();

    let empty_source =
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::DeepAgents)
            .into_iter()
            .find(|source| source.path == db)
            .unwrap();
    assert_eq!(empty_source.status, ProviderSourceStatus::Missing);

    std::fs::write(&db, b"not sqlite").unwrap();
    let unreadable_source =
        discover_provider_sources_for_provider(temp.path(), CaptureProvider::DeepAgents)
            .into_iter()
            .find(|source| source.path == db)
            .unwrap();
    assert_eq!(unreadable_source.status, ProviderSourceStatus::Unknown);

    std::fs::copy(
        shared_provider_history_fixture("deepagents/v1/sessions.db"),
        &db,
    )
    .unwrap();
    let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::DeepAgents)
        .into_iter()
        .find(|source| source.path == db)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "deepagents_sessions_sqlite");
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

#[test]
fn crush_discovery_uses_global_config_data_directory() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let config = temp.path().join("crush.json");
    let data_dir = temp.path().join("custom-crush-data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("crush.db"), b"sqlite fixture marker").unwrap();
    std::fs::write(
        &config,
        format!(
            "{{\"options\":{{\"data_directory\":\"{}\"}}}}",
            data_dir.display()
        ),
    )
    .unwrap();
    let _config = EnvGuard::set("CRUSH_GLOBAL_CONFIG", &config);
    let _data = EnvGuard::remove("CRUSH_GLOBAL_DATA");

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Crush);
    let source = sources
        .iter()
        .find(|source| source.path == data_dir.join("crush.db"))
        .unwrap_or_else(|| panic!("missing Crush config source in {sources:#?}"));
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "crush_sqlite");
}

#[test]
fn goose_discovery_uses_path_root_data_sessions_db() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let root = temp.path().join("goose-root");
    let sessions = root.join("data/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("sessions.db"), b"sqlite fixture marker").unwrap();
    let _path_root = EnvGuard::set("GOOSE_PATH_ROOT", &root);

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Goose);
    let source = sources
        .iter()
        .find(|source| source.path == sessions.join("sessions.db"))
        .unwrap_or_else(|| panic!("missing Goose path-root source in {sources:#?}"));
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.source_format, "goose_sessions_sqlite");
}

#[cfg(unix)]
#[test]
fn warp_discovery_uses_documented_state_and_localappdata_paths() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let xdg_state = temp.path().join("xdg-state");
    let local_app_data = temp.path().join("local-app-data");
    let linux_db = xdg_state.join("warp-terminal/warp.sqlite");
    let windows_db = local_app_data.join("warp/Warp/data/warp.sqlite");
    std::fs::create_dir_all(linux_db.parent().unwrap()).unwrap();
    std::fs::create_dir_all(windows_db.parent().unwrap()).unwrap();
    std::fs::write(&linux_db, b"sqlite fixture marker").unwrap();
    std::fs::write(&windows_db, b"sqlite fixture marker").unwrap();
    let _xdg_state = EnvGuard::set("XDG_STATE_HOME", xdg_state.as_os_str());
    let _local_app_data = EnvGuard::set("LOCALAPPDATA", local_app_data.as_os_str());

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Warp);
    for path in [&linux_db, &windows_db] {
        let source = sources
            .iter()
            .find(|source| source.path == *path)
            .unwrap_or_else(|| panic!("missing Warp source {path:?} in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "warp_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());
    }
}

#[test]
fn lingma_discovery_uses_waylog_default_local_db_paths() {
    let temp = tempdir();
    let stable = temp
        .path()
        .join(".lingma/vscode/sharedClientCache/cache/db/local.db");
    let insiders = temp
        .path()
        .join(".lingma/vscode-insiders/sharedClientCache/cache/db/local.db");
    write_lingma_discovery_db(&stable);
    write_lingma_discovery_db(&insiders);

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Lingma);
    for path in [&stable, &insiders] {
        let source = sources
            .iter()
            .find(|source| source.path == *path)
            .unwrap_or_else(|| panic!("missing Lingma source {path:?} in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "lingma_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }
}

#[test]
fn trae_discovery_uses_workspace_storage_roots_as_native_sources() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let appdata = temp.path().join("appdata");
    let _appdata = EnvGuard::set("APPDATA", appdata.as_os_str());

    let standard_mac_root = temp
        .path()
        .join("Library/Application Support/Trae/User/workspaceStorage");
    let mac_root = temp
        .path()
        .join("Library/Application Support/Trae CN/User/workspaceStorage");
    let standard_appdata_root = appdata.join("Trae/User/workspaceStorage");
    let appdata_root = appdata.join("Trae CN/User/workspaceStorage");
    for root in [
        &standard_mac_root,
        &mac_root,
        &standard_appdata_root,
        &appdata_root,
    ] {
        write_trae_discovery_db(&root.join("workspace-hash/state.vscdb"));
    }

    let empty_root = temp
        .path()
        .join("Library/Application Support/Trae/User/workspaceStorage-empty");
    write_trae_non_chat_state_db(&empty_root.join("workspace-hash/state.vscdb"));
    assert_eq!(
        has_trae_state_vscdb_chat_history(&empty_root, 10_000),
        BoundedProbe::NotFound
    );

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Trae);
    for path in [
        &standard_mac_root,
        &mac_root,
        &standard_appdata_root,
        &appdata_root,
    ] {
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::Trae && source.path == *path)
            .unwrap_or_else(|| panic!("missing Trae source {path:?} in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, TRAE_STATE_VSCDB_SOURCE_FORMAT);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());
        assert!(source.unsupported_reason.is_none());
    }
}

#[test]
fn pi_discovery_uses_env_session_dir() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
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
fn pi_discovery_uses_global_and_project_settings_session_dirs() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let project = tempdir();
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

#[test]
fn cline_discovery_uses_env_data_dirs() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let custom = temp.path().join("custom-cline-data");
    write_task_json_discovery_task(&custom, "cline-env-task", "api_conversation_history.json");
    let _data_dir = EnvGuard::set("CLINE_DATA_DIR", custom.as_os_str());
    let _cline_dir = EnvGuard::remove("CLINE_DIR");
    let _session_dir = EnvGuard::remove("CLINE_SESSION_DATA_DIR");
    let _db_dir = EnvGuard::remove("CLINE_DB_DATA_DIR");

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Cline);
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::Cline && source.path == custom)
        .unwrap();

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

#[test]
fn roo_discovery_uses_custom_storage_setting() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let custom = temp.path().join("roo-custom-storage");
    write_task_json_discovery_task(&custom, "roo-custom-task", "history_item.json");
    let settings = temp.path().join(".config/Code/User/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{"roo-cline.customStoragePath":"~/roo-custom-storage"}"#,
    )
    .unwrap();
    let _roo_code = EnvGuard::remove("ROO_CODE_DATA_DIR");
    let _roo = EnvGuard::remove("ROO_DATA_DIR");
    let _roo_cline = EnvGuard::remove("ROO_CLINE_DATA_DIR");

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::RooCode);
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::RooCode && source.path == custom)
        .unwrap();

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}
