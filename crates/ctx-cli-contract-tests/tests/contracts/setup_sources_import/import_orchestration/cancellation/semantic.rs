use super::*;

#[test]
fn manual_semantic_import_is_ready_on_return_and_defers_terminal_output_for_json_and_human_modes() {
    for (format, progress) in [(Some("json"), "json"), (None, "plain")] {
        let temp = tempdir();
        let server = LoopbackSemanticServer::start(false);
        write_manual_semantic_config(&temp, server.endpoint());
        let fixture = temp
            .path()
            .join(format!("semantic-manual-{progress}.jsonl"));
        write_valid_explicit_custom_source(
            &fixture,
            "manual semantic import returns only after the exact generation is query ready",
        );

        let mut command = ctx(&temp);
        command
            .args([
                "import",
                "--input-format",
                "ctx-history-jsonl-v2",
                "--path",
                fixture.to_str().unwrap(),
                "--progress",
                progress,
            ])
            .timeout(Duration::from_secs(20));
        if let Some(format) = format {
            command.args(["--format", format]);
        }
        let output = command.assert().success().get_output().clone();

        let status = semantic_status_ready(&temp);
        assert_eq!(
            status["semantic"]["flat_f32"]["core_generation_id"],
            status["lexical"]["generation_id"],
            "{status:#}"
        );
        assert!(
            status["semantic"]["flat_f32"]["projected_documents"]
                .as_u64()
                .is_some_and(|count| count > 0),
            "{status:#}"
        );
        assert_single_semantic_writer(&server.finish());

        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        let semantic = stderr
            .find("Reconciling semantic search.")
            .expect("semantic progress frame");
        let terminal = stderr
            .rfind("History refresh complete")
            .expect("terminal Core success frame");
        assert!(semantic < terminal, "{stderr}");
        if format.is_some() {
            let report: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(report["outcome"], "success", "{report:#}");
            let events = stderr
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            let semantic = events
                .iter()
                .position(|event| event["phase"] == "semantic")
                .expect("semantic JSON progress frame");
            let terminal = events
                .iter()
                .position(|event| event["done"] == true)
                .expect("terminal JSON progress frame");
            assert!(semantic < terminal, "{events:#?}");
        } else {
            assert!(
                stdout.starts_with("✓ History import completed\n"),
                "{stdout}"
            );
        }
    }
}

#[test]
fn full_daemon_observes_semantic_completion_once_even_with_no_daemon_import() {
    let temp = tempdir();
    let server = LoopbackSemanticServer::start(false);
    let mut daemon =
        start_source_refresh_daemon_with_semantic_executor(&temp, "full", server.endpoint());
    let daemon_pid = daemon.child.as_ref().unwrap().id();
    let daemon_process =
        NativeProcessProbe::open(daemon_pid).expect("open full semantic daemon process probe");
    let fixture = temp.path().join("semantic-daemon-observer.jsonl");
    write_valid_explicit_custom_source(
        &fixture,
        "daemon observer owns semantic completion while import waits for exact readiness",
    );

    let imported = json_output(
        ctx(&temp)
            .args([
                "import",
                "--input-format",
                "ctx-history-jsonl-v2",
                "--path",
                fixture.to_str().unwrap(),
                "--no-daemon",
                "--format=json",
                "--progress",
                "none",
            ])
            .timeout(Duration::from_secs(20)),
    );
    assert_eq!(imported["outcome"], "success", "{imported:#}");
    assert_eq!(
        imported["sources"][0]["daemon_request_metadata"]["owner"], "daemon",
        "{imported:#}"
    );
    let status = semantic_status_ready(&temp);
    assert_eq!(status["daemon"]["pid"], daemon_pid, "{status:#}");
    daemon_process.assert_running();
    assert!(daemon.child.as_mut().unwrap().try_wait().unwrap().is_none());
    assert_single_semantic_writer(&server.finish());
}

#[test]
fn ready_empty_manual_semantic_import_reuses_the_projection_without_an_executor() {
    let temp = tempdir();
    let server = LoopbackSemanticServer::start(false);
    write_manual_semantic_config(&temp, server.endpoint());
    let fixture = temp.path().join("semantic-ready-empty.jsonl");
    write_semantically_filtered_explicit_custom_source(&fixture);
    let arguments = [
        "import",
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        fixture.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ];

    let initial = json_output(ctx(&temp).args(arguments));
    assert_eq!(initial["outcome"], "success", "{initial:#}");
    let ready = semantic_status_ready(&temp);
    assert_eq!(
        ready["semantic"]["flat_f32"]["semantic_documents"], 0,
        "{ready:#}"
    );
    assert_eq!(
        ready["semantic"]["flat_f32"]["projected_documents"], 0,
        "{ready:#}"
    );
    let requests_after_initial_completion = server.request_count();
    assert!(
        requests_after_initial_completion > 0,
        "the initial ReadyEmpty projection must establish its semantic contract"
    );

    let repeated = json_output(ctx(&temp).args(arguments));
    assert_eq!(repeated["outcome"], "success", "{repeated:#}");
    semantic_status_ready(&temp);
    assert_eq!(
        server.request_count(),
        requests_after_initial_completion,
        "an already ReadyEmpty generation must return without executor traffic"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn foreground_semantic_sigint_exits_130_without_terminal_success_or_daemon_collateral_damage() {
    let temp = tempdir();
    let server = LoopbackSemanticServer::start(true);
    let mut daemon = start_source_refresh_daemon_with_semantic_executor(
        &temp,
        "source-refresh-only",
        server.endpoint(),
    );
    let daemon_pid = daemon.child.as_ref().unwrap().id();
    let daemon_process = NativeProcessProbe::open(daemon_pid)
        .expect("open source-refresh-only daemon process probe");
    let fixture = temp.path().join("semantic-foreground-sigint.jsonl");
    write_valid_explicit_custom_source(
        &fixture,
        "foreground semantic cancellation must not finish a terminal import response",
    );

    let prepared = ctx(&temp);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .args([
            "import",
            "--input-format",
            "ctx-history-jsonl-v2",
            "--path",
            fixture.to_str().unwrap(),
            "--no-daemon",
            "--format=json",
            "--progress",
            "json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_interruptible_client(&mut command);
    let mut client = SourceRefreshDaemon {
        child: Some(command.spawn().expect("start foreground semantic import")),
    };
    let client_pid = client.child.as_ref().unwrap().id();
    server.wait_for_embedding_request();

    interrupt_client_group(client_pid).expect("interrupt foreground semantic import");
    server.release_embedding_response();
    let exit_deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = client.child.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < exit_deadline,
            "foreground semantic import did not exit within the cancellation bound"
        );
        thread::sleep(Duration::from_millis(20));
    };
    let output = client.child.take().unwrap().wait_with_output().unwrap();
    assert_eq!(status.code(), Some(130));
    assert_eq!(output.status, status);
    assert!(
        output.stdout.is_empty(),
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("History refresh complete"),
        "stderr={stderr}"
    );
    assert!(!stderr.contains("\"done\":true"), "stderr={stderr}");

    daemon_process.assert_running();
    assert!(daemon.child.as_mut().unwrap().try_wait().unwrap().is_none());
    let lock: Value =
        serde_json::from_slice(&fs::read(data_root(&temp).join("daemon/daemon.lock")).unwrap())
            .expect("source-refresh-only daemon lock JSON");
    assert_eq!(lock["pid"], daemon_pid, "{lock:#}");
    assert_eq!(lock["released"], false, "{lock:#}");
}
