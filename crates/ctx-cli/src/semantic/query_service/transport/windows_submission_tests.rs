use super::*;

use super::super::daemon_query_roundtrip_windows;

#[test]
fn missing_named_pipe_is_definitely_pre_submission() {
    let error = daemon_query_roundtrip_windows(
        r"\\.\pipe\ctx-daemon-query-00000000000000000000000000000294",
        b"{\"op\":\"ping\"}\n",
        std::time::Duration::from_millis(100),
        1024,
    )
    .expect_err("an uncreated named pipe must be unavailable before submission");

    assert!(!request_may_have_been_submitted(&error));
}

#[test]
fn pending_overlapped_write_is_ambiguous_before_cancellation() {
    let mut may_have_been_submitted = false;
    super::mark_windows_pending_submission(Some(&mut may_have_been_submitted));
    assert!(may_have_been_submitted);
}
