use super::*;

#[test]
fn managed_data_root_show_is_read_only_and_ignores_history_override() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["data-root", "show"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        sandbox.root.join("home/.ctx").display().to_string()
    );
    assert!(!sandbox.root.join("home/.ctx").exists());
    assert!(!sandbox.root.join("home/.ctx-control").exists());
}

#[test]
fn managed_data_root_move_retains_search_and_custom_override_boundary() {
    let sandbox = Sandbox::new();
    import_synthetic_history(&sandbox);
    let source = sandbox.root.join("home/.ctx");
    promote_history_fixture(&sandbox, &source);
    let before = json_output(
        sandbox
            .command()
            .env_remove("CTX_DATA_ROOT")
            .args(["search", "kernel", "--scope", "history", "--format=json"])
            .output()
            .unwrap(),
    );
    assert_eq!(before["results"].as_array().unwrap().len(), 1);
    let identity = fs::read(source.join("install.json")).unwrap();
    let config = fs::read(source.join("config.toml")).unwrap();
    let provider_path = sandbox
        .root
        .join("providers/codex/sessions/rollout-acceptance.jsonl");
    let provider = fs::read(&provider_path).unwrap();
    let destination = sandbox.root.join("other disk with spaces");
    let output = sandbox
        .command()
        .env_remove("CTX_DATA_ROOT")
        .args(["data-root", "move", "--to"])
        .arg(&destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(destination.join("install.json")).unwrap(),
        identity
    );
    assert_eq!(fs::read(destination.join("config.toml")).unwrap(), config);
    assert_eq!(fs::read(source.join("install.json")).unwrap(), identity);
    assert_eq!(fs::read(&provider_path).unwrap(), provider);
    let after = json_output(
        sandbox
            .command()
            .env_remove("CTX_DATA_ROOT")
            .args(["search", "kernel", "--scope", "history", "--format=json"])
            .output()
            .unwrap(),
    );
    assert_eq!(after["results"], before["results"]);
    let root = json_output(
        sandbox
            .command()
            .env_remove("CTX_DATA_ROOT")
            .args(["data-root", "show", "--format=json"])
            .output()
            .unwrap(),
    );
    assert_eq!(root["path"], destination.display().to_string());
    let rejected = sandbox
        .command()
        .args(["data-root", "move", "--to"])
        .arg(sandbox.root.join("refused"))
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(!sandbox.root.join("refused").exists());
    for explicit_flag in [false, true] {
        for args in [
            vec!["status", "--usage", "disable", "--format=json"],
            vec!["sources", "remove", "missing", "--format=json"],
            vec!["daemon", "run", "--format=json"],
        ] {
            let mut command = sandbox.command();
            if explicit_flag {
                command.arg("--data-root").arg(&source);
            } else {
                command.env("CTX_DATA_ROOT", &source);
            }
            let rejected = command.args(args).output().unwrap();
            assert!(!rejected.status.success());
            assert!(
                String::from_utf8_lossy(&rejected.stderr).contains("retired managed data root"),
                "{}",
                String::from_utf8_lossy(&rejected.stderr)
            );
        }
    }
    assert_eq!(fs::read(source.join("config.toml")).unwrap(), config);
    fs::rename(&destination, sandbox.root.join("unmounted")).unwrap();
    fs::create_dir(&destination).unwrap();
    let missing = sandbox
        .command()
        .env_remove("CTX_DATA_ROOT")
        .args(["data-root", "show"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(!destination.join("install.json").exists());
    // An offline managed volume does not prevent a separate custom root's writes.
    let independent = sandbox
        .command()
        .args(["status", "--usage", "disable", "--format=json"])
        .output()
        .unwrap();
    assert_success(&independent);
    assert!(!sandbox.root.join("history/daemon/supervisor.json").exists());
}

#[test]
fn managed_data_root_move_resumes_automatic_background_maintenance() {
    let sandbox = Sandbox::new();
    import_synthetic_history(&sandbox);
    let source = sandbox.root.join("home/.ctx");
    promote_history_fixture(&sandbox, &source);
    let automatic = || {
        let mut command = sandbox.command();
        command
            .env_remove("CTX_DATA_ROOT")
            .env_remove("CTX_DAEMON_ENABLED")
            .env_remove("CTX_DAEMON_AUTOSTART_OFF");
        command
    };
    let enabled = automatic()
        .args(["index", "mode", "auto", "--format=json"])
        .output()
        .unwrap();
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    let destination = sandbox.root.join("automatic destination");
    let moved = automatic()
        .args(["data-root", "move", "--to"])
        .arg(&destination)
        .output()
        .unwrap();
    let status = automatic()
        .args(["daemon", "status", "--format=json"])
        .output()
        .unwrap();
    // Always stop our candidate's daemon before assertions, including a failed move.
    let stopped = automatic()
        .args(["index", "mode", "manual", "--format=json"])
        .output()
        .unwrap();
    assert!(
        stopped.status.success(),
        "stop: {}; move: {}",
        String::from_utf8_lossy(&stopped.stderr),
        String::from_utf8_lossy(&moved.stderr)
    );
    assert!(
        moved.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&moved.stdout),
        String::from_utf8_lossy(&moved.stderr)
    );
    assert_eq!(json_output(status)["daemon"]["running"], true);
    assert!(destination.join("install.json").exists());
}

// The finite import created managed installation coordination even though its
// history was custom. Retain that coordination while preparing the managed fixture.
fn promote_history_fixture(sandbox: &Sandbox, source: &Path) {
    fs::create_dir_all(source).unwrap();
    for entry in fs::read_dir(sandbox.root.join("history")).unwrap() {
        let entry = entry.unwrap();
        fs::rename(entry.path(), source.join(entry.file_name())).unwrap();
    }
    fs::remove_dir(sandbox.root.join("history")).unwrap();
    // Analytics are disabled in this fixture, so no installation ID was needed yet.
    let path = source.join("install.json");
    fs::write(&path, br#"{"schema_version":1,"install_id":"96f2c8b7-4696-4c11-8941-7b97cf3f88a5","created_at":"2026-01-01T00:00:00Z"}"#).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn managed_root_writing_status_and_sources_create_admission_on_first_use() {
    for args in [
        vec!["status", "--usage", "disable", "--format=json"],
        vec!["sources", "remove", "missing", "--format=json"],
        vec![
            "sources",
            "add",
            "example",
            "--provider",
            "codex",
            "--root",
            "missing",
            "--format=json",
        ],
    ] {
        let sandbox = Sandbox::new();
        let _ = sandbox.output(&args, b"");
        assert!(sandbox
            .root
            .join("home/.ctx-control/admission.lock")
            .is_file());
        assert!(sandbox.root.join("home/.ctx-control/users.lock").is_file());
    }
    let sandbox = Sandbox::new();
    let _ = sandbox.output(&["status", "--format=json"], b"");
    assert!(!sandbox.root.join("home/.ctx-control").exists());
}

#[test]
fn managed_root_admission_does_not_block_graph_only_search() {
    let sandbox = Sandbox::new();
    sandbox.write("repo/saved.json", graf_snapshot().to_string());
    sandbox.graph(&["--db", "selected.db", "import", "graf", "saved.json"]);
    let args = [
        "search",
        "compute",
        "--scope=graph",
        "--graph-db=selected.db",
        "--format=json",
    ];
    let _ = sandbox.json(&args);
    assert!(!sandbox.root.join("home/.ctx-control").exists());
    // Neither invalid history authority nor active relocation may block graph reads.
    let _ = sandbox.output(&["status", "--usage", "disable", "--format=json"], b"");
    sandbox.write("home/.ctx-control/data-root.json", b"invalid locator");
    let admission = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(sandbox.root.join("home/.ctx-control/admission.lock"))
        .unwrap();
    admission.try_lock().unwrap();
    let output = sandbox.json(&args);
    assert_eq!(output["graph"]["status"], "ok");
}

#[test]
fn managed_root_mcp_started_before_setup_rejects_history_after_relocation() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let sandbox = Sandbox::new();
    let template = sandbox.command();
    let mut child = Server(
        std::process::Command::new(&sandbox.binary)
            .env_clear()
            .envs(template.get_envs().filter_map(|(name, value)| {
                (name != "CTX_DATA_ROOT")
                    .then_some(value.map(|value| (name, value)))
                    .flatten()
            }))
            .env("CTX_LOCAL_USAGE_ENABLED", "true")
            .current_dir(sandbox.repo())
            .args(["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let reader = BufReader::new(child.0.stdout.take().unwrap());
    let (send, receive) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        for line in reader.lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut input = child.0.stdin.take().unwrap();
    let response = || -> Value {
        serde_json::from_str(&receive.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap()
    };
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).unwrap();
    assert_eq!(response()["id"], 1);
    assert!(!sandbox.root.join("home/.ctx-control").exists());
    let manual = sandbox
        .command()
        .env_remove("CTX_DATA_ROOT")
        .args(["index", "mode", "manual", "--format=json"])
        .output()
        .unwrap();
    assert_success(&manual);
    let destination = sandbox.root.join("new root");
    let moved = sandbox
        .command()
        .env_remove("CTX_DATA_ROOT")
        .args(["data-root", "move", "--to"])
        .arg(destination)
        .output()
        .unwrap();
    assert_success(&moved);
    fs::remove_dir_all(sandbox.root.join("home/.ctx")).unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"status","arguments":{}}})).unwrap();
    let retired = response();
    assert_eq!(retired["result"]["isError"], true, "{retired}");
    assert!(retired.to_string().contains("retired managed data root"));
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"output_compact","arguments":{"text":"still available"}}})).unwrap();
    let output = response();
    assert_ne!(output["result"]["isError"], true, "{output}");
    drop(input);
    drop(child);
    thread.join().unwrap();
    assert!(
        !sandbox.root.join("home/.ctx").exists(),
        "stale history or accounting recreated the retired root"
    );
}

#[test]
fn managed_root_offline_volume_keeps_combined_graph_and_mcp_output_available() {
    let sandbox = Sandbox::new();
    let manual = sandbox
        .command()
        .env_remove("CTX_DATA_ROOT")
        .args(["index", "mode", "manual", "--format=json"])
        .output()
        .unwrap();
    assert_success(&manual);
    let destination = sandbox.root.join("volume");
    let moved = sandbox
        .command()
        .env_remove("CTX_DATA_ROOT")
        .args(["data-root", "move", "--to"])
        .arg(&destination)
        .output()
        .unwrap();
    assert_success(&moved);
    fs::rename(&destination, sandbox.root.join("offline")).unwrap();
    sandbox.write("repo/saved.json", graf_snapshot().to_string());
    sandbox.graph(&["--db", "selected.db", "import", "graf", "saved.json"]);
    let combined = json_output(
        sandbox
            .command()
            .env_remove("CTX_DATA_ROOT")
            .args([
                "search",
                "compute",
                "--scope=all",
                "--graph-db=selected.db",
                "--format=json",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(combined["partial"], true);
    assert_eq!(combined["graph"]["status"], "ok");
    assert_unavailable(&combined["history"]);
    let input = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"output_compact","arguments":{"text":"offline output still works"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"status","arguments":{}}}),
    ].iter().map(|message| format!("{message}\n")).collect::<String>();
    let output = sandbox
        .command()
        .env_remove("CTX_DATA_ROOT")
        .args(["mcp", "serve"])
        .write_stdin(input)
        .output()
        .unwrap();
    assert_success(&output);
    let responses = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 3);
    assert_ne!(responses[1]["result"]["isError"], true, "{}", responses[1]);
    assert_eq!(responses[2]["result"]["isError"], true, "{}", responses[2]);
    assert!(
        !destination.exists(),
        "offline managed root was initialized"
    );
}

#[cfg(unix)]
#[test]
fn managed_root_observer_blocked_on_output_cannot_account_to_retired_copy() {
    use std::io::Read;
    use std::process::Stdio;
    struct Observer(std::process::Child);
    impl Drop for Observer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let sandbox = Sandbox::new();
    history_fixture::import_synthetic_history_with_text(
        &sandbox,
        &"kernel accounting canary ".repeat(32_768),
    );
    let source = sandbox.root.join("home/.ctx");
    promote_history_fixture(&sandbox, &source);
    let search = sandbox.json(&[
        "--data-root",
        source.to_str().unwrap(),
        "search",
        "kernel",
        "--format=json",
    ]);
    let session = search["results"][0]["ctx_session_id"].as_str().unwrap();
    // Simulate a pre-locator installation: its first observer finds no admission files.
    fs::remove_dir_all(sandbox.root.join("home/.ctx-control")).unwrap();
    let template = sandbox.command();
    let mut child = Observer(
        std::process::Command::new(&sandbox.binary)
            .env_clear()
            .envs(template.get_envs().filter_map(|(name, value)| {
                (name != "CTX_DATA_ROOT")
                    .then_some(value.map(|value| (name, value)))
                    .flatten()
            }))
            .env("CTX_LOCAL_USAGE_ENABLED", "true")
            .env("CTX_ANALYTICS_ENABLED", "true")
            .current_dir(sandbox.repo())
            .args(["show", "session", session, "--format=json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut stdout = child.0.stdout.take().unwrap();
    let (ready_send, ready_receive) = std::sync::mpsc::channel();
    let (release_send, release_receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut first = [0u8; 4096];
        stdout.read_exact(&mut first).unwrap();
        ready_send.send(()).unwrap();
        if release_receive
            .recv_timeout(Duration::from_secs(20))
            .is_err()
        {
            return Vec::new();
        }
        let mut rest = Vec::new();
        stdout.read_to_end(&mut rest).unwrap();
        first.into_iter().chain(rest).collect::<Vec<_>>()
    });
    ready_receive.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "observer should be blocked on its output pipe"
    );
    assert!(!sandbox.root.join("home/.ctx-control").exists());
    let destination = sandbox.root.join("new managed root");
    let moved = sandbox
        .command()
        .env_remove("CTX_DATA_ROOT")
        .args(["data-root", "move", "--to"])
        .arg(&destination)
        .output()
        .unwrap();
    assert_success(&moved);
    release_send.send(()).unwrap();
    let body = reader.join().unwrap();
    assert!(child.0.wait().unwrap().success());
    assert!(body.len() > 500_000);
    assert!(
        !source.join("usage.sqlite").exists(),
        "observer accounted to retained source"
    );
    assert!(
        !destination.join("usage.sqlite").exists(),
        "observer rebound accounting after delivery"
    );
    assert!(
        !source.join("device.json").exists(),
        "observer initialized analytics at retained source"
    );
    sandbox.assert_no_connection();
}
