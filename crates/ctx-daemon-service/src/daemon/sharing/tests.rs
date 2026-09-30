use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use ctx_history_sharing::{Connection, Credentials, Endpoint, SharingPolicy, SharingStore};
use serde_json::json;

use super::*;

const COLLECTION: &str = "00000000-0000-4000-8000-000000000001";

fn connect(data_root: &Path, name: &str) -> Result<(SharingStore, TcpListener)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let store = SharingStore::new(data_root.join("sharing").join(name));
    store.connect(
        Connection {
            endpoint: Endpoint::parse(&format!("http://{}", listener.local_addr()?))?,
            collection: COLLECTION.to_owned(),
        },
        Credentials::device("test-token".to_owned())?,
    )?;
    Ok((store, listener))
}

fn enable(store: &SharingStore, listener: TcpListener) -> Result<()> {
    let policy: SharingPolicy = serde_json::from_value(json!({
        "revision": 1,
        "archive_identity": {"origin": "test-device", "view": "test-team"},
        "writer_epoch": 1,
        "mode": {"kind": "automatic"},
        "sources": [{
            "source_id": "synthetic-source",
            "profile_root": null,
            "baseline_revisions": {},
            "backfill": {"kind": "all"},
            "include_future": true,
            "whole_source": true,
            "work_roots": []
        }]
    }))?;
    let server = thread::spawn(move || -> Result<()> {
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    anyhow::ensure!(
                        Instant::now() < deadline,
                        "missing publisher authentication"
                    );
                    thread::park_timeout(Duration::from_millis(5));
                }
                Err(error) => return Err(error.into()),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut input = BufReader::new(&stream);
        let mut line = String::new();
        input.read_line(&mut line)?;
        assert_eq!(
            line,
            format!("GET /v1/collections/{COLLECTION}/status HTTP/1.1\r\n")
        );
        let mut authenticated = false;
        loop {
            line.clear();
            anyhow::ensure!(
                input.read_line(&mut line)? != 0,
                "incomplete status request"
            );
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("authorization") {
                    assert_eq!(value.trim(), "Bearer test-token");
                    authenticated = true;
                }
            }
        }
        assert!(authenticated);
        drop(input);
        let body = json!({
            "principal": "synthetic-daemon-publisher", "collection": COLLECTION,
            "stored_sequence": 0, "searchable_sequence": 0, "generation": null,
            "reads_available": true, "off_host_checkpoint": null,
        })
        .to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
        stream.flush()?;
        Ok(())
    });
    let result = store.set_policy(policy);
    server.join().expect("status server exits")?;
    result?;
    Ok(())
}

#[test]
fn no_configuration_or_connection_without_policy_starts_no_worker() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    assert!(start_workers(&data_root, DaemonRunProfile::Persistent, true).is_empty());
    assert!(!data_root.exists(), "default startup must remain read-only");

    let (store, _listener) = connect(&data_root, "connected-only")?;
    let settings_before = fs::read(store.root().join("settings.json"))?;
    assert!(start_workers(&data_root, DaemonRunProfile::Persistent, true).is_empty());
    assert_eq!(
        fs::read(store.root().join("settings.json"))?,
        settings_before
    );
    assert!(!store.root().join("uploader.lock").exists());
    assert!(!data_root.join("search").exists());
    Ok(())
}

#[test]
fn finite_workers_and_unready_daemons_do_not_start_configured_sharing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let (store, listener) = connect(&data_root, "enabled")?;
    enable(&store, listener)?;

    for (profile, ready) in [
        (DaemonRunProfile::FiniteCoreWorker, true),
        (DaemonRunProfile::FiniteCoreWorker, false),
        (DaemonRunProfile::Persistent, false),
    ] {
        assert!(start_workers(&data_root, profile, ready).is_empty());
    }
    assert!(!store.root().join("uploader.lock").exists());
    assert!(!data_root.join("search").exists());
    Ok(())
}

#[test]
fn configured_destinations_start_despite_broken_and_unconfigured_neighbors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    for name in ["team-a", "team-b"] {
        let (store, listener) = connect(&data_root, name)?;
        enable(&store, listener)?;
    }
    connect(&data_root, "connected-only")?;
    let (broken, _listener) = connect(&data_root, "broken")?;
    fs::write(broken.root().join("settings.json"), b"{")?;
    fs::write(
        data_root.join("sharing").join("not-a-directory"),
        b"ignored",
    )?;

    let workers = start_workers(&data_root, DaemonRunProfile::Persistent, true);
    assert_eq!(workers.len(), 2);
    drop(workers);
    assert_eq!(fs::read(broken.root().join("settings.json"))?, b"{");
    assert!(!data_root.join("search").exists());
    Ok(())
}

#[test]
fn live_enable_discovers_new_policy_once_without_restarting_local_daemon() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("data");
    let mut workers = start_workers(&root, DaemonRunProfile::Persistent, true);
    let (store, listener) = connect(&root, "later")?;
    workers.next_discovery = Instant::now();
    workers.reconcile(&root);
    assert!(workers.is_empty(), "connect alone must not enable uploads");
    enable(&store, listener)?;
    workers.next_discovery = Instant::now();
    workers.reconcile(&root);
    assert_eq!(workers.len(), 1);
    let original = workers.handles.get(store.root()).unwrap() as *const SharingWorker;
    workers.next_discovery = Instant::now();
    workers.reconcile(&root);
    assert_eq!(workers.len(), 1);
    assert_eq!(
        workers.handles.get(store.root()).unwrap() as *const SharingWorker,
        original
    );
    assert!(workers.wait_duration(Duration::from_secs(60)) <= Duration::from_secs(30));
    Ok(())
}

#[test]
fn worker_handles_stop_on_success_and_error_without_waiting_for_retry_timer() -> Result<()> {
    // No index or pending payload exists, so an enabled worker can only idle.
    // Exercise the same scoped ownership used by daemon startup/error paths.
    for fail in [false, true] {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
        let (store, listener) = connect(&data_root, "enabled")?;
        enable(&store, listener)?;
        let (finished, completion) = std::sync::mpsc::sync_channel(1);
        let owner = thread::spawn(move || {
            let result = (|| -> Result<()> {
                let workers = start_workers(&data_root, DaemonRunProfile::Persistent, true);
                assert_eq!(workers.len(), 1);
                let deadline = Instant::now() + Duration::from_secs(2);
                while !data_root.join("sharing/enabled/uploader.lock").exists() {
                    assert!(Instant::now() < deadline, "sharing worker did not tick");
                    thread::park_timeout(Duration::from_millis(5));
                }
                if fail {
                    anyhow::bail!("synthetic daemon error");
                }
                drop(workers);
                Ok(())
            })();
            finished.send(result.is_err()).expect("completion receiver");
        });
        assert_eq!(completion.recv_timeout(Duration::from_secs(5))?, fail);
        owner.join().expect("worker owner exits cleanly");
    }
    Ok(())
}
