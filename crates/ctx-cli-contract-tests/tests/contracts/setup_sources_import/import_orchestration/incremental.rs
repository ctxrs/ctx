use super::*;

fn incremental_import(temp: &TempDir, selection: &[&str]) -> Value {
    let mut command = ctx(temp);
    command.args([
        "import",
        "--incremental",
        "--no-blame",
        "--format=json",
        "--progress",
        "none",
    ]);
    command.args(selection).timeout(Duration::from_secs(20));
    let report = json_output(&mut command);
    assert_eq!(report["outcome"], "success", "{report:#}");
    report
}

fn append_codex_import_message(path: &Path, text: &str) {
    // Authored append using the existing native fixture's response-item shape.
    writeln!(fs::OpenOptions::new().append(true).open(path).unwrap(), "{}", json!({
        "timestamp": "2026-08-02T12:00:02Z",
        "type": "response_item",
        "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}
    })).unwrap();
}

#[test]
fn manual_incremental_import_appends_reuses_no_op_and_reconciles_new_deleted_members() {
    let temp = tempdir();
    fs::create_dir_all(data_root(&temp)).unwrap();
    let config = "[indexing]\nmode = \"manual\"\n[search]\nsemantic = false\n";
    fs::write(data_root(&temp).join("config.toml"), config).unwrap();
    let root = temp.path().join(".codex/sessions/2026/10/01");
    let source = write_codex_message_fixture(
        &root,
        "019fcaaa-0000-7000-8000-000000000601",
        "initialincrementaloracle",
    );
    let baseline = json_output(ctx(&temp).args([
        "import",
        "--all",
        "--no-blame",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(baseline["outcome"], "success", "{baseline:#}");
    wait_for_daemon_status(&temp, "disabled", false, "import");

    append_codex_import_message(&source, "appendedincrementaloracle");
    let added = write_codex_message_fixture(
        &root,
        "019fcaaa-0000-7000-8000-000000000602",
        "newincrementaloracle",
    );
    let refreshed = incremental_import(&temp, &["--provider", "codex"]);
    assert_ne!(
        published_generation(&baseline),
        published_generation(&refreshed)
    );
    assert_eq!(refreshed["totals"]["current_indexed_documents"], 3);
    let stopped = wait_for_daemon_status(&temp, "disabled", false, "import");
    assert_eq!(stopped["daemon"]["running"], false);
    for marker in [
        "initialincrementaloracle",
        "appendedincrementaloracle",
        "newincrementaloracle",
    ] {
        let found =
            json_output(ctx(&temp).args(["search", marker, "--refresh=off", "--format=json"]));
        assert_eq!(found["results"].as_array().unwrap().len(), 1, "{found:#}");
    }
    let unchanged = incremental_import(&temp, &["--all"]);
    assert_eq!(
        published_generation(&unchanged),
        published_generation(&refreshed)
    );
    assert_eq!(unchanged["totals"]["current_indexed_documents"], 3);
    wait_for_daemon_status(&temp, "disabled", false, "import");
    fs::remove_file(added).unwrap();
    let deleted = incremental_import(&temp, &["--provider", "codex"]);
    assert_eq!(deleted["totals"]["current_indexed_documents"], 2);
    wait_for_daemon_status(&temp, "disabled", false, "import");
    let missing = json_output(ctx(&temp).args([
        "search",
        "newincrementaloracle",
        "--refresh=off",
        "--format=json",
    ]));
    assert!(
        missing["results"].as_array().unwrap().is_empty(),
        "{missing:#}"
    );
    assert_eq!(
        fs::read_to_string(data_root(&temp).join("config.toml")).unwrap(),
        config
    );
    let daemon = data_root(&temp).join("daemon");
    for file in [
        "source-refresh-endpoint.json",
        "wakeup.json",
        "supervisor.json",
        "semantic-index.json",
    ] {
        assert!(!daemon.join(file).exists(), "unexpected retained {file}");
    }
    ctx(&temp)
        .args([
            "import",
            "--incremental",
            "--provider",
            "codex",
            "--no-daemon",
            "--no-blame",
            "--progress",
            "none",
        ])
        .assert()
        .failure();
    assert!(!daemon.join("source-refresh-endpoint.json").exists());
}

#[test]
fn manual_incremental_import_retires_while_a_producer_keeps_appending() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };
    let temp = tempdir();
    fs::create_dir_all(data_root(&temp)).unwrap();
    let config = "[indexing]\nmode = \"manual\"\n[search]\nsemantic = false\n";
    fs::write(data_root(&temp).join("config.toml"), config).unwrap();
    let source = write_codex_message_fixture(
        &temp.path().join(".codex/sessions/2026/10/01"),
        "019fcaaa-0000-7000-8000-000000000603",
        "continuousseedmarker",
    );
    incremental_import(&temp, &["--provider", "codex"]);
    wait_for_daemon_status(&temp, "disabled", false, "import");
    let producing = Arc::new(AtomicBool::new(true));
    let append_lock = Mutex::new(());
    let gate = data_root(&temp).join(".block-source-refresh-after-availability-for-test");
    let blocked = data_root(&temp).join(".source-refresh-blocked-after-availability-for-test");
    fs::write(&gate, b"block\n").unwrap();
    std::thread::scope(|scope| {
        let active = Arc::clone(&producing);
        let source = &source;
        let append_lock = &append_lock;
        let writer = scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut count = 0;
            while active.load(Ordering::SeqCst) && Instant::now() < deadline {
                {
                    let _append = append_lock.lock().unwrap();
                    append_codex_import_message(
                        source,
                        &format!("continuousincrementalmarker{count}"),
                    );
                }
                count += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            count
        });
        let import = scope.spawn(|| incremental_import(&temp, &["--provider", "codex"]));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !blocked.exists() {
            assert!(
                !import.is_finished(),
                "import exited before its availability gate"
            );
            assert!(
                Instant::now() < deadline,
                "import did not reach availability gate"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let running = json_output(ctx(&temp).args(["daemon", "status", "--format=json"]));
        let pid = u32::try_from(running["daemon"]["pid"].as_u64().unwrap()).unwrap();
        assert_daemon_process_running(pid);
        fs::remove_file(&gate).unwrap();
        import.join().unwrap();
        {
            let _append = append_lock.lock().unwrap();
            append_codex_import_message(source, "afterterminaldeferredoracle");
        }
        let stopped = wait_for_daemon_status(&temp, "disabled", false, "import");
        assert_eq!(stopped["daemon"]["running"], false);
        assert!(!data_root(&temp)
            .join("daemon/source-refresh-endpoint.json")
            .exists());
        #[cfg(unix)]
        {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
                #[cfg(target_os = "linux")]
                let alive = alive
                    && fs::read_to_string(format!("/proc/{pid}/stat"))
                        .ok()
                        .and_then(|stat| {
                            stat.rsplit_once(") ")
                                .map(|(_, tail)| tail.starts_with('Z'))
                        })
                        != Some(true);
                if !alive {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "finite worker {pid} did not exit naturally"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert!(
            !writer.is_finished(),
            "producer must remain active through natural worker retirement"
        );
        producing.store(false, Ordering::SeqCst);
        assert!(writer.join().unwrap() > 0);
    });
    let deferred = json_output(ctx(&temp).args([
        "search",
        "afterterminaldeferredoracle",
        "--refresh=off",
        "--format=json",
    ]));
    assert!(
        deferred["results"].as_array().unwrap().is_empty(),
        "{deferred:#}"
    );
    incremental_import(&temp, &["--provider", "codex"]);
    wait_for_daemon_status(&temp, "disabled", false, "import");
    let caught_up = json_output(ctx(&temp).args([
        "search",
        "afterterminaldeferredoracle",
        "--refresh=off",
        "--format=json",
    ]));
    assert_eq!(
        caught_up["results"].as_array().unwrap().len(),
        1,
        "{caught_up:#}"
    );
    assert_eq!(
        fs::read_to_string(data_root(&temp).join("config.toml")).unwrap(),
        config
    );
}

#[test]
fn manual_incremental_source_failure_retains_history_and_retires_worker() {
    let temp = tempdir();
    fs::create_dir_all(data_root(&temp)).unwrap();
    let config = "[indexing]\nmode = \"manual\"\n[search]\nsemantic = false\n";
    fs::write(data_root(&temp).join("config.toml"), config).unwrap();
    let source = temp.path().join("incremental-failure.jsonl");
    write_valid_explicit_custom_source(&source, "retainedfailureoracle");
    let selection = [
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        source.to_str().unwrap(),
    ];
    let baseline = incremental_import(&temp, &selection);
    wait_for_daemon_status(&temp, "disabled", false, "import");
    fs::write(
        &source,
        b"{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v999\"}\n",
    )
    .unwrap();
    let mut command = ctx(&temp);
    command.args([
        "import",
        "--incremental",
        "--no-blame",
        "--progress",
        "none",
    ]);
    command
        .args(selection)
        .timeout(Duration::from_secs(20))
        .assert()
        .failure();
    wait_for_daemon_status(&temp, "disabled", false, "import");
    let current = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(
        current["lexical"]["indexed_documents"],
        baseline["totals"]["current_indexed_documents"]
    );
    let retained = json_output(ctx(&temp).args([
        "search",
        "retainedfailureoracle",
        "--refresh=off",
        "--format=json",
    ]));
    assert_eq!(
        retained["results"].as_array().unwrap().len(),
        1,
        "{retained:#}"
    );
    assert_eq!(
        fs::read_to_string(data_root(&temp).join("config.toml")).unwrap(),
        config
    );
    assert!(!data_root(&temp)
        .join("daemon/source-refresh-endpoint.json")
        .exists());
}

#[test]
fn incremental_import_preserves_automatic_mode_and_existing_owner() {
    let temp = tempdir();
    write_codex_message_fixture(
        &temp.path().join(".codex/sessions/2026/10/01"),
        "019fcaaa-0000-7000-8000-000000000604",
        "automaticincrementaloracle",
    );
    let daemon = start_source_refresh_daemon_with_config(
        &temp,
        "full",
        "[indexing]\nmode = \"auto\"\n[daemon]\nenabled = true\nmode = \"full\"\n[search]\nsemantic = false\n",
    );
    wait_for_initial_source_refresh(&temp);
    let config = fs::read(data_root(&temp).join("config.toml")).unwrap();
    let pid = daemon.child.as_ref().unwrap().id();
    incremental_import(&temp, &["--provider", "codex", "--no-daemon"]);
    let running = json_output(ctx(&temp).args(["daemon", "status", "--format=json"]));
    assert_eq!(running["daemon"]["running"], true, "{running:#}");
    assert_eq!(running["daemon"]["pid"], pid, "{running:#}");
    assert_eq!(
        fs::read(data_root(&temp).join("config.toml")).unwrap(),
        config
    );
}
