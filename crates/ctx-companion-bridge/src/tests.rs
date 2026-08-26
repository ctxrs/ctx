#![cfg(unix)]

use std::{
    ffi::OsStr,
    fs,
    io::{BufRead as _, BufReader, Write as _},
    os::{fd::OwnedFd, unix::fs::PermissionsExt as _, unix::net::UnixStream},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use super::*;

#[test]
fn detached_install_verifier_rejects_unsigned_input() {
    let expectations = ManagedPairExpectations::new(ReleaseChannel::Staging);
    let unsigned = br#"{"manifest_base64":"e30=","schema_version":1,"signature_base64":""}"#;
    assert!(matches!(
        verify_signed_managed_pair_envelope(&expectations, unsigned),
        Err(BridgeError::Verification(_))
    ));
}

#[test]
fn detached_install_verifier_embeds_the_canonical_contracts() {
    assert_eq!(
        crate::verifier::embedded_authority_for_tests(),
        include_bytes!("../../../contracts/ctx-managed-pair-release-authority-v1.json")
    );
    assert_eq!(
        crate::verifier::embedded_state_schema_for_tests(),
        include_bytes!("../../../contracts/ctx-managed-pair-state-v1.schema.json")
    );
    assert_eq!(
        crate::verifier::embedded_target_matrix_for_tests(),
        include_bytes!("../../../contracts/release-targets-v1.json")
    );
}

struct Fixture {
    _temp: tempfile::TempDir,
    pro: PathBuf,
}

impl Fixture {
    fn new(operation: &[u8]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("installed");
        let pro = root.join("libexec/ctx-pro");
        fs::create_dir_all(pro.parent().unwrap()).unwrap();
        write_executable(
            &pro,
            br##"#!/bin/sh
printf '%s' "$0" > "${0}.launched"
printf '%s\n' "$2" >> "${0}.commands"
if [ "$1" = "--ctx-pro-protocol-v4" ] && [ "$2" = "handshake" ]; then
  printf '{"protocol_version":4}\n'
  exit 0
fi
if [ "$1" != "--ctx-pro-protocol-v4" ]; then
  exit 90
fi
case "$2" in
  cli)
    shift 2
    [ "$1" = "--" ] || exit 91
    shift
    ;;
  mcp-serve|maintenance)
    shift 2
    ;;
  *) exit 92 ;;
esac
exec "${0}.operation" "$@"
"##,
        );
        write_executable(&pro.with_extension("operation"), operation);
        Self { _temp: temp, pro }
    }

    fn companion(&self) -> InstalledCompanion {
        InstalledCompanion::new(&self.pro)
    }
}

fn write_executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn launch_mcp_with(
    fixture: &Fixture,
    request: McpRequest,
    limits: LimitConfiguration,
    cancellation: &CancellationToken,
) -> Result<McpResponse, BridgeError> {
    CompanionBridge::new(BridgeLimits::new(limits).unwrap()).launch_mcp(
        &fixture.companion(),
        request,
        cancellation,
    )
}

#[test]
fn protocol_v4_mcp_request_and_response_are_sufficient() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os,sys\nrequest=sys.stdin.buffer.readline()\nos.write(1,b'compatible:'+request)\nreceipt=sys.stdin.buffer.readline()\nraise SystemExit(0 if receipt == b'written_and_flushed\\n' else 9)\n",
    );
    let response = CompanionBridge::default()
        .launch_mcp(
            &fixture.companion(),
            McpRequest::new(b"ok\n"),
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(response.response_frame(), b"compatible:ok\n");
    response
        .finish(McpFinishOutcome::WrittenAndFlushed)
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.pro.with_extension("commands")).unwrap(),
        "mcp-serve\n"
    );
}

#[test]
fn protocol_v4_cli_request_is_typed_and_launches_directly() {
    let prior_endpoint = std::env::var_os("CTX_ANALYTICS_ENDPOINT");
    std::env::set_var(
        "CTX_ANALYTICS_ENDPOINT",
        "https://ambient.example.test/private",
    );
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(value) = &self.0 {
                std::env::set_var("CTX_ANALYTICS_ENDPOINT", value);
            } else {
                std::env::remove_var("CTX_ANALYTICS_ENDPOINT");
            }
        }
    }
    let _restore = Restore(prior_endpoint);

    let fixture = Fixture::new(
        b"#!/bin/sh\n[ \"$1\" = paid ] && [ \"$2\" = action ] && [ \"$CTX_ANALYTICS_ENABLED\" = false ] && [ -z \"${CTX_ANALYTICS_ENDPOINT+x}\" ]\n",
    );
    let mut request = CliRequest::new(vec!["paid".into(), "action".into()]);
    request
        .environment_mut()
        .set(EnvironmentKey::AnalyticsEnabled, "false");
    let output = CompanionBridge::default()
        .launch_cli(&fixture.companion(), request, &CancellationToken::new())
        .unwrap();
    assert_eq!(output.exit_class(), ExitClass::Success);
    assert_eq!(
        fs::read(fixture.pro.with_extension("launched")).unwrap(),
        fixture.pro.as_os_str().as_encoded_bytes()
    );
}

#[test]
fn protocol_v4_maintenance_response_is_closed_and_typed() {
    let fixture = Fixture::new(
        b"#!/bin/sh\n/usr/bin/sleep 0.15\nprintf '{\"accepted\":true,\"schema_version\":1}\\n'\n",
    );
    let bridge = CompanionBridge::new(
        BridgeLimits::new(LimitConfiguration {
            captured_wall_time: Duration::from_millis(25),
            ..LimitConfiguration::default()
        })
        .unwrap(),
    );
    let started = Instant::now();
    let response = bridge
        .launch_maintenance(
            &fixture.companion(),
            MaintenanceRequest::new(),
            &CancellationToken::new(),
        )
        .unwrap();
    assert!(response.accepted());
    assert!(started.elapsed() >= Duration::from_millis(100));
}

#[test]
fn protocol_mismatch_is_typed_and_prevents_operation() {
    let temp = tempfile::tempdir().unwrap();
    let pro = temp.path().join("ctx-pro");
    let marker = temp.path().join("operation-started");
    write_executable(
        &pro,
        format!(
            "#!/bin/sh\nif [ \"$2\" = handshake ]; then printf '{{\"protocol_version\":2}}\\n'; exit 0; fi\nprintf started > '{}'\n",
            marker.display()
        )
        .as_bytes(),
    );
    let error = CompanionBridge::default()
        .handshake(&InstalledCompanion::new(&pro), &CancellationToken::new())
        .unwrap_err();
    assert!(matches!(
        error,
        BridgeError::ProtocolMismatch {
            expected: CORE_PRO_PROTOCOL_VERSION,
            observed,
        } if observed == ProtocolVersion::new(2)
    ));
    assert!(!marker.exists());
}

#[test]
fn pre_handshake_exit_is_a_launch_failure_with_bounded_stderr() {
    let temp = tempfile::tempdir().unwrap();
    let pro = temp.path().join("ctx-pro");
    write_executable(
        &pro,
        b"#!/bin/sh\nprintf 'loader diagnostic' >&2\nexit 70\n",
    );
    let error = CompanionBridge::default()
        .handshake(&InstalledCompanion::new(&pro), &CancellationToken::new())
        .unwrap_err();
    assert!(matches!(
        error,
        BridgeError::HandshakeFailed {
            exit: ExitClass::Code(70),
            ref stderr,
            stderr_truncated: false,
        } if stderr == b"loader diagnostic"
    ));
}

#[test]
fn missing_pro_is_distinct_from_protocol_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing-ctx-pro");
    let error = CompanionBridge::default()
        .launch_mcp(
            &InstalledCompanion::new(&missing),
            McpRequest::new(b"\n"),
            &CancellationToken::new(),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        BridgeError::MissingExecutable { ref path } if path == &missing
    ));
}

#[test]
fn launch_environment_contains_no_install_context_authority() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os,sys\nsys.stdin.buffer.readline()\nforbidden=['CTX_PRO_PATH','CTX_PRO_INSTALL_CONTEXT','CTX_DATA_ROOT','CTX_PRO_DATA_ROOT','CTX_MANAGED_PAIR_CHANNEL','CTX_PRO_INSTALLATION_ID','CTX_MANAGED_PAIR_INVOCATION_FINGERPRINT','CTX_MANAGED_PAIR_CORE_CAPABILITY_FINGERPRINT','CTX_RELEASE_BUILD_SOURCE_COMMIT']\npresent=[name for name in forbidden if name in os.environ]\nos.write(1, ('ok' if not present else ','.join(present)).encode()+b'\\n')\nreceipt=sys.stdin.buffer.readline()\nraise SystemExit(0 if receipt == b'written_and_flushed\\n' else 9)\n",
    );
    let response = CompanionBridge::default()
        .launch_mcp(
            &fixture.companion(),
            McpRequest::new(b"request\n"),
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(response.response_frame(), b"ok\n");
    response
        .finish(McpFinishOutcome::WrittenAndFlushed)
        .unwrap();
}

#[test]
fn relative_and_directory_pro_paths_have_typed_errors() {
    let relative = InstalledCompanion::new("ctx-pro");
    assert!(matches!(
        CompanionBridge::default().launch_mcp(
            &relative,
            McpRequest::new(b"request\n"),
            &CancellationToken::new(),
        ),
        Err(BridgeError::InvalidExecutablePath)
    ));

    let temp = tempfile::tempdir().unwrap();
    let directory = InstalledCompanion::new(temp.path());
    assert!(matches!(
        CompanionBridge::default().launch_mcp(
            &directory,
            McpRequest::new(b"request\n"),
            &CancellationToken::new(),
        ),
        Err(BridgeError::ExecutableNotFile { .. })
    ));
}

#[test]
fn typed_environment_allowlist_is_preserved_and_ambient_is_cleared() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os,sys\nsys.stdin.buffer.readline()\nnames=['PATH','LANG','HOME','TERM','COLORTERM','NO_COLOR','CLICOLOR','CLICOLOR_FORCE','CI','CTX_ANALYTICS_ENABLED']\nvalues=[os.getenv(name,'<missing>') for name in names]\nos.write(1, '\\0'.join(values).encode()+b'\\n')\nreceipt=sys.stdin.buffer.readline()\nraise SystemExit(0 if receipt == b'written_and_flushed\\n' else 9)\n",
    );
    let mut request = McpRequest::new(b"request\n");
    request
        .environment_mut()
        .set(EnvironmentKey::Home, OsStr::new("/home/tester"))
        .set(
            EnvironmentKey::Path,
            OsStr::new("/usr/local/bin:/usr/bin:/bin"),
        )
        .set(EnvironmentKey::Lang, OsStr::new("C.UTF-8"))
        .set(EnvironmentKey::Term, OsStr::new("xterm-256color"))
        .set(EnvironmentKey::ColorTerm, OsStr::new("truecolor"))
        .set(EnvironmentKey::NoColor, OsStr::new("0"))
        .set(EnvironmentKey::CliColor, OsStr::new("1"))
        .set(EnvironmentKey::CliColorForce, OsStr::new("1"))
        .set(EnvironmentKey::Ci, OsStr::new("true"))
        .set(EnvironmentKey::AnalyticsEnabled, OsStr::new("false"));
    let response = launch_mcp_with(
        &fixture,
        request,
        LimitConfiguration::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let frame = response.response_frame().strip_suffix(b"\n").unwrap();
    let fields: Vec<_> = frame.split(|byte| *byte == 0).collect();
    assert_eq!(
        fields,
        [
            b"/usr/local/bin:/usr/bin:/bin".as_slice(),
            b"C.UTF-8",
            b"/home/tester",
            b"xterm-256color",
            b"truecolor",
            b"0",
            b"1",
            b"1",
            b"true",
            b"false",
        ]
    );
    response
        .finish(McpFinishOutcome::WrittenAndFlushed)
        .unwrap();
}

#[test]
fn output_failed_sends_the_exact_closed_receipt() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os,pathlib,sys\nsys.stdin.buffer.readline()\nos.write(1,b'opaque\\n')\nreceipt=sys.stdin.buffer.readline()\npathlib.Path(sys.argv[0]).with_suffix('.receipt').write_bytes(receipt)\nraise SystemExit(0 if receipt == b'output_failed\\n' else 17)\n",
    );
    let receipt = fixture.pro.with_extension("receipt");
    let response = launch_mcp_with(
        &fixture,
        McpRequest::new(b"request\n"),
        LimitConfiguration::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(response.response_frame(), b"opaque\n");
    response.finish(McpFinishOutcome::OutputFailed).unwrap();
    assert_eq!(fs::read(receipt).unwrap(), b"output_failed\n");
}

#[test]
fn control_input_and_output_bounds_fail_closed() {
    let fixture = Fixture::new(b"#!/usr/bin/python3\nimport os\nos.write(1,b'x'*65536)\n");
    let small = LimitConfiguration {
        control_bytes: 512,
        input_bytes: 8,
        stdout_bytes: 32,
        stderr_bytes: 32,
        captured_wall_time: Duration::from_secs(2),
        ..LimitConfiguration::default()
    };
    assert!(matches!(
        launch_mcp_with(
            &fixture,
            McpRequest::new(vec![0; 9]),
            small,
            &CancellationToken::new(),
        ),
        Err(BridgeError::Limit("input bytes"))
    ));

    let mut oversized_control = McpRequest::new(b"\n");
    oversized_control
        .environment_mut()
        .set(EnvironmentKey::Home, "x".repeat(512));
    assert!(matches!(
        launch_mcp_with(
            &fixture,
            oversized_control,
            small,
            &CancellationToken::new(),
        ),
        Err(BridgeError::Limit("control bytes"))
    ));

    let mut invalid_environment_name = McpRequest::new(b"\n");
    invalid_environment_name
        .environment_mut()
        .set_named("INVALID=NAME", "value");
    assert!(matches!(
        launch_mcp_with(
            &fixture,
            invalid_environment_name,
            LimitConfiguration::default(),
            &CancellationToken::new(),
        ),
        Err(BridgeError::InvalidEnvironmentName)
    ));

    let error = launch_mcp_with(
        &fixture,
        McpRequest::new(b"ok\n"),
        small,
        &CancellationToken::new(),
    )
    .unwrap_err();
    assert!(matches!(error, BridgeError::Limit("stdout bytes")));

    assert!(matches!(
        launch_mcp_with(
            &fixture,
            McpRequest::new(b"bad"),
            small,
            &CancellationToken::new(),
        ),
        Err(BridgeError::InvalidProtocolResponse("MCP request frame"))
    ));
}

#[test]
fn missing_extra_and_stderr_output_fail_closed() {
    let eof = Fixture::new(b"#!/bin/sh\nIFS= read -r _\nexit 0\n");
    assert!(matches!(
        launch_mcp_with(
            &eof,
            McpRequest::new(b"request\n"),
            LimitConfiguration::default(),
            &CancellationToken::new(),
        ),
        Err(BridgeError::InvalidProtocolResponse("MCP response frame"))
            | Err(BridgeError::McpExchangeFailed {
                exit: ExitClass::Success
            })
    ));

    let extra = Fixture::new(
        b"#!/usr/bin/python3\nimport os,sys\nsys.stdin.buffer.readline()\nos.write(1,b'first\\nsecond\\n')\nsys.stdin.buffer.readline()\n",
    );
    assert!(matches!(
        launch_mcp_with(
            &extra,
            McpRequest::new(b"request\n"),
            LimitConfiguration::default(),
            &CancellationToken::new(),
        ),
        Err(BridgeError::InvalidProtocolResponse("MCP response frame"))
    ));

    let stderr = Fixture::new(
        b"#!/bin/sh\nIFS= read -r _\nprintf diagnostic >&2\nprintf 'response\\n'\nIFS= read -r _\n",
    );
    assert!(matches!(
        launch_mcp_with(
            &stderr,
            McpRequest::new(b"request\n"),
            LimitConfiguration::default(),
            &CancellationToken::new(),
        ),
        Err(BridgeError::InvalidProtocolResponse("MCP stderr"))
    ));
}

#[test]
fn streaming_transport_preserves_request_response_order() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os,sys\nfirst=sys.stdin.buffer.readline()\nos.write(1,b'ready:'+first)\nsecond=sys.stdin.buffer.readline()\nraise SystemExit(0 if second == b'second\\n' else 9)\n",
    );
    let companion = fixture.companion();
    let bridge = CompanionBridge::default();
    bridge
        .handshake(&companion, &CancellationToken::new())
        .unwrap();
    let (mut parent, child) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let parent_reader = parent.try_clone().unwrap();
    let child_stdin: OwnedFd = child.try_clone().unwrap().into();
    let child_stdout: OwnedFd = child.into();
    let launch = thread::spawn(move || {
        crate::process::run_streaming_with_stdio(
            &companion,
            CliRequest::new(Vec::new()).into_process(),
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
    assert_eq!(launch.join().unwrap().unwrap().exit, ExitClass::Success);
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
        .launch_cli(
            &fixture.companion(),
            CliRequest::new(Vec::new()),
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(exit.exit_class(), ExitClass::Success);
    assert!(started.elapsed() >= Duration::from_millis(100));
}

#[cfg(target_os = "linux")]
#[test]
fn known_outcome_after_wall_deadline_writes_ack_before_teardown() {
    let fixture = Fixture::new(
        b"#!/bin/sh\nIFS= read -r _\n/usr/bin/sleep 60 &\nprintf '%s\\n' \"$!\"\nIFS= read -r receipt\nprintf '%s\\n' \"$receipt\" > \"${0}.receipt\"\n/usr/bin/sleep 0.15\nexit 0\n",
    );
    let receipt = fixture.pro.with_extension("operation.receipt");
    let response = launch_mcp_with(
        &fixture,
        McpRequest::new(b"request\n"),
        LimitConfiguration {
            captured_wall_time: Duration::from_millis(100),
            ..LimitConfiguration::default()
        },
        &CancellationToken::new(),
    )
    .unwrap();
    let pid = response_pid(&response);
    let started = Instant::now();
    response
        .finish(McpFinishOutcome::WrittenAndFlushed)
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert_eq!(fs::read(receipt).unwrap(), b"written_and_flushed\n");
    assert_process_stopped(pid);
}

#[cfg(target_os = "linux")]
#[test]
fn queued_outcome_wins_over_cancellation_and_writes_nack() {
    let fixture = Fixture::new(
        b"#!/bin/sh\nIFS= read -r _\n/usr/bin/sleep 60 &\nprintf '%s\\n' \"$!\"\nIFS= read -r receipt\nprintf '%s\\n' \"$receipt\" > \"${0}.receipt\"\nexit 0\n",
    );
    let receipt = fixture.pro.with_extension("operation.receipt");
    let cancellation = CancellationToken::new();
    let response = launch_mcp_with(
        &fixture,
        McpRequest::new(b"request\n"),
        LimitConfiguration {
            captured_wall_time: Duration::from_secs(2),
            ..LimitConfiguration::default()
        },
        &cancellation,
    )
    .unwrap();
    let pid = response_pid(&response);
    cancellation.cancel();
    response.finish(McpFinishOutcome::OutputFailed).unwrap();
    assert_eq!(fs::read(receipt).unwrap(), b"output_failed\n");
    assert_process_stopped(pid);
}

#[cfg(target_os = "linux")]
#[test]
fn dropping_pending_response_is_unknown_and_sends_no_terminal_receipt() {
    let fixture = Fixture::new(
        b"#!/bin/sh\nIFS= read -r _\n/usr/bin/sleep 60 &\nprintf '%s\\n' \"$!\"\nif IFS= read -r receipt; then printf '%s\\n' \"$receipt\" > \"${0}.receipt\"; fi\nwait\n",
    );
    let response = launch_mcp_with(
        &fixture,
        McpRequest::new(b"request\n"),
        LimitConfiguration::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let operation = fixture.pro.with_extension("operation");
    let pid = response_pid(&response);
    let started = Instant::now();
    drop(response);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!PathBuf::from(format!("{}.receipt", operation.display())).exists());
    assert_process_stopped(pid);
}

#[cfg(target_os = "linux")]
#[test]
fn dropping_with_an_escaped_pipe_descendant_is_bounded_and_releases_the_permit() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os,pathlib,subprocess,sys\nsys.stdin.buffer.readline()\nchild=subprocess.Popen(['/usr/bin/sleep','60'],start_new_session=True)\nos.write(1,f'{child.pid}\\n'.encode())\nreceipt=sys.stdin.buffer.readline()\npathlib.Path(sys.argv[0]).with_suffix('.receipt').write_bytes(receipt)\n",
    );
    let bridge = CompanionBridge::new(
        BridgeLimits::new(LimitConfiguration {
            concurrent_processes: 1,
            admission_wait: Duration::from_millis(100),
            ..LimitConfiguration::default()
        })
        .unwrap(),
    );
    let response = bridge
        .launch_mcp(
            &fixture.companion(),
            McpRequest::new(b"request\n"),
            &CancellationToken::new(),
        )
        .unwrap();
    let escaped_pid = response_pid(&response);
    let receipt = fixture.pro.with_extension("receipt");
    let started = Instant::now();
    drop(response);
    assert!(started.elapsed() < Duration::from_secs(1));

    let permit = bridge
        .gate
        .acquire(&CancellationToken::new(), Duration::from_millis(100))
        .unwrap();
    drop(permit);

    assert!(!receipt.exists());
    unsafe {
        libc::kill(escaped_pid as libc::pid_t, libc::SIGKILL);
    }
    assert_process_stopped(escaped_pid);
}

#[test]
fn receipt_write_failure_is_typed_and_still_reaps_the_child() {
    let fixture = Fixture::new(
        b"#!/usr/bin/python3\nimport os,sys,time\nsys.stdin.buffer.readline()\nos.write(1,b'response\\n')\nos.close(0)\ntime.sleep(60)\n",
    );
    let response = launch_mcp_with(
        &fixture,
        McpRequest::new(b"request\n"),
        LimitConfiguration::default(),
        &CancellationToken::new(),
    )
    .unwrap();
    let started = Instant::now();
    let error = response
        .finish(McpFinishOutcome::WrittenAndFlushed)
        .unwrap_err();
    assert!(matches!(error, BridgeError::Transport(_)));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(target_os = "linux")]
fn response_pid(response: &McpResponse) -> u32 {
    String::from_utf8(response.response_frame().to_vec())
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[cfg(target_os = "linux")]
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
fn admission_queue_wait_remains_bounded() {
    let gate = Arc::new(ConcurrencyGate::new(1));
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

#[test]
fn configured_concurrency_is_a_real_gate() {
    let fixture = Arc::new(Fixture::new(
        b"#!/bin/sh\nIFS= read -r _\n/usr/bin/sleep 0.2\nprintf 'done\\n'\nIFS= read -r receipt\n[ \"$receipt\" = written_and_flushed ]\n",
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
            let response = bridge
                .launch_mcp(
                    &fixture.companion(),
                    McpRequest::new(b"request\n"),
                    &CancellationToken::new(),
                )
                .unwrap();
            let frame = response.response_frame().to_vec();
            response
                .finish(McpFinishOutcome::WrittenAndFlushed)
                .unwrap();
            frame
        }));
    }
    for worker in workers {
        assert_eq!(worker.join().unwrap(), b"done\n");
    }
    assert!(started.elapsed() >= Duration::from_millis(350));
}

#[test]
fn admission_and_captured_lifetime_bounds_are_independent() {
    assert!(matches!(
        BridgeLimits::new(LimitConfiguration {
            admission_wait: Duration::ZERO,
            ..LimitConfiguration::default()
        }),
        Err(BridgeError::Limit("admission wait"))
    ));
    assert!(matches!(
        BridgeLimits::new(LimitConfiguration {
            captured_wall_time: Duration::ZERO,
            ..LimitConfiguration::default()
        }),
        Err(BridgeError::Limit("captured wall time"))
    ));
}
