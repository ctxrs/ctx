use super::*;
use std::{fs, io::Seek, process::Stdio};

use super::super::{process_is_running, tempdir};

const PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(5);
const MODE: &str = "CTX_TEST_FINITE_CAPTURE_MODE";
const ROOT: &str = "CTX_TEST_FINITE_CAPTURE_ROOT";

fn fixture_command(root: &Path, mode: &str, exit_code: i32) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .args([
            "--exact",
            "native::capture::tests::output_fixture",
            "--nocapture",
        ])
        .env(MODE, mode)
        .env(ROOT, root)
        .env("CTX_TEST_FINITE_CAPTURE_EXIT", exit_code.to_string())
        .current_dir(root)
        .stdin(Stdio::null());
    #[cfg(windows)]
    if let Some(value) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", value);
    }
    command
}

fn wait_for_marker(root: &Path, name: &str) {
    let deadline = Instant::now() + FIXTURE_TIMEOUT;
    while !root.join(name).is_file() {
        assert!(
            Instant::now() < deadline,
            "inert child did not write {name}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn write_payload(mut writer: impl Write, byte: u8, tail: &[u8]) {
    let chunk = [byte; 4096];
    for _ in 0..PAYLOAD_BYTES / chunk.len() {
        writer.write_all(&chunk).unwrap();
    }
    writer.write_all(tail).unwrap();
    writer.flush().unwrap();
}

#[test]
fn output_fixture() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
    let code: i32 = std::env::var("CTX_TEST_FINITE_CAPTURE_EXIT")
        .unwrap()
        .parse()
        .unwrap();
    assert!(matches!(code, 0 | 7));
    if mode == "stall" {
        io::stdout().write_all(b"timeout stdout prefix\n").unwrap();
        io::stdout().flush().unwrap();
        io::stderr().write_all(b"timeout stderr prefix\n").unwrap();
        io::stderr().flush().unwrap();
    }
    fs::write(root.join("ready"), b"ready").unwrap();
    match mode.as_str() {
        "stdout-first" => {
            write_payload(io::stdout().lock(), b'O', b"\nstdout-tail\n");
            write_payload(io::stderr().lock(), b'E', b"\nstderr-tail\n");
        }
        "stderr-first" => {
            write_payload(io::stderr().lock(), b'E', b"\nstderr-tail\n");
            write_payload(io::stdout().lock(), b'O', b"\nstdout-tail\n");
        }
        "stall" | "hold" => loop {
            thread::sleep(Duration::from_secs(60));
        },
        _ => panic!("unknown inert capture fixture mode"),
    }
    fs::write(root.join("finished"), b"finished").unwrap();
    std::process::exit(code);
}

fn assert_payload(bytes: &[u8], byte: u8, tail: &[u8]) {
    assert!(bytes.ends_with(tail));
    let end = bytes.len() - tail.len();
    assert!(end >= PAYLOAD_BYTES);
    assert!(bytes[end - PAYLOAD_BYTES..end]
        .iter()
        .all(|value| *value == byte));
    // Only the libtest startup prelude may precede the exact fixture payload.
    assert!(end - PAYLOAD_BYTES < 512);
}

fn assert_large_output(output: &Output, code: i32) {
    assert_eq!(output.status.code(), Some(code));
    assert_payload(&output.stdout, b'O', b"\nstdout-tail\n");
    assert_payload(&output.stderr, b'E', b"\nstderr-tail\n");
}

#[test]
fn captures_both_large_streams_from_spawn_with_original_exit_status() {
    let mut children = Vec::new();
    for (mode, code) in [("stdout-first", 0), ("stderr-first", 7)] {
        let root = tempdir();
        let child =
            CapturedChild::spawn(&mut fixture_command(root.path(), mode, code), root.path())
                .unwrap();
        // Tuple drop order must reap the child before removing its temporary root.
        children.push((child, root, code));
    }
    // Neither child has been waited/drained: both must finish despite large output.
    for (_, root, _) in &children {
        wait_for_marker(root.path(), "finished");
    }
    for (child, _root, code) in children.into_iter().rev() {
        assert_large_output(&child.output(FIXTURE_TIMEOUT).unwrap(), code);
    }
}

#[test]
fn timeout_cancel_drop_and_unwind_reap_the_direct_child() {
    for action in ["timeout", "cancel", "drop", "unwind"] {
        let root = tempdir();
        let mut child =
            CapturedChild::spawn(&mut fixture_command(root.path(), "stall", 0), root.path())
                .unwrap();
        let pid = child.id();
        wait_for_marker(root.path(), "ready");
        match action {
            "timeout" => {
                let started = Instant::now();
                let error = child.output(Duration::from_millis(100)).unwrap_err();
                assert!(error.contains("exceeded 100ms"), "{error}");
                assert!(error.contains("timeout stdout prefix"), "{error}");
                assert!(error.contains("timeout stderr prefix"), "{error}");
                assert!(started.elapsed() < FIXTURE_TIMEOUT);
            }
            "cancel" => child.terminate().unwrap(),
            "drop" => drop(child),
            "unwind" => {
                assert!(std::panic::catch_unwind(move || {
                    let _owned = child;
                    panic!("inert owner unwind");
                })
                .is_err());
            }
            _ => unreachable!(),
        }
        assert!(
            !process_is_running(pid),
            "{action} left its direct child alive"
        );
    }
}

#[test]
fn oversized_capture_fails_and_reaps_instead_of_returning_a_prefix() {
    let root = tempdir();
    let child =
        CapturedChild::spawn(&mut fixture_command(root.path(), "stall", 0), root.path()).unwrap();
    let pid = child.id();
    wait_for_marker(root.path(), "ready");
    child
        .stdout
        .as_file()
        .set_len(MAX_CAPTURE_BYTES + 1)
        .unwrap();
    let error = child.output(FIXTURE_TIMEOUT).unwrap_err();
    assert!(error.contains("stdout capture: capture exceeds"), "{error}");
    assert!(!process_is_running(pid));
}

#[test]
fn snapshots_have_independent_offsets_finite_lengths_and_read_errors() {
    let root = tempdir();
    let file = NamedTempFile::new_in(root.path()).unwrap();
    let mut writer = file.reopen().unwrap();
    writer.write_all(b"abcdef").unwrap();
    assert_eq!(snapshot(&file).unwrap(), b"abcdef");
    assert_eq!(writer.stream_position().unwrap(), 6);
    assert_eq!(read_snapshot(file.reopen().unwrap(), 3).unwrap(), b"abc");
    assert_eq!(
        read_snapshot(file.reopen().unwrap(), 7).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    let write_only = File::create(root.path().join("write-only")).unwrap();
    assert!(read_snapshot(write_only, 1).is_err());
}

// Only the legacy-pipe and retained-writer controls need a raw direct child.
struct FixtureChild(Option<Child>);

impl Drop for FixtureChild {
    fn drop(&mut self) {
        if let Err(error) = terminate_and_reap_test_child(&mut self.0, "inert capture control") {
            if thread::panicking() {
                eprintln!("{error}");
            } else {
                panic!("{error}");
            }
        }
    }
}

#[test]
fn a_live_output_handle_does_not_hold_capture_open() {
    let root = tempdir();
    let child = CapturedChild::spawn(
        &mut fixture_command(root.path(), "stdout-first", 0),
        root.path(),
    )
    .unwrap();
    wait_for_marker(root.path(), "finished");
    let holder_root = tempdir();
    let mut command = fixture_command(holder_root.path(), "hold", 0);
    let mut stdout = child.stdout.reopen().unwrap();
    let mut stderr = child.stderr.reopen().unwrap();
    stdout.seek(io::SeekFrom::End(0)).unwrap();
    stderr.seek(io::SeekFrom::End(0)).unwrap();
    command.stdout(stdout).stderr(stderr);
    // Model a surviving inherited writer, but retain direct ownership for cleanup.
    let mut holder = FixtureChild(Some(command.spawn().unwrap()));
    wait_for_marker(holder_root.path(), "ready");
    let expected_stdout = fs::read(child.stdout.path()).unwrap();
    let expected_stderr = fs::read(child.stderr.path()).unwrap();
    let started = Instant::now();
    let output = child.output(FIXTURE_TIMEOUT).unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, expected_stdout);
    assert_eq!(output.stderr, expected_stderr);
    assert!(started.elapsed() < FIXTURE_TIMEOUT);
    assert!(holder.0.as_mut().unwrap().try_wait().unwrap().is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn legacy_wait_before_drain_blocks_on_either_full_pipe() {
    use std::os::fd::AsRawFd;

    for mode in ["stdout-first", "stderr-first"] {
        let root = tempdir();
        let mut command = fixture_command(root.path(), mode, 0);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut legacy = FixtureChild(Some(command.spawn().unwrap()));
        let child = legacy.0.as_mut().unwrap();
        for fd in [
            child.stdout.as_ref().unwrap().as_raw_fd(),
            child.stderr.as_ref().unwrap().as_raw_fd(),
        ] {
            // SAFETY: both owned pipe descriptors remain live throughout this query.
            let capacity = unsafe { libc::fcntl(fd, libc::F_GETPIPE_SZ) };
            assert!(capacity > 0 && (capacity as usize) < PAYLOAD_BYTES);
        }
        wait_for_marker(root.path(), "ready");
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            assert!(
                child.try_wait().unwrap().is_none(),
                "legacy control unexpectedly exited"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!root.path().join("finished").exists());
        let pid = child.id();
        drop(legacy); // Kill/reap without ever waiting for pipe EOF.
        assert!(!process_is_running(pid));
    }
}
