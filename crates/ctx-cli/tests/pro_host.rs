#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

mod support;
use support::{
    initialize_current_query_store, initialize_empty_store, initialize_pro_installation_identity,
    write_locate_helper, write_python_helper, write_startup_error_helper,
};

fn write_helper(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

const SUCCESS_HELPER: &str = r#"#!/usr/bin/python3
import json, os, struct, sys

if os.path.basename(os.environ.get('CTX_DATA_ROOT', '')) == 'require-secret-service':
    if os.environ.get('DBUS_SESSION_BUS_ADDRESS') != 'unix:path=/ctx-test/bus':
        sys.exit(21)
    if os.environ.get('XDG_RUNTIME_DIR') != '/ctx-test/runtime':
        sys.exit(22)
    if os.environ.get('CTX_TEST_SECRET') is not None:
        sys.exit(23)
    git_executable = os.environ.get('CTX_PRO_GIT_EXECUTABLE')
    if not git_executable or not os.path.isabs(git_executable):
        sys.exit(24)
    if os.environ.get('PATH') is not None:
        sys.exit(25)

def receive():
    header = sys.stdin.buffer.read(12)
    if len(header) != 12 or header[:6] != b'CTXPRO':
        sys.exit(20)
    size = struct.unpack('>I', header[8:12])[0]
    return json.loads(sys.stdin.buffer.read(size))

def send(value):
    payload = json.dumps(value, separators=(',', ':')).encode()
    sys.stdout.buffer.write(b'CTXPRO' + struct.pack('>H', 1) + struct.pack('>I', len(payload)) + payload)
    sys.stdout.buffer.flush()

hello = receive()
send({
  'sequence': hello['sequence'],
  'request_id': hello['request_id'],
  'message': {'kind':'hello','body':{
    'protocol_version':1,
    'protocol_fingerprint':'f9c77c0df491f276dd3d8c2cdb7f6c95daf8ebb9a216b2ca9a158ff0be1024c9',
    'helper_version':'fake-e2e',
    'authorization_challenge_base64url':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    'capabilities':['query']
  }}
})
request = receive()
send({
  'sequence': request['sequence'],
  'request_id': request['request_id'],
  'message': {'kind':'query','body':{'records':[{
    'resource': {'id':'commit:0123456789abcdef','kind':'commit','display':'0123456789abcdef'},
    'summary': 'Produced by a local agent session',
    'occurred_at_ms': 1,
    'facts': [{
      'id':'fact:test',
      'fact_type':'commit_produced',
      'subject': {'id':'commit:0123456789abcdef','kind':'commit','display':'0123456789abcdef'},
      'predicate':'produced_by',
      'object': {'type':'text','value':'session'},
      'confidence':'explicit',
      'state':'asserted',
      'detector_version':'test-v1',
      'owning_root_session_id':None,
      'direct_actor_session_id':None,
      'citations':[{'event_id':'00000000-0000-0000-0000-000000000001','event_seq':1}]
    }],
    'citations': []
  }],'next_cursor':None,'truncated':False,'stale':False}}
})
"#;

const PAGINATED_HELPER: &str = r#"#!/usr/bin/python3
import json, struct, sys

def receive():
    header = sys.stdin.buffer.read(12)
    if len(header) != 12 or header[:6] != b'CTXPRO':
        sys.exit(20)
    size = struct.unpack('>I', header[8:12])[0]
    return json.loads(sys.stdin.buffer.read(size))

def send(value):
    payload = json.dumps(value, separators=(',', ':')).encode()
    sys.stdout.buffer.write(b'CTXPRO' + struct.pack('>H', 1) + struct.pack('>I', len(payload)) + payload)
    sys.stdout.buffer.flush()

def fail(request, detail):
    send({
      'sequence': request['sequence'],
      'request_id': request['request_id'],
      'message': {'kind':'error','body':{
        'class':'invalid_request',
        'message':'private cursor detail at /secret/graph.db: ' + detail,
        'retryable':False
      }}
    })
    sys.exit(0)

hello = receive()
supported = {'query'}
capabilities = [
    capability for capability in hello['message']['body']['capabilities']
    if capability in supported
]
send({
  'sequence': hello['sequence'],
  'request_id': hello['request_id'],
  'message': {'kind':'hello','body':{
    'protocol_version':1,
    'protocol_fingerprint':'f9c77c0df491f276dd3d8c2cdb7f6c95daf8ebb9a216b2ca9a158ff0be1024c9',
    'helper_version':'fake-pagination-v1',
    'authorization_challenge_base64url':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    'capabilities':capabilities
  }}
})

request = receive()
body = request['message']['body']
kind = body['kind']
target = body['target']['value']
cursor = body.get('cursor')
expected_cursor = 'cursor-' + kind + '-page-2'
if cursor is not None and cursor != expected_cursor:
    fail(request, 'invalid or tampered')
if cursor == expected_cursor and target != '0123456789abcdef':
    fail(request, 'query fingerprint mismatch')

page = 2 if cursor == expected_cursor else 1
next_cursor = expected_cursor if page == 1 else None
send({
  'sequence': request['sequence'],
  'request_id': request['request_id'],
  'message': {'kind':'query','body':{
    'records':[{
      'resource': {
        'id':'commit:' + kind + '-page-' + str(page),
        'kind':'commit',
        'display':kind + '-page-' + str(page)
      },
      'summary':'Page ' + str(page),
      'occurred_at_ms':page,
      'facts':[],
      'citations':[{
        'event_id':'00000000-0000-0000-0000-00000000000' + str(page),
        'event_seq':page
      }]
    }],
    'next_cursor':next_cursor,
    'truncated':page == 1,
    'stale':False
  }}
})
"#;

const OLD_V2_HELPER: &str = r#"#!/usr/bin/python3
import json, struct, sys

header = sys.stdin.buffer.read(12)
if len(header) != 12 or header[:6] != b'CTXPRO':
    sys.exit(20)
if struct.unpack('>H', header[6:8])[0] != 1:
    sys.exit(21)
size = struct.unpack('>I', header[8:12])[0]
hello = json.loads(sys.stdin.buffer.read(size))
response = {
  'sequence': hello['sequence'],
  'request_id': hello['request_id'],
  'message': {'kind':'hello','body':{
    'protocol_version':2,
    'protocol_fingerprint':'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
    'helper_version':'frozen-v2-helper',
    'capabilities':['query']
  }}
}
payload = json.dumps(response, separators=(',', ':')).encode()
sys.stdout.buffer.write(b'CTXPRO' + struct.pack('>H', 2) + struct.pack('>I', len(payload)) + payload)
sys.stdout.buffer.flush()
"#;

#[test]
fn ctx_status_reports_when_pro_helper_is_missing() {
    let root = tempdir().unwrap();
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_CHANNEL", "staging")
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "status",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"installed\": false"))
        .stdout(predicate::str::contains("pro_not_installed"));
}

#[test]
fn query_negotiates_with_an_exact_fake_helper_path() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-fake");
    write_python_helper(&helper, SUCCESS_HELPER);
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "facts",
            "commit",
            "0123456789abcdef",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"payload_type\": \"pro_facts\""))
        .stdout(predicate::str::contains("\"fact:test\""))
        .stdout(predicate::str::contains(
            "00000000-0000-0000-0000-000000000001",
        ));
}

#[test]
fn exact_v1_host_rejects_an_old_v2_helper() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-v2");
    write_python_helper(&helper, OLD_V2_HELPER);
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "facts",
            "commit",
            "0123456789abcdef",
        ])
        .assert()
        .failure()
        .stderr(predicate::eq(
            "Error: protocol_mismatch: the Pro helper needs repair; run `ctx pro`\n",
        ));
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[test]
fn helper_inherits_only_required_secret_service_environment() {
    let root = tempdir().unwrap();
    let data_root = root.path().join("require-secret-service");
    initialize_current_query_store(&data_root);
    let helper = root.path().join("ctx-pro-environment");
    write_python_helper(&helper, SUCCESS_HELPER);
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/ctx-test/bus")
        .env("XDG_RUNTIME_DIR", "/ctx-test/runtime")
        .env("CTX_TEST_SECRET", "must-not-survive")
        .args([
            "--data-root",
            data_root.to_str().unwrap(),
            "facts",
            "commit",
            "0123456789abcdef",
            "--json",
        ])
        .assert()
        .success();
}

#[test]
fn missing_git_fails_before_starting_the_helper() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let marker = root.path().join("helper-was-started");
    let helper = root.path().join("ctx-pro-must-not-start");
    write_helper(
        &helper,
        &format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 91\n",
            marker.display()
        ),
    );
    let empty_path = root.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .env("PATH", &empty_path)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "facts",
            "commit",
            "abc",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("repository_unavailable"))
        .stderr(predicate::str::contains("helper_crashed").not());
    assert!(!marker.exists());
}

#[test]
fn paginated_cli_commands_forward_cursors_without_gaps_or_duplicates() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-pagination");
    write_python_helper(&helper, PAGINATED_HELPER);

    for (command, payload_type) in [("facts", "pro_facts"), ("timeline", "pro_timeline")] {
        let first = Command::cargo_bin("ctx")
            .unwrap()
            .env("CTX_PRO_HELPER", &helper)
            .args([
                "--data-root",
                root.path().to_str().unwrap(),
                command,
                "commit",
                "0123456789abcdef",
                "--limit",
                "1",
                "--json",
            ])
            .output()
            .unwrap();
        assert!(
            first.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&first.stderr)
        );
        let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
        assert_eq!(first["payload_type"], payload_type);
        let cursor = first["pagination"]["next_cursor"]
            .as_str()
            .expect("first page must provide a cursor");
        assert_eq!(cursor, format!("cursor-{command}-page-2"));

        let second = Command::cargo_bin("ctx")
            .unwrap()
            .env("CTX_PRO_HELPER", &helper)
            .args([
                "--data-root",
                root.path().to_str().unwrap(),
                command,
                "commit",
                "0123456789abcdef",
                "--limit",
                "1",
                "--cursor",
                cursor,
                "--json",
            ])
            .output()
            .unwrap();
        assert!(
            second.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&second.stderr)
        );
        let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();

        let displays = [&first, &second]
            .into_iter()
            .map(|page| page["results"][0]["resource"]["display"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            displays,
            vec![format!("{command}-page-1"), format!("{command}-page-2")]
        );
        assert_eq!(second["pagination"]["next_cursor"], serde_json::Value::Null);
        assert_eq!(second["pagination"]["truncated"], false);
    }
}

#[test]
fn invalid_tampered_and_filter_mismatch_cursors_are_typed_and_sanitized() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-pagination-errors");
    write_python_helper(&helper, PAGINATED_HELPER);

    for (command, target, cursor) in [
        ("facts", "0123456789abcdef", "not-a-valid-cursor"),
        ("facts", "0123456789abcdef", "cursor-facts-page-X"),
        ("facts", "different-filter", "cursor-facts-page-2"),
    ] {
        let output = Command::cargo_bin("ctx")
            .unwrap()
            .env("CTX_PRO_HELPER", &helper)
            .args([
                "--data-root",
                root.path().to_str().unwrap(),
                command,
                "commit",
                target,
                "--cursor",
                cursor,
                "--json",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(stderr, "Error: invalid_request\n");
        assert!(!stderr.contains("private cursor detail"));
        assert!(!stderr.contains("/secret/graph.db"));
    }
}

#[test]
fn cli_help_exposes_cursors_only_for_paginated_work_graph_commands() {
    for command in ["facts", "timeline"] {
        Command::cargo_bin("ctx")
            .unwrap()
            .args([command, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("--cursor <CURSOR>"));
    }
    for arguments in [
        &["show", "commit", "--help"][..],
        &["locate", "commit", "--help"][..],
        &["blame", "--help"][..],
    ] {
        Command::cargo_bin("ctx")
            .unwrap()
            .args(arguments)
            .assert()
            .success()
            .stdout(predicate::str::contains("--cursor").not());
    }
}

#[test]
fn cli_help_does_not_advertise_the_developer_helper_override() {
    Command::cargo_bin("ctx")
        .unwrap()
        .args(["pro", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("CTX_PRO_HELPER").not());
}

#[test]
fn advanced_locate_negotiates_and_sends_the_distinct_locate_operation() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-locate");
    write_locate_helper(&helper);
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "locate",
            "commit",
            "0123456789abcdef",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"payload_type\": \"pro_location\"",
        ))
        .stdout(predicate::str::contains(
            "Exact canonical evidence location",
        ))
        .stdout(predicate::str::contains(
            "00000000-0000-0000-0000-000000000001",
        ));
}

#[test]
fn invalid_helper_frames_are_rejected_without_exposing_stderr() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-invalid");
    write_helper(
        &helper,
        "#!/bin/sh\nprintf 'not-a-frame!'\nprintf 'sensitive-helper-stderr' >&2\n/bin/sleep 10\n",
    );
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "facts",
            "commit",
            "abc",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid_response"))
        .stderr(predicate::str::contains("sensitive-helper-stderr").not());
}

#[test]
fn helper_crash_is_reported_explicitly() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-crash");
    write_helper(&helper, "#!/bin/sh\nexit 7\n");
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "facts",
            "commit",
            "abc",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("helper_crashed"));
}

#[test]
fn hung_helper_is_killed_at_the_handshake_deadline() {
    let root = tempdir().unwrap();
    initialize_current_query_store(root.path());
    let helper = root.path().join("ctx-pro-hang");
    let pid_file = root.path().join("helper.pid");
    write_helper(
        &helper,
        &format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nread ignored\n",
            pid_file.display()
        ),
    );
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "facts",
            "commit",
            "abc",
        ])
        .timeout(std::time::Duration::from_secs(6))
        .assert()
        .failure()
        .stderr(predicate::str::contains("helper_timeout"));
    let pid: i32 = fs::read_to_string(pid_file).unwrap().parse().unwrap();
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "timed-out helper remained alive"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
fn query_without_a_store_fails_before_starting_the_helper() {
    let root = tempdir().unwrap();
    let marker = root.path().join("helper-was-started");
    let helper = root.path().join("ctx-pro-must-not-start");
    write_helper(
        &helper,
        &format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 91\n",
            marker.display()
        ),
    );
    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "facts",
            "commit",
            "abc",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("source_unavailable"))
        .stderr(predicate::str::contains("helper_crashed").not());
    assert!(!marker.exists());
}

#[test]
fn typed_key_store_startup_errors_are_stable_and_sanitized_for_cli_commands() {
    for error_code in ["key_store_unavailable", "key_store_locked"] {
        let root = tempdir().unwrap();
        initialize_empty_store(&root);
        initialize_pro_installation_identity(root.path());
        let helper = root.path().join(format!("ctx-pro-{error_code}"));
        write_startup_error_helper(&helper, error_code);

        let status_output = Command::cargo_bin("ctx")
            .unwrap()
            .env("CTX_PRO_HELPER", &helper)
            .args([
                "--data-root",
                root.path().to_str().unwrap(),
                "status",
                "--json",
            ])
            .output()
            .unwrap();
        assert!(status_output.status.success());
        let status: serde_json::Value = serde_json::from_slice(&status_output.stdout).unwrap();
        let status = &status["pro"];
        assert_eq!(status["installed"], true);
        assert_eq!(status["ready"], false);
        assert_eq!(status["error_code"], error_code);
        assert!(!String::from_utf8_lossy(&status_output.stdout).contains("private helper detail"));

        let output = Command::cargo_bin("ctx")
            .unwrap()
            .env("CTX_PRO_HELPER", &helper)
            .arg("--data-root")
            .arg(root.path())
            .args(["facts", "commit", "abc"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.starts_with(&format!("Error: {error_code}:")));
        assert!(stderr.contains("ctx pro"));
        assert!(!stderr.contains("helper_crashed"));
        assert!(!stderr.contains("private helper detail"));
        assert!(!stderr.contains("/secret/key-store/path"));
    }
}

#[test]
fn expired_entitlement_is_locked_with_manage_guidance() {
    let root = tempdir().unwrap();
    initialize_empty_store(&root);
    initialize_pro_installation_identity(root.path());
    let helper = root.path().join("ctx-pro-entitlement-expired");
    write_startup_error_helper(&helper, "entitlement_expired");

    let output = Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .arg("--data-root")
        .arg(root.path())
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let status = &status["pro"];
    assert_eq!(status["state"], "locked");
    assert_eq!(status["error_code"], "entitlement_expired");
    assert_eq!(status["next_action"]["command"], "ctx pro manage");

    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_HELPER", &helper)
        .arg("--data-root")
        .arg(root.path())
        .args(["facts", "commit", "abc"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("entitlement_expired"))
        .stderr(predicate::str::contains("ctx pro manage"));
}
