#[path = "hosted_history/support.rs"]
mod support;

use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use ctx_history_archive::{ArchiveIdentity, ImportBinding};
use ctx_history_index::VerifiedIndex;
use support::{failure, seed_history, success, Sandbox};

#[test]
fn export_verify_restore_roundtrip_retains_missing_time_and_structured_content() {
    let source = Sandbox::new();
    let expected = seed_history(&source);
    let exported = success(
        source
            .command()
            .args([
                "archive",
                "export",
                "--origin",
                "synthetic-origin",
                "--output",
            ])
            .arg(source.path("snapshot"))
            .arg("--format=json"),
    );
    assert_eq!(exported["manifest"]["records"], 1);
    let verified = success(
        source
            .command()
            .args(["archive", "verify"])
            .arg(source.path("snapshot"))
            .arg("--format=json"),
    );
    assert_eq!(verified["manifest"], exported["manifest"]);

    // Transport location and original provider/index availability are irrelevant.
    let destination = Sandbox::new();
    fs::rename(source.path("snapshot"), destination.path("moved-snapshot")).unwrap();
    fs::remove_dir_all(source.path("history")).unwrap();
    let restored = success(
        destination
            .command()
            .args(["archive", "restore"])
            .arg(destination.path("moved-snapshot"))
            .arg("--format=json"),
    );
    assert_eq!(restored["receipt"]["imported_members"], 1);
    let repeated = success(
        destination
            .command()
            .args(["archive", "restore"])
            .arg(destination.path("moved-snapshot"))
            .arg("--format=json"),
    );
    assert_eq!(repeated["receipt"]["imported_members"], 0);
    assert_eq!(repeated["receipt"]["unchanged_members"], 1);

    let binding = ImportBinding {
        namespace: "personal".to_owned(),
        identity: ArchiveIdentity {
            origin: "synthetic-origin".to_owned(),
            view: "personal".to_owned(),
        },
    };
    let id = ctx_history_archive::mapped_event(&binding, expected.session_id, expected.event_id)
        .unwrap();
    let index = VerifiedIndex::open_pinned(destination.path("history/search/lexical")).unwrap();
    let actual = index.core_record_by_id(id.as_uuid()).unwrap().unwrap();
    assert_eq!(actual.occurred_at_unix_ms, None);
    assert_eq!(actual.native_event_id, None);
    assert_eq!(actual.content, expected.content);
    drop(index);
    fs::remove_dir_all(destination.path("moved-snapshot")).unwrap();
    let reexported = success(
        destination
            .command()
            .args([
                "archive",
                "export",
                "--origin",
                "synthetic-origin",
                "--output",
            ])
            .arg(destination.path("reexported-snapshot"))
            .arg("--format=json"),
    );
    assert_eq!(
        reexported["manifest"]["identity"],
        exported["manifest"]["identity"]
    );
    assert_eq!(
        reexported["manifest"]["inventory_sha256"],
        exported["manifest"]["inventory_sha256"]
    );
    let wrong_origin = failure(
        destination
            .command()
            .args([
                "archive",
                "export",
                "--origin",
                "different-origin",
                "--output",
            ])
            .arg(destination.path("relabeled-snapshot"))
            .arg("--format=json"),
    );
    assert_eq!(wrong_origin["error"]["code"], "invalid_archive");
    let message = wrong_origin["error"]["message"].as_str().unwrap();
    assert!(message.contains("--origin") && message.contains("--view"));
    assert!(message.contains("cannot be relabeled"));
    assert!(!destination.path("relabeled-snapshot").exists());
    let search = success(destination.command().args([
        "search",
        "observatory",
        "--backend",
        "lexical",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(search
        .to_string()
        .contains("portable meadow observatory fixture"));
}

#[test]
fn malformed_archive_fails_without_creating_local_storage() {
    let sandbox = Sandbox::new();
    fs::create_dir(sandbox.path("archive")).unwrap();
    fs::write(sandbox.path("archive/manifest.json"), b"{}").unwrap();
    failure(
        sandbox
            .command()
            .args(["archive", "verify"])
            .arg(sandbox.path("archive"))
            .arg("--format=json"),
    );
    failure(
        sandbox
            .command()
            .args(["archive", "restore"])
            .arg(sandbox.path("archive"))
            .arg("--format=json"),
    );
    assert!(!sandbox.path("history").exists());
}

#[test]
fn corrupt_member_does_not_change_already_restored_history() {
    let sandbox = Sandbox::new();
    seed_history(&sandbox);
    success(
        sandbox
            .command()
            .args([
                "archive",
                "export",
                "--origin",
                "synthetic-origin",
                "--output",
            ])
            .arg(sandbox.path("snapshot"))
            .arg("--format=json"),
    );
    let destination = Sandbox::new();
    let restored = success(
        destination
            .command()
            .args(["archive", "restore"])
            .arg(sandbox.path("snapshot"))
            .arg("--format=json"),
    );
    let member = fs::read_dir(sandbox.path("snapshot/members"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(member, b"corrupt normalized bytes\n").unwrap();
    failure(
        destination
            .command()
            .args(["archive", "restore"])
            .arg(sandbox.path("snapshot"))
            .arg("--format=json"),
    );
    let index = VerifiedIndex::open_pinned(destination.path("history/search/lexical")).unwrap();
    assert_eq!(
        index.generation_id(),
        restored["receipt"]["generation_id"].as_str().unwrap()
    );
}

#[test]
fn restored_root_stays_archive_only_through_default_and_wait_refresh() {
    let source = Sandbox::new();
    seed_history(&source);
    success(
        source
            .command()
            .args([
                "archive",
                "export",
                "--origin",
                "archive-refresh-fixture",
                "--output",
            ])
            .arg(source.path("snapshot"))
            .arg("--format=json"),
    );

    let destination = Sandbox::new();
    let sessions = destination.path("home/.codex/sessions/2026/09/01");
    fs::create_dir_all(&sessions).unwrap();
    let native_records = [
        serde_json::json!({
            "timestamp": "2026-09-01T12:00:00Z", "type": "session_meta",
            "payload": {
                "id": "019faaaa-0000-7000-8000-000000000456",
                "timestamp": "2026-09-01T12:00:00Z", "cwd": destination.root(),
                "originator": "codex_cli_rs", "cli_version": "0.1.0",
                "source": "cli", "model_provider": "openai"
            }
        }),
        serde_json::json!({
            "timestamp": "2026-09-01T12:00:01Z", "type": "response_item",
            "payload": {"type": "message", "role": "user", "content": [{
                "type": "input_text", "text": "unrelatednativeasteroid fixture"
            }]}
        }),
    ];
    fs::write(
        sessions.join("rollout-archive-refresh.jsonl"),
        native_records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>(),
    )
    .unwrap();

    // Prove this provider is discoverable through ordinary native-root refresh.
    // Manual indexing still permits the explicit finite worker for --refresh wait.
    let native_root = destination.path("native-history");
    fs::create_dir(&native_root).unwrap();
    fs::write(
        native_root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n",
    )
    .unwrap();
    let native_cleanup = WorkerCleanup {
        sandbox: &destination,
        root: native_root.clone(),
    };
    let native = success(
        destination
            .command()
            .env_remove("CTX_DAEMON_AUTOSTART_OFF")
            .arg("--data-root")
            .arg(&native_root)
            .args([
                "search",
                "unrelatednativeasteroid",
                "--refresh",
                "wait",
                "--format=json",
            ]),
    );
    assert!(native
        .to_string()
        .contains("unrelatednativeasteroid fixture"));
    let native_config = fs::read(native_root.join("config.toml")).unwrap();
    failure(
        destination
            .command()
            .arg("--data-root")
            .arg(&native_root)
            .args(["archive", "restore"])
            .arg(source.path("snapshot"))
            .arg("--format=json"),
    );
    assert_eq!(
        fs::read(native_root.join("config.toml")).unwrap(),
        native_config
    );
    drop(native_cleanup);
    // The native positive control owns installation coordination in sandbox HOME.
    assert!(destination.path("home/.ctx/daemon-installations").is_dir());

    let restored = success(
        destination
            .command()
            .args(["archive", "restore"])
            .arg(source.path("snapshot"))
            .arg("--format=json"),
    );
    let archive_cleanup = WorkerCleanup {
        sandbox: &destination,
        root: destination.path("history"),
    };
    for refresh in [None, Some("wait")] {
        let mut command = destination.command();
        command.env_remove("CTX_DAEMON_AUTOSTART_OFF").args([
            "search",
            "observatory",
            "--format=json",
        ]);
        if let Some(refresh) = refresh {
            command.args(["--refresh", refresh]);
        }
        let found = success(&mut command);
        assert!(found
            .to_string()
            .contains("portable meadow observatory fixture"));
        assert!(
            !destination.path("history/daemon").exists(),
            "archive-only search must not start a native refresh worker"
        );
        let absent = success(destination.command().args([
            "search",
            "unrelatednativeasteroid",
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert!(absent["results"].as_array().unwrap().is_empty());
        let repeated = success(
            destination
                .command()
                .args(["archive", "restore"])
                .arg(source.path("snapshot"))
                .arg("--format=json"),
        );
        assert_eq!(repeated["receipt"]["imported_members"], 0);
        assert_eq!(repeated["receipt"]["unchanged_members"], 1);
        assert_eq!(
            repeated["receipt"]["generation_id"],
            restored["receipt"]["generation_id"]
        );
    }
    drop(archive_cleanup);
    // Only the native control's coordination directory is allowed here: no
    // fallback history index, daemon, config, or provider catalog in managed HOME.
    for entry in fs::read_dir(destination.path("home/.ctx")).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(
            entry.file_name(),
            "daemon-installations",
            "unexpected managed-root state: {}",
            entry.path().display()
        );
        assert!(entry.file_type().unwrap().is_dir());
    }
}

/// Always stop task-owned finite/background workers, including on assertion failure.
struct WorkerCleanup<'a> {
    sandbox: &'a Sandbox,
    root: PathBuf,
}

impl WorkerCleanup<'_> {
    fn stop(&self) -> Result<(), String> {
        let stopped = self
            .sandbox
            .command()
            .timeout(Duration::from_secs(12))
            .arg("--data-root")
            .arg(&self.root)
            .args(["daemon", "disable", "--format=json"])
            .output()
            .map_err(|error| format!("stop fixture worker: {error}"))?;
        if !stopped.status.success() {
            return Err(format!(
                "stop fixture worker: {}",
                String::from_utf8_lossy(&stopped.stderr)
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = self
                .sandbox
                .command()
                .timeout(Duration::from_secs(2))
                .arg("--data-root")
                .arg(&self.root)
                .args(["daemon", "status", "--format=json"])
                .output()
                .map_err(|error| format!("inspect fixture worker: {error}"))?;
            if output.status.success()
                && serde_json::from_slice::<serde_json::Value>(&output.stdout)
                    .is_ok_and(|status| status["daemon"]["running"] == false)
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("fixture worker did not stop".to_owned());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for WorkerCleanup<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            if std::thread::panicking() {
                eprintln!("fixture cleanup failed: {error}");
            } else {
                panic!("fixture cleanup failed: {error}");
            }
        }
    }
}
