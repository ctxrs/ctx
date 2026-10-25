use super::*;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

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
        rows.iter().map(|row| &row["data_root"]).collect::<Vec<_>>(),
        [&json!(active), &json!(stopped), &json!(missing)]
    );
    assert_eq!(rows[0]["daemon"]["running"], true);
    assert_eq!(
        rows[1]["daemon"]["running"], false,
        "a retained PID is not a live owner"
    );
    assert_eq!(
        rows[1]["daemon"]["enabled"], false,
        "read-only config was not parsed"
    );
    assert_eq!(rows[2]["daemon"]["running"], false);
    assert!(!missing.exists());
    assert_eq!(
        files(temp.path()),
        before,
        "inventory mutated lifecycle files"
    );

    fs::write(registrations.join("malformed.json"), b"not JSON")?;
    fs::write(
        registrations.join("relative.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1, "registration_id": "relative", "status": "live",
            "data_root": "old-relative", "pid": std::process::id(),
        }))?,
    )?;
    let before = files(temp.path());
    let observed = collect()?;
    assert_eq!(observed.len(), 5);
    assert_eq!(
        observed[..3]
            .iter()
            .map(|row| &row["data_root"])
            .collect::<Vec<_>>(),
        [&json!(active), &json!(stopped), &json!(missing)]
    );
    assert_eq!(observed[0]["daemon"]["running"], true);
    assert_eq!(observed[1]["daemon"]["running"], false);
    assert_eq!(observed[2]["daemon"]["running"], false);
    assert!(observed[3]["data_root"].is_null());
    assert!(observed[3]["daemon"].is_null());
    assert!(observed[3]["error"]
        .as_str()
        .unwrap()
        .contains("parse ctx daemon acknowledgement"));
    assert_eq!(
        observed[3]["registration_path"],
        json!(registrations.join("malformed.json"))
    );
    assert_eq!(observed[4]["data_root"], "old-relative");
    assert!(observed[4]["daemon"].is_null());
    assert!(observed[4]["error"]
        .as_str()
        .unwrap()
        .contains("invalid ctx daemon acknowledgement"));
    assert!(ctx_daemon_runtime::registered_installation_roots(&registrations).is_err());
    assert_eq!(
        files(temp.path()),
        before,
        "partial inventory mutated records"
    );
    Ok(())
}
