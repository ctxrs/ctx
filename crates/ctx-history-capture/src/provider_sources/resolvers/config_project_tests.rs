use std::fs;

use super::*;
use crate::provider_sources::{
    context::DiscoveryPlatformDirs, discovery::provider_source_for_path,
};
use crate::ProviderSourceStatus;

fn tempdir() -> tempfile::TempDir {
    crate::test_support_paths::tempdir()
        .expect("system temporary directory should support resolver fixtures")
}

fn context(home: &Path, cwd: &Path) -> DiscoveryContext {
    DiscoveryContext::new(
        home,
        cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs {
            data: Some(home.join(".local/share")),
            config: Some(home.join(".config")),
            state: Some(home.join(".local/state")),
            local_data: Some(home.join(".local/share")),
        },
    )
}

fn spec(provider: CaptureProvider) -> &'static ProviderSourceSpec {
    crate::provider_source_specs()
        .iter()
        .find(|spec| spec.provider == provider)
        .expect("provider must have a source spec")
}

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().expect("fixture path has a parent")).unwrap();
    fs::write(path, body).unwrap();
}

fn touch(path: &Path) {
    write(path, "");
}

fn source_paths(report: &DiscoveryReport) -> Vec<PathBuf> {
    report
        .sources
        .iter()
        .map(|source| source.path.clone())
        .collect()
}

#[test]
fn pi_official_root_policy_is_winner_only_and_trust_bounded() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();

    let global = temp.path().join("global-sessions");
    let project = cwd.join(".pi/sessions");
    write(
        &home.join(".pi/agent/settings.json"),
        &format!(
            r#"{{"sessionDir":{},"defaultProjectTrust":"ask"}}"#,
            serde_json::to_string(global.to_str().unwrap()).unwrap()
        ),
    );
    write(
        &cwd.join(".pi/settings.json"),
        r#"{"sessionDir":".pi/sessions"}"#,
    );
    write(
        &home.join(".pi/agent/trust.json"),
        &format!(
            r#"{{{}:true}}"#,
            serde_json::to_string(cwd.to_str().unwrap()).unwrap()
        ),
    );
    write(&project.join("session.jsonl"), "{}\n");
    write(&home.join(".omp/agent/sessions/stale.jsonl"), "{}\n");

    let report = resolve(&context(&home, &cwd), spec(CaptureProvider::Pi));
    assert_eq!(source_paths(&report), vec![project.clone()]);
    assert!(report.issues.is_empty());
    assert_eq!(report.sources[0].source_format, PI_FORMAT);

    let explicit = provider_source_for_path(CaptureProvider::Pi, project.clone());
    assert_eq!(report.sources[0].path, explicit.path);
    assert_eq!(report.sources[0].source_format, explicit.source_format);
}

#[test]
fn pi_session_env_replaces_agent_config_and_default_without_trimming() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let winner = cwd.join(" session root ");
    write(&winner.join("session.jsonl"), "{}\n");
    let report = resolve(
        &context(&home, &cwd)
            .with_env("PI_CODING_AGENT_DIR", temp.path().join("agent"))
            .with_env("PI_CODING_AGENT_SESSION_DIR", " session root "),
        spec(CaptureProvider::Pi),
    );
    assert_eq!(source_paths(&report), vec![winner]);
}

#[cfg(unix)]
#[test]
fn pi_selected_link_is_manual_and_does_not_restore_default() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let target = temp.path().join("target");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    symlink(&target, cwd.join("linked")).unwrap();
    write(&home.join(".pi/agent/sessions/default.jsonl"), "{}\n");
    let report = resolve(
        &context(&home, &cwd).with_env("PI_CODING_AGENT_SESSION_DIR", "linked"),
        spec(CaptureProvider::Pi),
    );
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn crush_official_config_and_registry_are_git_bounded() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let cwd = repo.join("nested");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write(
        &repo.join("crush.json"),
        r#"{"options":{"data_directory":"workspace-data"}}"#,
    );
    let current_db = cwd.join("workspace-data/crush.db");
    touch(&current_db);
    touch(&temp.path().join(".crush/crush.db"));

    let registered_root = temp.path().join("registered");
    let registered_db = registered_root.join("crush.db");
    touch(&registered_db);
    write(
        &home.join(".local/share/crush/projects.json"),
        &format!(
            r#"{{"projects":[{{"path":"ignored","data_dir":{}}}]}}"#,
            serde_json::to_string(registered_root.to_str().unwrap()).unwrap()
        ),
    );

    let report = resolve(&context(&home, &cwd), spec(CaptureProvider::Crush));
    assert_eq!(source_paths(&report), vec![current_db, registered_db]);
    assert!(!source_paths(&report).contains(&home.join(".local/share/crush/crush.db")));
}

#[test]
fn crush_unknown_nearest_default_suppresses_outer_and_cwd_fallbacks() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let cwd = repo.join("nested");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    touch(&repo.join(".crush/crush.db"));
    write(&cwd.join(".crush"), "not a directory");

    let report = resolve(&context(&home, &cwd), spec(CaptureProvider::Crush));
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn crush_registry_is_finite_sorted_and_rejects_relative_entries() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    let a = temp.path().join("a");
    let z = temp.path().join("z");
    write(
        &home.join(".local/share/crush/projects.json"),
        &format!(
            r#"{{"projects":[{{"data_dir":{}}},{{"data_dir":"relative"}},{{"data_dir":{}}}]}}"#,
            serde_json::to_string(z.to_str().unwrap()).unwrap(),
            serde_json::to_string(a.to_str().unwrap()).unwrap()
        ),
    );
    let report = resolve(&context(&home, &cwd), spec(CaptureProvider::Crush));
    let paths = source_paths(&report);
    assert_eq!(paths[0], cwd.join(".crush/crush.db"));
    assert_eq!(paths[1..], [a.join("crush.db"), z.join("crush.db")]);
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn crush_inventory_retains_official_project_keys_and_reobserves_revision() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    let first_project = temp.path().join("project-a");
    let first_data = temp.path().join("data-a");
    touch(&first_data.join("crush.db"));
    write(
        &home.join(".local/share/crush/projects.json"),
        &format!(
            r#"{{"projects":[{{"path":{},"data_dir":{}}}]}}"#,
            serde_json::to_string(first_project.to_str().unwrap()).unwrap(),
            serde_json::to_string(first_data.to_str().unwrap()).unwrap(),
        ),
    );
    let context = context(&home, &cwd);
    let selector = CrushProjectInventorySelector::new(context);
    let provider = spec(CaptureProvider::Crush);
    let opening = selector.observe(provider).unwrap();
    assert_eq!(opening.databases().len(), 1);
    assert_eq!(
        opening.databases()[0].selector_key(),
        &CrushProjectSelectorKey::RegisteredProject(first_project.clone())
    );
    assert_eq!(
        opening.databases()[0].database_path(),
        first_data.join("crush.db")
    );

    let second_project = temp.path().join("project-b");
    let second_data = temp.path().join("data-b");
    touch(&second_data.join("crush.db"));
    write(
        &home.join(".local/share/crush/projects.json"),
        &format!(
            r#"{{"projects":[
                {{"path":{},"data_dir":{}}},
                {{"path":{},"data_dir":{}}}
            ]}}"#,
            serde_json::to_string(first_project.to_str().unwrap()).unwrap(),
            serde_json::to_string(first_data.to_str().unwrap()).unwrap(),
            serde_json::to_string(second_project.to_str().unwrap()).unwrap(),
            serde_json::to_string(second_data.to_str().unwrap()).unwrap(),
        ),
    );
    let closing = selector.observe(provider).unwrap();
    assert_eq!(closing.databases().len(), 2);
    assert_ne!(opening.revision(), closing.revision());
}

#[test]
fn qwen_code_official_root_policy_honors_runtime_winner() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let runtime = temp.path().join("runtime");
    let qwen_home = temp.path().join("qwen-home");
    write(&runtime.join("projects/p/chats/session.jsonl"), "{}\n");
    write(
        &qwen_home.join("settings.json"),
        r#"{"advanced":{"runtimeOutputDir":"/stale"}}"#,
    );
    let report = resolve(
        &context(&home, &cwd)
            .with_env("QWEN_RUNTIME_DIR", &runtime)
            .with_env("QWEN_HOME", &qwen_home),
        spec(CaptureProvider::QwenCode),
    );
    assert_eq!(source_paths(&report), vec![runtime.join("projects")]);
}

#[test]
fn qwen_code_system_and_trust_selector_envs_follow_scope_precedence() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    let defaults_path = temp.path().join("selectors/system-defaults.json");
    let system_path = temp.path().join("selectors/settings.json");
    let trusted_path = temp.path().join("selectors/trustedFolders.json");
    let defaults_root = temp.path().join("defaults-runtime");
    let user_root = temp.path().join("user-runtime");
    let project_root = cwd.join("project-runtime");
    let system_root = temp.path().join("system-runtime");
    write(
        &defaults_path,
        &format!(
            r#"{{"advanced":{{"runtimeOutputDir":{}}}}}"#,
            serde_json::to_string(defaults_root.to_str().unwrap()).unwrap()
        ),
    );
    let base = context(&home, &cwd)
        .with_env("QWEN_CODE_SYSTEM_DEFAULTS_PATH", &defaults_path)
        .with_env("QWEN_CODE_SYSTEM_SETTINGS_PATH", &system_path);
    assert_eq!(
        source_paths(&resolve(&base, spec(CaptureProvider::QwenCode))),
        vec![defaults_root.join("projects")]
    );

    write(
        &home.join(".qwen/settings.json"),
        &format!(
            r#"{{"advanced":{{"runtimeOutputDir":{}}},"security":{{"folderTrust":{{"enabled":true}}}}}}"#,
            serde_json::to_string(user_root.to_str().unwrap()).unwrap()
        ),
    );
    write(
        &cwd.join(".qwen/settings.json"),
        &format!(
            r#"{{"advanced":{{"runtimeOutputDir":{}}}}}"#,
            serde_json::to_string(project_root.to_str().unwrap()).unwrap()
        ),
    );
    write(
        &trusted_path,
        &format!(
            r#"{{{}:"TRUST_FOLDER"}}"#,
            serde_json::to_string(cwd.to_str().unwrap()).unwrap()
        ),
    );
    let trusted = base
        .clone()
        .with_env("QWEN_CODE_TRUSTED_FOLDERS_PATH", &trusted_path);
    assert_eq!(
        source_paths(&resolve(&trusted, spec(CaptureProvider::QwenCode))),
        vec![project_root.join("projects")]
    );

    write(
        &system_path,
        &format!(
            r#"{{"advanced":{{"runtimeOutputDir":{}}}}}"#,
            serde_json::to_string(system_root.to_str().unwrap()).unwrap()
        ),
    );
    assert_eq!(
        source_paths(&resolve(&trusted, spec(CaptureProvider::QwenCode))),
        vec![system_root.join("projects")]
    );
}

#[test]
fn qwen_code_persisted_trust_selects_exact_project_setting() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    let qwen_home = temp.path().join("qwen-home");
    write(
        &qwen_home.join("settings.json"),
        r#"{"advanced":{"runtimeOutputDir":"/stale"},"security":{"folderTrust":{"enabled":true}}}"#,
    );
    write(
        &qwen_home.join("trustedFolders.json"),
        &format!(
            r#"{{{}:"TRUST_FOLDER"}}"#,
            serde_json::to_string(cwd.to_str().unwrap()).unwrap()
        ),
    );
    write(
        &cwd.join(".qwen/settings.json"),
        r#"{/* jsonc */"advanced":{"runtimeOutputDir":".qwen-runtime",},}"#,
    );
    let project_root = cwd.join(".qwen-runtime/projects");
    write(&project_root.join("p/chats/session.jsonl"), "{}\n");
    let report = resolve(
        &context(&home, &cwd).with_env("QWEN_HOME", &qwen_home),
        spec(CaptureProvider::QwenCode),
    );
    assert_eq!(source_paths(&report), vec![project_root]);
    assert!(report.issues.is_empty());
}

#[test]
fn qwen_code_untrusted_external_replacement_does_not_scan_stale_default() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &home.join(".qwen/settings.json"),
        r#"{"security":{"folderTrust":{"enabled":true}}}"#,
    );
    write(
        &cwd.join(".qwen/settings.json"),
        r#"{"advanced":{"runtimeOutputDir":"/external"}}"#,
    );
    write(&home.join(".qwen/projects/p/chats/stale.jsonl"), "{}\n");
    let report = resolve(&context(&home, &cwd), spec(CaptureProvider::QwenCode));
    assert_eq!(source_paths(&report), vec![home.join(".qwen/projects")]);
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn qwen_code_folder_trust_merges_all_scopes_and_gates_project_settings() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    let defaults = temp.path().join("system-defaults.json");
    let system = temp.path().join("system-settings.json");
    let defaults_root = temp.path().join("defaults-runtime");
    let user_root = temp.path().join("user-runtime");
    let project_root = cwd.join("project-runtime");
    write(
        &defaults,
        &format!(
            r#"{{"advanced":{{"runtimeOutputDir":{}}},"security":{{"folderTrust":{{"enabled":true}}}}}}"#,
            serde_json::to_string(defaults_root.to_str().unwrap()).unwrap()
        ),
    );
    write(
        &home.join(".qwen/settings.json"),
        &format!(
            r#"{{"advanced":{{"runtimeOutputDir":{}}},"security":{{"folderTrust":{{"enabled":false}}}}}}"#,
            serde_json::to_string(user_root.to_str().unwrap()).unwrap()
        ),
    );
    write(
        &cwd.join(".qwen/settings.json"),
        r#"{"advanced":{"runtimeOutputDir":"project-runtime"}}"#,
    );
    write(&system, r#"{"security":{"folderTrust":{"enabled":true}}}"#);
    let base = context(&home, &cwd)
        .with_env("QWEN_CODE_SYSTEM_DEFAULTS_PATH", &defaults)
        .with_env("QWEN_CODE_SYSTEM_SETTINGS_PATH", &system);

    let system_override = resolve(&base, spec(CaptureProvider::QwenCode));
    assert_eq!(
        source_paths(&system_override),
        vec![user_root.join("projects")]
    );
    assert_eq!(system_override.issues.len(), 1);

    fs::remove_file(&system).unwrap();
    assert_eq!(
        source_paths(&resolve(&base, spec(CaptureProvider::QwenCode))),
        vec![project_root.join("projects")]
    );

    fs::remove_file(home.join(".qwen/settings.json")).unwrap();
    let defaults_gate = resolve(&base, spec(CaptureProvider::QwenCode));
    assert_eq!(
        source_paths(&defaults_gate),
        vec![defaults_root.join("projects")]
    );
    assert_eq!(defaults_gate.issues.len(), 1);
}

#[test]
fn qwen_code_interpolation_preserves_non_ascii_path_identity() {
    let temp = tempdir();
    let home = temp.path().join("家");
    let cwd = temp.path().join("工作区");
    let qwen_home = temp.path().join("配置");
    let system = temp.path().join("system-settings.json");
    let defaults = temp.path().join("system-defaults.json");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &qwen_home.join("settings.json"),
        r#"{"advanced":{"runtimeOutputDir":"$QWEN_HOME/資料/проект"}}"#,
    );
    let report = resolve(
        &context(&home, &cwd)
            .with_env("QWEN_HOME", &qwen_home)
            .with_env("QWEN_CODE_SYSTEM_DEFAULTS_PATH", &defaults)
            .with_env("QWEN_CODE_SYSTEM_SETTINGS_PATH", &system),
        spec(CaptureProvider::QwenCode),
    );
    assert_eq!(
        source_paths(&report),
        vec![qwen_home.join("資料/проект/projects")]
    );
}

#[test]
fn mistral_vibe_official_root_policy_uses_persisted_nearest_project() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let cwd = repo.join("nested");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    write(
        &repo.join(".vibe/config.toml"),
        "[session_logging]\nsave_dir = \".history\"\n",
    );
    write(
        &home.join(".vibe/trusted_folders.toml"),
        &format!(
            "trusted = [{}]\nuntrusted = []\n",
            serde_json::to_string(repo.to_str().unwrap()).unwrap()
        ),
    );
    let root = cwd.join(".history");
    write(&root.join("session/meta.json"), "{}");
    write(&root.join("session/messages.jsonl"), "{}\n");
    let report = resolve(&context(&home, &cwd), spec(CaptureProvider::MistralVibe));
    assert_eq!(source_paths(&report), vec![root]);
    assert!(report.issues.is_empty());
}

#[test]
fn mistral_vibe_untrusted_project_uses_user_winner_with_manual_issue() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    let user_root = temp.path().join("user-vibe");
    write(
        &cwd.join(".vibe/config.toml"),
        "[session_logging]\nsave_dir = \"/untrusted\"\n",
    );
    write(
        &home.join(".vibe/config.toml"),
        &format!(
            "[session_logging]\nsave_dir = {}\n",
            serde_json::to_string(user_root.to_str().unwrap()).unwrap()
        ),
    );
    let report = resolve(&context(&home, &cwd), spec(CaptureProvider::MistralVibe));
    assert_eq!(source_paths(&report), vec![user_root]);
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn mistral_vibe_session_logging_env_uses_nested_save_dir_precedence() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let json_root = temp.path().join("json-root");
    let nested_root = temp.path().join("nested-root");
    let json = format!(
        r#"{{"save_dir":{}}}"#,
        serde_json::to_string(json_root.to_str().unwrap()).unwrap()
    );
    let report = resolve(
        &context(&home, &cwd)
            .with_env("VIBE_SESSION_LOGGING", json)
            .with_env("VIBE_SESSION_LOGGING__SAVE_DIR", &nested_root),
        spec(CaptureProvider::MistralVibe),
    );
    assert_eq!(source_paths(&report), vec![nested_root]);
}

#[test]
fn mistral_vibe_disabled_session_logging_keeps_existing_winner_discoverable() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let selected = temp.path().join("existing-vibe-root");
    write(&selected.join("session/meta.json"), "{}");
    write(&selected.join("session/messages.jsonl"), "{}\n");
    let logging = format!(
        r#"{{"enabled":false,"save_dir":{}}}"#,
        serde_json::to_string(selected.to_str().unwrap()).unwrap()
    );
    let report = resolve(
        &context(&home, &cwd).with_env("VIBE_SESSION_LOGGING", logging),
        spec(CaptureProvider::MistralVibe),
    );
    assert_eq!(source_paths(&report), vec![selected]);
    assert!(report.issues.is_empty());
}

#[test]
fn rovodev_official_root_policy_replaces_default_and_suppresses_unreconstructible() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let custom = temp.path().join("rovo-sessions");
    write(
        &home.join(".rovodev/config.yml"),
        &format!(
            "sessions:\n  persistenceDir: {}\n",
            serde_json::to_string(custom.to_str().unwrap()).unwrap()
        ),
    );
    write(&custom.join("one/session_context.json"), "{}");
    write(
        &home.join(".rovodev/sessions/stale/session_context.json"),
        "{}",
    );
    let report = resolve(&context(&home, &cwd), spec(CaptureProvider::RovoDev));
    assert_eq!(source_paths(&report), vec![custom]);

    write(
        &home.join(".rovodev/config.yml"),
        "sessions:\n  persistenceDir: relative/path\n",
    );
    let blocked = resolve(&context(&home, &cwd), spec(CaptureProvider::RovoDev));
    assert!(blocked.sources.is_empty());
    assert_eq!(blocked.issues.len(), 1);
}

#[test]
fn roo_code_official_root_policy_uses_jsonc_profiles_and_ignores_ctx_envs() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    let custom = temp.path().join("roo-custom");
    let profile_custom = temp.path().join("roo-profile-nightly");
    write(&custom.join("tasks/one/history_item.json"), "{}");
    write(&profile_custom.join("tasks/two/history_item.json"), "{}");
    write(
        &home.join(".config/Code/User/settings.json"),
        &format!(
            "{{/* jsonc */\"roo-cline.customStoragePath\":{},}}",
            serde_json::to_string(custom.to_str().unwrap()).unwrap()
        ),
    );
    write(
        &home.join(".config/Code/User/profiles/p1/settings.json"),
        &format!(
            "{{\"roo-code-nightly.customStoragePath\":{}}}",
            serde_json::to_string(profile_custom.to_str().unwrap()).unwrap()
        ),
    );
    let false_env = temp.path().join("ctx-only-env");
    write(&false_env.join("tasks/bad/history_item.json"), "{}");
    let report = resolve(
        &context(&home, &cwd).with_env("ROO_DATA_DIR", &false_env),
        spec(CaptureProvider::RooCode),
    );
    let paths = source_paths(&report);
    assert!(paths.contains(&custom));
    assert!(paths.contains(&profile_custom));
    assert!(paths.contains(&home.join(".vscode-mock/global-storage")));
    assert!(!paths.contains(&false_env));
    assert!(
        !paths.contains(&home.join(".config/Code/User/globalStorage/RooVeterinaryInc.roo-cline"))
    );
}

#[test]
fn roo_code_macos_uses_application_support_and_ignores_xdg() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();
    let mac_config = home.join("Library/Application Support");
    let selected = mac_config.join("Code/User/globalStorage/rooveterinaryinc.roo-cline");
    let ignored_xdg = home.join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline");
    write(&selected.join("tasks/one/history_item.json"), "{}");
    write(&ignored_xdg.join("tasks/two/history_item.json"), "{}");
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::MacOS,
        DiscoveryPlatformDirs {
            config: Some(mac_config),
            ..DiscoveryPlatformDirs::default()
        },
    )
    .with_env("XDG_CONFIG_HOME", home.join(".config"));

    let report = resolve(&context, spec(CaptureProvider::RooCode));
    let source = report
        .sources
        .iter()
        .find(|source| source.path == selected)
        .expect("macOS Roo root should be discovered");
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert!(!source_paths(&report).contains(&ignored_xdg));
}

#[test]
fn roo_code_external_workspace_selector_requires_consent_and_suppresses_defaults() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    write(
        &cwd.join(".vscode/settings.json"),
        r#"{"roo-cline.customStoragePath":"/external-roo"}"#,
    );
    let report = resolve(&context(&home, &cwd), spec(CaptureProvider::RooCode));
    assert!(!source_paths(&report)
        .iter()
        .any(|path| path.ends_with("rooveterinaryinc.roo-cline")));
    assert!(source_paths(&report).contains(&home.join(".vscode-mock/global-storage")));
    assert!(!report.issues.is_empty());
}

#[test]
fn owned_non_bsd_rows_emit_no_other_unix_defaults() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let discovery_context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::OtherUnix,
        DiscoveryPlatformDirs::default(),
    );
    for provider in [
        CaptureProvider::Pi,
        CaptureProvider::QwenCode,
        CaptureProvider::MistralVibe,
        CaptureProvider::RovoDev,
        CaptureProvider::RooCode,
    ] {
        let report = resolve(&discovery_context, spec(provider));
        assert!(report.sources.is_empty(), "unexpected {provider:?} source");
    }
}

#[test]
fn exact_selected_paths_preserve_explicit_source_identity_inputs() {
    let temp = tempdir();
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let cases = [
        (CaptureProvider::Pi, PI_FORMAT, temp.path().join("pi")),
        (
            CaptureProvider::QwenCode,
            QWEN_FORMAT,
            temp.path().join("qwen"),
        ),
        (
            CaptureProvider::MistralVibe,
            VIBE_FORMAT,
            temp.path().join("vibe"),
        ),
        (
            CaptureProvider::RovoDev,
            ROVO_FORMAT,
            temp.path().join("rovo"),
        ),
        (
            CaptureProvider::RooCode,
            ROO_FORMAT,
            temp.path().join("roo"),
        ),
    ];
    for (provider, format, path) in cases {
        fs::create_dir_all(&path).unwrap();
        let mut report = DiscoveryReport::default();
        add_source(&mut report, spec(provider), path.clone(), format);
        let explicit = provider_source_for_path(provider, path);
        assert_eq!(report.sources[0].path, explicit.path);
        assert_eq!(report.sources[0].source_format, explicit.source_format);
    }

    let crush = temp.path().join("crush.db");
    touch(&crush);
    let mut report = DiscoveryReport::default();
    add_source(
        &mut report,
        spec(CaptureProvider::Crush),
        crush.clone(),
        CRUSH_FORMAT,
    );
    let explicit = provider_source_for_path(CaptureProvider::Crush, crush);
    assert_eq!(report.sources[0].path, explicit.path);
    assert_eq!(report.sources[0].source_format, explicit.source_format);
}
