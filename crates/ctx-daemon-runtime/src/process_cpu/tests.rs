use super::*;

#[test]
fn native_units_are_checked_and_truncated_to_microseconds() {
    assert_eq!(cpu_units_to_us(25, 1_000_000, 100), Ok(250_000));
    assert_eq!(cpu_units_to_us(129, 1, 10), Ok(12)); // Windows 100 ns units.
    assert_eq!(cpu_units_to_us(24_000, 125, 3_000), Ok(1_000)); // Mach 125/3 ns ticks.
    assert_eq!(
        cpu_units_to_us(u64::MAX, 1_000_000, 1_000_000),
        Ok(u64::MAX)
    );
    for (value, numerator, denominator) in [(1, 0, 1), (1, 1, 0), (u64::MAX, 2, 1)] {
        assert_eq!(
            cpu_units_to_us(value, numerator, denominator),
            Err(ProcessCpuUnavailable::Unavailable)
        );
    }
}

#[test]
fn identity_is_opaque_and_pid_zero_is_not_a_process() {
    let identity = ProcessCreationIdentity {
        pid: 4321,
        started: 987654,
        boot_id: None,
    };
    assert_eq!(format!("{identity:?}"), "ProcessCreationIdentity { .. }");
    assert_eq!(
        observe_process_cpu(0),
        Err(ProcessCpuUnavailable::NotRunning)
    );
}

#[test]
fn boot_id_parsing_rejects_missing_invalid_and_oversized_values() {
    let expected = uuid::Uuid::from_bytes([0x12; 16]);
    assert_eq!(
        parse_boot_id(b"12121212-1212-1212-1212-121212121212\n"),
        Some(expected)
    );
    for bytes in [
        b"".as_slice(),
        b"not-a-boot-id",
        b"00000000-0000-0000-0000-000000000000",
        &[0xff],
        &[b' '; 65],
    ] {
        assert_eq!(parse_boot_id(bytes), None);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn private_token_binds_pid_native_birth_platform_and_boot() {
    let boot_id = cfg!(any(target_os = "linux", target_os = "macos"))
        .then(|| uuid::Uuid::from_bytes([0x12; 16]));
    let identity = ProcessCreationIdentity {
        pid: 321,
        started: 9_007_199_254_740_993,
        boot_id,
    };
    let token = serde_json::json!({
        "schema_version": 1,
        "platform": std::env::consts::OS,
        "pid": 321,
        "started": 9_007_199_254_740_993_u64,
        "boot_id": if boot_id.is_some() { Some("12121212-1212-1212-1212-121212121212") } else { None },
    });
    assert_eq!(identity.private_json_token(), Some(token.clone()));
    assert!(identity.matches_private_json_token(&token));
    let from_disk = serde_json::from_slice(&serde_json::to_vec(&token).unwrap()).unwrap();
    assert!(identity.matches_private_json_token(&from_disk));
    for (key, value) in [
        ("schema_version", serde_json::json!(2)),
        ("platform", serde_json::json!("foreign")),
        ("pid", serde_json::json!(322)),
        ("started", serde_json::json!(9_007_199_254_740_994_u64)),
        (
            "boot_id",
            serde_json::json!("34343434-3434-3434-3434-343434343434"),
        ),
    ] {
        let mut different = token.clone();
        different[key] = value;
        assert!(!identity.matches_private_json_token(&different), "{key}");
    }
    for invalid in [
        serde_json::Value::Null,
        serde_json::json!({}),
        serde_json::json!("canary"),
    ] {
        assert!(!identity.matches_private_json_token(&invalid));
    }
    let mut extra = token.clone();
    extra["unrecognized"] = serde_json::json!("canary");
    assert!(!identity.matches_private_json_token(&extra));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let unbound = ProcessCreationIdentity {
            boot_id: None,
            ..identity
        };
        assert_eq!(unbound.private_json_token(), None);
        assert!(!unbound.matches_private_json_token(&token));
        let rebooted = ProcessCreationIdentity {
            boot_id: Some(uuid::Uuid::from_bytes([0x34; 16])),
            ..identity
        };
        assert!(!rebooted.matches_private_json_token(&token));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn daemon_lock_captures_self_birth_and_quiescence_cannot_authenticate_a_stale_birth() {
    let temp = tempfile::tempdir().unwrap();
    let lock = crate::DaemonLock::acquire(temp.path())
        .unwrap()
        .expect("daemon owner");
    let path = crate::daemon_lock_path(temp.path());
    let mut payload = crate::read_pid_lock_json(&path).unwrap();
    let first = observe_process_cpu(std::process::id()).unwrap();
    let second = observe_process_cpu(std::process::id()).unwrap();
    assert!(first
        .identity
        .matches_private_json_token(&payload["process_creation_token"]));
    assert!(second
        .identity
        .matches_private_json_token(&payload["process_creation_token"]));
    assert_eq!(crate::observe_pid_advisory_guard(&path), Some(true));
    assert!(crate::pid_lock_payload(serde_json::json!({}))
        .get("process_creation_token")
        .is_none());
    drop(lock);

    // Retained unreleased metadata for this PID, but from a different birth.
    let old_identity = ProcessCreationIdentity {
        started: first.identity.started.checked_add(1).unwrap(),
        ..first.identity
    };
    payload["process_creation_token"] = old_identity.private_json_token().unwrap();
    std::fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();
    let _quiescence = crate::DaemonQuiescenceGuard::acquire(temp.path())
        .unwrap()
        .unwrap();
    assert_eq!(crate::observe_pid_advisory_guard(&path), Some(true));
    assert_eq!(payload["released"], false);
    for _ in 0..2 {
        let current = observe_process_cpu(std::process::id()).unwrap();
        assert!(!current
            .identity
            .matches_private_json_token(&payload["process_creation_token"]));
    }
    payload
        .as_object_mut()
        .unwrap()
        .remove("process_creation_token");
    assert!(!first
        .identity
        .matches_private_json_token(&payload["process_creation_token"]));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unix_failures_are_closed_and_distinguish_permissions_and_exit() {
    for (code, expected) in [
        (libc::EACCES, ProcessCpuUnavailable::PermissionDenied),
        (libc::EPERM, ProcessCpuUnavailable::PermissionDenied),
        (libc::ENOENT, ProcessCpuUnavailable::NotRunning),
        (libc::ESRCH, ProcessCpuUnavailable::NotRunning),
        (libc::ENOSYS, ProcessCpuUnavailable::Unsupported),
        (libc::EIO, ProcessCpuUnavailable::Unavailable),
    ] {
        assert_eq!(
            unix_error(std::io::Error::from_raw_os_error(code)),
            expected
        );
    }
    assert_eq!(
        unix_error(std::io::Error::other("private-error-canary")),
        ProcessCpuUnavailable::Unavailable
    );
}

#[cfg(windows)]
#[test]
fn windows_failures_are_closed_and_distinguish_permissions_and_exit() {
    use windows_sys::Win32::Foundation::*;
    for (code, expected) in [
        (ERROR_ACCESS_DENIED, ProcessCpuUnavailable::PermissionDenied),
        (ERROR_INVALID_PARAMETER, ProcessCpuUnavailable::NotRunning),
        (ERROR_NOT_SUPPORTED, ProcessCpuUnavailable::Unsupported),
        (
            ERROR_CALL_NOT_IMPLEMENTED,
            ProcessCpuUnavailable::Unsupported,
        ),
        (ERROR_INVALID_HANDLE, ProcessCpuUnavailable::Unavailable),
    ] {
        assert_eq!(windows_error(code), expected);
    }
}

#[cfg(target_os = "linux")]
fn stat(user: &str, system: &str, start: &str) -> Vec<u8> {
    // Authored fields 1..22, with nonzero child CPU to detect wrong offsets.
    // The command deliberately includes ')', '(' and whitespace.
    format!("321 (name ) (with\nspaces) R 1 2 3 4 5 6 7 8 9 10 {user} {system} 900 800 20 0 1 0 {start}\n").into_bytes()
}

#[cfg(target_os = "linux")]
#[test]
fn linux_fixture_arithmetic_and_reuse_reset_controls() {
    let before = parse_linux_stat(321, &stat("100", "20", "1234"), 100).unwrap();
    let after = parse_linux_stat(321, &stat("120", "25", "1234"), 100).unwrap();
    assert_eq!(
        (before.user_cpu_us, before.system_cpu_us),
        (1_000_000, 200_000)
    );
    assert_eq!(
        (after.user_cpu_us, after.system_cpu_us),
        (1_200_000, 250_000)
    );
    assert_eq!(before.identity, after.identity);
    let delta = after.user_cpu_us.checked_sub(before.user_cpu_us).unwrap()
        + after
            .system_cpu_us
            .checked_sub(before.system_cpu_us)
            .unwrap();
    assert_eq!(delta, 250_000);
    assert_eq!(100.0 * delta as f64 / 2_500_000.0, 10.0);

    let replacement = parse_linux_stat(321, &stat("120", "25", "1235"), 100).unwrap();
    assert_ne!(before.identity, replacement.identity);
    let different_pid = parse_linux_stat(
        322,
        &String::from_utf8(stat("100", "20", "1234"))
            .unwrap()
            .replacen("321", "322", 1)
            .into_bytes(),
        100,
    )
    .unwrap();
    assert_ne!(before.identity, different_pid.identity);
    let reset = parse_linux_stat(321, &stat("99", "21", "1234"), 100).unwrap();
    assert_eq!(before.identity, reset.identity);
    assert_eq!(reset.user_cpu_us.checked_sub(before.user_cpu_us), None);
    let system_reset = parse_linux_stat(321, &stat("110", "19", "1234"), 100).unwrap();
    assert_eq!(
        system_reset.system_cpu_us.checked_sub(before.system_cpu_us),
        None
    );
    let zero = parse_linux_stat(321, &stat("0", "0", "1234"), 100).unwrap();
    assert_eq!((zero.user_cpu_us, zero.system_cpu_us), (0, 0));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_stat_rejects_bad_fields_but_allows_arbitrary_command_bytes() {
    use ProcessCpuUnavailable::{NotRunning, Unavailable};
    for bytes in [
        Vec::new(),
        b"321 no-parentheses R 0 0".to_vec(),
        b"321 (truncated) R 0 0".to_vec(),
        stat("-1", "20", "1234"),
        stat("bad", "20", "1234"),
        stat("100", "18446744073709551616", "1234"),
        stat("100", "20", "not-an-identity"),
    ] {
        assert_eq!(parse_linux_stat(321, &bytes, 100), Err(Unavailable));
    }
    assert_eq!(
        parse_linux_stat(320, &stat("100", "20", "1234"), 100),
        Err(Unavailable)
    );
    assert_eq!(
        parse_linux_stat(321, &stat("100", "20", "1234"), 0),
        Err(Unavailable)
    );
    let mut arbitrary_command = stat("100", "20", "1234");
    arbitrary_command[5] = 0xff;
    assert!(parse_linux_stat(321, &arbitrary_command, 100).is_ok());
    for state in ["Z", "X", "x"] {
        let exited = String::from_utf8(stat("100", "20", "1234"))
            .unwrap()
            .replace(") R ", &format!(") {state} "));
        assert_eq!(
            parse_linux_stat(321, exited.as_bytes(), 100),
            Err(NotRunning)
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn nonexistent_process_is_not_running() {
    assert_eq!(
        observe_process_cpu(u32::MAX),
        Err(ProcessCpuUnavailable::NotRunning)
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn real_self_observation_has_stable_identity_and_positive_cpu_delta() {
    let before = observe_process_cpu(std::process::id()).expect("self accounting");
    // Enough actual work to cross accounting ticks; bound even a broken clock
    // conversion. This runs only when the owning test target is explicitly run.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut value = 1_u64;
    loop {
        for _ in 0..100_000 {
            value = std::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        let after = observe_process_cpu(std::process::id()).expect("self accounting after work");
        assert_eq!(before.identity, after.identity);
        let user = after
            .user_cpu_us
            .checked_sub(before.user_cpu_us)
            .expect("user CPU monotonic");
        let system = after
            .system_cpu_us
            .checked_sub(before.system_cpu_us)
            .expect("system CPU monotonic");
        if user + system > 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "CPU accounting never advanced"
        );
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
#[test]
fn other_platforms_report_unsupported() {
    assert_eq!(
        observe_process_cpu(std::process::id()),
        Err(ProcessCpuUnavailable::Unsupported)
    );
}
