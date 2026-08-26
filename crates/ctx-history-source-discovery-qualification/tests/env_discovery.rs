mod support;

use ctx_history_core::CaptureProvider;
use ctx_history_source_discovery::*;
use rusqlite::Connection;

use support::{
    shared_provider_history_fixture, tempdir, write_junie_discovery_session,
    write_kimi_discovery_wire, write_lingma_discovery_db, write_mistral_vibe_discovery_session,
    write_mux_discovery_session, write_pi_discovery_session, write_qwen_discovery_chat,
    write_task_json_discovery_task, CwdGuard, EnvGuard, ENV_LOCK, TEST_PROVIDER_PROBES,
};

fn discover_provider_sources(home: &std::path::Path) -> Vec<ProviderSource> {
    ctx_history_source_discovery::discover_provider_sources(&TEST_PROVIDER_PROBES, home)
}

fn discover_provider_sources_for_provider(
    home: &std::path::Path,
    provider: CaptureProvider,
) -> Vec<ProviderSource> {
    ctx_history_source_discovery::discover_provider_sources_for_provider(
        &TEST_PROVIDER_PROBES,
        home,
        provider,
    )
}

fn discover_provider_sources_for_provider_report(
    home: &std::path::Path,
    provider: CaptureProvider,
) -> DiscoveryReport {
    ctx_history_source_discovery::discover_provider_sources_for_provider_report(
        &TEST_PROVIDER_PROBES,
        home,
        provider,
    )
}

fn discover_provider_sources_for_provider_with_context(
    context: &DiscoveryContext,
    provider: CaptureProvider,
) -> DiscoveryReport {
    ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &TEST_PROVIDER_PROBES,
        context,
        provider,
    )
}

fn discover_provider_sources_for_provider_with_projects(
    home: &std::path::Path,
    provider: CaptureProvider,
    projects: &[std::path::PathBuf],
) -> Vec<ProviderSource> {
    ctx_history_source_discovery::discover_provider_sources_for_provider_with_projects(
        &TEST_PROVIDER_PROBES,
        home,
        provider,
        projects,
    )
}

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
fn kilo_discovery_selects_one_active_database() {
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
        vec![data_dir.join("kilo.db")]
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
fn qwen_runtime_override_suppresses_home_root() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let runtime = temp.path().join("qwen-runtime");
    write_qwen_discovery_chat(&runtime.join("projects"));
    let qwen_home = temp.path().join("qwen-home");
    write_qwen_discovery_chat(&qwen_home.join("projects"));
    let _runtime = EnvGuard::set("QWEN_RUNTIME_DIR", runtime.as_os_str());
    let _home = EnvGuard::set("QWEN_HOME", qwen_home.as_os_str());

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::QwenCode);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].path, runtime.join("projects"));
    assert_eq!(sources[0].status, ProviderSourceStatus::Available);
    assert_eq!(sources[0].import_support, ProviderImportSupport::Native);
    assert_ne!(sources[0].path, qwen_home.join("projects"));
}

#[test]
fn kimi_discovery_uses_home_env_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let kimi_home = temp.path().join("kimi-home");
    write_kimi_discovery_wire(&kimi_home);
    let _home = EnvGuard::set("KIMI_CODE_HOME", kimi_home.as_os_str());

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::KimiCodeCli);
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::KimiCodeCli && source.path == kimi_home)
        .unwrap_or_else(|| panic!("missing Kimi Code CLI source in {sources:#?}"));
    assert_eq!(source.status, ProviderSourceStatus::Available);
}

#[test]
fn codebuddy_discovery_uses_cli_config_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let codebuddy = temp.path().join("codebuddy-cli");
    let project = codebuddy.join("projects/workspace");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("session.jsonl"), "{}\n").unwrap();
    let _config = EnvGuard::set("CODEBUDDY_CONFIG_DIR", codebuddy.as_os_str());

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::CodeBuddy);
    assert_eq!(sources.len(), 1);
    let source = sources
        .iter()
        .find(|source| source.provider == CaptureProvider::CodeBuddy && source.path == codebuddy)
        .unwrap_or_else(|| panic!("missing CodeBuddy CLI source in {sources:#?}"));

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

#[test]
fn firebender_project_db_participates_in_current_project_discovery() {
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

    let report =
        discover_provider_sources_for_provider_report(temp.path(), CaptureProvider::Firebender);
    assert_eq!(
        (
            report.sources.len(),
            &report.sources[0].path,
            report.sources[0].status
        ),
        (1, &db, ProviderSourceStatus::Unknown)
    );
    assert!(report.issues.is_empty());
}
#[test]
fn junie_home_replaces_default_and_retired_sessions_override() {
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
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].path, home_sessions);
    assert_ne!(sources[0].path, env_sessions);
    assert_eq!(sources[0].status, ProviderSourceStatus::Available);
    assert_eq!(sources[0].source_format, "junie_session_events_jsonl_tree");
    assert_eq!(sources[0].import_support, ProviderImportSupport::Native);
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
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].path, custom_sessions);
    assert_eq!(sources[0].status, ProviderSourceStatus::Available);
    assert_ne!(sources[0].path, default_sessions);
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
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].path, custom_sessions);
    assert_eq!(sources[0].status, ProviderSourceStatus::Available);
    assert_ne!(sources[0].path, default_sessions);
}

#[test]
fn deepagents_discovery_does_not_open_the_selected_database() {
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
    assert_eq!(unreadable_source.status, ProviderSourceStatus::Available);
    assert_eq!(unreadable_source.unsupported_reason, None);

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
    let config_dir = temp.path().join("crush-config");
    let config = config_dir.join("crush.json");
    let data_dir = temp.path().join("custom-crush-data");
    std::fs::create_dir_all(&config_dir).unwrap();
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
    let _config = EnvGuard::set("CRUSH_GLOBAL_CONFIG", &config_dir);
    let _data = EnvGuard::remove("CRUSH_GLOBAL_DATA");

    let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Crush);
    assert_eq!(sources.len(), 1);
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

#[test]
fn warp_linux_state_root_does_not_union_windows_localappdata() {
    let temp = tempdir();
    let xdg_state = temp.path().join("xdg-state");
    let local_app_data = temp.path().join("local-app-data");
    let linux_db = xdg_state.join("warp-terminal/warp.sqlite");
    let windows_db = local_app_data.join("warp/Warp/data/warp.sqlite");
    std::fs::create_dir_all(linux_db.parent().unwrap()).unwrap();
    std::fs::create_dir_all(windows_db.parent().unwrap()).unwrap();
    std::fs::write(&linux_db, b"sqlite fixture marker").unwrap();
    std::fs::write(&windows_db, b"sqlite fixture marker").unwrap();
    let context = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_env("XDG_STATE_HOME", xdg_state.as_os_str())
    .with_env("LOCALAPPDATA", local_app_data.as_os_str());

    let sources =
        discover_provider_sources_for_provider_with_context(&context, CaptureProvider::Warp)
            .sources;
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].path, linux_db);
    assert_ne!(sources[0].path, windows_db);
    assert_eq!(sources[0].status, ProviderSourceStatus::Available);
    assert_eq!(sources[0].source_format, "warp_sqlite");
    assert_eq!(sources[0].import_support, ProviderImportSupport::Native);
    assert!(sources[0].import_support.is_auto_importable());
}

#[test]
fn lingma_discovery_uses_current_vscode_root_only() {
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
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].path, stable);
    assert_ne!(sources[0].path, insiders);
    assert_eq!(sources[0].status, ProviderSourceStatus::Available);
    assert_eq!(sources[0].source_format, "lingma_sqlite");
    assert_eq!(sources[0].import_support, ProviderImportSupport::Native);
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
fn pi_project_setting_replaces_global_setting_when_persistently_trusted() {
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
        r#"{"sessionDir":"~/global-pi-sessions","defaultProjectTrust":"ask"}"#,
    )
    .unwrap();

    let project_sessions = project.path().join(".pi/custom-sessions");
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    write_pi_discovery_session(&project_sessions);
    std::fs::write(
        project.path().join(".pi/settings.json"),
        r#"{"sessionDir":".pi/custom-sessions"}"#,
    )
    .unwrap();
    std::fs::write(
        temp.path().join(".pi/agent/trust.json"),
        format!(
            "{{{}:true}}",
            serde_json::to_string(project.path().to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();

    let context = DiscoveryContext::new(
        temp.path(),
        project.path(),
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let report = discover_provider_sources_for_provider_with_context(&context, CaptureProvider::Pi);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, project_sessions);
    assert_ne!(report.sources[0].path, global);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Available);
}

#[test]
fn project_discovery_fans_out_only_across_supplied_activity_locators() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir();
    let root = tempdir();

    let projects = [root.path().join("one"), root.path().join("two")];
    let mut databases = Vec::new();
    for project in &projects {
        std::fs::create_dir_all(project).unwrap();
        let database = project.join("shelley.db");
        Connection::open(&database).unwrap();
        databases.push(database);
    }
    let unrelated = root.path().join("unrelated");
    std::fs::create_dir_all(&unrelated).unwrap();
    Connection::open(unrelated.join("shelley.db")).unwrap();

    let sources = discover_provider_sources_for_provider_with_projects(
        home.path(),
        CaptureProvider::Shelley,
        &projects,
    );
    for database in databases {
        assert!(sources.iter().any(|source| source.path == database));
    }
    assert!(sources
        .iter()
        .all(|source| !source.path.starts_with(&unrelated)));
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
    let temp = tempdir();
    let custom = temp.path().join("roo-custom-storage");
    write_task_json_discovery_task(&custom, "roo-custom-task", "history_item.json");
    let settings = temp.path().join(".config/Code/User/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        format!(
            r#"{{"roo-cline.customStoragePath":{}}}"#,
            serde_json::to_string(custom.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    let context = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs {
            config: Some(temp.path().join(".config")),
            ..DiscoveryPlatformDirs::default()
        },
    );

    let report =
        discover_provider_sources_for_provider_with_context(&context, CaptureProvider::RooCode);
    let source = report
        .sources
        .iter()
        .find(|source| source.provider == CaptureProvider::RooCode && source.path == custom)
        .unwrap();

    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

#[test]
fn injected_context_isolates_resolvers_from_process_env_and_cwd() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let process_mux = temp.path().join("process-mux");
    write_mux_discovery_session(&process_mux.join("sessions"));
    let _mux = EnvGuard::set("MUX_ROOT", &process_mux);

    let injected_cwd = temp.path().join("injected-cwd");
    std::fs::create_dir_all(&injected_cwd).unwrap();
    let context = DiscoveryContext::new(
        temp.path(),
        &injected_cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let report =
        discover_provider_sources_for_provider_with_context(&context, CaptureProvider::Mux);
    assert!(!report
        .sources
        .iter()
        .any(|source| source.path == process_mux.join("sessions")));

    let report = discover_provider_sources_for_provider_with_context(
        &context.with_env("MUX_ROOT", &process_mux),
        CaptureProvider::Mux,
    );
    assert!(report
        .sources
        .iter()
        .any(|source| source.path == process_mux.join("sessions")));
}

#[test]
fn canonical_alias_dedupe_keeps_first_operational_spelling_only_for_existing_paths() {
    let temp = tempdir();
    let default_sessions = temp.path().join(".mux/sessions");
    write_mux_discovery_session(&default_sessions);
    let alias_root = temp.path().join(".mux/../.mux");
    let context = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_env("MUX_ROOT", &alias_root);
    let report =
        discover_provider_sources_for_provider_with_context(&context, CaptureProvider::Mux);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, alias_root.join("sessions"));

    let missing_home = temp.path().join("missing-home");
    let missing_alias = missing_home.join(".mux/../.mux/sessions");
    let missing_context = DiscoveryContext::new(
        &missing_home,
        temp.path(),
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_env("MUX_ROOT", missing_home.join(".mux/../.mux"));
    let report =
        discover_provider_sources_for_provider_with_context(&missing_context, CaptureProvider::Mux);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, missing_alias);
}
