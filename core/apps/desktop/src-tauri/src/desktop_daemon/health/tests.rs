use super::*;

fn expected_identity(
    exact_version: &str,
    build_id: &str,
    compatibility_token: &str,
) -> DesktopBuildIdentity {
    DesktopBuildIdentity {
        schema_version: 1,
        exact_version: exact_version.to_string(),
        build_id: build_id.to_string(),
        compatibility_token: compatibility_token.to_string(),
        _legacy_channel: None,
        _provenance_channel: Some("stable".to_string()),
    }
}

#[test]
fn local_daemon_health_match_requires_expected_data_root_and_mode_specific_identity() {
    let expected_dir =
        std::env::temp_dir().join(format!("ctx-daemon-health-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&expected_dir).expect("create expected dir");
    let other_dir = expected_dir.join("other");
    std::fs::create_dir_all(&other_dir).expect("create other dir");

    let matching = DaemonHealthSummary {
        pid: 42,
        data_root: expected_dir.to_string_lossy().to_string(),
        compatibility: DaemonHealthCompatibility {
            desktop_exact_version: "1.2.3".to_string(),
            desktop_build_id: "build-a".to_string(),
            desktop_dev_instance_id: "dev-wt-a".to_string(),
            protocol_compatibility_token: "dev-wt-a".to_string(),
        },
    };
    assert!(local_daemon_health_matches_expected(
        &matching,
        &expected_dir,
        &expected_identity("1.2.3", "build-a", "dev-wt-a"),
    ));
    assert!(!local_daemon_health_matches_expected(
        &matching,
        &expected_dir,
        &expected_identity("9.9.9", "build-a", "dev-wt-a"),
    ));
    assert!(!local_daemon_health_matches_expected(
        &matching,
        &expected_dir,
        &expected_identity("1.2.3", "build-b", "dev-wt-a"),
    ));
    assert!(!local_daemon_health_matches_expected(
        &matching,
        &expected_dir,
        &expected_identity("1.2.3", "build-a", "dev-wt-b"),
    ));

    let wrong_root = DaemonHealthSummary {
        pid: 42,
        data_root: other_dir.to_string_lossy().to_string(),
        compatibility: DaemonHealthCompatibility {
            desktop_exact_version: "1.2.3".to_string(),
            desktop_build_id: "build-a".to_string(),
            desktop_dev_instance_id: "dev-wt-a".to_string(),
            protocol_compatibility_token: "dev-wt-a".to_string(),
        },
    };
    assert!(!local_daemon_health_matches_expected(
        &wrong_root,
        &expected_dir,
        &expected_identity("1.2.3", "build-a", "dev-wt-a"),
    ));

    std::fs::remove_dir_all(&expected_dir).ok();
}

#[test]
fn existing_local_daemon_match_errors_are_treated_as_absent() {
    let expected_dir = std::env::temp_dir().join(format!(
        "ctx-daemon-existing-match-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&expected_dir).expect("create expected dir");

    assert!(!existing_local_daemon_matches_or_absent(
        "not-a-url",
        &expected_dir,
        &expected_identity("1.2.3", "build-a", "dev-wt-a"),
    ));

    std::fs::remove_dir_all(&expected_dir).ok();
}

#[test]
fn daemon_compatibility_classification_distinguishes_exact_compatible_and_incompatible() {
    let expected = expected_identity("1.2.3", "build-a", "token-a");
    let health = DaemonHealthSummary {
        pid: 42,
        data_root: "/tmp/ctx".to_string(),
        compatibility: DaemonHealthCompatibility {
            desktop_exact_version: "1.2.3".to_string(),
            desktop_build_id: "build-a".to_string(),
            desktop_dev_instance_id: "legacy-token".to_string(),
            protocol_compatibility_token: "token-a".to_string(),
        },
    };
    assert_eq!(
        classify_daemon_compatibility(&health, &expected),
        DaemonCompatibilityState::Exact
    );

    let compatible = DaemonHealthSummary {
        compatibility: DaemonHealthCompatibility {
            desktop_exact_version: "1.2.4".to_string(),
            desktop_build_id: "build-b".to_string(),
            ..health.compatibility.clone()
        },
        ..health.clone()
    };
    assert_eq!(
        classify_daemon_compatibility(&compatible, &expected),
        DaemonCompatibilityState::CompatibleMismatch
    );

    let incompatible = DaemonHealthSummary {
        compatibility: DaemonHealthCompatibility {
            protocol_compatibility_token: "token-b".to_string(),
            ..compatible.compatibility.clone()
        },
        ..compatible.clone()
    };
    assert_eq!(
        classify_daemon_compatibility(&incompatible, &expected),
        DaemonCompatibilityState::IncompatibleMismatch
    );

    let missing_protocol_token = DaemonHealthSummary {
        compatibility: DaemonHealthCompatibility {
            protocol_compatibility_token: String::new(),
            desktop_dev_instance_id: "token-a".to_string(),
            ..compatible.compatibility
        },
        ..compatible
    };
    assert_eq!(
        classify_daemon_compatibility(&missing_protocol_token, &expected),
        DaemonCompatibilityState::IncompatibleMismatch
    );
}

#[test]
fn spawned_daemon_incompatibility_message_reports_expected_and_actual_values() {
    let expected_dir = std::env::temp_dir().join(format!(
        "ctx-daemon-spawn-incompatible-{}",
        uuid::Uuid::new_v4()
    ));
    let health = DaemonHealthSummary {
        pid: 4242,
        data_root: "/tmp/ctx-daemon-other".to_string(),
        compatibility: DaemonHealthCompatibility {
            desktop_exact_version: "0.1.1".to_string(),
            desktop_build_id: "build-other".to_string(),
            desktop_dev_instance_id: "dev-other".to_string(),
            protocol_compatibility_token: String::new(),
        },
    };

    let msg = spawned_local_daemon_incompatibility_message(
        "http://127.0.0.1:4123",
        &expected_dir,
        &expected_identity("0.2.20", "build-main", "dev-main"),
        &health,
    );

    assert!(msg.contains("expected_version=0.2.20"));
    assert!(msg.contains("daemon_version=0.1.1"));
    assert!(msg.contains("expected_build_id=build-main"));
    assert!(msg.contains("daemon_build_id=build-other"));
    assert!(msg.contains("expected_dev_instance_id=dev-main"));
    assert!(msg.contains("daemon_dev_instance_id=dev-other"));
    assert!(msg.contains("daemon_data_root=/tmp/ctx-daemon-other"));
    assert!(msg.contains("daemon_pid=4242"));
    assert!(msg.contains("url=http://127.0.0.1:4123"));
}

#[test]
fn daemon_health_reuses_cached_client_for_same_timeout() {
    reset_daemon_health_client_build_count();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let server = std::thread::spawn(move || {
        let body =
            "{\"pid\":1,\"data_root\":\"/tmp/test\",\"compatibility\":{\"desktop_exact_version\":\"1.0.0\",\"desktop_build_id\":\"build-a\",\"desktop_dev_instance_id\":\"dev\"}}";
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buf = [0_u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
        }
    });

    let base_url = format!("http://{}", addr);
    let timeout = Duration::from_millis(4321);
    for _ in 0..2 {
        let health =
            daemon_health_with_timeout(&base_url, timeout).expect("daemon health succeeds");
        assert_eq!(health.pid, 1);
    }

    server.join().expect("join test server");
    assert_eq!(daemon_health_client_build_count(), 1);
}

#[test]
fn daemon_health_with_auth_validates_a_protected_route() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_server = std::sync::Arc::clone(&observed);
    let server = std::thread::spawn(move || {
        let health_body =
            "{\"pid\":1,\"data_root\":\"/tmp/test\",\"compatibility\":{\"desktop_exact_version\":\"1.0.0\",\"desktop_build_id\":\"build-a\",\"desktop_dev_instance_id\":\"dev\"}}";
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buf = [0_u8; 2048];
            let size = std::io::Read::read(&mut stream, &mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..size]).to_string();
            let request_lower = request.to_ascii_lowercase();
            observed_server
                .lock()
                .expect("lock observed requests")
                .push(request.clone());
            let (status_line, body) = if request.starts_with("GET /api/health ") {
                ("HTTP/1.1 200 OK", health_body)
            } else if request.starts_with("GET /api/workspaces ")
                && request_lower.contains("authorization: bearer desktop-token")
            {
                ("HTTP/1.1 200 OK", "[]")
            } else {
                ("HTTP/1.1 401 Unauthorized", "{\"error\":\"unauthorized\"}")
            };
            let response = format!(
                "{status_line}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
        }
    });

    let base_url = format!("http://{}", addr);
    let health = daemon_health_with_auth(&base_url, Some("desktop-token"))
        .expect("daemon auth health succeeds");

    server.join().expect("join test server");
    let requests = observed.lock().expect("lock observed requests");
    assert_eq!(health.pid, 1);
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/health "));
    assert!(requests[1].starts_with("GET /api/workspaces "));
    assert!(requests[1]
        .to_ascii_lowercase()
        .contains("authorization: bearer desktop-token"));
}

#[test]
fn reclaim_predicate_requires_loopback_same_data_dir_and_pid() {
    let expected_dir =
        std::env::temp_dir().join(format!("ctx-daemon-reclaim-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&expected_dir).expect("create expected dir");
    let other_dir = expected_dir.join("other");
    std::fs::create_dir_all(&other_dir).expect("create other dir");

    let compatible_root = DaemonHealthSummary {
        pid: 100,
        data_root: expected_dir.to_string_lossy().to_string(),
        compatibility: DaemonHealthCompatibility::default(),
    };
    assert!(should_reclaim_incompatible_local_daemon(
        "http://127.0.0.1:4123",
        &compatible_root,
        &expected_dir,
    ));

    let non_loopback = DaemonHealthSummary {
        pid: 100,
        data_root: expected_dir.to_string_lossy().to_string(),
        compatibility: DaemonHealthCompatibility::default(),
    };
    assert!(!should_reclaim_incompatible_local_daemon(
        "http://192.168.1.30:4123",
        &non_loopback,
        &expected_dir,
    ));

    let wrong_root = DaemonHealthSummary {
        pid: 100,
        data_root: other_dir.to_string_lossy().to_string(),
        compatibility: DaemonHealthCompatibility::default(),
    };
    assert!(!should_reclaim_incompatible_local_daemon(
        "http://127.0.0.1:4123",
        &wrong_root,
        &expected_dir,
    ));

    let missing_pid = DaemonHealthSummary {
        pid: 0,
        data_root: expected_dir.to_string_lossy().to_string(),
        compatibility: DaemonHealthCompatibility::default(),
    };
    assert!(!should_reclaim_incompatible_local_daemon(
        "http://127.0.0.1:4123",
        &missing_pid,
        &expected_dir,
    ));

    std::fs::remove_dir_all(&expected_dir).ok();
}

#[test]
fn reclaim_complete_requires_pid_exit_and_no_same_pid_health() {
    let pid = 4242u32;
    let same_pid_health = DaemonHealthSummary {
        pid,
        data_root: "/tmp/ctx".to_string(),
        compatibility: DaemonHealthCompatibility::default(),
    };
    let other_pid_health = DaemonHealthSummary {
        pid: pid + 1,
        data_root: "/tmp/ctx".to_string(),
        compatibility: DaemonHealthCompatibility::default(),
    };

    assert!(!reclaim_complete(pid, true, None));
    assert!(!reclaim_complete(pid, true, Some(&same_pid_health)));
    assert!(!reclaim_complete(pid, false, Some(&same_pid_health)));
    assert!(reclaim_complete(pid, false, None));
    assert!(reclaim_complete(pid, false, Some(&other_pid_health)));
}

#[test]
fn health_reports_expected_pid_only_when_health_matches_pid() {
    let pid = 5151u32;
    let matching = DaemonHealthSummary {
        pid,
        data_root: "/tmp/ctx".to_string(),
        compatibility: DaemonHealthCompatibility::default(),
    };
    let other = DaemonHealthSummary {
        pid: pid + 1,
        data_root: "/tmp/ctx".to_string(),
        compatibility: DaemonHealthCompatibility::default(),
    };

    assert!(health_reports_expected_pid(pid, Some(&matching)));
    assert!(!health_reports_expected_pid(pid, Some(&other)));
    assert!(!health_reports_expected_pid(pid, None));
}

#[test]
fn reclaim_health_probe_timeout_respects_remaining_budget() {
    let max_probe = Duration::from_millis(250);
    assert_eq!(
        reclaim_health_probe_timeout(Duration::from_millis(900), max_probe),
        max_probe
    );
    assert_eq!(
        reclaim_health_probe_timeout(Duration::from_millis(40), max_probe),
        Duration::from_millis(40)
    );
    assert_eq!(
        reclaim_health_probe_timeout(Duration::ZERO, max_probe),
        Duration::from_millis(1)
    );
}
