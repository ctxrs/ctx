use super::*;

#[test]
fn semantic_activation_retry_wakes_the_idle_daemon_at_its_deadline() {
    let now = Instant::now();
    let mut runtime = DaemonRuntime::default();
    runtime.semantic_activation_retry.consecutive_failures = 1;
    runtime.semantic_activation_retry.retry_not_before = Some(now + StdDuration::from_secs(5));
    runtime.semantic_activation_retry.retry_not_before_at_ms =
        Some(utc_now().timestamp_millis() + 5_000);

    let wait_for = daemon_wait_duration(&runtime, None, now + StdDuration::from_secs(30), now);

    assert!(wait_for > StdDuration::from_secs(4));
    assert!(wait_for <= StdDuration::from_secs(5));
}
