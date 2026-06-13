use std::collections::HashMap;
use std::io::Cursor;

use super::process_setup::prepare_exec_request;
use super::protocol::{AvfLinuxExecRequest, AVF_LINUX_EXEC_PROTOCOL_VERSION};
use super::*;

#[test]
fn exec_stream_payload_budget_stays_within_shared_vm_safe_limit() {
    const {
        assert!(
            AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD <= 1024,
            "shared-VM exec transport truncated larger stdin frames in live tar-import repros",
        );
    }
}

#[test]
fn prepare_exec_request_rejects_empty_command() {
    let err = prepare_exec_request(&AvfLinuxExecRequest {
        protocol_version: AVF_LINUX_EXEC_PROTOCOL_VERSION,
        command: " ".to_string(),
        args: Vec::new(),
        cwd: "/tmp".to_string(),
        user: None,
        env: HashMap::new(),
        pty: false,
    })
    .expect_err("empty command should fail");
    assert!(err.to_string().contains("must not be empty"));
}

#[test]
fn emit_exec_stream_frames_splits_large_stdout_payloads() {
    let payload = vec![b'x'; AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD + 33];
    let mut bytes = Vec::new();

    emit_exec_stream_frames(&payload, true, |frame| {
        write_exec_frame(&mut bytes, &frame).map_err(anyhow::Error::from)
    })
    .expect("split stdout frames");

    let mut cursor = Cursor::new(bytes);
    let first = read_exec_frame(&mut cursor)
        .expect("read first")
        .expect("first frame");
    let second = read_exec_frame(&mut cursor)
        .expect("read second")
        .expect("second frame");
    let eof = read_exec_frame(&mut cursor).expect("read eof");

    let AvfLinuxExecFrame::Stdout(first) = first else {
        panic!("expected stdout frame");
    };
    let AvfLinuxExecFrame::Stdout(second) = second else {
        panic!("expected stdout frame");
    };

    assert_eq!(first.len(), AVF_EXEC_STREAM_FRAME_MAX_PAYLOAD);
    assert_eq!(second.len(), 33);
    assert!(eof.is_none());
}
