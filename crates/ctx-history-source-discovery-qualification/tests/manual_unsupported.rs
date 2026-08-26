mod support;

use ctx_history_core::CaptureProvider;
use ctx_history_source_discovery::*;
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};

use support::TEST_PROVIDER_PROBES;

const MAX_FINITE_SELECTOR_ENTRIES: usize = 128;
const MAX_DIRECT_DIRECTORY_ENTRIES: usize = 1_024;
const MAX_PROJECT_ANCESTORS: usize = 64;

fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    discover_provider_sources_for_provider_with_context(
        &TEST_PROVIDER_PROBES,
        context,
        spec.provider,
    )
}

fn provider_source_for_path(provider: CaptureProvider, path: PathBuf) -> ProviderSource {
    ctx_history_source_discovery::provider_source_for_path(&TEST_PROVIDER_PROBES, provider, path)
}

fn tempdir() -> tempfile::TempDir {
    support::tempdir()
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

fn assert_automatic_role(source: &ProviderSource, components: &[&[u8]]) {
    let expected =
        ctx_history_capture_model::ProviderRouteRole::from_dynamic(components.iter().copied())
            .expect("expected test role should be bounded");
    assert_eq!(
        source.route_provenance.automatic_route_role(),
        Some(&expected),
        "unexpected route role for {}",
        source.path.display()
    );
}

fn write_firebender_chat_history_db(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE chat_sessions (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            deleted_at INTEGER DEFAULT NULL,
            messages_json TEXT NOT NULL,
            metadata_json TEXT NOT NULL
        );
        CREATE TABLE schema_info (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE subagent_conversations (
            tool_call_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            subagent_type TEXT NOT NULL,
            description TEXT NOT NULL,
            messages_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (session_id, tool_call_id),
            FOREIGN KEY (session_id) REFERENCES chat_sessions(id) ON DELETE CASCADE
        );
        "#,
    )
    .unwrap();
}

fn write_malformed_firebender_db(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("CREATE TABLE chat_sessions (id TEXT PRIMARY KEY);")
        .unwrap();
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
fn qoder_direct_and_transcript_histories_share_one_supported_source() {
    for (name, relative) in [
        ("direct", "project/session.jsonl"),
        ("transcript", "project/transcript/session.jsonl"),
        ("mixed", "project/direct.jsonl"),
    ] {
        let temp = tempdir();
        let context = context(temp.path(), DiscoveryPlatform::Linux);
        let projects = context.home().join(".qoder/projects");
        write(&projects.join(relative), b"{}\n");
        if name == "mixed" {
            write(
                &projects.join("project/transcript/transcript.jsonl"),
                b"{}\n",
            );
        }

        let report = resolve(&context, spec(CaptureProvider::Qoder));
        assert_eq!(report.sources.len(), 1, "{name}: {report:?}");
        let source = &report.sources[0];
        assert_eq!(source.path, projects);
        assert_eq!(source.source_format, "qoder_transcript_jsonl_tree");
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.unsupported_reason.is_none());
    }
}

#[test]
fn firebender_unmarked_cwd_does_not_synthesize_a_project_store() {
    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux);
    let mut cwd = temp.path().join("unmarked");
    for index in 0..MAX_PROJECT_ANCESTORS {
        cwd.push(format!("level-{index}"));
    }
    fs::create_dir_all(&cwd).unwrap();
    let context = base.with_cwd(Some(cwd));

    let report = resolve(&context, spec(CaptureProvider::Firebender));

    assert!(report.sources.is_empty());
    assert!(report.issues.is_empty());
}

#[test]
fn firebender_project_local_default_reports_missing_at_current_project_root() {
    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux);
    let project = temp.path().join("project");
    let child = project.join("src/module");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(&child).unwrap();
    let context = base.with_cwd(Some(child));
    let report = resolve(&context, spec(CaptureProvider::Firebender));
    assert_eq!(
        (
            report.sources.len(),
            &report.sources[0].path,
            report.sources[0].status,
            report.issues.len()
        ),
        (
            1,
            &project.join(".idea/firebender/chat_history.db"),
            ProviderSourceStatus::Missing,
            0
        )
    );
}

#[test]
fn firebender_project_local_default_accepts_valid_official_shape_without_writes() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let db = context
        .cwd()
        .unwrap()
        .join(".idea/firebender/chat_history.db");
    write_firebender_chat_history_db(&db);
    let before = fs::read(&db).unwrap();

    let report = resolve(&context, spec(CaptureProvider::Firebender));
    assert_eq!(
        (
            report.sources.len(),
            &report.sources[0].path,
            report.sources[0].status,
            report.sources[0].import_support
        ),
        (
            1,
            &db,
            ProviderSourceStatus::Available,
            ProviderImportSupport::Native
        )
    );
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn firebender_project_local_default_rejects_wrong_shape_as_unknown() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let db = context
        .cwd()
        .unwrap()
        .join(".idea/firebender/chat_history.db");
    write_malformed_firebender_db(&db);
    let report = resolve(&context, spec(CaptureProvider::Firebender));
    assert_eq!(
        (&report.sources[0].path, report.sources[0].status),
        (&db, ProviderSourceStatus::Unknown)
    );
    assert!(report.sources[0]
        .unsupported_reason
        .unwrap()
        .contains("could not be read"));
}

#[test]
fn firebender_project_local_default_stops_at_git_boundary() {
    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux);
    let outer = temp.path().join("outer");
    let repo = outer.join("repo");
    let child = repo.join("src/module");
    write_firebender_chat_history_db(&outer.join(".idea/firebender/chat_history.db"));
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(&child).unwrap();
    let report = resolve(
        &base.with_cwd(Some(child)),
        spec(CaptureProvider::Firebender),
    );
    assert_eq!(
        (&report.sources[0].path, report.sources[0].status),
        (
            &repo.join(".idea/firebender/chat_history.db"),
            ProviderSourceStatus::Missing
        )
    );
}

#[test]
fn firebender_project_local_default_accepts_git_worktree_file_boundary() {
    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux);
    let project = temp.path().join("project");
    let child = project.join("src/module");
    fs::create_dir_all(&child).unwrap();
    fs::write(project.join(".git"), "gitdir: ../git/worktrees/project\n").unwrap();

    let report = resolve(
        &base.with_cwd(Some(child)),
        spec(CaptureProvider::Firebender),
    );

    assert_eq!(
        (&report.sources[0].path, report.sources[0].status),
        (
            &project.join(".idea/firebender/chat_history.db"),
            ProviderSourceStatus::Missing
        )
    );
}

#[test]
fn firebender_nearest_project_marker_suppresses_outer_project_store() {
    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux);
    let outer = temp.path().join("outer");
    let nested = outer.join("nested");
    let child = nested.join("src");
    write_firebender_chat_history_db(&outer.join(".idea/firebender/chat_history.db"));
    fs::create_dir_all(nested.join(".idea")).unwrap();
    fs::create_dir_all(&child).unwrap();
    let report = resolve(
        &base.with_cwd(Some(child)),
        spec(CaptureProvider::Firebender),
    );
    assert_eq!(
        (&report.sources[0].path, report.sources[0].status),
        (
            &nested.join(".idea/firebender/chat_history.db"),
            ProviderSourceStatus::Missing
        )
    );
}

#[cfg(unix)]
#[test]
fn firebender_linked_idea_marker_fails_closed_and_does_not_touch_target() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let outside = temp.path().join("outside");
    let db = outside.join("firebender/chat_history.db");
    write_firebender_chat_history_db(&db);
    let before = fs::read(&db).unwrap();
    symlink(&outside, context.cwd().unwrap().join(".idea")).unwrap();

    let report = resolve(&context, spec(CaptureProvider::Firebender));
    assert!(report.sources.is_empty());
    assert!(report.issues.is_empty());
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn firebender_linked_git_marker_fails_closed_before_an_outer_project() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux);
    let outer = temp.path().join("outer");
    let project = outer.join("project");
    let child = project.join("src");
    let outer_db = outer.join(".idea/firebender/chat_history.db");
    write_firebender_chat_history_db(&outer_db);
    fs::create_dir_all(&child).unwrap();
    symlink(temp.path().join("missing-git-target"), project.join(".git")).unwrap();
    let before = fs::read(&outer_db).unwrap();

    let report = resolve(
        &base.with_cwd(Some(child)),
        spec(CaptureProvider::Firebender),
    );

    assert!(report.sources.is_empty());
    assert!(report.issues.is_empty());
    assert_eq!(fs::read(&outer_db).unwrap(), before);
}

#[test]
fn firebender_project_marker_beyond_ancestor_cap_is_not_discovered() {
    let temp = tempdir();
    let base = context(temp.path(), DiscoveryPlatform::Linux);
    let project = temp.path().join("project");
    let db = project.join(".idea/firebender/chat_history.db");
    write_firebender_chat_history_db(&db);
    let mut child = project.clone();
    for index in 0..MAX_PROJECT_ANCESTORS {
        child.push(format!("level-{index}"));
    }
    fs::create_dir_all(&child).unwrap();
    let before = fs::read(&db).unwrap();

    let report = resolve(
        &base.with_cwd(Some(child)),
        spec(CaptureProvider::Firebender),
    );

    assert!(report.sources.is_empty());
    assert!(report.issues.is_empty());
    assert_eq!(fs::read(&db).unwrap(), before);
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
fn mux_root_is_one_raw_supported_winner_when_active_and_archive_history_coexist() {
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
    assert_eq!(report.sources.len(), 1);
    assert_eq!(
        (&supported.path, supported.status),
        (&custom.join("sessions"), ProviderSourceStatus::Available)
    );
    assert!(supported.unsupported_reason.is_none());
    assert!(matches!(
        supported.route_provenance,
        ctx_history_capture_model::ProviderSourceRouteProvenance::Unroled
    ));
    assert!(report
        .sources
        .iter()
        .all(|item| !item.path.starts_with(context.home().join(".mux"))));
}

#[test]
fn mux_archive_only_history_makes_the_session_tree_available() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let sessions = context.home().join(".mux/sessions");
    write(&sessions.join("workspace/chat-archive.jsonl"), b"{}\n");

    let report = resolve(&context, spec(CaptureProvider::Mux));
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, sessions);
    assert_eq!(report.sources[0].status, ProviderSourceStatus::Available);
    assert!(report.sources[0].unsupported_reason.is_none());
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
    let selected_source = native
        .iter()
        .find(|source| source.path == selected)
        .expect("selected Cline data root");
    assert_automatic_role(selected_source, &[b"task-store", b"selected-data-root"]);
    let base_source = native
        .iter()
        .find(|source| source.path == code)
        .expect("stable VS Code base Cline store");
    assert_automatic_role(base_source, &[b"task-store", b"vscode", b"stable", b"base"]);
    let profile_source = native
        .iter()
        .find(|source| source.path == profile)
        .expect("stable VS Code profile Cline store");
    assert_automatic_role(
        profile_source,
        &[
            b"task-store",
            b"vscode",
            b"stable",
            b"profile",
            b"native-id",
            b"utf8",
            b"profile-a",
        ],
    );
}

#[test]
fn cline_unicode_profile_ids_have_distinct_stable_roles() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let profiles = context
        .platform_dirs()
        .config
        .as_ref()
        .unwrap()
        .join("Code/User/profiles");
    let snow = profiles
        .join("profile-雪")
        .join("globalStorage/saoudrizwan.claude-dev");
    let fire = profiles
        .join("profile-火")
        .join("globalStorage/saoudrizwan.claude-dev");
    write(&snow.join("tasks/snow/ui_messages.json"), b"[]");
    write(&fire.join("tasks/fire/ui_messages.json"), b"[]");

    let report = resolve(&context, spec(CaptureProvider::Cline));
    let snow_source = report
        .sources
        .iter()
        .find(|source| source.path == snow)
        .expect("Unicode snow profile should be discovered");
    let fire_source = report
        .sources
        .iter()
        .find(|source| source.path == fire)
        .expect("Unicode fire profile should be discovered");
    assert_automatic_role(
        snow_source,
        &[
            b"task-store",
            b"vscode",
            b"stable",
            b"profile",
            b"native-id",
            b"utf8",
            "profile-雪".as_bytes(),
        ],
    );
    assert_automatic_role(
        fire_source,
        &[
            b"task-store",
            b"vscode",
            b"stable",
            b"profile",
            b"native-id",
            b"utf8",
            "profile-火".as_bytes(),
        ],
    );
    assert_ne!(
        snow_source.route_provenance.automatic_route_role(),
        fire_source.route_provenance.automatic_route_role()
    );
}

#[test]
fn cline_stable_and_insiders_base_stores_have_distinct_order_independent_roles() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Windows);
    let config = context.platform_dirs().config.as_ref().unwrap();
    let stable = config.join("Code/User/globalStorage/saoudrizwan.claude-dev");
    let insiders = config.join("Code - Insiders/User/globalStorage/saoudrizwan.claude-dev");
    write(&stable.join("tasks/stable/ui_messages.json"), b"[]");
    write(&insiders.join("tasks/insiders/ui_messages.json"), b"[]");

    let report = resolve(&context, spec(CaptureProvider::Cline));
    let stable_source = report
        .sources
        .iter()
        .find(|source| source.path == stable)
        .expect("stable Cline host store");
    let insiders_source = report
        .sources
        .iter()
        .find(|source| source.path == insiders)
        .expect("Cline Insiders host store");
    assert_automatic_role(
        stable_source,
        &[b"task-store", b"vscode", b"stable", b"base"],
    );
    assert_automatic_role(
        insiders_source,
        &[b"task-store", b"vscode", b"insiders", b"base"],
    );
    assert_ne!(
        stable_source.route_provenance.automatic_route_role(),
        insiders_source.route_provenance.automatic_route_role()
    );
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
fn cline_common_data_root_publishes_separate_sdk_and_legacy_routes() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let data = context.home().join(".cline/data");
    write(
        &data.join("sessions/sessions.index.json"),
        br#"{"version":1,"sessions":{}}"#,
    );
    write(
        &data.join("tasks/legacy/api_conversation_history.json"),
        b"[]",
    );

    let report = resolve(&context, spec(CaptureProvider::Cline));
    let sdk = source(&report, "cline_sdk_session_store");
    let legacy = source(&report, "cline_task_directory_json");
    assert_eq!(sdk.path, data);
    assert_eq!(sdk.status, ProviderSourceStatus::Available);
    assert_eq!(sdk.import_support, ProviderImportSupport::Native);
    assert_eq!(legacy.path, sdk.path);
    assert_eq!(legacy.status, ProviderSourceStatus::Available);
    assert_automatic_role(legacy, &[b"task-store", b"selected-data-root"]);
    assert!(matches!(
        sdk.route_provenance,
        ctx_history_capture_model::ProviderSourceRouteProvenance::Unroled
    ));

    let explicit = provider_source_for_path(CaptureProvider::Cline, sdk.path.clone());
    assert_eq!(explicit.path, sdk.path);
    assert_eq!(explicit.source_format, sdk.source_format);
    assert_eq!(explicit.status, ProviderSourceStatus::Available);
    assert_eq!(explicit.import_support, ProviderImportSupport::Native);

    for selected in [
        data.join("sessions"),
        data.join("sessions/sessions.index.json"),
    ] {
        let exact_catalog = provider_source_for_path(CaptureProvider::Cline, selected);
        assert_eq!(exact_catalog.path, data);
        assert_eq!(exact_catalog.source_format, "cline_sdk_session_store");
        assert_eq!(exact_catalog.status, ProviderSourceStatus::Available);
    }
}

#[test]
fn cline_sdk_discovery_requires_an_ordinary_catalog_leaf() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let data = context.home().join(".cline/data");
    fs::create_dir_all(data.join("sessions/sessions.index.json")).unwrap();
    assert!(resolve(&context, spec(CaptureProvider::Cline))
        .sources
        .iter()
        .all(|source| source.source_format != "cline_sdk_session_store"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        fs::remove_dir_all(data.join("sessions/sessions.index.json")).unwrap();
        let outside = temp.path().join("outside-index.json");
        write(&outside, br#"{"version":1,"sessions":{}}"#);
        symlink(&outside, data.join("sessions/sessions.index.json")).unwrap();
        assert!(resolve(&context, spec(CaptureProvider::Cline))
            .sources
            .iter()
            .all(|source| source.source_format != "cline_sdk_session_store"));
    }
}

#[test]
fn cline_db_only_catalog_selects_the_common_data_root_automatically_and_exactly() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let data = context.home().join(".cline/data");
    let database = data.join("db/sessions.db");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch("CREATE TABLE sessions (session_id TEXT PRIMARY KEY);")
        .unwrap();
    drop(connection);

    let report = resolve(&context, spec(CaptureProvider::Cline));
    let automatic = source(&report, "cline_sdk_session_store");
    assert_eq!(automatic.path, data);
    assert_eq!(automatic.status, ProviderSourceStatus::Available);

    let exact = provider_source_for_path(CaptureProvider::Cline, database);
    assert_eq!(exact.path, data);
    assert_eq!(exact.source_format, "cline_sdk_session_store");
    assert_eq!(exact.status, ProviderSourceStatus::Available);
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
    assert!(profiles
        .iter()
        .all(|source| source.route_provenance.automatic_route_role().is_some()));
}

#[test]
fn cline_oversized_native_profile_id_keeps_its_source_with_a_bounded_stable_role() {
    let temp = tempdir();
    let context = context(temp.path(), DiscoveryPlatform::Linux);
    let profile_id = "p".repeat(240);
    let root = context
        .platform_dirs()
        .config
        .as_ref()
        .unwrap()
        .join("Code/User/profiles")
        .join(&profile_id)
        .join("globalStorage/saoudrizwan.claude-dev");
    write(&root.join("tasks/t/ui_messages.json"), b"[]");

    let first = resolve(&context, spec(CaptureProvider::Cline));
    let source = first
        .sources
        .iter()
        .find(|source| source.path == root)
        .expect("oversized provider-native profile should remain selected");
    let role = source
        .route_provenance
        .automatic_route_role()
        .expect("oversized profile should retain an automatic role");
    assert!(role.as_bytes().len() <= ctx_history_capture_model::MAX_PROVIDER_ROUTE_ROLE_BYTES);
    assert!(role
        .as_bytes()
        .windows(b"native-id-sha256".len())
        .any(|window| window == b"native-id-sha256"));
    assert!(role
        .as_bytes()
        .windows(b"utf8".len())
        .any(|window| window == b"utf8"));
    assert_eq!(
        first.sources,
        resolve(&context, spec(CaptureProvider::Cline)).sources
    );
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
        let report = resolve(&context, spec(provider));
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
