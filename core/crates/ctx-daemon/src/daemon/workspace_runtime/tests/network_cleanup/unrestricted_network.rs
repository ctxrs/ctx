use super::*;

#[cfg(unix)]
mod fixtures;

#[cfg(unix)]
use fixtures::{UnrestrictedNetworkHarness, UnrestrictedNetworkPidFile};

#[cfg(unix)]
#[tokio::test]
async fn unrestricted_network_transition_surfaces_teardown_failures() {
    let harness = UnrestrictedNetworkHarness::new().await;
    harness.write_proxy_pid(UnrestrictedNetworkPidFile::Malformed);
    harness.write_failing_teardown_helpers();
    let err = apply_container_network_policy(
        harness.root(),
        &SandboxCommandMode::NativeContainer,
        WorkspaceId::new(),
        "ctx-harness-test",
        &harness.settings(),
        "127.0.0.1",
        4399,
    )
    .await
    .expect_err("teardown failure should be explicit");

    let message = format!("{err:#}");
    assert!(
        message.contains("failed to tear down restricted container network policy"),
        "unexpected teardown error: {message}"
    );
    assert!(
        message.contains("stop transparent proxy"),
        "unexpected teardown error: {message}"
    );
    assert!(
        message.contains("clear egress guard"),
        "unexpected teardown error: {message}"
    );
    assert!(
        message.contains("failed to reset output policy"),
        "unexpected teardown error: {message}"
    );

    let log = harness.sandbox_log();
    assert_eq!(
        log.lines().filter(|line| line.starts_with("exec ")).count(),
        2,
        "expected both teardown steps to run before surfacing the failure"
    );

    let helper_log = harness.helper_log();
    assert!(helper_log.contains(&format!("rm -f {}", harness.pid_file_path().display())));
    assert!(helper_log.contains("iptables -t nat -F OUTPUT"));
    assert!(helper_log.contains("iptables -F OUTPUT"));
    assert!(helper_log.contains("iptables -P OUTPUT ACCEPT"));
}

#[cfg(unix)]
#[tokio::test]
async fn unrestricted_network_transition_ignores_stale_proxy_pid_file() {
    let harness = UnrestrictedNetworkHarness::new().await;
    harness.write_proxy_pid(UnrestrictedNetworkPidFile::Stale);
    harness.write_successful_teardown_helpers();
    let applied = apply_container_network_policy(
        harness.root(),
        &SandboxCommandMode::NativeContainer,
        WorkspaceId::new(),
        "ctx-harness-test",
        &harness.settings(),
        "127.0.0.1",
        4399,
    )
    .await
    .expect("stale proxy pid should be ignored during unrestricted teardown");

    assert!(!applied.egress_guard);
    assert!(
        !harness.pid_file_path().exists(),
        "stale proxy pid file should be removed during teardown"
    );

    let log = harness.sandbox_log();
    assert_eq!(
        log.lines().filter(|line| line.starts_with("exec ")).count(),
        2,
        "expected both unrestricted teardown steps to run"
    );

    let helper_log = harness.helper_log();
    assert!(helper_log.contains(&format!("rm -f {}", harness.pid_file_path().display())));
    assert!(helper_log.contains("iptables -t nat -F OUTPUT"));
    assert!(helper_log.contains("iptables -F OUTPUT"));
    assert!(helper_log.contains("iptables -P OUTPUT ACCEPT"));
}
