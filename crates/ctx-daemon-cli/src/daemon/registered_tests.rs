use super::*;
use std::{collections::BTreeMap, fs, path::Path};

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    if root.exists() {
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.extend(files(&path));
            } else {
                result.insert(path.clone(), fs::read(path).unwrap());
            }
        }
    }
    result
}

#[test]
fn registered_roots_use_existing_liveness_without_mutation() -> Result<()> {
    let environment =
        crate::test_environment::EnvironmentGuard::capture(&["HOME", "CTX_DATA_ROOT"]);
    let temp = tempfile::tempdir()?;
    let managed = temp.path().join("managed");
    environment.set("HOME", Some(temp.path().as_os_str()));
    environment.set("CTX_DATA_ROOT", Some(managed.as_os_str()));
    assert!(collect()?.is_empty());
    assert!(!managed.exists(), "empty inventory created a root");

    let active = temp.path().join("a-active");
    let stopped = temp.path().join("b-stopped");
    let missing = temp.path().join("c-missing");
    let _active_lock = ctx_daemon_runtime::DaemonLock::acquire(&active)?.unwrap();
    let stopped_lock = ctx_daemon_runtime::DaemonLock::acquire(&stopped)?.unwrap();
    drop(stopped_lock);
    for root in [&active, &stopped] {
        fs::write(
            root.join("config.toml"),
            "[upgrade]\nallow_rfc2544_fake_ip = true\n",
        )?;
    }
    fs::write(
        stopped.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n[upgrade]\nallow_rfc2544_fake_ip = true\n",
    )?;
    ctx_daemon_runtime::secure_private_file_permissions(&stopped.join("config.toml"))?;
    let (_, registrations) = ctx_upgrade_engine::installation_daemon_coordination_paths()?;
    fs::create_dir_all(&registrations)?;
    for (id, root, status) in [
        ("active", &active, "live"),
        ("duplicate", &active, "acknowledged"),
        ("stopped", &stopped, "live"),
        ("missing", &missing, "released"),
    ] {
        fs::write(
            registrations.join(format!("{id}.json")),
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "registration_id": id,
                "data_root": root,
                "status": status,
                "pid": std::process::id(),
            }))?,
        )?;
    }
    let other = managed.join("daemon-installations/other-executable/daemon-quiescence-acks");
    fs::create_dir_all(&other)?;
    fs::write(
        other.join("not-our-registration.json"),
        b"unrelated registry",
    )?;
    let before = files(temp.path());

    let rows = collect()?;

    assert_eq!(
        rows.iter().map(|(root, _)| root).collect::<Vec<_>>(),
        [&active, &stopped, &missing]
    );
    assert_eq!(rows[0].1["running"], true);
    assert_eq!(
        rows[1].1["running"], false,
        "a retained PID is not a live owner"
    );
    assert_eq!(
        rows[1].1["enabled"], false,
        "read-only config was not parsed"
    );
    assert_eq!(rows[2].1["running"], false);
    assert!(!missing.exists());
    assert_eq!(
        files(temp.path()),
        before,
        "inventory mutated lifecycle files"
    );

    fs::write(registrations.join("malformed.json"), b"not JSON")?;
    let before = files(temp.path());
    assert!(
        collect().is_err(),
        "invalid registry must not look complete"
    );
    assert_eq!(
        files(temp.path()),
        before,
        "failed inventory pruned records"
    );
    Ok(())
}

#[test]
fn registered_root_rendering_preserves_paths_and_unverified_state() {
    use ctx_terminal::{ColorMode, StreamKind, TestContext};
    for width in [32, 48, 80, 120] {
        let context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never),
        );
        let rendered = render(
            &context,
            &[
                (
                    PathBuf::from("/scratch/two  spaces\u{1b}"),
                    json!({"running": false, "status": "unverified"}),
                ),
                (
                    PathBuf::from("/scratch/active"),
                    json!({"running": true, "status": "running"}),
                ),
            ],
        )
        .render_plain();
        assert!(rendered.contains("/scratch/two  spaces\\x1b"), "{rendered}");
        assert!(rendered.contains("Unverified"), "{rendered}");
        assert!(rendered.contains("Running"), "{rendered}");
        assert!(!rendered.contains('\u{1b}'));
        assert!(render(&context, &[])
            .render_plain()
            .contains("No registered daemons"));
    }
}
