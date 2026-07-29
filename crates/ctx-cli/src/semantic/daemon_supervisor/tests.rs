use super::*;

#[cfg(target_os = "linux")]
#[test]
fn systemd_unit_is_persistent_and_restart_on_failure() {
    let unit = linux_systemd_unit(
        Path::new("/home/user/.local/bin/ctx"),
        Path::new("/home/user/.local/share/ctx"),
    );
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("WantedBy=default.target"));
    assert!(unit.contains("ExecStart=/usr/bin/env -i "));
    assert!(!unit.contains("CTX_RELEASE_"));
    assert!(!unit.contains("idle-exit-seconds"));
    assert!(!unit.contains("loop-interval-seconds"));
}

#[test]
fn systemd_registration_requires_a_nonzero_live_main_pid() {
    assert_eq!(systemd_main_pid(b"4242\n").unwrap(), 4242);
    assert!(systemd_main_pid(b"0\n").is_err());
    assert!(systemd_main_pid(b"\n").is_err());
}

#[test]
fn launch_agent_plist_is_persistent_sanitized_and_gui_registration_is_identity_bearing() {
    let plist = launch_agent_plist(
        Path::new("/Users/test/Library/Application Support/ctx/ctx"),
        Path::new("/Users/test/Library/Application Support/ctx/data"),
    );
    assert!(plist.contains("<key>Label</key><string>rs.ctx.daemon</string>"));
    assert!(plist.contains("<key>RunAtLoad</key><true/>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("<string>/usr/bin/env</string><string>-i</string>"));
    assert!(!plist.contains("CTX_RELEASE_"));
    assert!(!plist.contains("idle-exit-seconds"));
    assert_eq!(
        launchctl_print_pid("state = running\n\tpid = 73\n"),
        Some(73)
    );
    assert_eq!(launchctl_print_pid("state = waiting\n"), None);
}

#[test]
fn windows_task_contract_is_current_user_restartable_and_spawns_with_a_clear_environment() {
    let script = windows_sanitized_daemon_script(
        Path::new(r"C:\Program Files\ctx\ctx.exe"),
        Path::new(r"C:\Users\test\AppData\Local\ctx"),
    );
    assert!(script.contains("EnvironmentVariables.Clear()"));
    assert!(script.contains("UseShellExecute=$false"));
    assert!(!script.contains("CTX_RELEASE_"));
    assert!(!script.contains("idle-exit-seconds"));

    let xml = windows_task_xml(
        Path::new(r"C:\Program Files\ctx\ctx.exe"),
        Path::new(r"C:\Users\test\AppData\Local\ctx"),
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    );
    assert!(windows_task_registration_matches(
        &xml,
        Path::new(r"C:\Program Files\ctx\ctx.exe"),
        Path::new(r"C:\Users\test\AppData\Local\ctx"),
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    ));
    assert!(xml.contains("<LogonTrigger>"));
    assert!(xml.contains("<UserId>S-1-5-21-1000</UserId>"));
    assert!(xml.contains("<RestartOnFailure>"));
    assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    assert!(!windows_task_registration_matches(
        "<Task><LogonType>InteractiveToken</LogonType></Task>",
        Path::new(r"C:\Program Files\ctx\ctx.exe"),
        Path::new(r"C:\Users\test\AppData\Local\ctx"),
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    ));
    assert_eq!(
        windows_task_name("S-1-5-21-1000"),
        r"\ctx-daemon-S-1-5-21-1000"
    );
    let state_script = windows_task_state_script(r"\ctx-daemon-S-1-5-21-1000");
    assert!(state_script.contains("-TaskPath '\\'"));
    assert!(state_script.contains("-TaskName 'ctx-daemon-S-1-5-21-1000'"));
    assert_eq!(parse_windows_task_state(b"4\r\n"), Some(4));
    assert_ne!(parse_windows_task_state(b"3\r\n"), Some(4));
}

#[test]
fn windows_task_status_decoder_handles_task_scheduler_utf16_xml() {
    let source =
        r#"<Task><RegistrationInfo><URI>\ctx-daemon-S-1-5-21-1000</URI></RegistrationInfo></Task>"#;
    let mut encoded = vec![0xff, 0xfe];
    encoded.extend(source.encode_utf16().flat_map(u16::to_le_bytes));
    assert_eq!(decode_supervisor_text(&encoded), source);
}

#[test]
fn windows_command_line_quoting_preserves_spaces_quotes_and_trailing_separators() {
    assert_eq!(windows_command_line_quote("plain"), "plain");
    assert_eq!(windows_command_line_quote("two words"), "\"two words\"");
    assert_eq!(windows_command_line_quote(r#"C:\a b\"#), r#""C:\a b\\""#,);
}

#[test]
fn freebsd_limitation_names_the_missing_product_authority_without_claiming_support() {
    let limitation = freebsd_supervisor_authority_blocker();
    assert!(limitation.contains("no standard current-user service manager"));
    assert!(limitation.contains("will not mutate the user's crontab"));
    assert!(limitation.contains("typed CLI self-healing"));
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn supervisor_live_ownership_requires_exact_manager_pid_and_executable() {
    let temp = tempfile::tempdir().unwrap();
    let _lock = super::super::paths_status::DaemonLock::acquire(temp.path())
        .unwrap()
        .expect("daemon lock");
    let executable = env::current_exe().unwrap();
    assert_eq!(
        verify_daemon_owner_identity(temp.path(), &executable, Some(std::process::id())).unwrap(),
        std::process::id()
    );
    assert!(verify_daemon_owner_identity(
        temp.path(),
        &executable,
        Some(std::process::id().saturating_add(1)),
    )
    .is_err());
    assert!(verify_daemon_owner_identity(
        temp.path(),
        &temp.path().join("not-the-owner"),
        Some(std::process::id()),
    )
    .is_err());
}

#[test]
fn fallback_disable_status_is_retry_safe_without_claiming_registration() {
    let temp = tempfile::tempdir().unwrap();
    write_supervisor_receipt(
        temp.path(),
        "cli_self_heal",
        "fallback",
        false,
        false,
        None,
        Some("test limitation"),
        None,
    )
    .unwrap();
    disable_daemon_supervisor(temp.path()).unwrap();
    disable_daemon_supervisor(temp.path()).unwrap();
    let status = daemon_supervisor_report(temp.path());
    assert_eq!(status["status"], "disabled");
    assert_eq!(status["registration_verified"], false);
    assert_eq!(status["live_owner_verified"], false);
    assert_eq!(status["autostart_supported"], false);
    assert_eq!(status["restart_supported"], false);
}
