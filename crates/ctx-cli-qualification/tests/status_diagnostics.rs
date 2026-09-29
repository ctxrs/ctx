#[path = "../../ctx-cli-contract-tests/tests/contracts/support/mod.rs"]
mod support;

use support::*;

#[test]
fn sampled_status_is_passive_even_with_malformed_config_and_missing_history() {
    let temp = tempdir();
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    let config = root.join("config.toml");
    let contents = b"malformed config SECRET_SAMPLE_CONFIG_28f9";
    fs::write(&config, contents).unwrap();
    for format in ["json", "text"] {
        let output = ctx(&temp)
            .args([
                "--color=always",
                "status",
                "--sample",
                "1",
                "--format",
                format,
            ])
            .env("XDG_CONFIG_HOME", temp.path().join("config-home"))
            .env("XDG_DATA_HOME", temp.path().join("data-home"))
            .env("XDG_STATE_HOME", temp.path().join("state-home"))
            .env("XDG_RUNTIME_DIR", temp.path().join("runtime"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.contains(&0x1b));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains("SECRET_SAMPLE_CONFIG_28f9"));
        assert!(!text.contains(root.to_string_lossy().as_ref()));
        if format == "json" {
            let report: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(report["schema_version"], 1);
            assert_eq!(report["ctx_version"], env!("CARGO_PKG_VERSION"));
            assert_eq!(report["os"], std::env::consts::OS);
            assert_eq!(report["arch"], std::env::consts::ARCH);
            assert_eq!(report["sample"]["requested_seconds"], 1);
            assert_eq!(report["daemon"]["state"], "unknown");
            assert!(report["observed"]["cpu"]["cpu_ms"].is_null());
            assert_eq!(report["blame"]["writer_observed"], false);
            assert_eq!(report["semantic"]["last_recorded_status"], "unknown");
        } else {
            let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(normalized.contains(&format!("ctx version {}", env!("CARGO_PKG_VERSION"))));
            assert!(normalized.contains(&format!("OS {}", std::env::consts::OS)));
            assert!(normalized.contains(&format!("Architecture {}", std::env::consts::ARCH)));
        }
        assert_eq!(fs::read(&config).unwrap(), contents);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        for directory in ["config-home", "data-home", "state-home", "runtime"] {
            assert!(!temp.path().join(directory).exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn sampled_status_keeps_blame_store_observation_and_semantic_receipts_advisory() {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let temp = tempdir();
    let root = data_root(&temp);
    let attribution = root.join("search/attribution");
    fs::create_dir_all(&attribution).unwrap();
    fs::set_permissions(&attribution, fs::Permissions::from_mode(0o700)).unwrap();
    let private_file = |path: &Path| {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
        file
    };
    let lock_path = attribution.join("attribution-materializer.lock");
    let writer = private_file(&lock_path);
    writer.try_lock().unwrap();
    let progress_path = attribution.join("attribution-progress.json");
    drop(private_file(&progress_path));
    let marker = "SECRET_SAMPLE_ADVISORY_91c8";
    // Synthetic MaterializationProgress wire record with the producer's serde
    // field names; no attribution library or new fixture dependency is needed.
    let progress = serde_json::to_vec(&json!({
        "phase": "indexing", "core_generation_id": marker,
        "completed_sources": 2, "total_sources": 4, "applied_changes": 17, "elapsed_millis": 25,
    }))
    .unwrap();
    fs::write(&progress_path, &progress).unwrap();
    let semantic_path = root.join("daemon/jobs/semantic-index.json");
    fs::create_dir_all(semantic_path.parent().unwrap()).unwrap();
    fs::write(
        &semantic_path,
        serde_json::to_vec(&json!({
            "status": "budget_exhausted", "source_work_remaining": true, "last_run_at_ms": 1,
            "core_generation_id": marker, "last_error": marker, "model_key": marker,
            "semantic_runtime_active": true,
        }))
        .unwrap(),
    )
    .unwrap();

    let sample = || {
        let paths = [&lock_path, &progress_path, &semantic_path];
        let before = paths.map(|path| fs::read(path).ok());
        let output = ctx(&temp)
            .args(["status", "--sample", "1", "--format", "json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains(marker));
        assert!(!text.contains(root.to_string_lossy().as_ref()));
        assert_eq!(paths.map(|path| fs::read(path).ok()), before);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        assert_eq!(fs::read_dir(&attribution).unwrap().count(), 2);
        assert_eq!(
            fs::read_dir(semantic_path.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        serde_json::from_str::<Value>(&text).unwrap()
    };
    let active = sample();
    assert_eq!(active["daemon"]["state"], "unknown");
    assert!(active["observed"]["cpu"]["cpu_ms"].is_null());
    assert_eq!(
        active["blame"],
        json!({
            "writer_observed": true, "phase": "indexing", "completed_sources": 2,
            "total_sources": 4, "applied_changes": 17,
        })
    );
    // An old, unbound checkpoint is retained as context, never current work.
    assert_eq!(
        active["semantic"],
        json!({"last_recorded_status": "budget_exhausted", "work_remaining": true})
    );

    fs::write(&progress_path, b"{").unwrap();
    let partial = sample();
    assert_eq!(partial["blame"]["writer_observed"], true);
    assert_eq!(partial["blame"]["phase"], "snapshot_unavailable");
    assert!(partial["blame"]["completed_sources"].is_null());

    drop(writer);
    fs::write(&progress_path, &progress).unwrap();
    let stale = sample();
    assert_eq!(stale["blame"]["writer_observed"], false);
    assert_eq!(stale["blame"]["phase"], "unknown");
    assert!(stale["blame"]["applied_changes"].is_null());
    assert_eq!(stale["semantic"], active["semantic"]);

    // A malformed lock is an inspection failure, not evidence of no writer.
    fs::remove_file(&lock_path).unwrap();
    fs::create_dir(&lock_path).unwrap();
    fs::write(
        &semantic_path,
        serde_json::to_vec(&json!({"status": marker, "source_work_remaining": marker})).unwrap(),
    )
    .unwrap();
    let unknown = sample();
    assert!(unknown["blame"]["writer_observed"].is_null());
    assert_eq!(unknown["blame"]["phase"], "unknown");
    assert_eq!(
        unknown["semantic"],
        json!({"last_recorded_status": "unknown", "work_remaining": null})
    );
}
