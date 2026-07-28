use super::super::super::{
    context::DiscoveryPlatformDirs, discovery::provider_source_for_path,
    specs::provider_source_spec, types::ProviderImportSupport,
};
use super::super::dedupe_report;
use std::fs;

use super::*;

fn tempdir() -> tempfile::TempDir {
    crate::test_support_paths::tempdir()
        .expect("temporary directory should support resolver fixtures")
}

fn context(root: &Path, platform: DiscoveryPlatform) -> DiscoveryContext {
    let home = root.join("home");
    let cwd = root.join("work");
    let config = root.join("config");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&config).unwrap();
    DiscoveryContext::new(
        home,
        cwd,
        platform,
        DiscoveryPlatformDirs {
            config: Some(config),
            ..DiscoveryPlatformDirs::default()
        },
    )
}

fn spec(provider: CaptureProvider) -> &'static ProviderSourceSpec {
    provider_source_spec(provider).expect("owned provider must have a source spec")
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn source<'a>(report: &'a DiscoveryReport, format: &str) -> &'a ProviderSource {
    report
        .sources
        .iter()
        .find(|source| source.source_format == format)
        .expect("expected source format")
}

#[test]
fn qoder_custom_sdk_root_is_manual_and_suppresses_all_default_reporting() {
    let temp = tempdir();
    let discovery_context = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("QODER_CONFIG_DIR", temp.path().join("sdk-root"));
    let projects = discovery_context.home().join(".qoder/projects");
    write(&projects.join("legacy/transcript/legacy.jsonl"), b"{}\n");
    write(&projects.join("current/current.jsonl"), b"{}\n");
    write(
        &temp.path().join("sdk-root/projects/custom/custom.jsonl"),
        b"{}\n",
    );
    let report = resolve(&discovery_context, spec(CaptureProvider::Qoder));
    assert!(report.sources.is_empty());
    assert_eq!(
        (report.issues[0].kind, report.issues[0].path.as_deref()),
        (
            DiscoveryIssueKind::SelectorUnreconstructible,
            Some(temp.path().join("sdk-root/projects").as_path())
        )
    );
    assert_eq!(report.issues[0].reason, "QODER_CONFIG_DIR is SDK-scoped and not a registered standalone writer root; use its exact projects path with --path");
    assert!(!report
        .issues
        .iter()
        .any(|issue| issue.path.as_deref() == Some(projects.as_path())));

    let nonempty_relative =
        context(temp.path(), DiscoveryPlatform::Linux).with_env("QODER_CONFIG_DIR", " ");
    let report = resolve(&nonempty_relative, spec(CaptureProvider::Qoder));
    assert!(report.sources.is_empty());
    assert_eq!(
        (report.issues.len(), report.issues[0].path.as_ref()),
        (1, None)
    );
}

#[test]
fn qoder_probe_is_shallow_bounded_and_deterministic() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let projects = context.home().join(".qoder/projects");
    for index in 0..=MAX_DIRECT_DIRECTORY_ENTRIES {
        fs::create_dir_all(projects.join(format!("bucket-{index:04}"))).unwrap();
    }
    let report = resolve(&context, spec(CaptureProvider::Qoder));
    assert_eq!(
        (report.sources.len(), report.sources[0].status),
        (1, ProviderSourceStatus::Unknown)
    );
}

#[test]
fn factory_and_firebender_emit_only_insufficient_evidence_issues() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let factory_path = context.home().join(".factory/sessions");
    write(&factory_path.join("session.jsonl"), b"{}\n");
    write(
        &context
            .cwd()
            .unwrap()
            .join(".idea/firebender/chat_history.db"),
        b"not consulted",
    );
    for provider in [CaptureProvider::FactoryAiDroid, CaptureProvider::Firebender] {
        let report = resolve(&context, spec(provider));
        assert!(report.sources.is_empty());
        assert_eq!(
            (report.issues.len(), report.issues[0].kind),
            (1, DiscoveryIssueKind::InsufficientOfficialEvidence)
        );
    }
    assert_eq!(
        resolve(&context, spec(CaptureProvider::FactoryAiDroid)).issues[0]
            .path
            .as_deref(),
        Some(factory_path.as_path())
    );
    assert_eq!(
        resolve(&context, spec(CaptureProvider::Firebender)).issues[0].path,
        None
    );
}

#[test]
fn auggie_uses_only_the_fixed_sessions_default_on_supported_platforms() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let session = context.home().join(".augment/sessions/session.json");
    write(&session, br#"{"sessionId":"one","chatHistory":[]}"#);
    let before = fs::read(&session).unwrap();
    let report = resolve(&context, spec(CaptureProvider::Auggie));
    assert_eq!(
        (&report.sources[0].path, report.sources[0].status),
        (
            &context.home().join(".augment/sessions"),
            ProviderSourceStatus::Available
        )
    );
    assert_eq!(fs::read(session).unwrap(), before);
    let other = DiscoveryContext::new(
        context.home(),
        context.cwd().unwrap(),
        DiscoveryPlatform::OtherUnix,
        DiscoveryPlatformDirs::default(),
    );
    assert!(resolve(&other, spec(CaptureProvider::Auggie))
        .sources
        .is_empty());
}

#[test]
fn deepagents_selects_current_over_legacy_and_legacy_only_when_present() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let current = context.home().join(".deepagents/.state/sessions.db");
    let legacy = context.home().join(".deepagents/sessions.db");
    write(&current, b"current");
    write(&legacy, b"legacy");
    assert_eq!(
        resolve(&context, spec(CaptureProvider::DeepAgents)).sources[0].path,
        current
    );
    fs::remove_file(&current).unwrap();
    assert_eq!(
        resolve(&context, spec(CaptureProvider::DeepAgents)).sources[0].path,
        legacy
    );
    fs::remove_file(&legacy).unwrap();
    let report = resolve(&context, spec(CaptureProvider::DeepAgents));
    assert_eq!(
        (&report.sources[0].path, report.sources[0].status),
        (&current, ProviderSourceStatus::Missing)
    );
}

#[cfg(unix)]
#[test]
fn linked_selected_paths_are_not_followed_or_replaced_by_stale_fallbacks() {
    use std::os::unix::fs::symlink;
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let outside = temp.path().join("outside");
    write(&outside.join("session.json"), b"{}");
    fs::create_dir_all(context.home().join(".augment")).unwrap();
    symlink(&outside, context.home().join(".augment/sessions")).unwrap();
    assert_eq!(
        resolve(&context, spec(CaptureProvider::Auggie)).sources[0].status,
        ProviderSourceStatus::Unknown
    );
    let target = temp.path().join("current.db");
    write(&target, b"current");
    let current = context.home().join(".deepagents/.state/sessions.db");
    fs::create_dir_all(current.parent().unwrap()).unwrap();
    symlink(&target, &current).unwrap();
    write(&context.home().join(".deepagents/sessions.db"), b"legacy");
    let report = resolve(&context, spec(CaptureProvider::DeepAgents));
    assert_eq!(
        (
            &report.sources[0].path,
            report.sources[0].status,
            report.issues.len()
        ),
        (&current, ProviderSourceStatus::Unknown, 1)
    );
}

#[test]
fn mux_root_is_one_raw_winner_and_archive_is_detection_only() {
    let temp = tempdir();
    let custom = temp.path().join("custom-mux");
    let context =
        context(temp.path(), DiscoveryPlatform::Linux).with_env("MUX_ROOT", custom.as_os_str());
    write(&custom.join("sessions/workspace/chat.jsonl"), b"{}\n");
    write(
        &custom.join("sessions/workspace/chat-archive.jsonl"),
        b"{}\n",
    );
    write(
        &context.home().join(".mux/sessions/stale/chat.jsonl"),
        b"{}\n",
    );
    let report = resolve(&context, spec(CaptureProvider::Mux));
    let supported = source(&report, "mux_session_jsonl_tree");
    let unsupported = source(&report, "unsupported");
    assert_eq!(
        (&supported.path, supported.status),
        (&custom.join("sessions"), ProviderSourceStatus::Available)
    );
    assert_eq!(
        (&unsupported.path, unsupported.unsupported_reason),
        (&supported.path, Some(MUX_ARCHIVE_UNSUPPORTED))
    );
    assert!(report
        .sources
        .iter()
        .all(|item| !item.path.starts_with(context.home().join(".mux"))));
}

#[test]
fn mux_development_root_requires_exact_node_env_value() {
    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux);
    let development = base.clone().with_env("NODE_ENV", "development");
    assert_eq!(
        resolve(&development, spec(CaptureProvider::Mux)).sources[0].path,
        base.home().join(".mux-dev/sessions")
    );
    let other_case = base.clone().with_env("NODE_ENV", "Development");
    assert_eq!(
        resolve(&other_case, spec(CaptureProvider::Mux)).sources[0].path,
        base.home().join(".mux/sessions")
    );
}

#[test]
fn mux_empty_relative_and_pre_migration_selection_preserve_precedence() {
    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux);
    write(
        &base.home().join(".mux/sessions/normal/chat.jsonl"),
        b"{}\n",
    );
    assert_eq!(
        resolve(
            &base.clone().with_env("MUX_ROOT", ""),
            spec(CaptureProvider::Mux)
        )
        .sources[0]
            .path,
        base.home().join(".mux/sessions")
    );
    let relative = base.clone().with_env("MUX_ROOT", "  relative root  ");
    write(
        &base
            .cwd()
            .unwrap()
            .join("  relative root  /sessions/one/chat.jsonl"),
        b"{}\n",
    );
    assert_eq!(
        resolve(&relative, spec(CaptureProvider::Mux)).sources[0].path,
        base.cwd().unwrap().join("  relative root  /sessions")
    );
    let legacy_temp = tempdir();
    let legacy = context(legacy_temp.path(), DiscoveryPlatform::Linux);
    write(
        &legacy.home().join(".cmux/sessions/old/chat.jsonl"),
        b"{}\n",
    );
    assert_eq!(
        resolve(&legacy, spec(CaptureProvider::Mux)).sources[0].path,
        legacy.home().join(".cmux/sessions")
    );
    fs::create_dir_all(legacy.home().join(".mux")).unwrap();
    assert_eq!(
        resolve(&legacy, spec(CaptureProvider::Mux)).sources[0].path,
        legacy.home().join(".mux/sessions")
    );
}

#[test]
fn cline_selects_one_owned_legacy_root_and_only_installed_microsoft_hosts() {
    let temp = tempdir();
    let selected = temp.path().join("selected-cline-data");
    let context = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("CLINE_DATA_DIR", selected.as_os_str());
    write(
        &selected.join("tasks/owned/api_conversation_history.json"),
        b"[]",
    );
    write(
        &context
            .home()
            .join(".cline/data/tasks/stale/ui_messages.json"),
        b"[]",
    );
    let config = context.platform_dirs().config.as_ref().unwrap();
    let code = config.join("Code/User/globalStorage/saoudrizwan.claude-dev");
    let profile = config.join("Code/User/profiles/profile-a/globalStorage/saoudrizwan.claude-dev");
    write(&code.join("tasks/code/task_metadata.json"), b"{}");
    write(&profile.join("tasks/profile/ui_messages.json"), b"[]");
    write(
        &config
            .join("Cursor/User/globalStorage/saoudrizwan.claude-dev/tasks/nope/ui_messages.json"),
        b"[]",
    );
    let report = resolve(&context, spec(CaptureProvider::Cline));
    let native = report
        .sources
        .iter()
        .filter(|item| item.source_format == "cline_task_directory_json")
        .collect::<Vec<_>>();
    assert_eq!(native.len(), 3);
    assert_eq!(native[0].path, selected);
    assert!(
        native.iter().any(|item| item.path == code)
            && native.iter().any(|item| item.path == profile)
    );
    assert!(native
        .iter()
        .all(|item| !item.path.starts_with(config.join("Cursor"))
            && !item.path.starts_with(context.home().join(".cline"))));
}

#[test]
fn cline_enabled_sandbox_selects_its_exact_data_root() {
    let temp = tempdir();
    let selected = temp.path().join("sandbox-data");
    let context = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("CLINE_SANDBOX", " 1 ")
        .with_env("CLINE_SANDBOX_DATA_DIR", &selected);
    write(&selected.join("tasks/owned/ui_messages.json"), b"[]");
    let report = resolve(&context, spec(CaptureProvider::Cline));
    assert_eq!(source(&report, "cline_task_directory_json").path, selected);
}

#[test]
fn cline_disabled_sandbox_ignores_its_data_root() {
    let temp = tempdir();
    let sandbox = temp.path().join("sandbox-data");
    let legacy = temp.path().join("legacy");
    let context = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("CLINE_SANDBOX", "0")
        .with_env("CLINE_SANDBOX_DATA_DIR", &sandbox)
        .with_env("CLINE_DIR", &legacy);
    write(&sandbox.join("tasks/ignored/ui_messages.json"), b"[]");
    let report = resolve(&context, spec(CaptureProvider::Cline));
    assert_eq!(
        source(&report, "cline_task_directory_json").path,
        legacy.join("data")
    );
}

#[test]
fn cline_data_dir_precedes_sandbox_and_cline_dir() {
    let temp = tempdir();
    let selected = temp.path().join("selected");
    let context = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("CLINE_DATA_DIR", &selected)
        .with_env("CLINE_SANDBOX", "1")
        .with_env("CLINE_SANDBOX_DATA_DIR", temp.path().join("sandbox"))
        .with_env("CLINE_DIR", temp.path().join("legacy"));
    assert_eq!(
        source(
            &resolve(&context, spec(CaptureProvider::Cline)),
            "cline_task_directory_json"
        )
        .path,
        selected
    );
}

#[test]
fn cline_sandbox_root_is_cwd_relative_without_tilde_expansion() {
    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux);
    for (raw, expected) in [
        (
            "relative-sandbox",
            base.cwd().unwrap().join("relative-sandbox"),
        ),
        ("~/sandbox", base.cwd().unwrap().join("~/sandbox")),
    ] {
        let context = base
            .clone()
            .with_env("CLINE_SANDBOX", "1")
            .with_env("CLINE_SANDBOX_DATA_DIR", raw);
        assert_eq!(
            source(
                &resolve(&context, spec(CaptureProvider::Cline)),
                "cline_task_directory_json"
            )
            .path,
            expected
        );
    }
}

#[test]
fn cline_blank_sandbox_path_falls_back_but_unreconstructible_path_does_not() {
    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("CLINE_SANDBOX", "1")
        .with_env("CLINE_SANDBOX_DATA_DIR", "   ");
    assert_eq!(
        source(
            &resolve(&base, spec(CaptureProvider::Cline)),
            "cline_task_directory_json"
        )
        .path,
        base.home().join(".cline/data")
    );

    let home = temp.path().join("no-cwd-home");
    fs::create_dir_all(&home).unwrap();
    let invalid = DiscoveryContext::without_cwd(
        &home,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_env("CLINE_SANDBOX", "1")
    .with_env("CLINE_SANDBOX_DATA_DIR", "relative");
    let report = resolve(&invalid, spec(CaptureProvider::Cline));
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn cline_detects_current_sdk_roots_without_mapping_them_to_task_json() {
    let temp = tempdir();
    let selected = temp.path().join("selected");
    let sessions = temp.path().join("sdk-sessions");
    let db = temp.path().join("sdk-db");
    let context = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("CLINE_DATA_DIR", selected.as_os_str())
        .with_env("CLINE_SESSION_DATA_DIR", sessions.as_os_str())
        .with_env("CLINE_DB_DATA_DIR", db.as_os_str());
    write(
        &selected.join("tasks/legacy/api_conversation_history.json"),
        b"[]",
    );
    write(&sessions.join("abc/abc.json"), b"{}");
    write(&sessions.join("abc/abc.messages.json"), b"[]");
    write(&db.join("sessions.db"), b"admission-only");
    let report = resolve(&context, spec(CaptureProvider::Cline));
    let unsupported = report
        .sources
        .iter()
        .filter(|item| item.status == ProviderSourceStatus::Unsupported)
        .collect::<Vec<_>>();
    assert_eq!(unsupported.len(), 2);
    assert!(
        unsupported.iter().any(|item| item.path == sessions)
            && unsupported
                .iter()
                .any(|item| item.path == db.join("sessions.db"))
    );
    assert!(report
        .sources
        .iter()
        .all(|item| item.source_format != "cline_task_directory_json"
            || (item.path != sessions && item.path != db)));
}

#[test]
fn cline_probe_rejects_context_only_compatibility_false_positive() {
    let temp = tempdir();
    let selected = temp.path().join("selected");
    let context = context(temp.path(), DiscoveryPlatform::Linux)
        .with_env("CLINE_DATA_DIR", selected.as_os_str());
    write(&selected.join("tasks/task/context_history.json"), b"[]");
    assert_eq!(
        source(
            &resolve(&context, spec(CaptureProvider::Cline)),
            "cline_task_directory_json"
        )
        .status,
        ProviderSourceStatus::Empty
    );
}

#[test]
fn cline_profile_enumeration_is_finite_and_sorted() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let user = context
        .platform_dirs()
        .config
        .as_ref()
        .unwrap()
        .join("Code/User");
    let profiles = user.join("profiles");
    fs::create_dir_all(&user).unwrap();
    for index in 0..(MAX_FINITE_SELECTOR_ENTRIES + 1) {
        fs::create_dir_all(profiles.join(format!("profile-{index:03}"))).unwrap();
    }
    let report = resolve(&context, spec(CaptureProvider::Cline));
    let profiles = report
        .sources
        .iter()
        .filter(|item| {
            item.path
                .components()
                .any(|part| part.as_os_str() == "profiles")
        })
        .collect::<Vec<_>>();
    assert_eq!(profiles.len(), MAX_FINITE_SELECTOR_ENTRIES);
    assert!(profiles.windows(2).all(|pair| pair[0].path < pair[1].path));
}

#[test]
fn supported_exact_paths_match_explicit_source_identity_inputs() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    write(
        &context.home().join(".qoder/projects/p/transcript/s.jsonl"),
        b"{}\n",
    );
    write(&context.home().join(".augment/sessions/s.json"), b"{}");
    write(
        &context.home().join(".deepagents/.state/sessions.db"),
        b"db",
    );
    write(&context.home().join(".mux/sessions/w/chat.jsonl"), b"{}\n");
    write(
        &context
            .home()
            .join(".cline/data/tasks/t/api_conversation_history.json"),
        b"[]",
    );
    for provider in [
        CaptureProvider::Qoder,
        CaptureProvider::Auggie,
        CaptureProvider::DeepAgents,
        CaptureProvider::Mux,
        CaptureProvider::Cline,
    ] {
        let report = dedupe_report(resolve(&context, spec(provider)));
        let automatic = report
            .sources
            .iter()
            .find(|item| {
                item.import_support == ProviderImportSupport::Native
                    && item.status == ProviderSourceStatus::Available
            })
            .unwrap();
        let explicit = provider_source_for_path(provider, automatic.path.clone());
        assert_eq!(
            (
                explicit.provider,
                &explicit.path,
                explicit.source_format,
                explicit.import_support,
                explicit.catalog_support
            ),
            (
                automatic.provider,
                &automatic.path,
                automatic.source_format,
                automatic.import_support,
                automatic.catalog_support
            )
        );
    }
}

#[test]
fn factory_and_firebender_explicit_compatibility_routes_remain_supported() {
    let temp = tempdir();
    let factory_path = temp.path().join("factory/session.jsonl");
    let firebender_path = temp.path().join("project/.idea/firebender/chat_history.db");
    write(&factory_path, b"{}\n");
    write(&firebender_path, b"db");
    let factory = provider_source_for_path(CaptureProvider::FactoryAiDroid, factory_path);
    let firebender = provider_source_for_path(CaptureProvider::Firebender, firebender_path);
    assert_eq!(
        (factory.import_support, factory.source_format),
        (
            ProviderImportSupport::Native,
            "factory_ai_droid_sessions_jsonl"
        )
    );
    assert_eq!(
        (firebender.import_support, firebender.source_format),
        (
            ProviderImportSupport::Native,
            "firebender_chat_history_sqlite"
        )
    );
}
