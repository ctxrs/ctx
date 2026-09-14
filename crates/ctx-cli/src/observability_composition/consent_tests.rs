use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};

use super::{
    tests::{daemon_event, RestoreEnvironment},
    *,
};
use crate::analytics_outbox::{AnalyticsOutbox, DeliveryDisposition};
use std::fs;

pub(super) fn isolate_analytics_environment(root: &Path) -> Vec<RestoreEnvironment> {
    let mut guards = [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
        "LOCALAPPDATA",
        "APPDATA",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "CTX_DATA_ROOT",
    ]
    .into_iter()
    .map(|key| RestoreEnvironment::set(key, root))
    .collect::<Vec<_>>();
    guards.extend(
        [
            "CTX_ANALYTICS_OFF",
            "CTX_DISABLE_ANALYTICS",
            "CTX_INSTALL_DIAGNOSTICS_OFF",
            "CTX_ANALYTICS_DRY_RUN",
            "CTX_ANALYTICS_ENDPOINT",
        ]
        .into_iter()
        .map(RestoreEnvironment::remove),
    );
    guards.push(RestoreEnvironment::set("CTX_ANALYTICS_ENABLED", "true"));
    guards.push(RestoreEnvironment::set("CTX_UPGRADE_AUTO", "off"));
    guards
}

fn configure(root: &Path, enabled: bool, endpoint: &str) {
    let endpoint = serde_json::to_string(endpoint).unwrap();
    fs::write(
        AppConfig::config_path(root),
        format!("[analytics]\nenabled = {enabled}\nendpoint = {endpoint}\n"),
    )
    .unwrap();
}

fn request_body(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut content_length = None;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        header_bytes += line.len();
        assert!(header_bytes <= 16 * 1024);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let length = content_length.expect("bounded telemetry Content-Length");
    assert!(length <= 512 * 1024);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    body
}

// Hold the first real HTTP response until the root mutation has completed.
// Continue answering unexpected later requests so the pre-fix failure reports
// the sent bodies, rather than relying on a timeout to hide a second send.
fn drain_across_barrier(
    root: &Path,
    listener: TcpListener,
    mutate: impl FnOnce(),
) -> (anyhow::Result<()>, Vec<Vec<u8>>) {
    listener.set_nonblocking(true).unwrap();
    let done = AtomicBool::new(false);
    let (arrived, first) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    thread::scope(|scope| {
        let done = &done;
        let server = scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut bodies = Vec::new();
            while !done.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "barrier transport did not finish");
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        bodies.push(request_body(&mut stream));
                        if bodies.len() == 1 {
                            arrived.send(()).unwrap();
                            released.recv_timeout(Duration::from_secs(5)).unwrap();
                        }
                        stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                        stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(1)),
                    Err(error) => panic!("accept loopback telemetry: {error}"),
                }
            }
            bodies
        });
        let drain = scope.spawn(|| drain_analytics_outbox(root, Duration::from_secs(10)));
        first
            .recv_timeout(Duration::from_secs(5))
            .expect("first A request must reach the response barrier");
        mutate();
        release.send(()).unwrap();
        let result = drain.join().unwrap();
        done.store(true, Ordering::SeqCst);
        (result, server.join().unwrap())
    })
}

#[derive(Clone, Copy, Debug)]
enum Change {
    MoveRoot,
    MissingIdentity,
    CorruptIdentity,
    Endpoint,
    OptOut,
}

fn drain_transition(change: Change) {
    let _env_lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let sandbox = tempfile::tempdir().unwrap();
    let _environment = isolate_analytics_environment(sandbox.path());
    let a = sandbox.path().join("a");
    let b = sandbox.path().join("b");
    let moved_a = sandbox.path().join("moved-a");
    let id_a = crate::identity::installation_id(&a).unwrap();
    let id_b = crate::identity::installation_id(&b).unwrap();
    let b_identity = fs::read(crate::identity::install_path(&b)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/events", listener.local_addr().unwrap());
    let other_sink = sandbox.path().join("unexpected.jsonl");
    let other_endpoint = url::Url::from_file_path(&other_sink).unwrap().to_string();
    configure(&a, true, &endpoint);
    configure(&b, true, &endpoint);
    append_analytics_batch(&a, &[daemon_event()]).unwrap();
    append_analytics_batch(&a, &[daemon_event()]).unwrap();
    append_analytics_batch(&b, &[daemon_event()]).unwrap();
    let path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, &a).unwrap();
    let outbox_a = AnalyticsOutbox::open(path.clone(), &id_a).unwrap();
    let outbox_b = AnalyticsOutbox::open(path.clone(), &id_b).unwrap();
    let queued_a = outbox_a.snapshot(&endpoint).unwrap();
    assert_eq!(queued_a.len(), 2);
    let queued_b = outbox_b.snapshot(&endpoint).unwrap();
    let (result, sent) = drain_across_barrier(&a, listener, || match change {
        Change::MoveRoot => {
            fs::rename(&a, &moved_a).unwrap();
            configure(&moved_a, false, &endpoint);
            fs::rename(&b, &a).unwrap();
        }
        Change::MissingIdentity => fs::remove_file(crate::identity::install_path(&a)).unwrap(),
        Change::CorruptIdentity => {
            fs::write(crate::identity::install_path(&a), b"invalid identity").unwrap()
        }
        Change::Endpoint => configure(&a, true, &other_endpoint),
        Change::OptOut => configure(&a, false, &endpoint),
    });
    assert_eq!(
        sent.len(),
        1,
        "stale owner authorized a second A batch after {change:?}"
    );
    assert_eq!(sent[0], queued_a[0].payload());
    assert!(!other_sink.exists());
    assert_eq!(
        outbox_b.snapshot(&endpoint).unwrap()[0].payload(),
        queued_b[0].payload()
    );
    let b_now = if matches!(change, Change::MoveRoot) {
        &a
    } else {
        &b
    };
    assert_eq!(
        fs::read(crate::identity::install_path(b_now)).unwrap(),
        b_identity
    );
    match change {
        Change::MissingIdentity => {
            assert!(result.is_err());
            assert!(!crate::identity::install_path(&a).exists());
        }
        Change::CorruptIdentity => {
            assert!(result.is_err());
            assert_eq!(
                fs::read(crate::identity::install_path(&a)).unwrap(),
                b"invalid identity"
            );
        }
        Change::MoveRoot => {
            assert!(result.is_err());
            assert_eq!(
                crate::identity::existing_installation_id(&moved_a)
                    .unwrap()
                    .as_deref(),
                Some(id_a.as_str())
            );
        }
        Change::Endpoint => {
            result.unwrap();
            let remaining = outbox_a.snapshot(&endpoint).unwrap();
            assert_eq!(remaining.len(), 1);
            assert_eq!(remaining[0].payload(), queued_a[1].payload());
        }
        Change::OptOut => {
            result.unwrap();
            assert!(outbox_a.snapshot(&endpoint).unwrap().is_empty());
        }
    }
}

#[test]
fn moved_owner_stops_later_requests_at_the_response_barrier() {
    drain_transition(Change::MoveRoot);
}
#[test]
fn missing_owner_stops_later_requests_at_the_response_barrier() {
    drain_transition(Change::MissingIdentity);
}
#[test]
fn corrupt_owner_stops_later_requests_at_the_response_barrier() {
    drain_transition(Change::CorruptIdentity);
}
#[test]
fn endpoint_change_stops_later_requests_at_the_response_barrier() {
    drain_transition(Change::Endpoint);
}
#[test]
fn opt_out_purges_only_its_owner_at_the_response_barrier() {
    drain_transition(Change::OptOut);
}

#[test]
fn opt_out_without_original_identity_preserves_other_owners_and_creates_no_identity() {
    let _env_lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for corrupt in [false, true] {
        let sandbox = tempfile::tempdir().unwrap();
        let _environment = isolate_analytics_environment(sandbox.path());
        let a = sandbox.path().join("a");
        let b = sandbox.path().join("b");
        let id_a = crate::identity::installation_id(&a).unwrap();
        let id_b = crate::identity::installation_id(&b).unwrap();
        let sink = sandbox.path().join("unexpected.jsonl");
        let endpoint = url::Url::from_file_path(&sink).unwrap().to_string();
        configure(&a, true, &endpoint);
        configure(&b, true, &endpoint);
        append_analytics_batch(&a, &[daemon_event()]).unwrap();
        append_analytics_batch(&b, &[daemon_event()]).unwrap();
        let path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, &a).unwrap();
        let original_queue = fs::read(&path).unwrap();
        let b_identity = fs::read(crate::identity::install_path(&b)).unwrap();
        configure(&a, false, &endpoint);
        if corrupt {
            fs::write(crate::identity::install_path(&a), b"invalid identity").unwrap();
        } else {
            fs::remove_file(crate::identity::install_path(&a)).unwrap();
        }
        assert_eq!(
            append_analytics_batch(&a, &[daemon_event()]).is_err(),
            corrupt
        );
        assert_eq!(
            drain_analytics_outbox(&a, Duration::from_secs(1)).is_err(),
            corrupt
        );
        assert_eq!(fs::read(&path).unwrap(), original_queue);
        assert_eq!(
            fs::read(crate::identity::install_path(&b)).unwrap(),
            b_identity
        );
        assert!(!sink.exists());
        if corrupt {
            assert_eq!(
                fs::read(crate::identity::install_path(&a)).unwrap(),
                b"invalid identity"
            );
        } else {
            assert!(!crate::identity::install_path(&a).exists());
        }
        let b_outbox = AnalyticsOutbox::open(path.clone(), &id_b).unwrap();
        assert_eq!(b_outbox.snapshot(&endpoint).unwrap().len(), 1);
        let a_outbox = AnalyticsOutbox::open(path, &id_a).unwrap();
        assert_eq!(a_outbox.snapshot(&endpoint).unwrap().len(), 1);
    }
}

#[test]
fn recovery_queueing_rechecks_the_captured_owner_without_creating_a_replacement() {
    let _env_lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let sandbox = tempfile::tempdir().unwrap();
    let _environment = isolate_analytics_environment(sandbox.path());
    let a = sandbox.path().join("a");
    let id_a = crate::identity::installation_id(&a).unwrap();
    let endpoint = url::Url::from_file_path(sandbox.path().join("sink.jsonl"))
        .unwrap()
        .to_string();
    configure(&a, true, &endpoint);
    let config = AppConfig::load(&a).unwrap();
    append_analytics_batch(&a, &[daemon_event()]).unwrap();
    append_analytics_batch(&a, &[daemon_event()]).unwrap();
    let path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, &a).unwrap();
    let outbox = AnalyticsOutbox::open(path.clone(), &id_a).unwrap();
    let snapshot = outbox.snapshot(&endpoint).unwrap();
    outbox
        .reconcile(&[
            (
                snapshot[0].clone(),
                DeliveryDisposition::Permanent {
                    class: crate::analytics::AnalyticsDeliveryFailureClass::ClientRejection,
                },
            ),
            (snapshot[1].clone(), DeliveryDisposition::Accepted),
        ])
        .unwrap();
    assert!(outbox.pending_observation().unwrap().is_some());
    let before = fs::read(&path).unwrap();
    fs::remove_file(crate::identity::install_path(&a)).unwrap();
    assert!(queue_pending_delivery_observation(&a, &id_a, &config, &outbox).is_err());
    assert!(!crate::identity::install_path(&a).exists());
    assert_eq!(fs::read(&path).unwrap(), before);
}
