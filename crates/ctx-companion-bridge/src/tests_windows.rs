#![cfg(windows)]

use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use super::*;

struct Fixture {
    _temp: tempfile::TempDir,
    pro: PathBuf,
}

impl Fixture {
    fn new(operation: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let pro = temp.path().join("ctx-pro.cmd");
        fs::write(
            &pro,
            format!(
                "@echo off\r\nif not \"%~1\"==\"--ctx-pro-protocol-v4\" exit /b 90\r\nif not \"%~2\"==\"mcp-serve\" exit /b 91\r\n{operation}\r\n"
            ),
        )
        .unwrap();
        Self { _temp: temp, pro }
    }

    fn companion(&self) -> InstalledCompanion {
        InstalledCompanion::new(&self.pro)
    }

    fn receipt(&self) -> PathBuf {
        PathBuf::from(format!("{}.receipt", self.pro.display()))
    }
}

fn launch(fixture: &Fixture) -> Result<McpResponse, BridgeError> {
    launch_request(fixture, b"request\n".to_vec())
}

fn launch_request(fixture: &Fixture, request: Vec<u8>) -> Result<McpResponse, BridgeError> {
    launch_request_with(fixture, request, LimitConfiguration::default())
}

fn launch_request_with(
    fixture: &Fixture,
    request: Vec<u8>,
    limits: LimitConfiguration,
) -> Result<McpResponse, BridgeError> {
    CompanionBridge::new(BridgeLimits::new(limits).unwrap()).launch_mcp(
        &fixture.companion(),
        McpRequest::new(request),
        &CancellationToken::new(),
    )
}

#[test]
fn windows_known_outcome_wins_over_the_generic_deadline() {
    let fixture = Fixture::new(
        "set /p request=\r\necho response:%request%\r\nset /p receipt=\r\n>\"%~f0.receipt\" echo %receipt%\r\nif \"%receipt%\"==\"written_and_flushed\" exit /b 0\r\nexit /b 9",
    );
    let response = launch_request_with(
        &fixture,
        b"request\n".to_vec(),
        LimitConfiguration {
            captured_wall_time: Duration::from_millis(100),
            ..LimitConfiguration::default()
        },
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(125));
    response
        .finish(McpFinishOutcome::WrittenAndFlushed)
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.receipt()).unwrap().trim(),
        "written_and_flushed"
    );
}

#[test]
fn windows_mcp_ack_and_nack_reach_one_child_and_reap_it() {
    for (outcome, expected) in [
        (McpFinishOutcome::WrittenAndFlushed, "written_and_flushed"),
        (McpFinishOutcome::OutputFailed, "output_failed"),
    ] {
        let fixture = Fixture::new(
            "set /p request=\r\necho response:%request%\r\nset /p receipt=\r\n>\"%~f0.receipt\" echo %receipt%\r\nif \"%receipt%\"==\"written_and_flushed\" exit /b 0\r\nif \"%receipt%\"==\"output_failed\" exit /b 0\r\nexit /b 9",
        );
        let response = launch(&fixture).unwrap();
        assert_eq!(response.response_frame(), b"response:request\r\n");
        response.finish(outcome).unwrap();
        assert_eq!(
            fs::read_to_string(fixture.receipt()).unwrap().trim(),
            expected
        );
    }
}

#[test]
fn windows_unknown_drop_with_inherited_pipes_is_bounded_and_releases_the_permit() {
    let fixture = Arc::new(Fixture::new(
        "set /p request=\r\nstart \"\" /b \"%ComSpec%\" /d /c \"timeout /t 60 /nobreak ^>nul\"\r\necho response:%request%\r\nset /p receipt=\r\n>\"%~f0.receipt\" echo %receipt%",
    ));
    let bridge = CompanionBridge::new(
        BridgeLimits::new(LimitConfiguration {
            concurrent_processes: 1,
            admission_wait: Duration::from_millis(250),
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
    let started = Instant::now();
    drop(response);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!fixture.receipt().exists());

    let permit = bridge
        .gate
        .acquire(&CancellationToken::new(), Duration::from_millis(250))
        .unwrap();
    drop(permit);
}

#[test]
fn windows_mcp_child_crash_is_bounded() {
    let fixture = Fixture::new("set /p request=\r\nexit /b 23");
    let started = Instant::now();
    let error = launch(&fixture).unwrap_err();
    assert!(matches!(
        error,
        BridgeError::McpExchangeFailed {
            exit: ExitClass::Code(23)
        } | BridgeError::InvalidProtocolResponse("MCP response frame")
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn windows_malformed_response_is_rejected_and_reaped() {
    let fixture = Fixture::new(
        "set /p request=\r\necho response-one\r\necho response-two\r\nset /p receipt=",
    );
    let started = Instant::now();
    let error = launch(&fixture).unwrap_err();
    assert!(matches!(
        error,
        BridgeError::InvalidProtocolResponse("MCP response frame")
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn windows_blocked_receipt_reaches_one_deadline_and_joins_the_writer() {
    // The child reads exactly enough of a 64 KiB request for the remaining
    // 4 KiB to occupy the anonymous pipe, then returns its response without
    // consuming those bytes. The receipt write must therefore be cancelled
    // through the same owned PipeWriter mechanism at the test deadline.
    let fixture = Fixture::new(
        r#"powershell.exe -NoLogo -NoProfile -NonInteractive -Command "$inputStream=[Console]::OpenStandardInput();$buffer=New-Object byte[] 61440;$offset=0;while($offset -lt $buffer.Length){$read=$inputStream.Read($buffer,$offset,$buffer.Length-$offset);if($read -eq 0){exit 81};$offset += $read};[Console]::Out.WriteLine('response');[Console]::Out.Flush();Start-Sleep -Seconds 60""#,
    );
    let mut request = vec![b'x'; 64 * 1024];
    *request.last_mut().unwrap() = b'\n';
    let response = launch_request(&fixture, request).unwrap();
    assert_eq!(response.response_frame(), b"response\n");
    let started = Instant::now();
    let error = response
        .finish(McpFinishOutcome::WrittenAndFlushed)
        .unwrap_err();
    assert!(matches!(error, BridgeError::Transport(_)));
    assert!(started.elapsed() >= Duration::from_millis(400));
    assert!(started.elapsed() < Duration::from_secs(2));
}
