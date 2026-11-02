use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use serde_json::json;

use crate::provider_sources::{
    resolvers::test_support::{provider_source_for_path, resolve_provider, tempdir, write_fixture},
    DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs, ProviderSourceStatus,
};

fn context(home: &Path, cwd: &Path) -> DiscoveryContext {
    DiscoveryContext::new(
        home,
        cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
}

fn session(path: &Path) {
    write_fixture(
        path,
        format!(
            "{}\n",
            json!({
                "type": "session", "version": 3, "id": "019a0000-0000-7000-8000-000000000001",
                "timestamp": "2026-01-01T00:00:00Z", "cwd": "/example/project", "rlmDepth": 0,
            })
        ),
    );
}

fn paths(context: &DiscoveryContext) -> Vec<PathBuf> {
    resolve_provider(context, CaptureProvider::PrimeAgent)
        .sources
        .into_iter()
        .map(|source| source.path)
        .collect()
}

#[test]
fn default_agent_artifacts_and_t3_coexist_without_project_tree_scanning() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let sessions = home.join(".prime/agent/sessions");
    let artifacts = home.join(".prime/agent/session-artifacts");
    let t3 = home.join(".t3/userdata/prime-agent");
    session(&sessions.join("root.jsonl"));
    session(&artifacts.join("parent/sub-one/child.jsonl"));
    // Grandchildren share the same artifacts root; they are not a recursive
    // walk beneath the first child's directory.
    session(&artifacts.join("child/sub-two/grandchild.jsonl"));
    session(&t3.join("prime/thread-one/root.jsonl"));
    write_fixture(
        &t3.join("prime/thread-one/rlm-ledger.jsonl"),
        "{\"type\":\"spawn\"}\n",
    );
    session(&cwd.join("unrelated/.prime/agent/sessions/not-selected.jsonl"));
    session(&home.join(".prime/agent-old/sessions/not-selected.jsonl"));
    let report = resolve_provider(&context(&home, &cwd), CaptureProvider::PrimeAgent);
    assert!(report.issues.is_empty());
    assert_eq!(report.sources.len(), 3);
    let roles = report
        .sources
        .iter()
        .map(|source| {
            source
                .route_provenance
                .automatic_route_role()
                .unwrap()
                .as_bytes()
                .to_vec()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        roles.len(),
        3,
        "coexisting stores must have separate automatic routes"
    );
    for path in [sessions, artifacts, t3] {
        let source = report
            .sources
            .iter()
            .find(|source| source.path == path)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "prime_agent_session_jsonl");
    }
}

#[test]
fn session_env_precedence_empty_current_and_no_trimming_match_upstream() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let current = temp.path().join(" current ");
    let legacy = temp.path().join("legacy");
    let settings = temp.path().join("settings");
    write_fixture(
        &home.join(".prime/agent/settings.json"),
        json!({"sessionDir": settings}).to_string(),
    );
    let ctx = context(&home, &cwd)
        .with_env("PRIME_AGENT_SESSION_DIR", current.as_os_str())
        .with_env("PRIME_AGENT_CODING_AGENT_SESSION_DIR", legacy.as_os_str());
    assert_eq!(paths(&ctx), [current]);
    assert_eq!(
        paths(&ctx.clone().with_env("PRIME_AGENT_SESSION_DIR", "")),
        [settings]
    );
    assert_eq!(
        paths(
            &context(&home, &cwd)
                .with_env("PRIME_AGENT_CODING_AGENT_SESSION_DIR", legacy.as_os_str())
        ),
        [legacy]
    );
    // A winning env root need not exist and ignores malformed lower settings.
    write_fixture(&home.join(".prime/agent/settings.json"), "{");
    assert_eq!(paths(&ctx).len(), 1);
    let source = &resolve_provider(&ctx, CaptureProvider::PrimeAgent).sources[0];
    assert_eq!(source.status, ProviderSourceStatus::Missing);
}

#[test]
fn agent_env_settings_exact_cwd_merge_and_relative_paths_match_upstream() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project/nested");
    let agent = home.join("custom-agent");
    let ctx = context(&home, &cwd).with_env("PRIME_AGENT_CODING_AGENT_DIR", "~/custom-agent");
    assert_eq!(paths(&ctx), [agent.join("sessions")]);
    write_fixture(
        &agent.join("settings.json"),
        r#"{"sessionDir":"global-relative"}"#,
    );
    assert_eq!(paths(&ctx), [cwd.join("global-relative")]);
    write_fixture(
        &temp.path().join("project/.prime/agent/settings.json"),
        r#"{"sessionDir":"/ancestor-not-selected"}"#,
    );
    assert_eq!(paths(&ctx), [cwd.join("global-relative")]);
    let project_settings = cwd.join(".prime/agent/settings.json");
    // Prime does not require Pi's trust.json or defaultProjectTrust setting.
    write_fixture(&project_settings, r#"{"sessionDir":"~/project-sessions"}"#);
    assert_eq!(paths(&ctx), [home.join("project-sessions")]);
    for value in [json!(null), json!(17), json!(false), json!([])] {
        write_fixture(&project_settings, json!({"sessionDir": value}).to_string());
        assert_eq!(paths(&ctx), [cwd.join("global-relative")]);
    }
    write_fixture(&project_settings, r#"{"sessionDir":""}"#);
    assert_eq!(paths(&ctx), [cwd]);
}

#[test]
fn invalid_selector_suppresses_stale_sessions_but_preserves_current_independent_stores() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    session(&home.join(".prime/agent/sessions/stale.jsonl"));
    let artifacts = home.join(".prime/agent/session-artifacts");
    let t3 = home.join(".t3/userdata/prime-agent");
    session(&artifacts.join("parent/sub-one/child.jsonl"));
    session(&t3.join("prime/thread/root.jsonl"));
    for malformed in ["{", ""] {
        write_fixture(&home.join(".prime/agent/settings.json"), malformed);
        let report = resolve_provider(&context(&home, &cwd), CaptureProvider::PrimeAgent);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(
            paths(&context(&home, &cwd)),
            [artifacts.clone(), t3.clone()]
        );
    }
}

#[test]
fn redirected_sessions_do_not_relocate_rlm_children_or_union_old_agent_root() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let agent = temp.path().join("agent");
    let sessions = temp.path().join("custom-sessions");
    let artifacts = agent.join("session-artifacts");
    session(&sessions.join("root.jsonl"));
    session(&artifacts.join("parent/sub-one/child.jsonl"));
    session(&home.join(".prime/agent/session-artifacts/stale/sub/child.jsonl"));
    let ctx = context(&home, &cwd)
        .with_env("PRIME_AGENT_CODING_AGENT_DIR", agent.as_os_str())
        .with_env("PRIME_AGENT_SESSION_DIR", sessions.as_os_str());
    assert_eq!(paths(&ctx), [sessions, artifacts]);
}

#[test]
fn missing_cwd_rejects_relative_replacements_and_keeps_absolute_selection() {
    let temp = tempdir();
    let ctx = context(temp.path(), temp.path()).with_cwd(None);
    assert_eq!(
        paths(&ctx.with_env("PRIME_AGENT_SESSION_DIR", "~/sessions")),
        [temp.path().join("sessions")]
    );
    let ctx = context(temp.path(), temp.path())
        .with_cwd(None)
        .with_env("PRIME_AGENT_SESSION_DIR", "relative");
    let report = resolve_provider(&ctx, CaptureProvider::PrimeAgent);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn explicit_files_require_first_line_v3_header_and_reject_ledgers_caches_and_bad_metadata() {
    let temp = tempdir();
    let path = temp.path().join("session.jsonl");
    session(&path);
    let header: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        provider_source_for_path(CaptureProvider::PrimeAgent, path.clone()).status,
        ProviderSourceStatus::Available
    );
    for (key, value) in [
        ("version", json!(2)),
        ("version", json!(4)),
        ("version", json!("3")),
        ("type", json!("rlm_ledger")),
        ("id", json!("../escape")),
        ("id", json!("")),
        ("id", json!("C:escape")),
        ("id", json!(" leading")),
        ("id", json!("NUL")),
        ("id", json!("trailing.")),
        ("timestamp", json!("not-a-time")),
        ("cwd", json!(null)),
        ("rlmDepth", json!(-1)),
        ("rlmDepth", json!(u64::MAX)),
        ("parentSession", json!(42)),
    ] {
        let mut invalid = header.clone();
        invalid[key] = value;
        write_fixture(&path, format!("{invalid}\n"));
        assert_eq!(
            provider_source_for_path(CaptureProvider::PrimeAgent, path.clone()).status,
            ProviderSourceStatus::Unsupported,
            "{invalid}"
        );
    }
    for invalid in ["{\n", "\n", "{\"type\":\"spawn\"}\n"] {
        write_fixture(&path, format!("{invalid}{header}\n"));
        assert_eq!(
            provider_source_for_path(CaptureProvider::PrimeAgent, path.clone()).status,
            ProviderSourceStatus::Unsupported
        );
    }
    let cache = temp.path().join("session.window-cache.json");
    session(&cache);
    assert_eq!(
        provider_source_for_path(CaptureProvider::PrimeAgent, cache).status,
        ProviderSourceStatus::Unsupported
    );
}

#[test]
fn header_only_sessions_are_valid_and_ledgers_alone_do_not_admit_directories() {
    let temp = tempdir();
    let sessions = temp.path().join("sessions");
    write_fixture(&sessions.join("rlm-ledger.jsonl"), "{\"type\":\"spawn\"}\n");
    write_fixture(
        &sessions.join("rlm-subagents.jsonl"),
        "{\"type\":\"subagent\"}\n",
    );
    // Reserved journal names never become sessions even if a row resembles a
    // header. The real journal is not a conversation authority.
    session(&sessions.join("semantic-edges.jsonl"));
    assert_eq!(
        provider_source_for_path(CaptureProvider::PrimeAgent, sessions.clone()).status,
        ProviderSourceStatus::Empty
    );
    session(&sessions.join("root.jsonl"));
    assert_eq!(
        provider_source_for_path(CaptureProvider::PrimeAgent, sessions).status,
        ProviderSourceStatus::Available
    );
}

#[test]
fn large_transcript_with_small_header_is_admitted_without_reading_its_body() {
    use std::io::Write;
    let temp = tempdir();
    let path = temp.path().join("large-session.jsonl");
    session(&path);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
    assert_eq!(
        provider_source_for_path(CaptureProvider::PrimeAgent, path).status,
        ProviderSourceStatus::Available
    );
}

#[test]
fn absolute_session_env_survives_unreconstructible_agent_directory() {
    let temp = tempdir();
    let ctx = context(temp.path(), temp.path())
        .with_cwd(None)
        .with_env("PRIME_AGENT_CODING_AGENT_DIR", "relative-agent")
        .with_env("PRIME_AGENT_SESSION_DIR", "~/sessions");
    let report = resolve_provider(&ctx, CaptureProvider::PrimeAgent);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, temp.path().join("sessions"));
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn traversal_is_not_lexically_normalized_through_an_unobserved_component() {
    let temp = tempdir();
    let ctx = context(temp.path(), temp.path())
        .with_env("PRIME_AGENT_SESSION_DIR", "missing/../sessions");
    let report = resolve_provider(&ctx, CaptureProvider::PrimeAgent);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn fixed_layouts_do_not_search_deeper_unrelated_trees() {
    let temp = tempdir();
    let sessions = temp.path().join("sessions");
    session(&sessions.join("arbitrary/project/history.jsonl"));
    assert_eq!(
        provider_source_for_path(CaptureProvider::PrimeAgent, sessions).status,
        ProviderSourceStatus::Empty
    );
    let artifacts = temp.path().join("session-artifacts");
    session(&artifacts.join("not-a-child.jsonl"));
    session(&artifacts.join("parent/sub/deeper/history.jsonl"));
    assert_eq!(
        crate::provider_sources::prime_agent::probe_for_role(&artifacts, Some(b"rlm-artifacts")),
        crate::provider_sources::probes::BoundedProbe::NotFound
    );
    let t3 = temp.path().join(".t3/userdata/prime-agent");
    session(&t3.join("prime/thread/unrelated/deeper/history.jsonl"));
    assert_eq!(
        provider_source_for_path(CaptureProvider::PrimeAgent, t3.clone()).status,
        ProviderSourceStatus::Empty
    );
    session(&t3.join("prime/session-artifacts/parent/sub-one/child.jsonl"));
    assert_eq!(
        provider_source_for_path(CaptureProvider::PrimeAgent, t3).status,
        ProviderSourceStatus::Available
    );
}

#[test]
fn oversized_header_and_directory_fanout_are_unknown_with_fixed_budgets() {
    let temp = tempdir();
    let path = temp.path().join("large.jsonl");
    write_fixture(&path, vec![b'x'; 64 * 1024 + 2]);
    assert_eq!(
        provider_source_for_path(CaptureProvider::PrimeAgent, path).status,
        ProviderSourceStatus::Unknown
    );
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    for index in 0..10_001 {
        fs::write(sessions.join(format!("cache-{index}.json")), b"").unwrap();
    }
    assert_eq!(
        provider_source_for_path(CaptureProvider::PrimeAgent, sessions).status,
        ProviderSourceStatus::Unknown
    );
}

#[cfg(unix)]
#[test]
fn symlink_files_roots_ancestors_and_project_selectors_are_not_followed() {
    use std::os::unix::fs::symlink;
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let actual = temp.path().join("actual");
    session(&actual.join("session.jsonl"));
    let linked = temp.path().join("linked");
    symlink(&actual, &linked).unwrap();
    for path in [&linked, &linked.join("session.jsonl")] {
        assert_ne!(
            provider_source_for_path(CaptureProvider::PrimeAgent, path.clone()).status,
            ProviderSourceStatus::Available
        );
    }
    let sessions = home.join(".prime/agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    symlink(actual.join("session.jsonl"), sessions.join("session.jsonl")).unwrap();
    assert_ne!(
        provider_source_for_path(CaptureProvider::PrimeAgent, sessions).status,
        ProviderSourceStatus::Available
    );
    write_fixture(
        &actual.join("settings.json"),
        r#"{"sessionDir":"/do-not-follow"}"#,
    );
    fs::create_dir_all(cwd.join(".prime/agent")).unwrap();
    symlink(
        actual.join("settings.json"),
        cwd.join(".prime/agent/settings.json"),
    )
    .unwrap();
    let report = resolve_provider(&context(&home, &cwd), CaptureProvider::PrimeAgent);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
}

#[test]
fn selected_flat_layout_is_independent_of_directory_name() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    for suffix in ["session-artifacts", ".t3/userdata/prime-agent"] {
        let root = temp.path().join("custom").join(suffix);
        session(&root.join("direct.jsonl"));
        let selected = context(&home, &cwd).with_env("PRIME_AGENT_SESSION_DIR", root.as_os_str());
        assert_eq!(
            resolve_provider(&selected, CaptureProvider::PrimeAgent).sources[0].status,
            ProviderSourceStatus::Available
        );
        write_fixture(
            &cwd.join(".prime/agent/settings.json"),
            json!({"sessionDir":root}).to_string(),
        );
        assert_eq!(
            resolve_provider(&context(&home, &cwd), CaptureProvider::PrimeAgent).sources[0].status,
            ProviderSourceStatus::Available
        );
        assert_eq!(
            provider_source_for_path(CaptureProvider::PrimeAgent, root.clone()).status,
            ProviderSourceStatus::Available
        );
        fs::remove_file(root.join("direct.jsonl")).unwrap();
        session(&root.join("parent/sub-one/decoy.jsonl"));
        session(&root.join("instance/thread/decoy.jsonl"));
        assert_eq!(
            resolve_provider(&selected, CaptureProvider::PrimeAgent).sources[0].status,
            ProviderSourceStatus::Empty
        );
    }
}
