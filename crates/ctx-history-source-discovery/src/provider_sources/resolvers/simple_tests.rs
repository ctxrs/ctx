use super::super::super::{
    context::DiscoveryPlatformDirs,
    types::{DiscoveryIssueKind, ProviderSourceStatus},
};
use std::fs;

use super::*;
use crate::test_support_paths;

fn tempdir() -> tempfile::TempDir {
    test_support_paths::tempdir()
        .expect("system temporary directory should support simple resolver fixtures")
}

fn context(temp: &tempfile::TempDir, platform: DiscoveryPlatform) -> DiscoveryContext {
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    DiscoveryContext::new(home, cwd, platform, DiscoveryPlatformDirs::default())
}

fn resolve_provider(context: &DiscoveryContext, provider: CaptureProvider) -> DiscoveryReport {
    crate::provider_sources::discover_provider_sources_for_provider_with_context(
        &crate::provider_sources::TEST_PROVIDER_PROBES,
        context,
        provider,
    )
}

fn provider_source_for_path(
    provider: CaptureProvider,
    path: PathBuf,
) -> crate::provider_sources::ProviderSource {
    crate::provider_sources::provider_source_for_path(
        &crate::provider_sources::TEST_PROVIDER_PROBES,
        provider,
        path,
    )
}

fn paths(report: &DiscoveryReport) -> Vec<PathBuf> {
    report
        .sources
        .iter()
        .map(|source| source.path.clone())
        .collect()
}

#[test]
fn codex_official_root_includes_active_and_compressed_archive_history() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let custom = temp.path().join("custom-codex");
    fs::create_dir_all(custom.join("sessions/2026/07/21")).unwrap();
    fs::create_dir_all(custom.join("archived_sessions")).unwrap();
    fs::write(custom.join("sessions/2026/07/21/rollout.jsonl"), "{}\n").unwrap();
    fs::write(
        custom.join("archived_sessions/z-last.jsonl.zst"),
        "compressed",
    )
    .unwrap();
    fs::write(
        custom.join("archived_sessions/a-first.jsonl.zst"),
        "compressed",
    )
    .unwrap();
    fs::write(custom.join("history.jsonl"), "{}\n").unwrap();
    fs::create_dir_all(base.home().join(".codex/sessions")).unwrap();
    fs::write(base.home().join(".codex/sessions/stale.jsonl"), "{}\n").unwrap();

    let selected_root = custom;
    let report = resolve_provider(
        &base
            .clone()
            .with_env("CODEX_HOME", selected_root.as_os_str()),
        CaptureProvider::Codex,
    );
    assert_eq!(report.sources.len(), 3);
    assert_eq!(report.sources[0].path, selected_root.join("sessions"));
    assert_eq!(
        report.sources[1].path,
        selected_root.join("archived_sessions")
    );
    assert_eq!(report.sources[2].path, selected_root.join("history.jsonl"));
    assert_eq!(report.sources[1].status, ProviderSourceStatus::Available);
    assert!(report
        .sources
        .iter()
        .all(|source| !source.path.starts_with(base.home().join(".codex"))));

    let missing = temp.path().join("missing-codex");
    let invalid = resolve_provider(
        &base.clone().with_env("CODEX_HOME", missing.as_os_str()),
        CaptureProvider::Codex,
    );
    assert!(invalid.sources.is_empty());
    assert_eq!(
        invalid.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn configured_codex_roots_add_to_scalar_selection_and_expand_independently() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let personal = temp.path().join("codex-personal");
    let work = temp.path().join("codex-work");
    let ignored_env = temp.path().join("codex-env");
    for root in [&personal, &work, &ignored_env] {
        fs::create_dir_all(root.join("sessions")).unwrap();
    }
    let configured = vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Codex,
            path: personal.clone(),
            group: Some("personal".to_owned()),
            kind: None,
        },
        ctx_history_capture_model::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Codex,
            path: work.clone(),
            group: Some("work".to_owned()),
            kind: None,
        },
    ];
    let report = resolve_provider(
        &base
            .with_env("CODEX_HOME", ignored_env.as_os_str())
            .with_configured_provider_roots(configured),
        CaptureProvider::Codex,
    );

    assert_eq!(
        paths(&report),
        vec![
            ignored_env.join("sessions"),
            ignored_env.join("archived_sessions"),
            ignored_env.join("history.jsonl"),
            personal.join("sessions"),
            personal.join("archived_sessions"),
            personal.join("history.jsonl"),
            work.join("sessions"),
            work.join("archived_sessions"),
            work.join("history.jsonl"),
        ]
    );
    assert!(report
        .sources
        .iter()
        .any(|source| source.path.starts_with(&ignored_env)));
}

#[test]
fn configured_claude_roots_add_to_automatic_discovery() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let claude_personal = temp.path().join("claude-personal");
    let claude_work = temp.path().join("claude-work");
    let codex_default = base.home().join(".codex");
    fs::create_dir_all(claude_personal.join("projects")).unwrap();
    fs::create_dir_all(claude_work.join("projects")).unwrap();
    fs::create_dir_all(codex_default.join("sessions")).unwrap();
    let configured = vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: claude_personal.clone(),
            group: Some("personal".to_owned()),
            kind: None,
        },
        ctx_history_capture_model::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Claude,
            path: claude_work.clone(),
            group: Some("work".to_owned()),
            kind: None,
        },
    ];
    let context = base.clone().with_configured_provider_roots(configured);

    assert_eq!(
        paths(&resolve_provider(&context, CaptureProvider::Claude)),
        vec![
            base.home().join(".claude/projects"),
            claude_personal.join("projects"),
            claude_work.join("projects")
        ]
    );
    assert_eq!(
        paths(&resolve_provider(&context, CaptureProvider::Codex)),
        vec![
            codex_default.join("sessions"),
            codex_default.join("archived_sessions"),
            codex_default.join("history.jsonl"),
        ]
    );
}

#[test]
fn distinct_configured_root_ids_cannot_share_one_physical_root() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let shared = temp.path().join("claude-shared");
    fs::create_dir_all(shared.join("projects")).unwrap();
    let definition = |id: &str| ctx_history_capture_model::ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::Claude,
        path: shared.clone(),
        group: None,
        kind: None,
    };
    let report = resolve_provider(
        &base
            .with_automatic_provider_discovery(false)
            .with_configured_provider_roots(vec![definition("personal"), definition("work")]),
        CaptureProvider::Claude,
    );

    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::ConfiguredRootConflict
    );
}

#[test]
fn global_automatic_disable_keeps_only_named_provider_roots() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let automatic_claude = base.home().join(".claude/projects");
    let automatic_codex = base.home().join(".codex/sessions");
    let named_claude = temp.path().join("claude-personal");
    fs::create_dir_all(&automatic_claude).unwrap();
    fs::create_dir_all(&automatic_codex).unwrap();
    fs::create_dir_all(named_claude.join("projects")).unwrap();

    let context = base
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: named_claude.clone(),
            group: Some("personal".to_owned()),
            kind: None,
        }]);

    assert_eq!(
        paths(&resolve_provider(&context, CaptureProvider::Claude)),
        vec![named_claude.join("projects")]
    );
    assert!(resolve_provider(&context, CaptureProvider::Codex)
        .sources
        .is_empty());
    assert!(crate::discover_provider_sources_for_provider_with_context(
        &crate::provider_sources::TEST_PROVIDER_PROBES,
        &context,
        CaptureProvider::Gemini,
    )
    .sources
    .is_empty());
}

#[test]
fn naming_the_automatic_home_deduplicates_the_physical_source() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let home = base.home().join(".claude");
    fs::create_dir_all(home.join("projects")).unwrap();
    let context = base.with_configured_provider_roots(vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: home.clone(),
            group: Some("personal".to_owned()),
            kind: None,
        },
    ]);

    let report = crate::discover_provider_sources_for_provider_with_context(
        &crate::provider_sources::TEST_PROVIDER_PROBES,
        &context,
        CaptureProvider::Claude,
    );

    assert_eq!(paths(&report), vec![home.join("projects")]);
}

#[cfg(unix)]
#[test]
fn naming_the_automatic_home_through_a_symlink_deduplicates_the_physical_source() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let home = base.home().join(".claude");
    let alias = temp.path().join("claude-alias");
    fs::create_dir_all(home.join("projects")).unwrap();
    symlink(&home, &alias).unwrap();
    let context = base.with_configured_provider_roots(vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: alias,
            group: Some("personal".to_owned()),
            kind: None,
        },
    ]);

    let report = crate::discover_provider_sources_for_provider_with_context(
        &crate::provider_sources::TEST_PROVIDER_PROBES,
        &context,
        CaptureProvider::Claude,
    );

    assert_eq!(paths(&report), vec![home.join("projects")]);
}

#[cfg(unix)]
#[test]
fn configured_roots_reject_present_roots_of_the_wrong_kind() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let unavailable_claude = temp.path().join("claude-unavailable");
    let unavailable_codex = temp.path().join("codex-unavailable");
    fs::write(&unavailable_claude, b"not a directory").unwrap();
    fs::write(&unavailable_codex, b"not a directory").unwrap();
    let configured = vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "claude-unavailable".to_owned(),
            provider: CaptureProvider::Claude,
            path: unavailable_claude.clone(),
            group: None,
            kind: None,
        },
        ctx_history_capture_model::ProviderRootDefinition {
            id: "codex-unavailable".to_owned(),
            provider: CaptureProvider::Codex,
            path: unavailable_codex.clone(),
            group: None,
            kind: None,
        },
    ];
    let context = base
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(configured);

    let claude = resolve_provider(&context, CaptureProvider::Claude);
    assert!(claude.sources.is_empty());
    assert_eq!(claude.issues.len(), 1);

    let codex = resolve_provider(&context, CaptureProvider::Codex);
    assert!(codex.sources.is_empty());
    assert_eq!(codex.issues.len(), 1);
}

#[test]
fn grok_build_absolute_home_override_replaces_default_and_selects_updates() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let default_session = base.home().join(".grok/sessions/default-session");
    fs::create_dir_all(&default_session).unwrap();
    fs::write(default_session.join("summary.json"), "{}\n").unwrap();
    fs::write(default_session.join("updates.jsonl"), "{}\n").unwrap();

    let selected_root = temp.path().join("selected-grok");
    let selected_session = selected_root.join("sessions/selected-session");
    fs::create_dir_all(&selected_session).unwrap();
    fs::write(selected_session.join("summary.json"), "{}\n").unwrap();
    fs::write(selected_session.join("updates.jsonl"), "{}\n").unwrap();
    fs::write(selected_session.join("chat_history.jsonl"), "{}\n").unwrap();

    let report = resolve_provider(
        &base
            .clone()
            .with_env("GROK_HOME", selected_root.as_os_str()),
        CaptureProvider::GrokBuild,
    );
    assert_eq!(paths(&report), [selected_root.join("sessions")]);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Available);
    assert_eq!(
        report.sources[0].source_format,
        "grok_build_session_updates_jsonl_tree"
    );
    assert!(!report.sources[0]
        .path
        .starts_with(base.home().join(".grok")));
}

#[test]
fn grok_build_home_override_must_be_nonempty_and_absolute() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);

    let relative = resolve_provider(
        &base.clone().with_env("GROK_HOME", "relative-grok-home"),
        CaptureProvider::GrokBuild,
    );
    assert!(relative.sources.is_empty());
    assert_eq!(
        relative.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );

    let fallback = resolve_provider(
        &base.clone().with_env("GROK_HOME", ""),
        CaptureProvider::GrokBuild,
    );
    assert!(fallback.sources.is_empty());
    assert_eq!(
        fallback.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn grok_build_discovery_uses_authoritative_updates_without_sidecars() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let sessions = base.home().join(".grok/sessions");
    let stale = sessions.join("sidecars-only-session");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("summary.json"), "{}\n").unwrap();
    fs::write(stale.join("chat_history.jsonl"), "{}\n").unwrap();

    let without_updates = resolve_provider(&base, CaptureProvider::GrokBuild);
    assert_eq!(
        without_updates.sources[0].status,
        ProviderSourceStatus::Empty
    );

    let updates_only = sessions.join("updates-only-session");
    fs::create_dir_all(&updates_only).unwrap();
    fs::write(updates_only.join("updates.jsonl"), "{}\n").unwrap();
    let with_updates = resolve_provider(&base, CaptureProvider::GrokBuild);
    assert_eq!(
        with_updates.sources[0].status,
        ProviderSourceStatus::Available
    );
}

#[test]
fn deepseek_harness_absolute_home_override_replaces_default() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let default_leaf = base
        .home()
        .join(".dsh/sessions/default-workspace/default-session/session.jsonl.zstd");
    fs::create_dir_all(default_leaf.parent().unwrap()).unwrap();
    fs::write(&default_leaf, b"compressed").unwrap();

    let selected_root = temp.path().join("selected-dsh");
    let selected_leaf =
        selected_root.join("sessions/selected-workspace/selected-session/session.jsonl.zstd");
    fs::create_dir_all(selected_leaf.parent().unwrap()).unwrap();
    fs::write(&selected_leaf, b"compressed").unwrap();

    let report = resolve_provider(
        &base.clone().with_env("DSH_HOME", selected_root.as_os_str()),
        CaptureProvider::DeepSeekHarness,
    );
    assert_eq!(paths(&report), [selected_root.join("sessions")]);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Available);
    assert_eq!(
        report.sources[0].source_format,
        "deepseek_harness_session_jsonl_tree"
    );
    assert!(!report.sources[0].path.starts_with(base.home().join(".dsh")));
}

#[test]
fn deepseek_harness_empty_home_is_unset_but_relative_home_is_manual() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let default_leaf = base
        .home()
        .join(".dsh/sessions/workspace/session/session.jsonl.zstd");
    fs::create_dir_all(default_leaf.parent().unwrap()).unwrap();
    fs::write(default_leaf, b"compressed").unwrap();

    for unset in ["", "  \t "] {
        let report = resolve_provider(
            &base.clone().with_env("DSH_HOME", unset),
            CaptureProvider::DeepSeekHarness,
        );
        assert_eq!(paths(&report), [base.home().join(".dsh/sessions")]);
        assert_eq!(report.sources[0].status, ProviderSourceStatus::Available);
    }

    let relative = resolve_provider(
        &base.clone().with_env("DSH_HOME", "relative-dsh-home"),
        CaptureProvider::DeepSeekHarness,
    );
    assert!(relative.sources.is_empty());
    assert_eq!(
        relative.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn deepseek_harness_probe_requires_exact_nested_session_leaf() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let sessions = base.home().join(".dsh/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("session.jsonl.zstd"), b"wrong depth").unwrap();
    fs::create_dir_all(sessions.join("workspace/session/extra")).unwrap();
    fs::write(
        sessions.join("workspace/session/extra/session.jsonl.zstd"),
        b"wrong depth",
    )
    .unwrap();

    let without_leaf = resolve_provider(&base, CaptureProvider::DeepSeekHarness);
    assert_eq!(without_leaf.sources[0].status, ProviderSourceStatus::Empty);

    fs::write(
        sessions.join("workspace/session/session.jsonl"),
        b"raw configured history",
    )
    .unwrap();
    let with_raw_leaf = resolve_provider(&base, CaptureProvider::DeepSeekHarness);
    assert_eq!(
        with_raw_leaf.sources[0].status,
        ProviderSourceStatus::Available
    );

    let explicit = provider_source_for_path(
        CaptureProvider::DeepSeekHarness,
        sessions.join("workspace/session/session.jsonl"),
    );
    assert_eq!(explicit.source_format, "deepseek_harness_session_jsonl");
    assert_eq!(explicit.status, ProviderSourceStatus::Available);

    let explicit_zstd = sessions.join("workspace/session/session.jsonl.zstd");
    fs::write(&explicit_zstd, b"compressed").unwrap();
    assert_eq!(
        provider_source_for_path(CaptureProvider::DeepSeekHarness, explicit_zstd).status,
        ProviderSourceStatus::Available
    );

    let arbitrary = sessions.join("workspace/session/notes.jsonl");
    fs::write(&arbitrary, b"not a native leaf").unwrap();
    let arbitrary = provider_source_for_path(CaptureProvider::DeepSeekHarness, arbitrary);
    assert_eq!(arbitrary.status, ProviderSourceStatus::Unsupported);
    assert_eq!(arbitrary.source_format, "unsupported");

    let empty = temp.path().join("empty-dsh-sessions");
    fs::create_dir_all(&empty).unwrap();
    let empty = provider_source_for_path(CaptureProvider::DeepSeekHarness, empty);
    assert_eq!(empty.status, ProviderSourceStatus::Unsupported);
    assert_eq!(empty.source_format, "unsupported");
}

#[cfg(unix)]
#[test]
fn codex_and_other_selected_root_symlinks_require_manual_paths() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let real = temp.path().join("real-root");
    let alias = temp.path().join("root-alias");
    fs::create_dir_all(real.join("sessions")).unwrap();
    symlink(&real, &alias).unwrap();

    let codex = resolve_provider(
        &base.clone().with_env("CODEX_HOME", alias.as_os_str()),
        CaptureProvider::Codex,
    );
    assert!(codex.sources.is_empty());
    assert!(codex.issues.iter().any(|issue| {
        issue.kind == DiscoveryIssueKind::SelectorUnreconstructible
            && issue.reason == SYMLINK_REASON
    }));

    let continue_report = resolve_provider(
        &base.with_env("CONTINUE_GLOBAL_DIR", alias.as_os_str()),
        CaptureProvider::Continue,
    );
    assert!(continue_report.sources.is_empty());
    assert!(continue_report
        .issues
        .iter()
        .any(|issue| { issue.kind == DiscoveryIssueKind::SelectorUnreconstructible }));
}

#[test]
fn claude_official_root_is_absolute_replacement_and_unsafe_selector_is_manual() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let custom = temp.path().join("claude-account");
    let report = resolve_provider(
        &base
            .clone()
            .with_env("CLAUDE_CONFIG_DIR", custom.as_os_str()),
        CaptureProvider::Claude,
    );
    assert_eq!(paths(&report), [custom.join("projects")]);

    let invalid = resolve_provider(
        &base
            .clone()
            .with_env("CLAUDE_CONFIG_DIR", "relative-account"),
        CaptureProvider::Claude,
    );
    assert!(invalid.sources.is_empty());
    assert_eq!(
        invalid.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn open_code_official_root_selects_one_stable_database_or_no_disk() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let xdg = temp.path().join("xdg");
    fs::create_dir_all(xdg.join("opencode")).unwrap();
    fs::write(xdg.join("opencode/opencode-dev.db"), "retired").unwrap();
    let stable = resolve_provider(
        &base.clone().with_env("XDG_DATA_HOME", xdg.as_os_str()),
        CaptureProvider::OpenCode,
    );
    assert_eq!(paths(&stable), [xdg.join("opencode/opencode.db")]);

    let exact = temp.path().join("selected.db");
    let override_report = resolve_provider(
        &base
            .clone()
            .with_env("XDG_DATA_HOME", xdg.as_os_str())
            .with_env("OPENCODE_DB", exact.as_os_str()),
        CaptureProvider::OpenCode,
    );
    assert_eq!(paths(&override_report), [exact]);

    let memory = resolve_provider(
        &base.clone().with_env("OPENCODE_DB", ":memory:"),
        CaptureProvider::OpenCode,
    );
    assert!(memory.sources.is_empty());
    assert_eq!(memory.issues[0].kind, DiscoveryIssueKind::NoDiskHistory);
}

#[test]
fn kilo_official_root_uses_only_exact_paired_fallback_and_exact_memory_sentinel() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let xdg = temp.path().join("xdg");
    let data = xdg.join("kilo");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("opencode.db"), "legacy").unwrap();
    fs::write(data.join("kilo-dev.db"), "retired").unwrap();
    let context = base.with_env("XDG_DATA_HOME", xdg.as_os_str());
    let fallback = resolve_provider(&context, CaptureProvider::Kilo);
    assert_eq!(paths(&fallback), [data.join("opencode.db")]);

    fs::write(data.join("kilo.db"), "current").unwrap();
    let current = resolve_provider(&context, CaptureProvider::Kilo);
    assert_eq!(paths(&current), [data.join("kilo.db")]);

    let memory = resolve_provider(
        &context.clone().with_env("KILO_DB", ":memory:"),
        CaptureProvider::Kilo,
    );
    assert_eq!(memory.issues[0].kind, DiscoveryIssueKind::NoDiskHistory);
    let literal = resolve_provider(
        &context.with_env("KILO_DB", " :memory: "),
        CaptureProvider::Kilo,
    );
    assert_eq!(paths(&literal), [data.join(" :memory: ")]);
}

#[cfg(unix)]
#[test]
fn kilo_unknown_current_presence_suppresses_readable_legacy() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let xdg = temp.path().join("xdg");
    let data = xdg.join("kilo");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("opencode.db"), "legacy").unwrap();
    let original = fs::metadata(&data).unwrap().permissions();
    fs::set_permissions(&data, fs::Permissions::from_mode(0o000)).unwrap();
    let report = resolve_provider(
        &base.with_env("XDG_DATA_HOME", xdg.as_os_str()),
        CaptureProvider::Kilo,
    );
    fs::set_permissions(&data, original).unwrap();

    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert!(report.issues[0].reason.contains("fallback was suppressed"));
    assert_eq!(report.issues[0].path, Some(data.join("kilo.db")));
}

#[test]
fn mimocode_official_root_applies_db_home_xdg_precedence_without_channel_scan() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let profile = temp.path().join("mimo-profile");
    let xdg = temp.path().join("xdg");
    fs::create_dir_all(profile.join("data")).unwrap();
    fs::write(profile.join("data/mimocode-preview.db"), "retired").unwrap();
    let home_selected = resolve_provider(
        &base
            .clone()
            .with_env("XDG_DATA_HOME", xdg.as_os_str())
            .with_env("MIMOCODE_HOME", profile.as_os_str()),
        CaptureProvider::MiMoCode,
    );
    assert_eq!(paths(&home_selected), [profile.join("data/mimocode.db")]);

    let exact = temp.path().join("exact-mimo.db");
    let db_selected = resolve_provider(
        &base
            .clone()
            .with_env("MIMOCODE_HOME", profile.as_os_str())
            .with_env("MIMOCODE_DB", exact.as_os_str()),
        CaptureProvider::MiMoCode,
    );
    assert_eq!(paths(&db_selected), [exact]);

    let memory = resolve_provider(
        &base.clone().with_env("MIMOCODE_DB", ":memory:"),
        CaptureProvider::MiMoCode,
    );
    assert_eq!(memory.issues[0].kind, DiscoveryIssueKind::NoDiskHistory);
}

#[test]
fn goose_official_root_uses_override_or_one_current_platform_strategy() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let custom = temp.path().join("goose-root");
    let override_report = resolve_provider(
        &base.clone().with_env("GOOSE_PATH_ROOT", custom.as_os_str()),
        CaptureProvider::Goose,
    );
    assert_eq!(
        paths(&override_report),
        [custom.join("data/sessions/sessions.db")]
    );

    let xdg = temp.path().join("xdg");
    let linux = resolve_provider(
        &base.clone().with_env("XDG_DATA_HOME", xdg.as_os_str()),
        CaptureProvider::Goose,
    );
    assert_eq!(paths(&linux), [xdg.join("goose/sessions/sessions.db")]);

    let windows_context = DiscoveryContext::new(
        temp.path().join("win-home"),
        temp.path().join("win-cwd"),
        DiscoveryPlatform::Windows,
        DiscoveryPlatformDirs {
            data: Some(temp.path().join("roaming")),
            ..DiscoveryPlatformDirs::default()
        },
    );
    let windows = resolve_provider(&windows_context, CaptureProvider::Goose);
    assert_eq!(
        paths(&windows),
        [temp
            .path()
            .join("roaming/Block/goose/data/sessions/sessions.db")]
    );
}

#[test]
fn continue_official_root_resolves_relative_replacement_and_suppresses_default() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let expected = base.cwd().unwrap().join("continue-profile/sessions");
    let report = resolve_provider(
        &base
            .clone()
            .with_env("CONTINUE_GLOBAL_DIR", "continue-profile"),
        CaptureProvider::Continue,
    );
    assert_eq!(paths(&report), [expected]);
    assert!(report
        .sources
        .iter()
        .all(|source| !source.path.starts_with(base.home().join(".continue"))));
}

#[test]
fn gemini_official_root_appends_dot_gemini_to_one_selected_home() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let selected = temp.path().join("gemini-home");
    let report = resolve_provider(
        &base
            .clone()
            .with_env("GEMINI_CLI_HOME", selected.as_os_str()),
        CaptureProvider::Gemini,
    );
    assert_eq!(paths(&report), [selected.join(".gemini")]);
}

#[test]
fn tabnine_official_root_uses_source_confirmed_shared_home_selector_only() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let selected = temp.path().join("tabnine-home");
    let report = resolve_provider(
        &base
            .clone()
            .with_env("GEMINI_CLI_HOME", selected.as_os_str())
            .with_env("TABNINE_CLI_HOME", temp.path().join("wrong")),
        CaptureProvider::Tabnine,
    );
    assert_eq!(paths(&report), [selected.join(".tabnine/agent")]);
}

#[test]
fn cursor_official_root_replaces_default_but_blank_selector_falls_back() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let selected = temp.path().join("cursor-data");
    let report = resolve_provider(
        &base
            .clone()
            .with_env("CURSOR_DATA_DIR", selected.as_os_str()),
        CaptureProvider::Cursor,
    );
    assert_eq!(paths(&report), [selected.join("projects")]);

    let blank = resolve_provider(
        &base.clone().with_env("CURSOR_DATA_DIR", "   "),
        CaptureProvider::Cursor,
    );
    assert_eq!(paths(&blank), [base.home().join(".cursor/projects")]);
}

#[test]
fn kimi_official_root_is_one_current_product_root_and_excludes_retired_predecessor() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let expected = base.cwd().unwrap().join("  ");
    let report = resolve_provider(
        &base
            .clone()
            .with_env("KIMI_CODE_HOME", "  ")
            .with_env("KIMI_SHARE_DIR", temp.path().join("retired")),
        CaptureProvider::KimiCodeCli,
    );
    assert_eq!(paths(&report), [expected]);

    let empty = resolve_provider(
        &base.clone().with_env("KIMI_CODE_HOME", ""),
        CaptureProvider::KimiCodeCli,
    );
    assert_eq!(paths(&empty), [base.home().join(".kimi-code")]);
}

#[test]
fn junie_official_root_uses_junie_home_and_ignores_retired_sessions_override() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let report = resolve_provider(
        &base
            .clone()
            .with_env("JUNIE_HOME", "")
            .with_env("JUNIE_SESSIONS_DIR", temp.path().join("retired")),
        CaptureProvider::Junie,
    );
    assert_eq!(paths(&report), [base.cwd().unwrap().join("sessions")]);

    let unsupported = resolve_provider(
        &context(&temp, DiscoveryPlatform::OtherUnix),
        CaptureProvider::Junie,
    );
    assert!(unsupported.sources.is_empty());
    assert_eq!(
        unsupported.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn factory_supported_default_is_the_sessions_directory_on_supported_platforms() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let sessions = base.home().join(".factory/sessions");

    for platform in [
        DiscoveryPlatform::Linux,
        DiscoveryPlatform::MacOS,
        DiscoveryPlatform::Windows,
    ] {
        let missing = resolve_provider(&context(&temp, platform), CaptureProvider::FactoryAiDroid);
        assert_eq!(paths(&missing), std::slice::from_ref(&sessions));
        assert_eq!(missing.sources[0].status, ProviderSourceStatus::Missing);
        assert_eq!(
            missing.sources[0].source_format,
            "factory_ai_droid_sessions_jsonl"
        );
        assert!(missing.issues.is_empty());
    }

    fs::create_dir_all(sessions.join("-Users-example-project")).unwrap();
    fs::write(
        sessions.join("-Users-example-project/session.jsonl"),
        "{}\n",
    )
    .unwrap();
    let available = resolve_provider(&base, CaptureProvider::FactoryAiDroid);
    assert_eq!(paths(&available), std::slice::from_ref(&sessions));
    assert_eq!(available.sources[0].status, ProviderSourceStatus::Available);

    fs::remove_file(sessions.join("-Users-example-project/session.jsonl")).unwrap();
    let empty = resolve_provider(&base, CaptureProvider::FactoryAiDroid);
    assert_eq!(empty.sources[0].status, ProviderSourceStatus::Empty);

    let unsupported = resolve_provider(
        &context(&temp, DiscoveryPlatform::OtherUnix),
        CaptureProvider::FactoryAiDroid,
    );
    assert!(unsupported.sources.is_empty());
    assert_eq!(
        unsupported.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn forgecode_official_root_preserves_raw_cwd_semantics_and_exists_winner() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let empty = resolve_provider(
        &base.clone().with_env("FORGE_CONFIG", ""),
        CaptureProvider::ForgeCode,
    );
    assert_eq!(paths(&empty), [base.cwd().unwrap().join(".forge.db")]);

    let relative = resolve_provider(
        &base.clone().with_env("FORGE_CONFIG", " forge "),
        CaptureProvider::ForgeCode,
    );
    assert_eq!(
        paths(&relative),
        [base.cwd().unwrap().join(" forge /.forge.db")]
    );

    fs::create_dir_all(base.home().join("forge")).unwrap();
    fs::create_dir_all(base.home().join(".forge")).unwrap();
    let legacy = resolve_provider(&base, CaptureProvider::ForgeCode);
    assert_eq!(paths(&legacy), [base.home().join("forge/.forge.db")]);
}

#[test]
fn simple_lane_has_seventeen_reviewed_winner_only_policies() {
    let temp = tempdir();
    let context = context(&temp, DiscoveryPlatform::Linux);
    let providers = [
        CaptureProvider::Codex,
        CaptureProvider::GrokBuild,
        CaptureProvider::DeepSeekHarness,
        CaptureProvider::Claude,
        CaptureProvider::OpenCode,
        CaptureProvider::Kilo,
        CaptureProvider::MiMoCode,
        CaptureProvider::Goose,
        CaptureProvider::Continue,
        CaptureProvider::Gemini,
        CaptureProvider::Tabnine,
        CaptureProvider::Cursor,
        CaptureProvider::KimiCodeCli,
        CaptureProvider::Junie,
        CaptureProvider::FactoryAiDroid,
        CaptureProvider::ForgeCode,
        CaptureProvider::Fx,
    ];
    assert_eq!(providers.len(), 17);
    for provider in providers {
        let report = resolve_provider(&context, provider);
        let expected = if provider == CaptureProvider::Codex {
            3
        } else {
            1
        };
        assert_eq!(report.sources.len(), expected, "{provider:?}");
        assert!(report.issues.is_empty(), "{provider:?}");
    }
}

#[test]
fn selected_paths_match_explicit_same_path_source_metadata() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let roots = temp.path().join("same-path");
    let codex = roots.join("codex");
    let claude = roots.join("claude");
    let open_code = roots.join("opencode.db");
    let kilo = roots.join("kilo.db");
    let mimocode = roots.join("mimocode.db");
    let goose = roots.join("goose");
    let continue_root = roots.join("continue");
    let gemini = roots.join("gemini-home");
    let tabnine = roots.join("tabnine-home");
    let cursor = roots.join("cursor");
    let kimi = roots.join("kimi");
    let junie = roots.join("junie");
    let forge = roots.join("forge");

    for directory in [
        codex.join("sessions"),
        codex.join("archived_sessions"),
        claude.join("projects"),
        goose.join("data/sessions"),
        continue_root.join("sessions"),
        gemini.join(".gemini"),
        tabnine.join(".tabnine/agent"),
        cursor.join("projects"),
        kimi.clone(),
        junie.join("sessions"),
        forge.clone(),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(
        codex.join("history.jsonl"),
        r#"{"session_id":"same-path","ts":1784371200,"text":"same path"}"#,
    )
    .unwrap();
    for file in [
        open_code.clone(),
        kilo.clone(),
        mimocode.clone(),
        goose.join("data/sessions/sessions.db"),
        forge.join(".forge.db"),
    ] {
        fs::write(file, "").unwrap();
    }

    let cases = [
        (
            CaptureProvider::Codex,
            base.clone().with_env("CODEX_HOME", codex.as_os_str()),
        ),
        (
            CaptureProvider::Claude,
            base.clone()
                .with_env("CLAUDE_CONFIG_DIR", claude.as_os_str()),
        ),
        (
            CaptureProvider::OpenCode,
            base.clone().with_env("OPENCODE_DB", open_code.as_os_str()),
        ),
        (
            CaptureProvider::Kilo,
            base.clone().with_env("KILO_DB", kilo.as_os_str()),
        ),
        (
            CaptureProvider::MiMoCode,
            base.clone().with_env("MIMOCODE_DB", mimocode.as_os_str()),
        ),
        (
            CaptureProvider::Goose,
            base.clone().with_env("GOOSE_PATH_ROOT", goose.as_os_str()),
        ),
        (
            CaptureProvider::Continue,
            base.clone()
                .with_env("CONTINUE_GLOBAL_DIR", continue_root.as_os_str()),
        ),
        (
            CaptureProvider::Gemini,
            base.clone().with_env("GEMINI_CLI_HOME", gemini.as_os_str()),
        ),
        (
            CaptureProvider::Tabnine,
            base.clone()
                .with_env("GEMINI_CLI_HOME", tabnine.as_os_str()),
        ),
        (
            CaptureProvider::Cursor,
            base.clone().with_env("CURSOR_DATA_DIR", cursor.as_os_str()),
        ),
        (
            CaptureProvider::KimiCodeCli,
            base.clone().with_env("KIMI_CODE_HOME", kimi.as_os_str()),
        ),
        (
            CaptureProvider::Junie,
            base.clone().with_env("JUNIE_HOME", junie.as_os_str()),
        ),
        (
            CaptureProvider::ForgeCode,
            base.clone().with_env("FORGE_CONFIG", forge.as_os_str()),
        ),
    ];

    for (provider, context) in cases {
        let report = resolve_provider(&context, provider);
        assert!(!report.sources.is_empty(), "{provider:?}");
        for selected in report.sources {
            let explicit = provider_source_for_path(provider, selected.path.clone());
            assert_eq!(explicit.path, selected.path, "{provider:?}");
            assert_eq!(
                explicit.source_format, selected.source_format,
                "{provider:?}"
            );
            assert_eq!(explicit.source_kind, selected.source_kind, "{provider:?}");
            assert_eq!(
                explicit.import_support, selected.import_support,
                "{provider:?}"
            );
            assert_eq!(
                explicit.catalog_support, selected.catalog_support,
                "{provider:?}"
            );
        }
    }
}
