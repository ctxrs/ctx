use super::*;

#[test]
fn provider_root_count_is_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let text = (0..=MAX_CONFIGURED_PROVIDER_ROOTS)
        .map(|index| {
            format!(
                "[sources.roots.root{index}]\nprovider = \"claude\"\npath = {:?}\n",
                provider_parent
                    .path()
                    .join(format!("claude-{index}"))
                    .display()
                    .to_string()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(temp.path().join(CONFIG_FILE), text).unwrap();

    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());
    assert!(error.contains("exceed the maximum"), "{error}");
}

#[test]
fn persisted_provider_root_kind_is_openhands_only_and_exact() {
    let data_root = tempfile::tempdir().unwrap();
    let path = data_root.path().join("openhands-root");
    let missing_kind = format!(
        "[sources.roots.work]\nprovider = \"openhands\"\npath = {:?}\n",
        path.display().to_string()
    );
    fs::write(data_root.path().join(CONFIG_FILE), missing_kind).unwrap();
    let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
    assert!(error.contains("require --kind"), "{error}");

    let old_provider_kind = format!(
        "[sources.roots.work]\nprovider = \"claude\"\npath = {:?}\nkind = \"legacy-persistence\"\n",
        path.display().to_string()
    );
    fs::write(data_root.path().join(CONFIG_FILE), old_provider_kind).unwrap();
    let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
    assert!(error.contains("only supported for openhands"), "{error}");

    let invalid_spelling = format!(
        "[sources.roots.work]\nprovider = \"openhands\"\npath = {:?}\nkind = \"Current-Conversations\"\n",
        path.display().to_string()
    );
    fs::write(data_root.path().join(CONFIG_FILE), invalid_spelling).unwrap();
    let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
    assert!(error.contains("must be current-conversations"), "{error}");
}

#[test]
fn provider_root_cli_mutation_rejects_data_root_overlap_before_writing() {
    for relationship in ["equal", "ancestor", "descendant"] {
        let fixture = tempfile::tempdir().unwrap();
        let data_root = fixture.path().join("ctx-data");
        fs::create_dir(&data_root).unwrap();
        let provider_root = match relationship {
            "equal" => data_root.clone(),
            "ancestor" => fixture.path().to_path_buf(),
            "descendant" => {
                let nested = data_root.join("provider-home");
                fs::create_dir(&nested).unwrap();
                nested
            }
            _ => unreachable!(),
        };

        let error = format!(
            "{:#}",
            add_claude_root(&data_root, "work", &provider_root, Some("work"), false).unwrap_err()
        );
        assert!(
            error.contains("must not overlap the ctx data root"),
            "{error}"
        );
        assert!(!data_root.join(CONFIG_FILE).exists());
    }
}

#[test]
fn provider_root_mutation_waits_for_the_shared_config_transaction_lock() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let provider_home = provider_parent.path().join("claude-personal");
    fs::create_dir(&provider_home).unwrap();
    let config_path = AppConfig::config_path(data_root.path());
    let lock = durable_write::ConfigMutationLock::acquire(&config_path).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let data_root_path = data_root.path().to_path_buf();
    let provider_home_path = provider_home.clone();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = add_claude_root(
            &data_root_path,
            "personal",
            &provider_home_path,
            Some("personal"),
            false,
        );
        finished_tx.send(result).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(
        finished_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "a concurrent read-modify-write must not pass the config lock"
    );
    drop(lock);
    assert!(finished_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("provider-root mutation did not resume after unlock")
        .is_ok());
    worker.join().unwrap();
}

#[test]
fn removing_the_last_member_of_a_group_is_allowed() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_home = tempfile::tempdir().unwrap();
    fs::write(
        data_root.path().join(CONFIG_FILE),
        format!(
            "[analytics]\nenabled = false\n\n[sources]\nautomatic = false\n\n[sources.roots.personal]\nprovider = \"claude\"\npath = {:?}\ngroup = \"personal\"\n",
            provider_home.path().display().to_string()
        ),
    )
    .unwrap();

    let removed = remove_provider_root(data_root.path(), "personal").unwrap();
    assert!(removed.changed);
    let loaded = AppConfig::load(data_root.path()).unwrap();
    assert!(!loaded.analytics.enabled);
    assert!(!loaded.automatic_source_discovery_enabled());
    assert!(loaded.provider_roots.is_empty());
}
