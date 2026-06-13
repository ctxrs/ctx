use super::*;

#[cfg(unix)]
#[path = "helper_processes/fixtures.rs"]
mod fixtures;

#[cfg(unix)]
use fixtures::HelperProcessFixture;

#[cfg(unix)]
#[test]
fn kill_ctx_managed_sandbox_helper_processes_reports_only_successful_kills() {
    let _serial = env_var_test_lock().blocking_lock();
    let fixture = HelperProcessFixture::new();
    let gvproxy = fixture.gvproxy_command();
    let vfkit = fixture.vfkit_command();
    let escaped_gvproxy = literal_pkill_pattern(&gvproxy);
    let escaped_vfkit = literal_pkill_pattern(&vfkit);

    fixture.write_fake_ps(
        &format!(" 6622 {gvproxy}\n12484 {vfkit}\n"),
        &format!("12484 {vfkit}\n"),
    );
    fixture.write_fake_pkill(&format!(
        "printf '%s\\n' \"$*\" >> \"{log}\"\nif [ \"$4\" = '{vfkit}' ]; then\n  exit 1\nfi\nexit 0\n",
        log = fixture.pkill_log_path().display(),
        vfkit = escaped_vfkit,
    ));
    let _guard = fixture.install_path_guard();

    let outcome =
        kill_ctx_managed_sandbox_helper_processes(fixture.root_path(), fixture.machine_name());
    assert_eq!(outcome.killed, vec![6622]);
    assert_eq!(outcome.failed, vec![12484]);
    assert!(outcome.skipped.is_empty());

    let kill_log = std::fs::read_to_string(fixture.pkill_log_path()).expect("read pkill log");
    assert!(kill_log.contains(&format!("-9 -f -x {escaped_gvproxy}")));
    assert!(kill_log.contains(&format!("-9 -f -x {escaped_vfkit}")));
}

#[cfg(unix)]
#[test]
fn kill_ctx_managed_sandbox_helper_processes_escapes_regex_metacharacters_for_pkill() {
    let _serial = env_var_test_lock().blocking_lock();
    let fixture = HelperProcessFixture::new();
    let gvproxy = fixture.gvproxy_command();

    fixture.write_fake_ps(&format!(" 6622 {gvproxy}\n"), "");
    fixture.write_fake_pkill(&format!(
        "printf '%s\\n' \"$4\" >> \"{log}\"\nexit 0\n",
        log = fixture.pkill_log_path().display(),
    ));
    let _guard = fixture.install_path_guard();

    let outcome =
        kill_ctx_managed_sandbox_helper_processes(fixture.root_path(), fixture.machine_name());
    assert_eq!(outcome.killed, vec![6622]);
    assert!(outcome.failed.is_empty());
    assert!(outcome.skipped.is_empty());

    let pkill_pattern = std::fs::read_to_string(fixture.pkill_log_path()).expect("read pkill log");
    assert_eq!(pkill_pattern.trim(), literal_pkill_pattern(&gvproxy));
}

#[cfg(unix)]
#[test]
fn kill_ctx_managed_sandbox_helper_processes_skips_reused_pid_after_command_scoped_kill() {
    let _serial = env_var_test_lock().blocking_lock();
    let fixture = HelperProcessFixture::new();
    let gvproxy = fixture.gvproxy_command();

    fixture.write_fake_ps(
        &format!(" 6622 {gvproxy}\n"),
        " 6622 /usr/bin/python3 /tmp/not-ctx-helper.py\n",
    );
    fixture.write_fake_pkill(&format!(
        "printf '%s\\n' \"$*\" >> \"{log}\"\nexit 1\n",
        log = fixture.pkill_log_path().display(),
    ));
    let _guard = fixture.install_path_guard();

    let outcome =
        kill_ctx_managed_sandbox_helper_processes(fixture.root_path(), fixture.machine_name());
    assert!(outcome.killed.is_empty());
    assert!(outcome.failed.is_empty());
    assert_eq!(outcome.skipped, vec![6622]);

    let pkill_log = std::fs::read_to_string(fixture.pkill_log_path()).expect("read pkill log");
    assert!(pkill_log.contains(&format!("-9 -f -x {}", literal_pkill_pattern(&gvproxy))));
}
