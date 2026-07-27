use super::super::super::{
    context::DiscoveryPlatformDirs,
    discovery::provider_source_for_path,
    types::{DiscoveryIssueKind, ProviderSourceStatus},
};
use super::*;
use crate::{provider_source_specs, test_support_paths};

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

fn spec(provider: CaptureProvider) -> &'static ProviderSourceSpec {
    provider_source_specs()
        .iter()
        .find(|spec| spec.provider == provider)
        .unwrap()
}

fn resolve_provider(context: &DiscoveryContext, provider: CaptureProvider) -> DiscoveryReport {
    resolve(context, spec(provider))
}

fn paths(report: &DiscoveryReport) -> Vec<PathBuf> {
    report
        .sources
        .iter()
        .map(|source| source.path.clone())
        .collect()
}

#[test]
fn codex_official_root_includes_active_archive_history_and_compression_detection() {
    let temp = tempdir();
    let base = context(&temp, DiscoveryPlatform::Linux);
    let custom = temp.path().join("custom-codex");
    fs::create_dir_all(custom.join("canonical-hop")).unwrap();
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

    let selected_root = custom.join("canonical-hop/..");
    let report = resolve_provider(
        &base
            .clone()
            .with_env("CODEX_HOME", selected_root.as_os_str()),
        CaptureProvider::Codex,
    );
    assert_eq!(report.sources.len(), 5);
    assert_eq!(report.sources[0].path, selected_root.join("sessions"));
    assert_eq!(
        report.sources[1].path,
        selected_root.join("archived_sessions")
    );
    assert_eq!(report.sources[2].path, selected_root.join("history.jsonl"));
    assert_eq!(
        report.sources[3].path,
        selected_root.join("archived_sessions/a-first.jsonl.zst")
    );
    assert_eq!(report.sources[3].status, ProviderSourceStatus::Unsupported);
    assert_eq!(
        report.sources[4].path,
        selected_root.join("archived_sessions/z-last.jsonl.zst")
    );
    assert_eq!(report.sources[4].status, ProviderSourceStatus::Unsupported);
    for selected in &report.sources[3..] {
        let explicit = provider_source_for_path(CaptureProvider::Codex, selected.path.clone());
        assert_eq!(explicit.path, selected.path);
        assert_eq!(explicit.source_format, selected.source_format);
        assert_eq!(explicit.status, ProviderSourceStatus::Unsupported);
    }
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
fn codex_compression_detection_stops_at_the_fixed_directory_bound() {
    let temp = tempdir();
    let root = temp.path().join("bounded-codex");
    let sessions = root.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    for index in 0..=super::super::super::selectors::MAX_DIRECT_DIRECTORY_ENTRIES {
        fs::write(sessions.join(format!("rollout-{index:04}.jsonl")), "{}\n").unwrap();
    }

    let report = resolve_provider(
        &context(&temp, DiscoveryPlatform::Linux).with_env("CODEX_HOME", root.as_os_str()),
        CaptureProvider::Codex,
    );
    assert_eq!(report.sources.len(), 3);
    assert!(report.issues.iter().any(|issue| {
        issue.kind == DiscoveryIssueKind::SelectorUnreconstructible
            && issue.reason == CODEX_COMPRESSION_SCAN_REASON
    }));
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
    assert!(continue_report.issues.iter().any(|issue| {
        issue.kind == DiscoveryIssueKind::SelectorUnreconstructible
            && issue.reason == SYMLINK_REASON
    }));
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
fn simple_lane_has_thirteen_reviewed_winner_only_policies() {
    let temp = tempdir();
    let context = context(&temp, DiscoveryPlatform::Linux);
    let providers = [
        CaptureProvider::Codex,
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
        CaptureProvider::ForgeCode,
    ];
    assert_eq!(providers.len(), 13);
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
