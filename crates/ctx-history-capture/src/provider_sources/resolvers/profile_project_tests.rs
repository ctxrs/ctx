use super::super::super::{
    context::DiscoveryPlatformDirs,
    discovery::{discover_provider_sources_for_provider_with_context, provider_source_for_path},
    types::{ProviderImportSupport, ProviderSourceStatus},
};
use std::fs;

use super::*;
fn tempdir() -> tempfile::TempDir {
    crate::test_support_paths::tempdir().expect("resolver fixture tempdir")
}

fn context(home: &Path, cwd: &Path) -> DiscoveryContext {
    DiscoveryContext::new(
        home,
        cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
}

fn windows_context(home: &Path, cwd: &Path, local_data: &Path) -> DiscoveryContext {
    DiscoveryContext::new(
        home,
        cwd,
        DiscoveryPlatform::Windows,
        DiscoveryPlatformDirs {
            local_data: Some(local_data.to_path_buf()),
            ..DiscoveryPlatformDirs::default()
        },
    )
}

fn write(path: &Path, body: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().expect("fixture parent")).unwrap();
    fs::write(path, body).unwrap();
}

fn write_nanoclaw_project(root: &Path) {
    write(&root.join("data/v2.db"), "sqlite fixture");
    fs::create_dir_all(root.join("data/v2-sessions")).unwrap();
}

fn nanoclaw_slug_for(root: &Path) -> String {
    nanoclaw_sha1_slug(root.to_string_lossy().as_bytes())
}

fn write_nanoclaw_systemd_unit(home: &Path, project: &Path) -> PathBuf {
    let slug = nanoclaw_slug_for(project);
    let unit = home
        .join(".config/systemd/user")
        .join(format!("nanoclaw-v2-{slug}.service"));
    write(
        &unit,
        format!(
            "[Unit]\nDescription=NanoClaw Personal Assistant\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=/usr/bin/node {}/dist/index.js\nWorkingDirectory={}\nRestart=always\nRestartSec=5\nKillMode=process\nEnvironment=HOME={}\nEnvironment=PATH=/usr/local/bin:/usr/bin:/bin:{}/.local/bin\nStandardOutput=append:{}/logs/nanoclaw.log\nStandardError=append:{}/logs/nanoclaw.error.log\n\n[Install]\nWantedBy=default.target",
            project.display(),
            project.display(),
            home.display(),
            home.display(),
            project.display(),
            project.display(),
        ),
    );
    unit
}

fn write_nanoclaw_launchd_plist(home: &Path, project: &Path) -> PathBuf {
    let slug = nanoclaw_slug_for(project);
    let label = format!("com.nanoclaw-v2-{slug}");
    let plist = home
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    write(
        &plist,
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n    <key>Label</key>\n    <string>{label}</string>\n    <key>ProgramArguments</key>\n    <array>\n        <string>/usr/local/bin/node</string>\n        <string>{}/dist/index.js</string>\n    </array>\n    <key>WorkingDirectory</key>\n    <string>{}</string>\n    <key>RunAtLoad</key>\n    <true/>\n    <key>KeepAlive</key>\n    <true/>\n    <key>EnvironmentVariables</key>\n    <dict>\n        <key>PATH</key>\n        <string>/usr/local/bin:/usr/bin:/bin:{}/.local/bin</string>\n        <key>HOME</key>\n        <string>{}</string>\n    </dict>\n    <key>StandardOutPath</key>\n    <string>{}/logs/nanoclaw.log</string>\n    <key>StandardErrorPath</key>\n    <string>{}/logs/nanoclaw.error.log</string>\n</dict>\n</plist>",
            project.display(),
            project.display(),
            home.display(),
            home.display(),
            project.display(),
            project.display(),
        ),
    );
    plist
}

fn report(context: &DiscoveryContext, provider: CaptureProvider) -> DiscoveryReport {
    discover_provider_sources_for_provider_with_context(context, provider)
}

#[test]
fn nanoclaw_linux_service_registration_discovers_checkout_without_cwd() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let project = temp.path().join("nanoclaw");
    fs::create_dir_all(&cwd).unwrap();
    write_nanoclaw_project(&project);
    write_nanoclaw_systemd_unit(&home, &project);

    let report = report(&context(&home, &cwd), CaptureProvider::NanoClaw);
    assert_eq!(report.issues, []);
    assert_eq!(report.sources.len(), 1);
    let source = &report.sources[0];
    assert_eq!(source.path, project);
    assert_eq!(source.source_format, "nanoclaw_project");
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.import_support, ProviderImportSupport::Native);
}

#[test]
fn nanoclaw_macos_launchd_registration_discovers_checkout() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let project = temp.path().join("nanoclaw");
    fs::create_dir_all(&cwd).unwrap();
    write_nanoclaw_project(&project);
    write_nanoclaw_launchd_plist(&home, &project);

    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::MacOS,
        DiscoveryPlatformDirs::default(),
    );
    let report = report(&context, CaptureProvider::NanoClaw);
    assert_eq!(report.issues, []);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, project);
    assert_eq!(
        report.sources[0].import_support,
        ProviderImportSupport::Native
    );
}

#[test]
fn nanoclaw_exact_cwd_coexists_with_distinct_registration_and_dedupes_itself() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd-nanoclaw");
    let registered = temp.path().join("registered-nanoclaw");
    write_nanoclaw_project(&cwd);
    write_nanoclaw_project(&registered);
    write_nanoclaw_systemd_unit(&home, &cwd);
    write_nanoclaw_systemd_unit(&home, &registered);

    let report = report(&context(&home, &cwd), CaptureProvider::NanoClaw);
    assert_eq!(report.issues, []);
    assert_eq!(
        report
            .sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>(),
        vec![cwd, registered]
    );
}

#[test]
fn nanoclaw_two_registered_checkouts_coexist_deterministically() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let first = temp.path().join("nanoclaw-first");
    let second = temp.path().join("nanoclaw-second");
    fs::create_dir_all(&cwd).unwrap();
    write_nanoclaw_project(&first);
    write_nanoclaw_project(&second);
    write_nanoclaw_systemd_unit(&home, &first);
    write_nanoclaw_systemd_unit(&home, &second);

    let mut expected = vec![first, second];
    expected.sort_by_key(|project| nanoclaw_slug_for(project));
    let discovery = context(&home, &cwd);
    for _ in 0..2 {
        let report = report(&discovery, CaptureProvider::NanoClaw);
        assert_eq!(report.issues, []);
        assert_eq!(
            report
                .sources
                .iter()
                .map(|source| source.path.clone())
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn nanoclaw_systemd_paths_follow_upstream_and_systemd_literal_quoting() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let project = temp.path().join("NanoClaw install with spaces");
    fs::create_dir_all(&cwd).unwrap();
    write_nanoclaw_project(&project);
    let unit = write_nanoclaw_systemd_unit(&home, &project);

    let discovery = context(&home, &cwd);
    let upstream = report(&discovery, CaptureProvider::NanoClaw);
    assert_eq!(upstream.issues, []);
    assert_eq!(upstream.sources[0].path, project);

    write(
        &unit,
        format!(
            "[Service]\nExecStart=\"/opt/Node Runtime/bin/node\" \"{}/dist/index.js\"\nWorkingDirectory=\"{}\"\n",
            project.display(),
            project.display(),
        ),
    );
    let quoted = report(&discovery, CaptureProvider::NanoClaw);
    assert_eq!(quoted.issues, []);
    assert_eq!(quoted.sources[0].path, project);

    let escaped_project = project.to_string_lossy().replace(' ', "\\s");
    write(
        &unit,
        format!(
            "[Service]\nExecStart=/opt/Node\\sRuntime/bin/node {escaped_project}/dist/index.js\nWorkingDirectory={escaped_project}\n",
        ),
    );
    let escaped = report(&discovery, CaptureProvider::NanoClaw);
    assert_eq!(escaped.issues, []);
    assert_eq!(escaped.sources[0].path, project);

    for unsafe_exec_start in [
        format!(
            "/usr/bin/node \"{}/dist/index.js\" --inspect",
            project.display()
        ),
        "/usr/bin/node \"${NANOCLAW_ROOT}/dist/index.js\"".to_owned(),
    ] {
        write(
            &unit,
            format!(
                "[Service]\nExecStart={unsafe_exec_start}\nWorkingDirectory=\"{}\"\n",
                project.display()
            ),
        );
        let unsafe_registration = report(&discovery, CaptureProvider::NanoClaw);
        assert!(unsafe_registration.sources.is_empty());
        assert_eq!(unsafe_registration.issues.len(), 1);
    }
}

#[test]
fn nanoclaw_launchd_rejects_nested_misordered_duplicate_and_malformed_fields() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let project = temp.path().join("nanoclaw");
    fs::create_dir_all(&cwd).unwrap();
    write_nanoclaw_project(&project);
    let plist = write_nanoclaw_launchd_plist(&home, &project);
    let label = format!("com.nanoclaw-v2-{}", nanoclaw_slug_for(&project));
    let program_arguments = format!(
        "<key>ProgramArguments</key><array><string>/usr/local/bin/node</string><string>{}/dist/index.js</string></array>",
        project.display()
    );
    let working_directory = format!(
        "<key>WorkingDirectory</key><string>{}</string>",
        project.display()
    );
    let cases = [
        format!(
            "<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string>{program_arguments}{working_directory}<key>EnvironmentVariables</key><dict><key>WorkingDirectory</key><string>/nested</string></dict></dict></plist>"
        ),
        format!(
            "<plist version=\"1.0\"><dict><string>{label}</string><key>Label</key>{program_arguments}{working_directory}</dict></plist>"
        ),
        format!(
            "<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string><key>Label</key><string>{label}</string>{program_arguments}{working_directory}</dict></plist>"
        ),
        format!(
            "<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string>{program_arguments}{working_directory}</array></plist>"
        ),
    ];

    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::MacOS,
        DiscoveryPlatformDirs::default(),
    );
    for body in cases {
        write(&plist, body);
        let report = report(&discovery, CaptureProvider::NanoClaw);
        assert!(report.sources.is_empty());
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].path.as_deref(), Some(plist.as_path()));
    }
}

#[test]
fn nanoclaw_launchd_decodes_entities_without_trimming_key_or_path_text() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let project = temp.path().join("nanoclaw & checkout");
    fs::create_dir_all(&cwd).unwrap();
    write_nanoclaw_project(&project);
    let plist = write_nanoclaw_launchd_plist(&home, &project);
    let label = format!("com.nanoclaw-v2-{}", nanoclaw_slug_for(&project));
    let encoded_project = project.to_string_lossy().replace('&', "&amp;");
    let body = |label_key: &str, label_value: &str, working_directory: &str| {
        format!(
            "<plist version=\"1.0\"><dict><key>{label_key}</key><string>{label_value}</string><key>ProgramArguments</key><array><string>/usr/local/bin/node</string><string>{encoded_project}/dist/index.js</string></array><key>WorkingDirectory</key><string>{working_directory}</string></dict></plist>"
        )
    };
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::MacOS,
        DiscoveryPlatformDirs::default(),
    );

    write(&plist, body("Lab&#x65;l", &label, &encoded_project));
    let valid = report(&discovery, CaptureProvider::NanoClaw);
    assert_eq!(valid.issues, []);
    assert_eq!(valid.sources[0].path, project);

    for invalid in [
        body(" Label ", &label, &encoded_project),
        body("&#32;Label&#32;", &label, &encoded_project),
        body("Label", &format!(" {label} "), &encoded_project),
        body("Label", &label, &format!(" {encoded_project} ")),
        body("Label", &label, &format!("&#32;{encoded_project}&#32;")),
    ] {
        write(&plist, invalid);
        let report = report(&discovery, CaptureProvider::NanoClaw);
        assert!(report.sources.is_empty());
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].path.as_deref(), Some(plist.as_path()));
    }
}

#[test]
fn nanoclaw_sha1_slug_matches_external_known_vectors() {
    // FIPS PUB 180-1's standard "abc" vector and the standard empty digest.
    assert_eq!(nanoclaw_sha1_slug(b"abc"), "a9993e36");
    assert_eq!(nanoclaw_sha1_slug(b""), "da39a3ee");
}

#[test]
fn nanoclaw_no_install_has_no_sources_or_issues() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();

    let report = report(&context(&home, &cwd), CaptureProvider::NanoClaw);
    assert!(report.sources.is_empty());
    assert!(report.issues.is_empty());
}

#[test]
fn nanoclaw_system_registry_selection_uses_effective_uid_not_home() {
    let effective_root = DiscoveryContext::new(
        "/srv/preserved-home",
        "/work/nanoclaw",
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_effective_uid(0);
    assert_eq!(
        nanoclaw_systemd_registry_dirs(&effective_root),
        vec![
            PathBuf::from("/srv/preserved-home/.config/systemd/user"),
            PathBuf::from("/etc/systemd/system"),
        ]
    );

    let non_root_with_root_home = DiscoveryContext::new(
        "/root",
        "/work/nanoclaw",
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_effective_uid(1000);
    assert_eq!(
        nanoclaw_systemd_registry_dirs(&non_root_with_root_home),
        vec![PathBuf::from("/root/.config/systemd/user")]
    );
}

#[test]
fn nanoclaw_over_limit_registry_reports_selector_limit_at_registry_directory() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let registry = home.join(".config/systemd/user");
    fs::create_dir_all(&cwd).unwrap();
    for index in 0..=super::super::super::selectors::MAX_DIRECT_DIRECTORY_ENTRIES {
        write(&registry.join(format!("unrelated-{index:04}.service")), "");
    }

    let report = report(&context(&home, &cwd), CaptureProvider::NanoClaw);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].path.as_deref(), Some(registry.as_path()));
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
    assert_eq!(report.issues[0].reason, SELECTOR_LIMIT_REASON);
}

#[cfg(unix)]
#[test]
fn nanoclaw_systemd_registry_ignores_unrelated_symlinks_and_reads_valid_unit() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let project = temp.path().join("nanoclaw");
    fs::create_dir_all(&cwd).unwrap();
    write_nanoclaw_project(&project);
    let unit = write_nanoclaw_systemd_unit(&home, &project);
    let registry = unit.parent().unwrap();

    let unrelated_unit = temp.path().join("unrelated.service");
    let unrelated_wants = temp.path().join("multi-user.target.wants");
    write(&unrelated_unit, "[Service]\nExecStart=/bin/true\n");
    fs::create_dir_all(&unrelated_wants).unwrap();
    symlink(
        &unrelated_unit,
        registry.join("dbus-org.freedesktop.timesync1.service"),
    )
    .unwrap();
    symlink(&unrelated_wants, registry.join("multi-user.target.wants")).unwrap();

    let report = report(&context(&home, &cwd), CaptureProvider::NanoClaw);
    assert_eq!(report.issues, []);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, project);
}

#[cfg(unix)]
#[test]
fn nanoclaw_unsafe_registry_reports_selector_issue_at_registry_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let registry = home.join(".config/systemd/user");
    let linked_registry = temp.path().join("linked-registry");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(registry.parent().unwrap()).unwrap();
    fs::create_dir_all(&linked_registry).unwrap();
    symlink(&linked_registry, &registry).unwrap();

    let report = report(&context(&home, &cwd), CaptureProvider::NanoClaw);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].path.as_deref(), Some(registry.as_path()));
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
    assert_eq!(report.issues[0].reason, NANOCLAW_SERVICE_REGISTRY_REASON);
}

#[test]
fn nanoclaw_malformed_or_mismatched_service_registration_fails_closed() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let project = temp.path().join("nanoclaw");
    fs::create_dir_all(&cwd).unwrap();
    write_nanoclaw_project(&project);
    let unit = write_nanoclaw_systemd_unit(&home, &project);
    let wrong_project = temp.path().join("wrong");
    write(
        &unit,
        format!(
            "[Service]\nExecStart=/usr/bin/node {}/dist/index.js\nWorkingDirectory={}\n",
            wrong_project.display(),
            project.display()
        ),
    );

    let report = report(&context(&home, &cwd), CaptureProvider::NanoClaw);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].path.as_deref(), Some(unit.as_path()));
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn nanoclaw_registration_to_missing_checkout_fails_closed() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let project = temp.path().join("missing-nanoclaw");
    fs::create_dir_all(&cwd).unwrap();
    let unit = write_nanoclaw_systemd_unit(&home, &project);

    let report = report(&context(&home, &cwd), CaptureProvider::NanoClaw);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].path.as_deref(), Some(unit.as_path()));
}

#[cfg(unix)]
#[test]
fn nanoclaw_registration_to_symlink_checkout_fails_closed() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let project = temp.path().join("nanoclaw");
    let linked = temp.path().join("linked-nanoclaw");
    fs::create_dir_all(&cwd).unwrap();
    write_nanoclaw_project(&project);
    symlink(&project, &linked).unwrap();
    let unit = write_nanoclaw_systemd_unit(&home, &linked);

    let report = report(&context(&home, &cwd), CaptureProvider::NanoClaw);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].path.as_deref(), Some(unit.as_path()));
}

#[test]
fn openclaw_selects_one_override_and_bounded_configured_agents_as_unsupported() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let state = temp.path().join("selected");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &state.join("openclaw.json"),
        "{agents: {$include: './agents.json5'}}",
    );
    write(
        &state.join("agents.json5"),
        "{list: [{id: 'Ops'}, {id: 'research'}]}",
    );
    for id in ["ops", "research"] {
        write(
            &state
                .join("agents")
                .join(id)
                .join("agent/openclaw-agent.sqlite"),
            "sqlite",
        );
    }
    write(
        &home.join(".openclaw/agents/main/agent/openclaw-agent.sqlite"),
        "stale",
    );
    let context = context(&home, &cwd).with_env("OPENCLAW_STATE_DIR", state.as_os_str().to_owned());
    let report = report(&context, CaptureProvider::OpenClaw);
    assert_eq!(
        report
            .sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>(),
        vec![
            state.join("agents/ops/agent/openclaw-agent.sqlite"),
            state.join("agents/research/agent/openclaw-agent.sqlite")
        ]
    );
    assert!(report.sources.iter().all(|source| {
        source.status == ProviderSourceStatus::Unsupported
            && source.source_kind == ProviderSourceKind::DetectionOnly
            && source.unsupported_reason == Some(OPENCLAW_UNSUPPORTED_REASON)
            && provider_source_for_path(CaptureProvider::OpenClaw, source.path.clone())
                .source_format
                == source.source_format
    }));
    assert!(report.issues.is_empty());
}

#[test]
fn openclaw_uses_conditional_clawdbot_but_never_moltbot_or_legacy_jsonl() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &home.join(".clawdbot/agents/main/agent/openclaw-agent.sqlite"),
        "sqlite",
    );
    write(
        &home.join(".moltbot/agents/main/agent/openclaw-agent.sqlite"),
        "false",
    );
    write(
        &home.join(".clawdbot/agents/main/sessions/legacy.jsonl"),
        "{}",
    );
    let report = report(&context(&home, &cwd), CaptureProvider::OpenClaw);
    assert_eq!(report.sources.len(), 1);
    assert!(report.sources[0].path.starts_with(home.join(".clawdbot")));
    assert!(!report.sources[0].path.to_string_lossy().contains("moltbot"));
    assert!(!report
        .sources
        .iter()
        .any(|source| source.source_format == "openclaw_session_jsonl_tree"));
}

#[test]
fn openclaw_include_escape_is_manual_and_does_not_scan_agents() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let state = home.join(".openclaw");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &state.join("openclaw.json"),
        "{$include: '../outside.json5'}",
    );
    write(
        &home.join("outside.json5"),
        "{agents:{list:[{id:'secret'}]}}",
    );
    write(
        &state.join("agents/secret/agent/openclaw-agent.sqlite"),
        "sqlite",
    );
    let report = report(&context(&home, &cwd), CaptureProvider::OpenClaw);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
    assert!(!report.issues[0].reason.contains("secret"));
}

#[cfg(unix)]
#[test]
fn openclaw_include_rejects_link_components_before_canonicalization() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let state = home.join(".openclaw");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &state.join("openclaw.json"),
        "{agents: {$include: './linked/agents.json5'}}",
    );
    write(&state.join("actual/agents.json5"), "{list:[{id:'secret'}]}");
    write(
        &state.join("agents/secret/agent/openclaw-agent.sqlite"),
        "sqlite",
    );
    symlink(state.join("actual"), state.join("linked")).unwrap();

    let report = report(&context(&home, &cwd), CaptureProvider::OpenClaw);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[cfg(unix)]
#[test]
fn openclaw_include_rejects_link_leaf_before_canonicalization() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let state = home.join(".openclaw");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &state.join("openclaw.json"),
        "{agents: {$include: './linked.json5'}}",
    );
    write(&state.join("actual.json5"), "{list:[{id:'secret'}]}");
    write(
        &state.join("agents/secret/agent/openclaw-agent.sqlite"),
        "sqlite",
    );
    symlink(state.join("actual.json5"), state.join("linked.json5")).unwrap();

    let report = report(&context(&home, &cwd), CaptureProvider::OpenClaw);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn hermes_selects_only_sticky_profile_for_ordinary_operation() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = home.join(".hermes");
    fs::create_dir_all(&cwd).unwrap();
    write(&root.join("active_profile"), "work\n");
    write(&root.join("profiles/work/state.db"), "db");
    write(&root.join("profiles/inactive/state.db"), "db");
    let report = report(&context(&home, &cwd), CaptureProvider::Hermes);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, root.join("profiles/work/state.db"));
}

#[cfg(unix)]
#[test]
fn hermes_unreadable_optional_selector_suppresses_default_profile() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = home.join(".hermes");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&root).unwrap();
    write(&root.join("state.db"), "must remain suppressed");
    let original = fs::metadata(&root).unwrap().permissions();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
    let report = report(&context(&home, &cwd), CaptureProvider::Hermes);
    fs::set_permissions(&root, original).unwrap();

    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

#[test]
fn hermes_gateway_multiplex_enumerates_default_and_sorted_valid_profiles() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let root = home.join(".hermes");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &root.join("config.yaml"),
        "gateway:\n  multiplex_profiles: true\n",
    );
    write(&root.join("state.db"), "db");
    write(&root.join("profiles/zeta/state.db"), "db");
    write(&root.join("profiles/alpha/state.db"), "db");
    write(&root.join("profiles/Bad.Name/state.db"), "db");
    let report = report(&context(&home, &cwd), CaptureProvider::Hermes);
    assert_eq!(
        report
            .sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>(),
        vec![
            root.join("state.db"),
            root.join("profiles/alpha/state.db"),
            root.join("profiles/zeta/state.db")
        ]
    );
}

#[test]
fn hermes_windows_default_prefers_modern_then_conditional_legacy() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let local = temp.path().join("local");
    fs::create_dir_all(&cwd).unwrap();
    write(&home.join(".hermes/state.db"), "legacy");
    let legacy = report(
        &windows_context(&home, &cwd, &local),
        CaptureProvider::Hermes,
    );
    assert_eq!(legacy.sources[0].path, home.join(".hermes/state.db"));
    write(&local.join("hermes/state.db"), "modern");
    let modern = report(
        &windows_context(&home, &cwd, &local),
        CaptureProvider::Hermes,
    );
    assert_eq!(modern.sources[0].path, local.join("hermes/state.db"));
}

#[test]
fn nanoclaw_exact_cwd_requires_project_shape_and_is_native() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let root = temp.path().join("nanoclaw");
    let child = root.join("child");
    fs::create_dir_all(&child).unwrap();
    write(&root.join("data/v2.db"), "db");
    fs::create_dir_all(root.join("data/v2-sessions")).unwrap();
    assert!(report(&context(&home, &child), CaptureProvider::NanoClaw)
        .sources
        .is_empty());
    let exact = report(&context(&home, &root), CaptureProvider::NanoClaw);
    assert_eq!(exact.sources.len(), 1);
    assert_eq!(exact.sources[0].path, root);
    assert_eq!(
        exact.sources[0].import_support,
        super::super::super::types::ProviderImportSupport::Native
    );
}

#[test]
fn astrbot_cli_marker_forces_exact_cwd_and_launcher_instances_coexist() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("astrbot-cli");
    let stale = temp.path().join("env-root");
    write(&cwd.join(".astrbot"), "");
    write(&cwd.join("data/data_v4.db"), "db");
    write(&stale.join("data/data_v4.db"), "db");
    let launcher =
        home.join(".astrbot_launcher/instances/123e4567-e89b-12d3-a456-426614174000/core");
    write(&launcher.join("data/data_v4.db"), "db");
    write(
        &home.join(".astrbot_launcher/instances/not-a-uuid/core/data/data_v4.db"),
        "db",
    );
    let context = context(&home, &cwd).with_env("ASTRBOT_ROOT", stale.as_os_str().to_owned());
    let report = report(&context, CaptureProvider::AstrBot);
    assert_eq!(
        report
            .sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>(),
        vec![
            cwd.join("data/data_v4.db"),
            launcher.join("data/data_v4.db")
        ]
    );
}

#[test]
fn astrbot_does_not_search_cwd_ancestors() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let root = temp.path().join("astrbot");
    let child = root.join("child");
    fs::create_dir_all(&child).unwrap();
    write(&root.join("data/data_v4.db"), "db");
    let report = report(&context(&home, &child), CaptureProvider::AstrBot);
    assert!(report
        .sources
        .iter()
        .all(|source| source.path != root.join("data/data_v4.db")));
}

#[test]
fn shelley_uses_only_exact_cwd_and_ignores_helper_environment() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    let helper = temp.path().join("helper.db");
    fs::create_dir_all(&cwd).unwrap();
    write(&cwd.join("shelley.db"), "db");
    write(&helper, "db");
    write(&home.join(".config/shelley/shelley.db"), "db");
    let context = context(&home, &cwd).with_env("SHELLEY_DB", helper.as_os_str().to_owned());
    let report = report(&context, CaptureProvider::Shelley);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, cwd.join("shelley.db"));
}

#[test]
fn openhands_v1_precedence_and_optional_user_partition_are_winner_only() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let oh = temp.path().join("oh");
    let legacy = temp.path().join("legacy");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &oh.join("alice/v1_conversations/0123456789abcdef/event.json"),
        "{}",
    );
    write(
        &legacy.join("v1_conversations/0123456789abcdef/event.json"),
        "{}",
    );
    let context = context(&home, &cwd)
        .with_env("OH_PERSISTENCE_DIR", oh.as_os_str().to_owned())
        .with_env("FILE_STORE_PATH", legacy.as_os_str().to_owned())
        .with_env("OPENHANDS_USER_ID", "alice");
    let report = report(&context, CaptureProvider::OpenHands);
    assert_eq!(
        report
            .sources
            .iter()
            .filter(|source| source.source_format == "openhands_file_events")
            .map(|source| source.path.clone())
            .collect::<Vec<_>>(),
        vec![oh.join("alice")]
    );
}

#[test]
fn openhands_remote_backend_selectors_suppress_v1_disk_root_with_primary_precedence() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &home.join(".openhands/v1_conversations/0123456789abcdef/event.json"),
        "{}",
    );

    let primary = context(&home, &cwd)
        .with_env("SHARED_EVENT_STORAGE_PROVIDER", "s3")
        .with_env("FILE_STORE", "filesystem")
        .with_env("FILE_STORE_PATH", "bucket-not-a-path");
    let primary_report = report(&primary, CaptureProvider::OpenHands);
    assert!(primary_report
        .sources
        .iter()
        .all(|source| source.source_format != "openhands_file_events"));
    assert_eq!(primary_report.issues.len(), 1);
    assert_eq!(
        primary_report.issues[0].kind,
        DiscoveryIssueKind::NoDiskHistory
    );

    let legacy = context(&home, &cwd)
        .with_env("SHARED_EVENT_STORAGE_PROVIDER", "")
        .with_env("FILE_STORE", "google_cloud")
        .with_env("FILE_STORE_PATH", "bucket-not-a-path");
    let legacy_report = report(&legacy, CaptureProvider::OpenHands);
    assert!(legacy_report
        .sources
        .iter()
        .all(|source| source.source_format != "openhands_file_events"));
    assert_eq!(legacy_report.issues.len(), 1);
    assert_eq!(
        legacy_report.issues[0].kind,
        DiscoveryIssueKind::NoDiskHistory
    );
}

#[test]
fn openhands_empty_oh_uses_exact_cwd_and_detects_cli_events_as_unsupported() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    write(
        &cwd.join("v1_conversations/0123456789abcdef/event.json"),
        "{}",
    );
    let cli = temp.path().join("cli");
    write(&cli.join("conversation/events/event-1.json"), "{}");
    let context = context(&home, &cwd)
        .with_env("OH_PERSISTENCE_DIR", "")
        .with_env("OPENHANDS_CONVERSATIONS_DIR", cli.as_os_str().to_owned());
    let report = report(&context, CaptureProvider::OpenHands);
    assert_eq!(report.sources.len(), 2);
    assert_eq!(report.sources[0].path, cwd);
    assert_eq!(report.sources[1].path, cli.join("conversation"));
    assert_eq!(report.sources[1].status, ProviderSourceStatus::Unsupported);
    assert_eq!(
        report.sources[1].unsupported_reason,
        Some(OPENHANDS_CLI_UNSUPPORTED_REASON)
    );
    let explicit = provider_source_for_path(CaptureProvider::OpenHands, cli.join("conversation"));
    assert_eq!(report.sources[1].source_format, explicit.source_format);
    assert_eq!(explicit.status, ProviderSourceStatus::Unsupported);
}

#[test]
fn exact_selected_paths_keep_the_same_explicit_formats() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    write(&cwd.join("data/v2.db"), "fixture");
    fs::create_dir_all(cwd.join("data/v2-sessions")).unwrap();
    write(
        &home.join(".openhands/v1_conversations/0123456789abcdef0123456789abcdef/event.json"),
        "{}",
    );
    write(&home.join(".hermes/state.db"), "fixture");
    write(&cwd.join("data/data_v4.db"), "fixture");
    write(&cwd.join("shelley.db"), "fixture");
    for (provider, path, format) in [
        (
            CaptureProvider::Hermes,
            home.join(".hermes/state.db"),
            "hermes_state_sqlite",
        ),
        (CaptureProvider::NanoClaw, cwd.clone(), "nanoclaw_project"),
        (
            CaptureProvider::AstrBot,
            cwd.join("data/data_v4.db"),
            "astrbot_data_v4_sqlite",
        ),
        (
            CaptureProvider::Shelley,
            cwd.join("shelley.db"),
            "shelley_sqlite",
        ),
        (
            CaptureProvider::OpenHands,
            home.join(".openhands"),
            "openhands_file_events",
        ),
    ] {
        let discovered = report(&context(&home, &cwd), provider)
            .sources
            .into_iter()
            .find(|source| source.path == path)
            .expect("selected source");
        let explicit = provider_source_for_path(provider, path);
        assert_eq!(discovered.source_format, format);
        assert_eq!(discovered.source_format, explicit.source_format);
    }
}

#[cfg(unix)]
#[test]
fn selected_symlink_roots_are_manual_instead_of_followed() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let target = temp.path().join("target");
    let link = temp.path().join("link");
    fs::create_dir_all(&cwd).unwrap();
    write(&target.join("data/data_v4.db"), "db");
    symlink(&target, &link).unwrap();
    let context = context(&home, &cwd).with_env("ASTRBOT_ROOT", link.as_os_str().to_owned());
    let report = report(&context, CaptureProvider::AstrBot);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
}
