use super::super::super::context::DiscoveryPlatformDirs;
use std::fs;

use super::*;
use crate::provider_source_spec;
use rusqlite::Connection;

fn tempdir() -> tempfile::TempDir {
    crate::test_support_paths::tempdir()
        .expect("system temporary directory should support platform resolver fixtures")
}

fn context(root: &Path, platform: DiscoveryPlatform) -> DiscoveryContext {
    DiscoveryContext::new(
        root.join("home"),
        root.join("cwd"),
        platform,
        DiscoveryPlatformDirs {
            data: Some(root.join("platform-data")),
            config: Some(root.join("platform-config")),
            state: Some(root.join("platform-state")),
            local_data: Some(root.join("platform-local-data")),
        },
    )
}

fn provider_report(context: &DiscoveryContext, provider: CaptureProvider) -> DiscoveryReport {
    resolve(
        context,
        provider_source_spec(provider).expect("provider spec should exist"),
    )
}

fn write_file(path: &Path, body: &[u8]) {
    fs::create_dir_all(path.parent().expect("fixture file should have a parent")).unwrap();
    fs::write(path, body).unwrap();
}

fn write_lingma_db(path: &Path) {
    fs::create_dir_all(path.parent().expect("database should have a parent")).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table chat_record (\
                 session_id text, request_id text, chat_prompt text, summary text, \
                 error_result text, gmt_create integer, extra text);",
        )
        .unwrap();
}

#[test]
fn kiro_current_sessions_are_unsupported_and_suppress_legacy_sqlite() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    fs::create_dir_all(context.home().join(".kiro").join("sessions")).unwrap();
    write_file(
        &context.home().join(".local/share/kiro-cli/data.sqlite3"),
        b"legacy",
    );

    let report = provider_report(&context, CaptureProvider::KiroCli);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Unsupported);
    assert_eq!(
        report.sources[0].path,
        context.home().join(".kiro").join("sessions")
    );
    assert_eq!(
        report.sources[0].source_kind,
        ProviderSourceKind::DetectionOnly
    );
}

#[test]
fn kiro_legacy_selection_is_os_gated_and_xdg_is_replacement() {
    let temp = tempdir();
    let xdg = temp.path().join("xdg-data");
    let xdg_db = xdg.join("kiro-cli/data.sqlite3");
    write_file(&xdg_db, b"legacy");
    let linux =
        context(temp.path(), DiscoveryPlatform::Linux).with_env("XDG_DATA_HOME", xdg.as_os_str());
    let report = provider_report(&linux, CaptureProvider::KiroCli);
    assert_eq!(
        report
            .sources
            .iter()
            .map(|source| &source.path)
            .collect::<Vec<_>>(),
        vec![&xdg_db]
    );

    let windows = context(temp.path(), DiscoveryPlatform::Windows);
    assert!(provider_report(&windows, CaptureProvider::KiroCli)
        .sources
        .is_empty());
}

#[test]
fn kiro_relative_home_is_manual_and_does_not_fall_back() {
    let temp = tempdir();
    let context =
        context(temp.path(), DiscoveryPlatform::Linux).with_env("KIRO_HOME", "relative/kiro");
    let report = provider_report(&context, CaptureProvider::KiroCli);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn warp_uses_one_linux_base_and_only_evidenced_extra_surfaces() {
    let temp = tempdir();
    let xdg = temp.path().join("xdg-state");
    let stable = xdg.join("warp-terminal/warp.sqlite");
    let stable_tui = xdg.join("warp-terminal/tui/warp.sqlite");
    let preview = xdg.join("warp-terminal-preview/warp.sqlite");
    write_file(&stable, b"sqlite");
    write_file(&stable_tui, b"sqlite");
    write_file(&preview, b"sqlite");
    write_file(
        &temp.path().join("platform-state/warp-terminal/warp.sqlite"),
        b"stale",
    );
    let context =
        context(temp.path(), DiscoveryPlatform::Linux).with_env("XDG_STATE_HOME", xdg.as_os_str());
    let report = provider_report(&context, CaptureProvider::Warp);
    assert_eq!(
        report
            .sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>(),
        vec![stable, stable_tui, preview]
    );
}

#[test]
fn warp_uses_mac_precedence_windows_known_folder_and_no_other_unix_default() {
    let temp = tempdir();
    let mac = context(temp.path(), DiscoveryPlatform::MacOS);
    let fallback = mac
        .home()
        .join("Library/Application Support/dev.warp.Warp-Stable/warp.sqlite");
    write_file(&fallback, b"sqlite");
    let report = provider_report(&mac, CaptureProvider::Warp);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, fallback);

    let windows = context(temp.path(), DiscoveryPlatform::Windows);
    let windows_path = temp
        .path()
        .join("platform-local-data/warp/Warp/data/warp.sqlite");
    assert_eq!(
        provider_report(&windows, CaptureProvider::Warp).sources[0].path,
        windows_path
    );
    assert!(provider_report(
        &context(temp.path(), DiscoveryPlatform::OtherUnix),
        CaptureProvider::Warp
    )
    .sources
    .is_empty());
}

#[test]
fn codebuddy_cli_override_replaces_default_while_installed_ide_coexists() {
    let temp = tempdir();
    let custom = temp.path().join("custom-codebuddy");
    write_file(&custom.join("projects/p/s.jsonl"), b"{}\n");
    let context = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("CODEBUDDY_CONFIG_DIR", custom.as_os_str());
    write_file(
        &context
            .home()
            .join(".local/share/CodeBuddyExtension/Data/history/hash/c/index.json"),
        b"{}",
    );
    write_file(
        &context.home().join(".codebuddy/projects/stale/s.jsonl"),
        b"{}\n",
    );

    let report = provider_report(&context, CaptureProvider::CodeBuddy);
    assert_eq!(report.sources.len(), 2);
    assert_eq!(report.sources[0].path, custom);
    assert!(report.sources[1]
        .path
        .ends_with(".local/share/CodeBuddyExtension/Data"));
    assert!(!report
        .sources
        .iter()
        .any(|source| source.path.ends_with(".codebuddy")));
}

#[test]
fn codebuddy_relative_override_uses_captured_cwd_and_other_unix_is_gated() {
    let temp = tempdir();
    let linux = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("CODEBUDDY_CONFIG_DIR", "relative-root");
    assert_eq!(
        provider_report(&linux, CaptureProvider::CodeBuddy).sources[0].path,
        linux.cwd().unwrap().join("relative-root")
    );
    assert!(provider_report(
        &context(temp.path(), DiscoveryPlatform::OtherUnix),
        CaptureProvider::CodeBuddy
    )
    .sources
    .is_empty());
}

#[test]
fn lingma_vscode_profiles_use_winning_keys_without_waylog_guesses() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let first = temp.path().join("lingma-vscode-one");
    let second = temp.path().join("lingma-vscode-two");
    write_lingma_db(&first.join("sharedClientCache/cache/db/local.db"));
    write_lingma_db(&second.join("sharedClientCache/cache/db/local.db"));
    write_file(
            &temp.path().join("platform-config/Code/User/settings.json"),
            format!(
                "{{\"Lingma.LocalMachineStoragePath\":\"/stale\",\"QoderCN.LocalMachineStoragePath\":{:?}}}",
                first.to_string_lossy()
            )
            .as_bytes(),
        );
    write_file(
        &temp
            .path()
            .join("platform-config/Code/User/profiles/p/settings.json"),
        format!(
            "{{\"QoderCN.LocalMachineStoragePath\":{:?}}}",
            second.to_string_lossy()
        )
        .as_bytes(),
    );
    write_lingma_db(
        &context
            .home()
            .join(".lingma/vscode-insiders/sharedClientCache/cache/db/local.db"),
    );

    let report = provider_report(&context, CaptureProvider::Lingma);
    let paths = report
        .sources
        .iter()
        .map(|source| source.path.clone())
        .collect::<Vec<_>>();
    assert!(paths.contains(&first.join("sharedClientCache/cache/db/local.db")));
    assert!(paths.contains(&second.join("sharedClientCache/cache/db/local.db")));
    assert!(!paths
        .iter()
        .any(|path| path.to_string_lossy().contains("vscode-insiders")));
}

#[test]
fn lingma_active_wal_probe_uses_the_authorized_ctx_data_root() {
    let temp = tempdir();
    let selected = temp.path().join("lingma-active-wal");
    let database = selected.join("sharedClientCache/cache/db/local.db");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let writer = Connection::open(&database).unwrap();
    writer
        .execute_batch(
            "create table chat_record (
                session_id text, request_id text, chat_prompt text, summary text,
                error_result text, gmt_create integer, extra text
            );",
        )
        .unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute(
            "insert into chat_record (
                session_id, request_id, chat_prompt, summary,
                error_result, gmt_create, extra
            ) values (?1, ?2, ?3, null, null, 1, null)",
            ("session", "request", "active WAL body"),
        )
        .unwrap();
    write_file(
        &temp.path().join("platform-config/Code/User/settings.json"),
        format!(
            "{{\"QoderCN.LocalMachineStoragePath\":{:?}}}",
            selected.to_string_lossy()
        )
        .as_bytes(),
    );
    let context =
        context(temp.path(), DiscoveryPlatform::Linux).with_data_root(temp.path().join("ctx-data"));

    let report = provider_report(&context, CaptureProvider::Lingma);

    let source = report
        .sources
        .iter()
        .find(|source| source.path == database)
        .unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert!(database.with_file_name("local.db-wal").is_file());
    assert!(database.with_file_name("local.db-shm").is_file());
    drop(writer);
}

#[test]
fn lingma_jetbrains_selects_current_leaf_over_migration_leftover() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let selected = temp.path().join("lingma-jetbrains");
    let current = selected.join("qoder-cn/cache/db/local.db");
    let legacy = selected.join("cache/db/local.db");
    write_lingma_db(&current);
    write_lingma_db(&legacy);
    write_file(
            &temp
                .path()
                .join("platform-config/JetBrains/Idea/options/cosy_setting.xml"),
            format!(
                r#"<application><component name="CosySettings"><option value="{}" name="localStoragePath"/></component></application>"#,
                selected.display()
            )
            .as_bytes(),
        );
    let report = provider_report(&context, CaptureProvider::Lingma);
    assert!(report.sources.iter().any(|source| source.path == current));
    assert!(!report.sources.iter().any(|source| source.path == legacy));
}

#[test]
fn lingma_jetbrains_missing_value_does_not_shift_from_the_next_option() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let default = context
        .home()
        .join(".qoder-cn/shared_client/cache/db/local.db");
    let unrelated = temp.path().join("unrelated");
    write_lingma_db(&default);
    write_lingma_db(&unrelated.join("qoder-cn/cache/db/local.db"));
    write_file(
        &temp
            .path()
            .join("platform-config/JetBrains/Idea/options/cosy_setting.xml"),
        format!(
            r#"<application><component name="CosySettings">
                <option name="localStoragePath"/>
                <option name="unrelated" value="{}"/>
            </component></application>"#,
            unrelated.display()
        )
        .as_bytes(),
    );

    let report = provider_report(&context, CaptureProvider::Lingma);

    assert!(report.sources.iter().any(|source| source.path == default));
    assert!(!report
        .sources
        .iter()
        .any(|source| source.path.starts_with(&unrelated)));
}

#[test]
fn lingma_jetbrains_rejects_multiple_components_instead_of_cross_binding() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let unrelated = temp.path().join("unrelated");
    let selected = temp.path().join("selected");
    let default = context
        .home()
        .join(".qoder-cn/shared_client/cache/db/local.db");
    write_lingma_db(&unrelated.join("qoder-cn/cache/db/local.db"));
    write_lingma_db(&selected.join("qoder-cn/cache/db/local.db"));
    write_lingma_db(&default);
    write_file(
        &temp
            .path()
            .join("platform-config/JetBrains/Idea/options/cosy_setting.xml"),
        format!(
            r#"<application>
                <component name="Unrelated"><option name="localStoragePath" value="{}"/></component>
                <component name="AlsoUnrelated"><option value="shift-decoy"/></component>
                <component name="CosySettings"><option value="{}" name="localStoragePath"/></component>
            </application>"#,
            unrelated.display(),
            selected.display()
        )
        .as_bytes(),
    );

    let report = provider_report(&context, CaptureProvider::Lingma);

    assert!(report.sources.iter().any(|source| source.path == default));
    assert!(!report
        .sources
        .iter()
        .any(|source| source.path.starts_with(&unrelated)));
    assert!(!report
        .sources
        .iter()
        .any(|source| source.path.starts_with(&selected)));
}

#[test]
fn zed_selects_one_xdg_or_platform_winner_and_gates_other_unix() {
    let temp = tempdir();
    let xdg = temp.path().join("xdg-data");
    let linux =
        context(temp.path(), DiscoveryPlatform::Linux).with_env("XDG_DATA_HOME", xdg.as_os_str());
    assert_eq!(
        provider_report(&linux, CaptureProvider::Zed).sources[0].path,
        xdg.join("zed/threads/threads.db")
    );
    let invalid =
        context(temp.path(), DiscoveryPlatform::Linux).with_env("XDG_DATA_HOME", "relative");
    assert_eq!(
        provider_report(&invalid, CaptureProvider::Zed).sources[0].path,
        temp.path().join("platform-data/zed/threads/threads.db")
    );
    assert!(provider_report(
        &context(temp.path(), DiscoveryPlatform::OtherUnix),
        CaptureProvider::Zed
    )
    .sources
    .is_empty());
}

#[test]
fn zed_flatpak_data_home_precedes_xdg_data_home() {
    let temp = tempdir();
    let flatpak = temp.path().join("flatpak-data");
    let xdg = temp.path().join("xdg-data");
    let selected_context = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("FLATPAK_XDG_DATA_HOME", flatpak.as_os_str())
        .with_env("XDG_DATA_HOME", xdg.as_os_str());

    let report = provider_report(&selected_context, CaptureProvider::Zed);
    assert_eq!(
        report.sources[0].path,
        flatpak.join("zed/threads/threads.db")
    );

    for value in ["", "relative-flatpak-data"] {
        let context = context(temp.path(), DiscoveryPlatform::Linux)
            .with_env("FLATPAK_XDG_DATA_HOME", value)
            .with_env("XDG_DATA_HOME", xdg.as_os_str());
        let report = provider_report(&context, CaptureProvider::Zed);
        assert!(report.sources.is_empty());
        assert_eq!(report.issues.len(), 1);
        assert_eq!(
            report.issues[0].kind,
            DiscoveryIssueKind::SelectorUnreconstructible
        );
    }
}

#[test]
fn zed_stateless_has_no_disk_history_root() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux).with_env("ZED_STATELESS", "0");

    let report = provider_report(&context, CaptureProvider::Zed);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].kind, DiscoveryIssueKind::NoDiskHistory);
}

#[test]
fn zed_uses_macos_and_windows_platform_data_directories() {
    let temp = tempdir();
    let mac = context(temp.path(), DiscoveryPlatform::MacOS);
    assert_eq!(
        provider_report(&mac, CaptureProvider::Zed).sources[0].path,
        temp.path().join("platform-data/Zed/threads/threads.db")
    );
    let windows = context(temp.path(), DiscoveryPlatform::Windows);
    assert_eq!(
        provider_report(&windows, CaptureProvider::Zed).sources[0].path,
        temp.path()
            .join("platform-local-data/Zed/threads/threads.db")
    );
}

#[test]
fn lingma_relative_persistent_setting_is_manual_and_suppresses_default() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    write_file(
        &temp.path().join("platform-config/Code/User/settings.json"),
        br#"{"QoderCN.LocalMachineStoragePath":"relative-root"}"#,
    );
    write_lingma_db(
        &context
            .home()
            .join(".lingma/vscode/sharedClientCache/cache/db/local.db"),
    );
    let report = provider_report(&context, CaptureProvider::Lingma);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn lingma_unreadable_base_selector_suppresses_stale_default() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    write_file(
        &temp.path().join("platform-config/Code/User/settings.json"),
        b"{ malformed",
    );
    write_lingma_db(
        &context
            .home()
            .join(".lingma/vscode/sharedClientCache/cache/db/local.db"),
    );

    let report = provider_report(&context, CaptureProvider::Lingma);

    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn lingma_unreadable_base_selector_with_empty_profile_suppresses_stale_default() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    write_file(
        &temp.path().join("platform-config/Code/User/settings.json"),
        b"{ malformed",
    );
    write_file(
        &temp
            .path()
            .join("platform-config/Code/User/profiles/p/settings.json"),
        b"{}",
    );
    write_lingma_db(
        &context
            .home()
            .join(".lingma/vscode/sharedClientCache/cache/db/local.db"),
    );

    let report = provider_report(&context, CaptureProvider::Lingma);

    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[cfg(unix)]
#[test]
fn lingma_linked_base_selector_suppresses_stale_default() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let target = temp.path().join("outside-settings.json");
    write_file(
        &target,
        br#"{"QoderCN.LocalMachineStoragePath":"/outside"}"#,
    );
    let settings = temp.path().join("platform-config/Code/User/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    symlink(&target, &settings).unwrap();
    write_lingma_db(
        &context
            .home()
            .join(".lingma/vscode/sharedClientCache/cache/db/local.db"),
    );

    let report = provider_report(&context, CaptureProvider::Lingma);

    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[cfg(unix)]
#[test]
fn lingma_linked_base_selector_with_empty_profile_suppresses_stale_default() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let target = temp.path().join("outside-settings.json");
    write_file(
        &target,
        br#"{"QoderCN.LocalMachineStoragePath":"/outside"}"#,
    );
    let settings = temp.path().join("platform-config/Code/User/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    symlink(&target, &settings).unwrap();
    write_file(
        &temp
            .path()
            .join("platform-config/Code/User/profiles/p/settings.json"),
        b"{}",
    );
    write_lingma_db(
        &context
            .home()
            .join(".lingma/vscode/sharedClientCache/cache/db/local.db"),
    );

    let report = provider_report(&context, CaptureProvider::Lingma);

    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[cfg(unix)]
#[test]
fn lingma_linked_profile_directory_suppresses_stale_default() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let outside = temp.path().join("outside-profile");
    write_file(&outside.join("settings.json"), b"{}");
    let profiles = temp.path().join("platform-config/Code/User/profiles");
    fs::create_dir_all(&profiles).unwrap();
    symlink(&outside, profiles.join("linked-profile")).unwrap();
    write_lingma_db(
        &context
            .home()
            .join(".lingma/vscode/sharedClientCache/cache/db/local.db"),
    );

    let report = provider_report(&context, CaptureProvider::Lingma);

    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[cfg(unix)]
#[test]
fn lingma_linked_jetbrains_product_suppresses_stale_default() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let outside = temp.path().join("outside-jetbrains");
    fs::create_dir_all(&outside).unwrap();
    let products = temp.path().join("platform-config/JetBrains");
    fs::create_dir_all(&products).unwrap();
    symlink(&outside, products.join("LinkedProduct")).unwrap();
    write_lingma_db(
        &context
            .home()
            .join(".qoder-cn/shared_client/cache/db/local.db"),
    );

    let report = provider_report(&context, CaptureProvider::Lingma);

    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn copilot_home_is_a_single_replacement_and_invalid_values_are_manual() {
    let temp = tempdir();
    let custom = temp.path().join("copilot-home");
    write_file(&custom.join("session-state/s/events.jsonl"), b"{}\n");
    let selected_context =
        context(temp.path(), DiscoveryPlatform::Linux).with_env("COPILOT_HOME", custom.as_os_str());
    let report = provider_report(&selected_context, CaptureProvider::CopilotCli);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, custom.join("session-state"));

    let invalid =
        context(temp.path(), DiscoveryPlatform::Linux).with_env("COPILOT_HOME", "relative");
    let report = provider_report(&invalid, CaptureProvider::CopilotCli);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn trae_uses_current_platform_database_and_gates_unknown_unix() {
    let temp = tempdir();
    let linux = context(temp.path(), DiscoveryPlatform::Linux);
    assert_eq!(
        provider_report(&linux, CaptureProvider::Trae).sources[0].path,
        temp.path()
            .join("platform-config/Trae/ModularData/ai-agent/database.db")
    );

    let xdg = temp.path().join("xdg-config");
    let linux_xdg =
        context(temp.path(), DiscoveryPlatform::Linux).with_env("XDG_CONFIG_HOME", xdg.as_os_str());
    assert_eq!(
        provider_report(&linux_xdg, CaptureProvider::Trae).sources[0].path,
        xdg.join("Trae/ModularData/ai-agent/database.db")
    );

    let mac = context(temp.path(), DiscoveryPlatform::MacOS);
    assert_eq!(
        provider_report(&mac, CaptureProvider::Trae).sources[0].path,
        temp.path()
            .join("platform-data/Trae/ModularData/ai-agent/database.db")
    );

    let windows = context(temp.path(), DiscoveryPlatform::Windows);
    assert_eq!(
        provider_report(&windows, CaptureProvider::Trae).sources[0].path,
        temp.path()
            .join("platform-data/Trae/ModularData/ai-agent/database.db")
    );

    assert!(provider_report(
        &context(temp.path(), DiscoveryPlatform::OtherUnix),
        CaptureProvider::Trae
    )
    .sources
    .is_empty());
}

#[test]
fn trae_current_database_reports_missing_valid_empty_and_malformed_states() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let current = context
        .platform_dirs()
        .config
        .as_ref()
        .unwrap()
        .join("Trae/ModularData/ai-agent/database.db");

    let report = provider_report(&context, CaptureProvider::Trae);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, current);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Missing);

    fs::create_dir_all(current.parent().unwrap()).unwrap();
    let connection = Connection::open(&current).unwrap();
    connection
        .execute(
            "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
    drop(connection);
    let report = provider_report(&context, CaptureProvider::Trae);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Empty);

    let connection = Connection::open(&current).unwrap();
    connection
        .execute(
            "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
            rusqlite::params![
                "memento/icube-ai-agent-storage",
                r#"{"list":[{"id":"input-1","messages":[{"role":"user","content":"trae discovery"}]}]}"#
            ],
        )
        .unwrap();
    drop(connection);
    let report = provider_report(&context, CaptureProvider::Trae);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Available);

    fs::write(&current, b"not sqlite").unwrap();
    let report = provider_report(&context, CaptureProvider::Trae);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Unknown);
}

#[test]
fn trae_current_database_does_not_union_stale_workspace_storage() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::MacOS);
    write_file(
        &context
            .home()
            .join("Library/Application Support/Trae/User/workspaceStorage/w/state.vscdb"),
        b"compatibility",
    );

    let report = provider_report(&context, CaptureProvider::Trae);

    assert_eq!(report.sources.len(), 1);
    assert_eq!(
        report.sources[0].path,
        temp.path()
            .join("platform-data/Trae/ModularData/ai-agent/database.db")
    );
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Missing);
}

#[test]
fn antigravity_accepts_only_official_fixed_transcript_leaves() {
    let temp = tempdir();
    let linux_context = context(temp.path(), DiscoveryPlatform::Linux);
    write_file(
        &linux_context
            .home()
            .join(".gemini/antigravity-cli/brain/c/.system_generated/logs/transcript_full.jsonl"),
        b"{}\n",
    );
    write_file(
        &linux_context
            .home()
            .join(".gemini/antigravity-ide/brain/c/.system_generated/logs/transcript.jsonl"),
        b"{}\n",
    );
    write_file(
        &linux_context
            .home()
            .join(".gemini/antigravity/brain/c/.system_generated/logs/transcript.jsonl"),
        b"{}\n",
    );
    let report = provider_report(&linux_context, CaptureProvider::Antigravity);
    assert_eq!(report.sources.len(), 2);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Empty);
    assert_eq!(report.sources[1].status, ProviderSourceStatus::Available);
    assert!(!report
        .sources
        .iter()
        .any(|source| source.path.to_string_lossy().contains("antigravity/brain")));

    assert!(provider_report(
        &context(temp.path(), DiscoveryPlatform::OtherUnix),
        CaptureProvider::Antigravity
    )
    .sources
    .is_empty());
}

#[test]
fn windsurf_accepts_only_direct_trajectory_jsonl_and_is_platform_gated() {
    let temp = tempdir();
    let selected_context = context(temp.path(), DiscoveryPlatform::Linux);
    let root = selected_context.home().join(".windsurf/transcripts");
    write_file(&root.join("nested/trajectory.jsonl"), b"{}\n");
    let report = provider_report(&selected_context, CaptureProvider::Windsurf);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Empty);
    write_file(&root.join("trajectory.jsonl"), b"{}\n");
    let report = provider_report(&selected_context, CaptureProvider::Windsurf);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Available);
    assert!(provider_report(
        &context(temp.path(), DiscoveryPlatform::OtherUnix),
        CaptureProvider::Windsurf
    )
    .sources
    .is_empty());
}

#[test]
fn fixed_leaf_discovery_is_bounded() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let root = context.home().join(".windsurf/transcripts");
    fs::create_dir_all(&root).unwrap();
    for index in 0..=super::super::super::selectors::MAX_DIRECT_DIRECTORY_ENTRIES {
        fs::create_dir(root.join(format!("entry-{index:04}"))).unwrap();
    }
    let report = provider_report(&context, CaptureProvider::Windsurf);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Unknown);
}

#[cfg(unix)]
#[test]
fn automatic_sources_do_not_follow_symlink_roots() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let target = temp.path().join("outside");
    write_file(&target.join("trajectory.jsonl"), b"{}\n");
    let root = context.home().join(".windsurf/transcripts");
    fs::create_dir_all(root.parent().unwrap()).unwrap();
    symlink(&target, &root).unwrap();
    let report = provider_report(&context, CaptureProvider::Windsurf);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Unsupported);
}

#[cfg(unix)]
#[test]
fn unsafe_automatic_sqlite_targets_are_rejected_before_probing() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let target = temp.path().join("outside.db");
    write_file(&target, b"not opened");
    let source = temp.path().join("platform-data/zed/threads/threads.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    symlink(&target, &source).unwrap();
    super::super::super::probes::reset_default_location_probe_calls();

    let report = provider_report(&context, CaptureProvider::Zed);

    assert_eq!(report.sources[0].status, ProviderSourceStatus::Unsupported);
    assert_eq!(
        super::super::super::probes::default_location_probe_calls(),
        0
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_reparse_automatic_sqlite_targets_are_rejected_before_probing() {
    use std::{io::ErrorKind, os::windows::fs::symlink_file};

    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Windows);
    let target = temp.path().join("outside.db");
    write_file(&target, b"not opened");
    let source = temp
        .path()
        .join("platform-local-data/Zed/threads/threads.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    if let Err(error) = symlink_file(&target, &source) {
        if error.kind() == ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314) {
            return;
        }
        panic!("failed to create Windows file symlink: {error}");
    }
    super::super::super::probes::reset_default_location_probe_calls();

    let report = provider_report(&context, CaptureProvider::Zed);

    assert_eq!(report.sources[0].status, ProviderSourceStatus::Unknown);
    assert_eq!(
        super::super::super::probes::default_location_probe_calls(),
        0
    );
}
