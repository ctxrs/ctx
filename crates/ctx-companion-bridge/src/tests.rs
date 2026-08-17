#![cfg(unix)]

use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{BufRead as _, BufReader, Write as _},
    os::unix::{
        ffi::OsStringExt as _,
        fs::{symlink, PermissionsExt as _},
        io::{FromRawFd as _, OwnedFd},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use sha2::{Digest as _, Sha256};

use super::*;
use crate::{
    slot::PreparedPair,
    verifier::{
        embedded_authority_for_tests, embedded_state_schema_for_tests,
        embedded_target_matrix_for_tests, PairVerifier,
    },
};

const CORE_BYTES: &[u8] = b"#!/bin/sh\nexit 0\n";

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    launcher: PathBuf,
    companion: PathBuf,
}

impl Fixture {
    fn new(companion_bytes: &[u8]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("managed");
        let bin = root.join("bin");
        let libexec = root.join("libexec");
        let share = root.join("share").join("ctx");
        let share_root = root.join("share");
        for directory in [&root, &bin, &libexec, &share_root, &share] {
            fs::create_dir(directory).unwrap();
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let launcher = bin.join("ctx");
        let companion = libexec.join("ctx-pro");
        write_mode(&launcher, CORE_BYTES, 0o700);
        write_mode(&companion, companion_bytes, 0o700);
        Self {
            _temp: temp,
            root,
            launcher,
            companion,
        }
    }

    fn shared_path(&self, filename: &str) -> PathBuf {
        self.root.join("share").join("ctx").join(filename)
    }
}

fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

struct AcceptingVerifier;

impl PairVerifier for AcceptingVerifier {
    fn verify(&self, _pair: &PreparedPair) -> Result<(), BridgeError> {
        Ok(())
    }
}

struct RefusingVerifier;

impl PairVerifier for RefusingVerifier {
    fn verify(&self, _pair: &PreparedPair) -> Result<(), BridgeError> {
        Err(BridgeError::Verification("test refusal".to_owned()))
    }
}

fn launch_with(
    fixture: &Fixture,
    request: CompanionRequest,
    limits: LimitConfiguration,
    cancellation: &CancellationToken,
) -> Result<CompanionOutput, BridgeError> {
    CompanionBridge::new(BridgeLimits::new(limits).unwrap()).launch_captured_at(
        &fixture.launcher,
        request.capture(Vec::new()),
        cancellation,
        &AcceptingVerifier,
    )
}

fn launch_with_verified_channel(
    fixture: &Fixture,
    request: CompanionRequest,
    limits: LimitConfiguration,
    cancellation: &CancellationToken,
    channel: ReleaseChannel,
) -> Result<CompanionOutput, BridgeError> {
    CompanionBridge::new(BridgeLimits::new(limits).unwrap()).launch_captured_at_with_channel(
        &fixture.launcher,
        request.capture(Vec::new()),
        cancellation,
        &AcceptingVerifier,
        Some(channel),
    )
}

static MANAGED_CHANNEL_ENV_LOCK: Mutex<()> = Mutex::new(());

struct ManagedChannelEnvironmentRestore(Option<OsString>);

impl Drop for ManagedChannelEnvironmentRestore {
    fn drop(&mut self) {
        // This test-only guard restores the parent process environment after a
        // child has proved that `env_clear` plus the verified injection wins.
        unsafe {
            match self.0.take() {
                Some(value) => std::env::set_var("CTX_MANAGED_PAIR_CHANNEL", value),
                None => std::env::remove_var("CTX_MANAGED_PAIR_CHANNEL"),
            }
        }
    }
}

fn request(fixture: &Fixture) -> CompanionRequest {
    CompanionRequest::new(fixture.root.join("data"))
}

#[test]
fn verified_managed_channel_replaces_ambient_spoof_for_each_release_channel() {
    let _environment_guard = MANAGED_CHANNEL_ENV_LOCK.lock().unwrap();
    let restore = ManagedChannelEnvironmentRestore(std::env::var_os("CTX_MANAGED_PAIR_CHANNEL"));
    let fixture = Fixture::new(b"#!/bin/sh\nprintf '%s' \"$CTX_MANAGED_PAIR_CHANNEL\"\n");

    for (verified, ambient, expected) in [
        (ReleaseChannel::Stable, "staging", b"stable".as_slice()),
        (ReleaseChannel::Staging, "stable", b"staging".as_slice()),
    ] {
        // The companion process receives no inherited environment. The only
        // channel source is the bridge value supplied after pair verification.
        unsafe { std::env::set_var("CTX_MANAGED_PAIR_CHANNEL", ambient) };
        let output = launch_with_verified_channel(
            &fixture,
            request(&fixture),
            LimitConfiguration::default(),
            &CancellationToken::new(),
            verified,
        )
        .unwrap();
        assert_eq!(output.stdout(), expected);
    }

    drop(restore);
}

#[test]
fn fixed_contract_paths_and_embedded_root_authority_are_exact() {
    let root_authority =
        include_bytes!("../../../contracts/ctx-managed-pair-release-authority-v1.json");
    let root_state_schema =
        include_bytes!("../../../contracts/ctx-managed-pair-state-v1.schema.json");
    let root_target_matrix = include_bytes!("../../../contracts/release-targets-v1.json");
    assert_eq!(MANAGED_PAIR_ENVELOPE_FILENAME, "managed-pair-envelope.json");
    assert_eq!(MANAGED_PAIR_STATE_FILENAME, "managed-pair-state.json");
    assert_eq!(
        crate::slot::ENVELOPE_RELATIVE_PATH,
        ["share", "ctx", "managed-pair-envelope.json"]
    );
    assert_eq!(
        crate::slot::STATE_RELATIVE_PATH,
        ["share", "ctx", "managed-pair-state.json"]
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(embedded_authority_for_tests())),
        "a381be00d1f4f1da43f58fcc3a9c6c392e6460c9f51c06669981e4a6b612e120"
    );
    assert_eq!(embedded_authority_for_tests(), root_authority);
    assert_eq!(embedded_state_schema_for_tests(), root_state_schema);
    assert_eq!(embedded_target_matrix_for_tests(), root_target_matrix);
    assert_eq!(
        format!("{:x}", Sha256::digest(embedded_target_matrix_for_tests())),
        "1cf089c8f494c9662428518ce07ff91a3ceb28fe4ac4d75b6a9d7dd3f16c75a5"
    );
    let registry: serde_json::Value =
        serde_json::from_slice(embedded_authority_for_tests()).unwrap();
    assert_eq!(registry["contract"], "ctx-managed-pair-release-authority");
    assert_eq!(registry["schema_version"], 1);
    assert_eq!(
        registry["channels"][0]["key_id"],
        "ctx-pro-release-stable-2026-07-27"
    );
    assert_eq!(
        registry["channels"][1]["key_id"],
        "ctx-pro-release-staging-2026-07-30"
    );
    assert_eq!(
        registry["channels"][1]["public_key_der_sha256"],
        "e08a1534a6cbbf3ebbf4fdb40d44d55d34c9fe3e89eba83b5d095b1036f1086a"
    );
    let state_schema: serde_json::Value = serde_json::from_slice(root_state_schema).unwrap();
    assert_eq!(
        state_schema["$id"],
        "https://ctx.rs/contracts/ctx-managed-pair-state-v1.schema.json"
    );
    assert_eq!(
        state_schema["required"],
        serde_json::json!([
            "contract",
            "schema_version",
            "identity",
            "envelope_sha256",
            "envelope_size_bytes"
        ])
    );
    assert_eq!(
        state_schema["$defs"]["pair_identity"]["required"],
        serde_json::json!([
            "release_name",
            "target",
            "rollback_generation",
            "manifest_sha256",
            "core",
            "companion"
        ])
    );
}

#[test]
fn fixed_slot_rejects_relative_wrong_traversal_and_symlink_paths() {
    assert!(matches!(
        crate::slot::SlotPaths::from_launcher(Path::new("bin/ctx")),
        Err(BridgeError::InvalidSlot(_))
    ));
    assert!(matches!(
        crate::slot::SlotPaths::from_launcher(Path::new("/managed/bin/not-ctx")),
        Err(BridgeError::InvalidSlot(_))
    ));
    assert!(matches!(
        crate::slot::SlotPaths::from_launcher(Path::new("/managed/other/../bin/ctx")),
        Err(BridgeError::InvalidSlot(_))
    ));

    let fixture = Fixture::new(b"#!/bin/sh\nexit 0\n");
    fs::remove_file(&fixture.companion).unwrap();
    symlink("/bin/true", &fixture.companion).unwrap();
    let error = CompanionBridge::default()
        .launch_captured_at(
            &fixture.launcher,
            request(&fixture).capture(Vec::new()),
            &CancellationToken::new(),
            &AcceptingVerifier,
        )
        .unwrap_err();
    assert!(matches!(error, BridgeError::Filesystem { .. }), "{error:?}");

    let linked_fixture = Fixture::new(b"#!/bin/sh\nexit 0\n");
    let linked_root = linked_fixture._temp.path().join("linked-managed");
    symlink(&linked_fixture.root, &linked_root).unwrap();
    let linked_launcher = linked_root.join("bin").join("ctx");
    let error = crate::slot::prepare(&linked_launcher).err().unwrap();
    assert!(matches!(error, BridgeError::Filesystem { .. }), "{error:?}");
}

#[test]
fn unsafe_permissions_and_shared_file_symlinks_are_rejected() {
    let fixture = Fixture::new(b"#!/bin/sh\nexit 0\n");
    fs::set_permissions(&fixture.companion, fs::Permissions::from_mode(0o720)).unwrap();
    let error = crate::slot::prepare(&fixture.launcher).err().unwrap();
    assert!(matches!(error, BridgeError::Filesystem { .. }));

    fs::set_permissions(&fixture.companion, fs::Permissions::from_mode(0o700)).unwrap();
    let pair = crate::slot::prepare(&fixture.launcher).unwrap();
    let outside = fixture._temp.path().join("outside-state.json");
    write_mode(&outside, b"{}", 0o600);
    symlink(&outside, fixture.shared_path(MANAGED_PAIR_STATE_FILENAME)).unwrap();
    let error = pair
        .read_shared_file(&crate::slot::STATE_RELATIVE_PATH, 64 * 1024)
        .unwrap_err();
    assert!(matches!(error, BridgeError::Filesystem { .. }));
}

#[test]
fn verifier_refusal_occurs_before_any_companion_code_runs() {
    let fixture = Fixture::new(b"#!/bin/sh\nprintf started > \"$1\"\n");
    let marker = fixture.root.join("started");
    let mut request = request(&fixture);
    request.push_argument(marker.as_os_str());
    let error = CompanionBridge::default()
        .launch_captured_at(
            &fixture.launcher,
            request.capture(Vec::new()),
            &CancellationToken::new(),
            &RefusingVerifier,
        )
        .unwrap_err();
    assert!(matches!(error, BridgeError::Verification(_)));
    assert!(!marker.exists());
}

#[test]
fn native_non_utf8_argument_round_trips_without_loss() {
    let fixture =
        Fixture::new(b"#!/usr/bin/python3\nimport os,sys\nos.write(1, os.fsencode(sys.argv[1]))\n");
    let expected = vec![b'n', b'a', 0xff, b't', b'i', b'v', b'e'];
    let mut request = request(&fixture);
    request.push_argument(OsString::from_vec(expected.clone()));
    let output = launch_with(
        &fixture,
        request,
        LimitConfiguration::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(output.exit_class(), ExitClass::Success);
    assert_eq!(output.stdout(), expected);
}

#[test]
fn environment_is_stripped_to_allowlist_and_cwd_is_managed_root() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os\nvalues=[os.getenv('PATH','<missing>'),os.getenv('LANG','<missing>'),os.getenv('HOME','<missing>'),os.getcwd(),os.getenv('CTX_DATA_ROOT','<missing>'),os.getenv('CTX_PRO_DATA_ROOT','<missing>'),os.getenv('CTX_LOCAL_USAGE_ENABLED','<missing>'),os.getenv('CTX_PRO_INSTALLATION_ID','<missing>')]\nos.write(1, '\\0'.join(values).encode())\n",
    );
    let mut request = request(&fixture);
    request
        .environment_mut()
        .set(EnvironmentKey::Home, OsStr::new("/home/tester"))
        .set(EnvironmentKey::Lang, OsStr::new("C.UTF-8"))
        .set(EnvironmentKey::LocalUsageEnabled, OsStr::new("false"))
        .set(
            EnvironmentKey::InstallationId,
            OsStr::new("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
        );
    let output = launch_with(
        &fixture,
        request,
        LimitConfiguration::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let fields: Vec<_> = output.stdout().split(|byte| *byte == 0).collect();
    assert_eq!(fields[0], b"<missing>");
    assert_eq!(fields[1], b"C.UTF-8");
    assert_eq!(fields[2], b"/home/tester");
    assert_eq!(fields[3], fixture.root.as_os_str().as_encoded_bytes());
    assert_eq!(
        fields[4],
        fixture.root.join("data").as_os_str().as_encoded_bytes()
    );
    assert_eq!(
        fields[5],
        fixture.root.join("data").as_os_str().as_encoded_bytes()
    );
    assert_eq!(fields[6], b"false");
    assert_eq!(fields[7], b"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
    assert_eq!(MAX_ENVIRONMENT_ENTRIES, 8);
}

#[test]
fn streaming_transport_preserves_interactive_request_response_order() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os,sys\nfirst=sys.stdin.buffer.readline()\nos.write(1,b'ready:'+first)\nsecond=sys.stdin.buffer.readline()\nraise SystemExit(0 if second == b'second\\n' else 9)\n",
    );
    let pair = crate::slot::prepare(&fixture.launcher).unwrap();
    let data_root = fixture.root.join("data");
    let (mut parent, child) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let parent_reader = parent.try_clone().unwrap();
    let child_stdin: OwnedFd = child.try_clone().unwrap().into();
    let child_stdout: OwnedFd = child.into();
    let launch = thread::spawn(move || {
        crate::process::run_streaming_with_stdio(
            &pair.execution,
            CompanionRequest::new(data_root),
            &CancellationToken::new(),
            Stdio::from(child_stdin),
            Stdio::from(child_stdout),
            Stdio::null(),
        )
    });

    parent.write_all(b"first\n").unwrap();
    parent.flush().unwrap();
    let mut line = String::new();
    BufReader::new(parent_reader).read_line(&mut line).unwrap();
    assert_eq!(line, "ready:first\n");
    parent.write_all(b"second\n").unwrap();
    parent.flush().unwrap();
    let exit = launch.join().unwrap().unwrap();
    assert_eq!(exit.exit, ExitClass::Success);
}

#[test]
fn streaming_transport_preserves_terminal_identity() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os\nok=all(os.isatty(fd) for fd in (0,1,2))\nos.write(1,b'tty-ok\\n' if ok else b'tty-missing\\n')\nraise SystemExit(0 if ok else 7)\n",
    );
    let pair = crate::slot::prepare(&fixture.launcher).unwrap();
    let (master, slave) = pseudo_terminal();
    let child_stdin: OwnedFd = slave.try_clone().unwrap().into();
    let child_stdout: OwnedFd = slave.try_clone().unwrap().into();
    let child_stderr: OwnedFd = slave.into();
    let exit = crate::process::run_streaming_with_stdio(
        &pair.execution,
        request(&fixture),
        &CancellationToken::new(),
        Stdio::from(child_stdin),
        Stdio::from(child_stdout),
        Stdio::from(child_stderr),
    )
    .unwrap();
    assert_eq!(exit.exit, ExitClass::Success);
    let mut output = String::new();
    BufReader::new(master).read_line(&mut output).unwrap();
    assert!(output.contains("tty-ok"), "{output:?}");
}

#[test]
fn streaming_child_lifetime_is_independent_of_captured_wall_time() {
    let fixture = Fixture::new(b"#!/bin/sh\n/usr/bin/sleep 0.15\nexit 0\n");
    let bridge = CompanionBridge::new(
        BridgeLimits::new(LimitConfiguration {
            admission_wait: Duration::from_secs(1),
            captured_wall_time: Duration::from_millis(25),
            ..LimitConfiguration::default()
        })
        .unwrap(),
    );
    let started = Instant::now();
    let exit = bridge
        .launch_streaming_at(
            &fixture.launcher,
            request(&fixture),
            &CancellationToken::new(),
            &AcceptingVerifier,
        )
        .unwrap();
    assert_eq!(exit.exit_class(), ExitClass::Success);
    assert!(started.elapsed() >= Duration::from_millis(100));
}

#[test]
#[ignore = "requires a dedicated controlling terminal"]
fn streaming_transport_hands_foreground_terminal_to_companion() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os,sys\nforeground=os.tcgetpgrp(0)==os.getpgrp()\nline=sys.stdin.buffer.readline()\nok=foreground and line==b'hello\\n'\nos.write(1,b'foreground-ok\\n' if ok else b'foreground-missing\\n')\nraise SystemExit(0 if ok else 8)\n",
    );
    let exit = CompanionBridge::default()
        .launch_streaming_at(
            &fixture.launcher,
            request(&fixture),
            &CancellationToken::new(),
            &AcceptingVerifier,
        )
        .unwrap();
    assert_eq!(exit.exit_class(), ExitClass::Success);
}

fn pseudo_terminal() -> (fs::File, fs::File) {
    let mut master = -1;
    let mut slave = -1;
    let result = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
    assert!(master >= 0 && slave >= 0);
    unsafe { (fs::File::from_raw_fd(master), fs::File::from_raw_fd(slave)) }
}

#[test]
fn data_root_is_mandatory_and_absolute() {
    let fixture = Fixture::new(b"#!/bin/sh\nexit 0\n");
    let error = launch_with(
        &fixture,
        CompanionRequest::new("relative-data"),
        LimitConfiguration::default(),
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(matches!(error, BridgeError::InvalidDataRoot));
}

#[test]
fn exit_class_and_binary_stdout_stderr_are_preserved() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os\nos.write(1,b'out\\xff')\nos.write(2,b'err\\xfe')\nraise SystemExit(17)\n",
    );
    let output = launch_with(
        &fixture,
        request(&fixture),
        LimitConfiguration::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(output.exit_class(), ExitClass::Code(17));
    assert_eq!(output.stdout(), b"out\xff");
    assert_eq!(output.stderr(), b"err\xfe");
    assert!(!output.stdout_truncated());
    assert!(!output.stderr_truncated());
}

#[test]
fn control_input_and_output_bounds_fail_closed() {
    let fixture = Fixture::new(b"#!/usr/bin/python3\nimport os\nos.write(1,b'x'*65536)\n");
    let small = LimitConfiguration {
        control_bytes: 1024,
        input_bytes: 8,
        stdout_bytes: 32,
        stderr_bytes: 32,
        captured_wall_time: Duration::from_secs(2),
        ..LimitConfiguration::default()
    };
    let oversized_input = request(&fixture).capture(vec![0; 9]);
    assert!(matches!(
        CompanionBridge::new(BridgeLimits::new(small).unwrap()).launch_captured_at(
            &fixture.launcher,
            oversized_input,
            &CancellationToken::new(),
            &AcceptingVerifier,
        ),
        Err(BridgeError::Limit("input bytes"))
    ));
    let mut oversized_control = request(&fixture);
    oversized_control.push_argument("x".repeat(1024));
    assert!(matches!(
        launch_with(
            &fixture,
            oversized_control,
            small,
            &CancellationToken::new()
        ),
        Err(BridgeError::Limit("control bytes"))
    ));
    let output = launch_with(
        &fixture,
        request(&fixture),
        small,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        output.exit_class(),
        ExitClass::Terminated(TerminationReason::StdoutLimit)
    );
    assert_eq!(output.stdout(), vec![b'x'; 32]);
    assert!(output.stdout_truncated());
}

#[test]
fn admission_and_captured_lifetime_bounds_are_independent() {
    let limits = LimitConfiguration {
        admission_wait: Duration::ZERO,
        ..LimitConfiguration::default()
    };
    assert!(matches!(
        BridgeLimits::new(limits),
        Err(BridgeError::Limit("admission wait"))
    ));

    let limits = LimitConfiguration {
        captured_wall_time: Duration::ZERO,
        ..LimitConfiguration::default()
    };
    assert!(matches!(
        BridgeLimits::new(limits),
        Err(BridgeError::Limit("captured wall time"))
    ));
}

#[test]
fn timeout_terminates_the_descendant_process_group() {
    let fixture = Fixture::new(b"#!/bin/sh\n/usr/bin/sleep 60 &\nprintf '%s\\n' \"$!\"\nwait\n");
    let limits = LimitConfiguration {
        captured_wall_time: Duration::from_millis(150),
        ..LimitConfiguration::default()
    };
    let output = launch_with(
        &fixture,
        request(&fixture),
        limits,
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(
        output.exit_class(),
        ExitClass::Terminated(TerminationReason::WallTime)
    );
    let pid: u32 = String::from_utf8(output.stdout().to_vec())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_process_stopped(pid);
}

#[test]
fn cancellation_terminates_the_descendant_process_group() {
    let fixture = Fixture::new(b"#!/bin/sh\n/usr/bin/sleep 60 &\nprintf '%s\\n' \"$!\"\nwait\n");
    let cancellation = CancellationToken::new();
    let trigger = cancellation.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        trigger.cancel();
    });
    let output = launch_with(
        &fixture,
        request(&fixture),
        LimitConfiguration {
            captured_wall_time: Duration::from_secs(2),
            ..LimitConfiguration::default()
        },
        &cancellation,
    )
    .unwrap();
    canceller.join().unwrap();
    assert_eq!(
        output.exit_class(),
        ExitClass::Terminated(TerminationReason::Cancelled)
    );
    let pid: u32 = String::from_utf8(output.stdout().to_vec())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_process_stopped(pid);
}

#[test]
fn streaming_cancellation_terminates_the_descendant_process_group() {
    let fixture =
        Fixture::new(b"#!/bin/sh\n/usr/bin/sleep 60 &\nprintf '%s\\n' \"$!\" > \"$1\"\nwait\n");
    let pid_path = fixture.root.join("streaming-descendant.pid");
    let mut request = request(&fixture);
    request.push_argument(pid_path.as_os_str());
    let cancellation = CancellationToken::new();
    let trigger = cancellation.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        trigger.cancel();
    });
    let exit = CompanionBridge::default()
        .launch_streaming_at(
            &fixture.launcher,
            request,
            &cancellation,
            &AcceptingVerifier,
        )
        .unwrap();
    canceller.join().unwrap();
    assert_eq!(
        exit.exit_class(),
        ExitClass::Terminated(TerminationReason::Cancelled)
    );
    let pid = fs::read_to_string(pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_process_stopped(pid);
}

#[test]
fn admission_queue_wait_remains_bounded() {
    let gate = ConcurrencyGate::new(1);
    let cancellation = CancellationToken::new();
    let _permit = gate.acquire(&cancellation, Duration::from_secs(1)).unwrap();
    let started = Instant::now();
    let error = match gate.acquire(&cancellation, Duration::from_millis(25)) {
        Ok(_) => panic!("a second process slot was admitted"),
        Err(error) => error,
    };
    assert!(matches!(error, BridgeError::QueueTimeout));
    assert!(started.elapsed() >= Duration::from_millis(20));
    assert!(started.elapsed() < Duration::from_secs(1));
}

fn assert_process_stopped(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
        match stat {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Ok(value) if value.split_whitespace().nth(2) == Some("Z") => return,
            _ if Instant::now() >= deadline => panic!("descendant {pid} survived tree termination"),
            _ => thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[test]
fn configured_concurrency_is_a_real_gate() {
    let fixture = Arc::new(Fixture::new(
        b"#!/bin/sh\n/usr/bin/sleep 0.2\nprintf done\n",
    ));
    let bridge = Arc::new(CompanionBridge::new(
        BridgeLimits::new(LimitConfiguration {
            concurrent_processes: 1,
            captured_wall_time: Duration::from_secs(2),
            ..LimitConfiguration::default()
        })
        .unwrap(),
    ));
    let started = Instant::now();
    let mut workers = Vec::new();
    for _ in 0..2 {
        let fixture = Arc::clone(&fixture);
        let bridge = Arc::clone(&bridge);
        workers.push(thread::spawn(move || {
            bridge
                .launch_captured_at(
                    &fixture.launcher,
                    request(&fixture).capture(Vec::new()),
                    &CancellationToken::new(),
                    &AcceptingVerifier,
                )
                .unwrap()
        }));
    }
    for worker in workers {
        assert_eq!(worker.join().unwrap().stdout(), b"done");
    }
    assert!(started.elapsed() >= Duration::from_millis(350));
}

#[test]
fn production_verifier_fails_closed_when_fixed_envelope_is_absent() {
    let fixture = Fixture::new(b"#!/bin/sh\nexit 0\n");
    let zero = Sha256Digest::from_bytes([0; 32]);
    let expectations = ManagedPairExpectations::new(
        ReleaseChannel::Staging,
        CoreBuildIdentity::new("a".repeat(40)).unwrap(),
        CompatibilityIdentity::new(zero, zero),
    );
    let verifier = crate::verifier::ProductionVerifier::new(&expectations);
    let error = CompanionBridge::default()
        .launch_captured_at(
            &fixture.launcher,
            request(&fixture).capture(Vec::new()),
            &CancellationToken::new(),
            &verifier,
        )
        .unwrap_err();
    assert!(matches!(error, BridgeError::Filesystem { .. }), "{error:?}");
    assert!(!fixture.shared_path(MANAGED_PAIR_ENVELOPE_FILENAME).exists());
}

#[test]
fn production_verifier_uses_root_authority_and_rejects_an_invalid_signature() {
    let companion_bytes = b"#!/bin/sh\nprintf started > started\n";
    let fixture = Fixture::new(companion_bytes);
    let zero = Sha256Digest::from_bytes([0; 32]);
    let expectations = ManagedPairExpectations::new(
        ReleaseChannel::Staging,
        CoreBuildIdentity::new("a".repeat(40)).unwrap(),
        CompatibilityIdentity::new(zero, zero),
    );
    let target = test_target();
    let core_sha = format!("{:x}", Sha256::digest(CORE_BYTES));
    let companion_sha = format!("{:x}", Sha256::digest(companion_bytes));
    let manifest = serde_json::json!({
        "channel": "staging",
        "compatibility": {
            "core_capability_fingerprint": zero.to_hex(),
            "invocation_fingerprint": zero.to_hex(),
        },
        "components": {
            "companion": test_component(
                "companion",
                target.companion_artifact,
                target.companion_slot,
                target.companion_rust_target,
                &companion_sha,
                companion_bytes.len(),
                "b",
            ),
            "core": test_component(
                "core",
                target.core_artifact,
                target.core_slot,
                target.core_rust_target,
                &core_sha,
                CORE_BYTES.len(),
                "a",
            ),
        },
        "contract": "ctx-managed-pair-manifest",
        "install_geometry": {
            "companion_slot": target.companion_slot,
            "core_slot": target.core_slot,
            "install_root": "<install-root>",
            "managed_bin_dir": "<install-root>/bin",
        },
        "release_authority_key_id": "ctx-pro-release-staging-2026-07-30",
        "release_name": "v1.2.3",
        "rollback_generation": 7,
        "schema_version": 1,
        "snapshot": {
            "contract": "ctx-managed-pair-snapshot-v1",
            "fingerprint": zero.to_hex(),
        },
        "target": {
            "arch": target.arch,
            "companion_rust_target": target.companion_rust_target,
            "core_rust_target": target.core_rust_target,
            "id": target.id,
            "os": target.os,
        },
        "target_matrix_sha256": "1cf089c8f494c9662428518ce07ff91a3ceb28fe4ac4d75b6a9d7dd3f16c75a5",
    });
    let payload = serde_json::to_vec(&manifest).unwrap();
    let envelope = serde_json::to_vec(&serde_json::json!({
        "manifest_base64": BASE64.encode(payload),
        "schema_version": 1,
        "signature_base64": BASE64.encode([0_u8; 384]),
    }))
    .unwrap();
    write_mode(
        &fixture.shared_path(MANAGED_PAIR_ENVELOPE_FILENAME),
        &envelope,
        0o600,
    );

    let verifier = crate::verifier::ProductionVerifier::new(&expectations);
    let error = CompanionBridge::default()
        .launch_captured_at(
            &fixture.launcher,
            request(&fixture).capture(Vec::new()),
            &CancellationToken::new(),
            &verifier,
        )
        .unwrap_err();
    assert!(
        matches!(error, BridgeError::Verification(ref message) if message.contains("signature")),
        "{error:?}"
    );
    assert!(!fixture.root.join("started").exists());
}

struct TestTarget {
    id: &'static str,
    os: &'static str,
    arch: &'static str,
    core_rust_target: &'static str,
    companion_rust_target: &'static str,
    core_slot: &'static str,
    companion_slot: &'static str,
    core_artifact: &'static str,
    companion_artifact: &'static str,
}

fn test_component(
    kind: &str,
    artifact: &str,
    slot: &str,
    rust_target: &str,
    sha256: &str,
    size_bytes: usize,
    revision_digit: &str,
) -> serde_json::Value {
    serde_json::json!({
        "artifact_name": artifact,
        "build_identity": {
            "build_fingerprint": "0".repeat(64),
            "component": kind,
            "rust_target": rust_target,
            "source_revision": revision_digit.repeat(40),
        },
        "install_slot": slot,
        "object_key": format!("sha256/{sha256}/{artifact}"),
        "sha256": sha256,
        "size_bytes": size_bytes,
    })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn test_target() -> TestTarget {
    TestTarget {
        id: "linux-x64",
        os: "linux",
        arch: "x86_64",
        core_rust_target: "x86_64-unknown-linux-gnu",
        companion_rust_target: "x86_64-unknown-linux-gnu",
        core_slot: "<install-root>/bin/ctx",
        companion_slot: "<install-root>/libexec/ctx-pro",
        core_artifact: "ctx-linux-x64",
        companion_artifact: "ctx-pro-linux-x64",
    }
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn test_target() -> TestTarget {
    TestTarget {
        id: "linux-arm64",
        os: "linux",
        arch: "aarch64",
        core_rust_target: "aarch64-unknown-linux-gnu",
        companion_rust_target: "aarch64-unknown-linux-gnu",
        core_slot: "<install-root>/bin/ctx",
        companion_slot: "<install-root>/libexec/ctx-pro",
        core_artifact: "ctx-linux-aarch64",
        companion_artifact: "ctx-pro-linux-arm64",
    }
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn test_target() -> TestTarget {
    TestTarget {
        id: "macos-x64",
        os: "macos",
        arch: "x86_64",
        core_rust_target: "x86_64-apple-darwin",
        companion_rust_target: "x86_64-apple-darwin",
        core_slot: "<install-root>/bin/ctx",
        companion_slot: "<install-root>/libexec/ctx-pro",
        core_artifact: "ctx-macos-x64",
        companion_artifact: "ctx-pro-macos-x64",
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn test_target() -> TestTarget {
    TestTarget {
        id: "macos-arm64",
        os: "macos",
        arch: "aarch64",
        core_rust_target: "aarch64-apple-darwin",
        companion_rust_target: "aarch64-apple-darwin",
        core_slot: "<install-root>/bin/ctx",
        companion_slot: "<install-root>/libexec/ctx-pro",
        core_artifact: "ctx-macos-arm64",
        companion_artifact: "ctx-pro-macos-arm64",
    }
}
