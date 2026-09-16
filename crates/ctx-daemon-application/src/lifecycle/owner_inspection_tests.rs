use super::*;
use ctx_daemon_runtime::{
    write_daemon_service_endpoint_at, write_private_json_file, DaemonLock, DaemonQueryEndpoint,
    ProcessExecutableInspectionDenied,
};
use std::{
    io::{Read as _, Write as _},
    os::unix::net::UnixListener,
};

#[test]
fn denied_image_observation_requires_live_stable_owner_and_authenticated_response() -> Result<()> {
    for case in [
        "ready",
        "wrong_response_pid",
        "malformed",
        "changed_owner",
        "released",
        "wrong_root",
        "wrong_endpoint_pid",
    ] {
        let temp = tempfile::tempdir_in("/tmp")?;
        let data_root = temp.path();
        let lock = DaemonLock::acquire(data_root)?.expect("new owner");
        let lock_path = daemon_lock_path(data_root);
        let original = fs::read(&lock_path)?;
        let endpoint_path = data_root.join("daemon/source-refresh-endpoint.json");
        let socket_path = data_root.join("owner.sock");
        let listener = UnixListener::bind(&socket_path)?;
        write_daemon_service_endpoint_at(
            &endpoint_path,
            &DaemonQueryEndpoint::Unix {
                path: socket_path,
                token: "test-owner-token-with-at-least-32-bytes".to_owned(),
            },
        )?;
        if case == "wrong_endpoint_pid" {
            let mut value: Value = serde_json::from_slice(&fs::read(&endpoint_path)?)?;
            value["pid"] = json!(u32::MAX);
            write_private_json_file(&endpoint_path, &value)?;
        }
        if case == "wrong_root" {
            let mut value: Value = serde_json::from_slice(&original)?;
            value["data_root"] = json!(data_root.join("another-root"));
            write_private_json_file(&lock_path, &value)?;
        }
        let lock = if case == "released" {
            drop(lock);
            None
        } else {
            Some(lock)
        };
        let before_endpoint = fs::read(&endpoint_path)?;
        let before_lock = fs::read(&lock_path)?;
        let will_connect = !matches!(case, "released" | "wrong_root" | "wrong_endpoint_pid");
        let server_lock_path = lock_path.clone();
        let server = will_connect.then(|| std::thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut request = String::new();
            stream.read_to_string(&mut request)?;
            let request: Value = serde_json::from_str(&request)?;
            assert_eq!(request["op"], "lifecycle_ping");
            assert_eq!(request["token"], "test-owner-token-with-at-least-32-bytes");
            if case == "changed_owner" {
                let mut value = read_pid_lock_json(&server_lock_path).unwrap();
                value["owner_id"] = json!("replacement-owner");
                write_private_json_file(&server_lock_path, &value)?;
            }
            let response = if case == "malformed" { json!({"ok": true}) } else {
                json!({
                    "schema_version": 1, "ok": true, "owner": "daemon", "service": "lifecycle",
                    "pid": if case == "wrong_response_pid" { u32::MAX } else { std::process::id() },
                    "readiness": "ready",
                })
            };
            stream.write_all(serde_json::to_string(&response)?.as_bytes())?;
            Ok(())
        }));
        let result = verify_inspection_denied_owner(
            data_root,
            &env::current_exe()?,
            &ProcessExecutableInspectionDenied {
                pid: std::process::id(),
            },
        );
        assert_eq!(result.is_ok(), case == "ready", "{case}: {result:?}");
        if let Some(server) = server {
            server.join().expect("probe server")?;
        }
        assert_eq!(
            fs::read(&endpoint_path)?,
            before_endpoint,
            "probe must not clean up endpoints"
        );
        if case != "changed_owner" {
            assert_eq!(
                fs::read(&lock_path)?,
                before_lock,
                "probe must not rewrite ownership"
            );
        }
        drop(lock);
    }
    Ok(())
}

#[test]
fn denied_image_observation_does_not_accept_missing_endpoint_or_a_different_binary() -> Result<()> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let _lock = DaemonLock::acquire(temp.path())?.expect("owner");
    let denied = ProcessExecutableInspectionDenied {
        pid: std::process::id(),
    };
    assert!(verify_inspection_denied_owner(temp.path(), &env::current_exe()?, &denied).is_err());
    let unrelated = temp.path().join("replacement");
    fs::write(&unrelated, b"different executable bytes")?;
    assert!(verify_inspection_denied_owner(temp.path(), &unrelated, &denied).is_err());
    assert!(daemon_lock_is_active(temp.path()));
    Ok(())
}
