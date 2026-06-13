use super::*;

#[test]
fn missing_machine_error_detection_matches_expected_shapes() {
    assert!(looks_like_missing_machine_error(
        "error: no machine with this name exists"
    ));
    assert!(looks_like_missing_machine_error(
        "Error: machine ctx not found"
    ));
    assert!(!looks_like_missing_machine_error(
        "error: machine already running"
    ));
}

#[test]
fn recoverable_machine_start_error_detection_matches_expected_shapes() {
    assert!(looks_like_recoverable_machine_start_error(
        "error: machine is already starting"
    ));
    assert!(looks_like_recoverable_machine_start_error(
        "Error: unable to start \"ctx\": already running\nStarting machine \"ctx\""
    ));
    assert!(looks_like_recoverable_machine_start_error(
        "error: resource busy while acquiring lock"
    ));
    assert!(looks_like_recoverable_machine_start_error(
        "error: operation timed out while waiting for vm startup"
    ));
    assert!(looks_like_recoverable_machine_start_error(
            "time=\"2026-03-05T00:23:28-06:00\" level=warning msg=\"detected port conflict on machine ssh port [49401], reassigning\"\nError: vfkit exited unexpectedly with exit code 1"
        ));
    assert!(looks_like_recoverable_machine_start_error(
        "Error: unable to connect to \"gvproxy\" socket at \"/tmp/sandbox-cli.sock\""
    ));
    assert!(!looks_like_recoverable_machine_start_error(
        "error: unknown vm provider configuration"
    ));
}

#[test]
fn running_but_unreachable_machine_start_error_detection_matches_expected_shapes() {
    assert!(looks_like_running_but_unreachable_machine_start_error(
        "Error: unable to start \"ctx\": already running"
    ));
    assert!(looks_like_running_but_unreachable_machine_start_error(
        "Error: unable to connect to \"gvproxy\" socket at \"/tmp/sandbox-cli.sock\""
    ));
    assert!(!looks_like_running_but_unreachable_machine_start_error(
        "error: resource busy while acquiring lock"
    ));
    assert!(!looks_like_running_but_unreachable_machine_start_error(
        "error: operation timed out while waiting for vm startup"
    ));
}
