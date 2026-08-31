use super::*;

#[test]
fn provider_root_replace_atomically_changes_path_and_complete_group_state() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let original = provider_parent.path().join("claude-original");
    let replacement = provider_parent.path().join("claude-replacement");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&replacement).unwrap();
    fs::write(
        data_root.path().join(CONFIG_FILE),
        "[analytics]\nenabled = false\n\n[sources]\nautomatic = false\n",
    )
    .unwrap();

    add_claude_root(
        data_root.path(),
        "work",
        &original,
        Some("old-group"),
        false,
    )
    .unwrap();
    let config_path = data_root.path().join(CONFIG_FILE);
    let before_rejected_replace = fs::read(&config_path).unwrap();
    let error = format!(
        "{:#}",
        add_claude_root(
            data_root.path(),
            "work",
            &replacement,
            Some("new-group"),
            false,
        )
        .unwrap_err()
    );
    assert!(error.contains("pass --replace"), "{error}");
    assert_eq!(fs::read(&config_path).unwrap(), before_rejected_replace);

    let replaced = add_claude_root(
        data_root.path(),
        "work",
        &replacement,
        Some("new-group"),
        true,
    )
    .unwrap();
    assert!(replaced.changed);
    assert!(replaced.replaced);
    assert_eq!(replaced.root.provider, CaptureProvider::Claude);
    assert_eq!(replaced.root.group.as_deref(), Some("new-group"));
    assert_eq!(replaced.root.path, fs::canonicalize(&replacement).unwrap());
    let loaded = AppConfig::load(data_root.path()).unwrap();
    assert!(!loaded.analytics.enabled);
    assert!(!loaded.automatic_source_discovery_enabled());
    assert_eq!(loaded.provider_roots["work"], replaced.root);

    let cleared = add_claude_root(data_root.path(), "work", &original, None, true).unwrap();
    assert!(cleared.changed);
    assert!(cleared.replaced);
    assert_eq!(cleared.root.group, None);
    let cleared_text = fs::read_to_string(&config_path).unwrap();
    assert!(!cleared_text.contains("group ="), "{cleared_text}");

    let before_noop = fs::read(&config_path).unwrap();
    let unchanged = add_claude_root(data_root.path(), "work", &original, None, false).unwrap();
    assert!(!unchanged.changed);
    assert!(!unchanged.replaced);
    assert_eq!(fs::read(&config_path).unwrap(), before_noop);
}

#[test]
fn provider_root_replace_rejects_provider_changes_under_a_stable_name() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let claude_root = provider_parent.path().join("claude");
    let codex_root = provider_parent.path().join("codex");
    fs::create_dir(&claude_root).unwrap();
    fs::create_dir(&codex_root).unwrap();
    add_claude_root(data_root.path(), "work", &claude_root, Some("team"), false).unwrap();
    let config_path = data_root.path().join(CONFIG_FILE);
    let before = fs::read(&config_path).unwrap();

    let error = format!(
        "{:#}",
        add_provider_root_with_kind(
            data_root.path(),
            "work",
            CaptureProvider::Codex,
            &codex_root,
            Some("team"),
            None,
            true,
        )
        .unwrap_err()
    );

    assert!(error.contains("provider cannot be changed"), "{error}");
    assert_eq!(fs::read(&config_path).unwrap(), before);
}

#[test]
fn provider_root_mutation_rejects_a_second_name_for_the_same_physical_root() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_root = tempfile::tempdir().unwrap();
    add_claude_root(
        data_root.path(),
        "personal",
        provider_root.path(),
        None,
        false,
    )
    .unwrap();
    let config_path = data_root.path().join(CONFIG_FILE);
    let before = fs::read(&config_path).unwrap();

    let error = format!(
        "{:#}",
        add_claude_root(data_root.path(), "work", provider_root.path(), None, false).unwrap_err()
    );

    assert!(
        error.contains("same physical root as `personal`"),
        "{error}"
    );
    assert_eq!(fs::read(&config_path).unwrap(), before);
}

#[test]
fn provider_root_replace_adds_an_absent_name_and_remove_rejects_a_missing_name() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_root = tempfile::tempdir().unwrap();

    let added =
        add_claude_root(data_root.path(), "work", provider_root.path(), None, true).unwrap();
    assert!(added.changed);
    assert!(!added.replaced);

    let config_path = data_root.path().join(CONFIG_FILE);
    let before = fs::read(&config_path).unwrap();
    let error = format!(
        "{:#}",
        remove_provider_root(data_root.path(), "missing").unwrap_err()
    );
    assert!(error.contains("is not configured"), "{error}");
    assert_eq!(fs::read(&config_path).unwrap(), before);
}

#[test]
fn provider_root_cli_mutation_uses_every_enabled_capability_path_kind() {
    use ctx_history_capture::{
        configured_root_capabilities, ConfiguredRootPathKind, ProviderRootKind,
    };

    for capability in configured_root_capabilities()
        .iter()
        .filter(|capability| capability.state.is_enabled())
    {
        let data_root = tempfile::tempdir().unwrap();
        let provider_parent = tempfile::tempdir().unwrap();
        let original = provider_parent.path().join("original");
        let replacement = provider_parent.path().join("replacement");
        let wrong_kind = provider_parent.path().join("wrong-kind");
        let expected = capability.state.expected_path_kind().unwrap();
        match expected {
            ConfiguredRootPathKind::Directory => {
                fs::create_dir(&original).unwrap();
                fs::create_dir(&replacement).unwrap();
                fs::write(&wrong_kind, b"not a directory").unwrap();
            }
            ConfiguredRootPathKind::File => {
                fs::write(&original, b"history").unwrap();
                fs::write(&replacement, b"replacement history").unwrap();
                fs::create_dir(&wrong_kind).unwrap();
            }
        }

        let kind = (capability.provider == CaptureProvider::OpenHands)
            .then_some(ProviderRootKind::OpenHandsCurrentConversations);
        add_provider_root_with_kind(
            data_root.path(),
            "root",
            capability.provider,
            &original,
            None,
            kind,
            false,
        )
        .unwrap();
        let persisted = fs::read_to_string(data_root.path().join(CONFIG_FILE)).unwrap();
        assert!(
            persisted.contains(&format!("provider = {:?}", capability.provider.as_str())),
            "{} did not retain its canonical storage identifier: {persisted}",
            capability.provider.as_str()
        );
        assert_eq!(
            AppConfig::load(data_root.path()).unwrap().provider_roots["root"].provider,
            capability.provider
        );
        let replaced = add_provider_root_with_kind(
            data_root.path(),
            "root",
            capability.provider,
            &replacement,
            None,
            kind,
            true,
        )
        .unwrap();
        assert!(replaced.replaced);
        let config_path = data_root.path().join(CONFIG_FILE);
        let before_wrong_kind = fs::read(&config_path).unwrap();

        let error = format!(
            "{:#}",
            add_provider_root_with_kind(
                data_root.path(),
                "root",
                capability.provider,
                &wrong_kind,
                None,
                kind,
                true,
            )
            .unwrap_err()
        );
        let kind = match expected {
            ConfiguredRootPathKind::Directory => "directory",
            ConfiguredRootPathKind::File => "file",
        };
        assert!(
            error.contains(&format!("existing non-symlink {kind}")),
            "{error}"
        );
        assert_eq!(fs::read(&config_path).unwrap(), before_wrong_kind);
    }
}

#[test]
fn provider_root_replace_revalidates_a_concurrent_config_edit_after_locking() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let original = provider_parent.path().join("original");
    let replacement = provider_parent.path().join("replacement");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&replacement).unwrap();
    add_claude_root(data_root.path(), "work", &original, Some("team"), false).unwrap();
    let config_path = AppConfig::config_path(data_root.path());
    let lock = durable_write::ConfigMutationLock::acquire(&config_path).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let data_root_path = data_root.path().to_path_buf();
    let replacement_path = replacement.clone();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = add_claude_root(&data_root_path, "work", &replacement_path, None, true);
        finished_tx.send(result).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(
        finished_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "a concurrent replacement must not pass the config lock"
    );
    let concurrent_text = format!(
        "[sources.roots.work]\nprovider = \"claude\"\npath = {:?}\ngroup = \"team\"\n\n[search]\nsemantics = true\n",
        original.display().to_string()
    );
    fs::write(&config_path, &concurrent_text).unwrap();
    drop(lock);
    let error = format!(
        "{:#}",
        finished_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("provider-root replacement did not resume after unlock")
            .unwrap_err()
    );
    worker.join().unwrap();
    assert!(error.contains("unknown config key"), "{error}");
    assert!(error.contains("search.semantics"), "{error}");
    assert_eq!(fs::read_to_string(&config_path).unwrap(), concurrent_text);
}

#[test]
fn openhands_root_kind_is_required_replaced_atomically_and_rejects_overlap() {
    use ctx_history_capture::ProviderRootKind;

    let data_root = tempfile::tempdir().unwrap();
    let fixture = tempfile::tempdir().unwrap();
    let legacy = fixture.path().join("legacy");
    let current = legacy.join("conversations");
    let disjoint_current = fixture.path().join("current");
    let reverse_current = fixture.path().join("reverse-current");
    let reverse_legacy = reverse_current.join("legacy-persistence");
    fs::create_dir_all(&current).unwrap();
    fs::create_dir(&disjoint_current).unwrap();
    fs::create_dir_all(&reverse_legacy).unwrap();

    let missing_kind = format!(
        "{:#}",
        add_provider_root_with_kind(
            data_root.path(),
            "legacy",
            CaptureProvider::OpenHands,
            &legacy,
            None,
            None,
            false,
        )
        .unwrap_err()
    );
    assert!(missing_kind.contains("require --kind"), "{missing_kind}");

    let added = add_provider_root_with_kind(
        data_root.path(),
        "legacy",
        CaptureProvider::OpenHands,
        &legacy,
        None,
        Some(ProviderRootKind::OpenHandsLegacyPersistence),
        false,
    )
    .unwrap();
    assert_eq!(
        added.root.kind,
        Some(ProviderRootKind::OpenHandsLegacyPersistence)
    );
    let config_path = AppConfig::config_path(data_root.path());
    let before = fs::read(&config_path).unwrap();

    let overlap = format!(
        "{:#}",
        add_provider_root_with_kind(
            data_root.path(),
            "current",
            CaptureProvider::OpenHands,
            &current,
            None,
            Some(ProviderRootKind::OpenHandsCurrentConversations),
            false,
        )
        .unwrap_err()
    );
    assert!(overlap.contains("overlaps legacy/current"), "{overlap}");
    assert_eq!(fs::read(&config_path).unwrap(), before);

    let changed_kind_without_replace = format!(
        "{:#}",
        add_provider_root_with_kind(
            data_root.path(),
            "legacy",
            CaptureProvider::OpenHands,
            &legacy,
            None,
            Some(ProviderRootKind::OpenHandsCurrentConversations),
            false,
        )
        .unwrap_err()
    );
    assert!(changed_kind_without_replace.contains("pass --replace"));

    let current = add_provider_root_with_kind(
        data_root.path(),
        "current",
        CaptureProvider::OpenHands,
        &disjoint_current,
        None,
        Some(ProviderRootKind::OpenHandsCurrentConversations),
        false,
    )
    .unwrap();
    assert_eq!(
        current.root.kind,
        Some(ProviderRootKind::OpenHandsCurrentConversations)
    );
    let before_noop = fs::read(&config_path).unwrap();
    let noop = add_provider_root_with_kind(
        data_root.path(),
        "current",
        CaptureProvider::OpenHands,
        &disjoint_current,
        None,
        Some(ProviderRootKind::OpenHandsCurrentConversations),
        true,
    )
    .unwrap();
    assert!(!noop.changed);
    assert_eq!(fs::read(&config_path).unwrap(), before_noop);

    add_provider_root_with_kind(
        data_root.path(),
        "reverse-current",
        CaptureProvider::OpenHands,
        &reverse_current,
        None,
        Some(ProviderRootKind::OpenHandsCurrentConversations),
        false,
    )
    .unwrap();
    let reverse_overlap = format!(
        "{:#}",
        add_provider_root_with_kind(
            data_root.path(),
            "reverse-legacy",
            CaptureProvider::OpenHands,
            &reverse_legacy,
            None,
            Some(ProviderRootKind::OpenHandsLegacyPersistence),
            false,
        )
        .unwrap_err()
    );
    assert!(
        reverse_overlap.contains("overlaps legacy/current"),
        "{reverse_overlap}"
    );
}
