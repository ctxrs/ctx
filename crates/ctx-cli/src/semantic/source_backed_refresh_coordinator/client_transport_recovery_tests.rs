use super::*;

use std::{
    io::{Read as _, Write as _},
    os::unix::net::UnixListener,
};

use crate::semantic::query_service::{
    write_daemon_service_endpoint, DaemonIpcService, DaemonQueryEndpoint,
};

const TEST_ENDPOINT_TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn short_data_root() -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("ctx-refresh-")
        .tempdir_in("/tmp")
        .map_err(Into::into)
}

fn source_refresh_endpoint(socket_path: &Path) -> DaemonQueryEndpoint {
    DaemonQueryEndpoint::Unix {
        path: socket_path.to_owned(),
        token: TEST_ENDPOINT_TOKEN.to_owned(),
    }
}

#[test]
fn pre_submission_connection_refusal_remains_typed_unavailable() -> Result<()> {
    let data_root = short_data_root()?;
    let socket_path = data_root.path().join("refused.sock");
    let listener = UnixListener::bind(&socket_path)?;
    drop(listener);
    write_daemon_service_endpoint(
        data_root.path(),
        DaemonIpcService::SourceRefresh,
        &source_refresh_endpoint(&socket_path),
    )?;

    let error = daemon_source_refresh_request(
        data_root.path(),
        compact_json(json!({
            "schema_version": 1,
            "op": SOURCE_REFRESH_REQUEST_OP,
            "request_id": "019fcaaa-0000-7000-8000-000000000299",
        })),
        StdDuration::from_millis(100),
        SOURCE_REFRESH_RESPONSE_MAX_BYTES,
    )
    .expect_err("a closed pre-submission endpoint must be unavailable");

    assert!(error
        .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
        .is_some());
    Ok(())
}

#[test]
fn same_id_reenqueue_replays_the_exact_payload_after_a_lost_ack() -> Result<()> {
    let data_root = short_data_root()?;
    let socket_path = data_root.path().join("lost-ack.sock");
    let listener = UnixListener::bind(&socket_path)?;
    write_daemon_service_endpoint(
        data_root.path(),
        DaemonIpcService::SourceRefresh,
        &source_refresh_endpoint(&socket_path),
    )?;
    let request_id = "019fcaaa-0000-7000-8000-000000000300";
    let server = std::thread::spawn(move || -> Result<[Vec<u8>; 2]> {
        let mut requests = Vec::with_capacity(2);
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept()?;
            let mut received = Vec::new();
            stream.read_to_end(&mut received)?;
            requests.push(received);
            if attempt == 0 {
                continue;
            }
            stream.write_all(
                format!(
                    "{{\"ok\":true,\"owner\":\"daemon\",\"request_id\":\"{request_id}\",\"request_state\":\"admission_pending\",\"schema_version\":1}}\n"
                )
                .as_bytes(),
            )?;
        }
        requests
            .try_into()
            .map_err(|_| anyhow!("test server did not observe exactly two requests"))
    });

    let recovered = enqueue_equivalent_wait_refresh_request(
        data_root.path(),
        request_id,
        SourceBackedRefreshOperation::Refresh,
        None,
        false,
    )?;
    let requests = server.join().expect("lost-ack test server panicked")?;

    assert_eq!(recovered, request_id);
    assert_eq!(requests[0], requests[1]);
    for request in requests {
        let request: Value = serde_json::from_slice(&request)?;
        assert_eq!(request["request_id"], request_id);
    }
    Ok(())
}

#[test]
fn exhausted_post_submission_disconnects_return_typed_ambiguous_admission() -> Result<()> {
    let data_root = short_data_root()?;
    let socket_path = data_root.path().join("lost-all-acks.sock");
    let listener = UnixListener::bind(&socket_path)?;
    write_daemon_service_endpoint(
        data_root.path(),
        DaemonIpcService::SourceRefresh,
        &source_refresh_endpoint(&socket_path),
    )?;
    let request_id = "019fcaaa-0000-7000-8000-000000000304";

    let server = std::thread::spawn(move || -> Result<Vec<Vec<u8>>> {
        let mut requests = Vec::new();
        for _ in 0..=AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT {
            let (mut stream, _) = listener.accept()?;
            let mut received = Vec::new();
            stream.read_to_end(&mut received)?;
            requests.push(received);
        }
        Ok(requests)
    });

    let error = enqueue_equivalent_wait_refresh_request(
        data_root.path(),
        request_id,
        SourceBackedRefreshOperation::Refresh,
        None,
        false,
    )
    .unwrap_err();
    let requests = server.join().expect("lost-ack test server panicked")?;

    let recovery = error
        .downcast_ref::<SourceRefreshAdmissionRecoveryFailed>()
        .expect("post-submission disconnect must use typed admission recovery");
    assert_eq!(recovery.request_id, request_id);
    assert_eq!(
        recovery.recovery_attempts,
        AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT
    );
    assert_eq!(
        requests.len(),
        1 + AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT
    );
    assert!(requests.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(!error.to_string().contains("timed out"));
    Ok(())
}

#[test]
fn typed_unknown_readmission_preserves_lost_ack_retention_uncertainty() -> Result<()> {
    let data_root = short_data_root()?;
    let socket_path = data_root.path().join("typed-unknown-lost-acks.sock");
    let listener = UnixListener::bind(&socket_path)?;
    write_daemon_service_endpoint(
        data_root.path(),
        DaemonIpcService::SourceRefresh,
        &source_refresh_endpoint(&socket_path),
    )?;
    let request_id = "019fcaaa-0000-7000-8000-000000000307";
    let server = std::thread::spawn(move || -> Result<Vec<Vec<u8>>> {
        let mut requests = Vec::new();
        for _ in 0..=AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT {
            let (mut stream, _) = listener.accept()?;
            let mut received = Vec::new();
            stream.read_to_end(&mut received)?;
            requests.push(received);
        }
        Ok(requests)
    });

    let mut recovery = TypedUnknownRequestRecovery::new(request_id);
    let error = recover_typed_unknown_request_with(
        &mut recovery,
        request_id,
        |_| {},
        || {
            enqueue_equivalent_wait_refresh_request(
                data_root.path(),
                request_id,
                SourceBackedRefreshOperation::Refresh,
                None,
                false,
            )
        },
    )
    .unwrap_err();
    let requests = server.join().expect("lost-ack test server panicked")?;

    let typed = error
        .downcast_ref::<SourceRefreshRequestRecoveryFailed>()
        .expect("typed unknown re-admission keeps its request-bound failure");
    assert_eq!(typed.request_id, request_id);
    assert_eq!(typed.recovery_attempts, 1);
    assert_eq!(
        typed.reason,
        SourceRefreshRequestRecoveryFailureReason::ReenqueueFailed
    );
    assert_eq!(
        typed.retention,
        SourceRefreshRequestRetention::MayBeRetained
    );
    assert_eq!(typed.disconnect_policy, Some(DISCONNECT_POLICY));
    assert!(error
        .to_string()
        .contains("disconnect_policy=retain_after_durable_admission"));
    assert_eq!(
        requests.len(),
        1 + AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT
    );
    assert!(requests.windows(2).all(|pair| pair[0] == pair[1]));
    Ok(())
}
