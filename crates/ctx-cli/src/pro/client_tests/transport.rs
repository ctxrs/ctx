use super::*;

#[cfg(unix)]
fn write_smoke_helper(path: &Path, capabilities: &str) {
    let script = format!(
        r#"#!/usr/bin/python3
import json, os, struct, sys

git_executable = os.environ.get('CTX_PRO_GIT_EXECUTABLE')
if not git_executable or not os.path.isabs(git_executable):
    sys.exit(19)

def receive():
    header = sys.stdin.buffer.read(12)
    if len(header) != 12 or header[:8] != b'CTXPRO\x00\x01':
        sys.exit(20)
    size = struct.unpack('>I', header[8:12])[0]
    return json.loads(sys.stdin.buffer.read(size))

def send(request, kind, body):
    value = {{'sequence':request['sequence'],'request_id':request['request_id'],
             'message':{{'kind':kind,'body':body}}}}
    payload = json.dumps(value, separators=(',', ':')).encode()
    sys.stdout.buffer.write(b'CTXPRO\x00\x01' + struct.pack('>I', len(payload)) + payload)
    sys.stdout.buffer.flush()

hello = receive()
send(hello, 'hello', {{
    'protocol_version':1,
    'protocol_fingerprint':'{PROTOCOL_FINGERPRINT}',
    'helper_version':'staged-smoke-test',
    'capabilities':{capabilities},
    'authorization_challenge_base64url':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'
}})
if 'entitlement_authorization' not in {capabilities}:
    sys.exit(0)
authorization = receive()
if authorization['message']['kind'] != 'authorize':
    sys.exit(21)
send(authorization, 'authorized', {{
    'state':'active','refresh_required':False,'expires_at_unix':5,
    'access_deadline_unix':3,'grace_deadline_unix':4,'capabilities':['graph_read']
}})
status = receive()
if status['message']['kind'] != 'status':
    sys.exit(22)
send(status, 'status', {{'state':'ready','checkpoint':None}})
"#
    );
    fs::write(path, script).expect("write helper");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("make helper executable");
}

#[cfg(target_os = "linux")]
fn write_nonreading_journal_helper(path: &Path) {
    let script = format!(
        r#"#!/usr/bin/python3
import json, os, struct, sys, time

def receive():
    header = sys.stdin.buffer.read(12)
    if len(header) != 12 or header[:8] != b'CTXPRO\x00\x01':
        sys.exit(20)
    size = struct.unpack('>I', header[8:12])[0]
    return json.loads(sys.stdin.buffer.read(size))

def send(request, kind, body):
    value = {{'sequence':request['sequence'],'request_id':request['request_id'],
             'message':{{'kind':kind,'body':body}}}}
    payload = json.dumps(value, separators=(',', ':')).encode()
    sys.stdout.buffer.write(b'CTXPRO\x00\x01' + struct.pack('>I', len(payload)) + payload)
    sys.stdout.buffer.flush()

hello = receive()
with open(sys.argv[0] + '.pid', 'w', encoding='ascii') as pid_file:
    pid_file.write(str(os.getpid()))
send(hello, 'hello', {{
    'protocol_version':1,
    'protocol_fingerprint':'{PROTOCOL_FINGERPRINT}',
    'helper_version':'nonreading-journal-test',
    'capabilities':['journal_sync'],
    'authorization_challenge_base64url':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'
}})
while True:
    time.sleep(3600)
"#
    );
    fs::write(path, script).expect("write non-reading helper");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("make non-reading helper executable");
}

#[cfg(target_os = "linux")]
fn valid_large_journal_request(payload_bytes: usize) -> JournalSyncRequest {
    let initial_digest = initial_journal_digest(1);
    let payload = json!({"body": "x".repeat(payload_bytes)});
    let stable_entity_id = Uuid::from_u128(101);
    let provenance = JournalProvenanceIdentity {
        entity_kind: JournalEntityKind::Event,
        stable_entity_id,
        capture_source_id: None,
        provider: None,
        provider_external_id: None,
    };
    let mut record = JournalRecord {
        generation: 1,
        sequence: 1,
        projection_contract_version: ctx_pro_host_protocol::PROJECTION_CONTRACT_VERSION,
        entity_kind: JournalEntityKind::Event,
        stable_entity_id,
        entity_revision: 1,
        operation: JournalOperation::Upsert,
        canonical_payload: Some(payload.clone()),
        payload_sha256: ctx_pro_host_protocol::sha256_hex(
            &ctx_pro_host_protocol::canonical_payload_bytes(&payload)
                .expect("encode canonical payload"),
        ),
        evidence: Vec::new(),
        provenance,
        cumulative_digest: "0".repeat(64),
    };
    record.cumulative_digest =
        ctx_pro_host_protocol::journal_record_digest(&initial_digest, &record)
            .expect("chain journal record");
    let request = JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 1,
        canonical_schema_identity: "ctx-store-schema-test".to_owned(),
        projection_contract_version: ctx_pro_host_protocol::PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: JournalCheckpoint {
            position: JournalPosition {
                generation: 1,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial_digest.clone(),
        },
        context: JournalContextWindow {
            base_checkpoint: JournalCheckpoint {
                position: JournalPosition {
                    generation: 1,
                    sequence: 0,
                },
                contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
                cumulative_digest: initial_digest,
            },
            records: Vec::new(),
        },
        frozen_through: JournalCheckpoint {
            position: JournalPosition {
                generation: 1,
                sequence: 1,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: record.cumulative_digest.clone(),
        },
        authorized_repository_roots: Vec::new(),
        records: vec![record],
    };
    request.validate().expect("large journal request is valid");
    request
}

#[cfg(target_os = "linux")]
#[test]
fn exchange_deadline_covers_a_request_write_blocked_by_a_nonreading_helper() {
    let temp = tempdir().expect("temp dir");
    crate::identity::installation_id(temp.path()).expect("installation identity");
    let helper = temp.path().join("ctx-pro-nonreading-journal");
    write_nonreading_journal_helper(&helper);

    let required = BTreeSet::from([Capability::JournalSync]);
    let mut client = ProClient::connect_to_path_with_authorization_mode(
        temp.path(),
        &helper,
        None,
        &required,
        None,
        false,
    )
    .expect("helper handshake");
    let stdin = client.stdin.as_ref().expect("helper stdin");
    let pipe_capacity = unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_GETPIPE_SZ) };
    assert!(
        pipe_capacity > 0,
        "read helper pipe capacity: {}",
        std::io::Error::last_os_error()
    );
    let pipe_capacity = usize::try_from(pipe_capacity).expect("positive pipe capacity");
    let request = valid_large_journal_request(pipe_capacity + 256 * 1024);
    let encoded = serde_json::to_vec(&HostEnvelope {
        sequence: client.sequence,
        request_id: Uuid::from_u128(1),
        message: HostMessage::SyncJournal(request.clone()),
    })
    .expect("encode framed request");
    assert!(
        encoded.len() > pipe_capacity,
        "request payload {} must exceed helper pipe capacity {pipe_capacity}",
        encoded.len()
    );

    let started = Instant::now();
    let error = client
        .exchange(
            HostMessage::SyncJournal(request),
            Duration::from_millis(250),
        )
        .expect_err("blocked write must time out");
    assert_eq!(stable_error_code(&error), Some("helper_timeout"));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "blocked exchange outlived its bounded cleanup"
    );
    assert!(client.stdin.is_none(), "timed-out helper stdin stayed open");

    let pid: i32 = fs::read_to_string(format!("{}.pid", helper.display()))
        .expect("read helper pid")
        .parse()
        .expect("parse helper pid");
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "timed-out non-reading helper remained alive"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[cfg(unix)]
#[test]
fn staged_smoke_binds_git_and_proves_authorization_and_status_before_success() {
    let temp = tempdir().expect("temp dir");
    crate::identity::installation_id(temp.path()).expect("installation identity");
    let helper = temp.path().join("ctx-pro-smoke");
    write_smoke_helper(&helper, "['entitlement_authorization','status']");
    let authorization = RecordingAuthorization {
        calls: Cell::new(0),
    };
    let (smoke, status) =
        super::super::client_status::smoke_helper_at_path_with_authorization_and_status(
            temp.path(),
            &helper,
            Some(&authorization),
        )
        .expect("full staged smoke");
    assert_eq!(authorization.calls.get(), 1);
    assert_eq!(status.state, GraphState::Ready);
    assert!(smoke
        .capabilities
        .contains(&Capability::EntitlementAuthorization));
}

#[cfg(unix)]
#[test]
fn staged_smoke_rejects_a_helper_without_entitlement_authorization() {
    let temp = tempdir().expect("temp dir");
    crate::identity::installation_id(temp.path()).expect("installation identity");
    let helper = temp.path().join("ctx-pro-smoke");
    write_smoke_helper(&helper, "['status']");
    let authorization = RecordingAuthorization {
        calls: Cell::new(0),
    };
    let error = smoke_helper_at_path_with_authorization(temp.path(), &helper, Some(&authorization))
        .expect_err("missing entitlement capability must fail");
    assert!(error.to_string().starts_with("protocol_mismatch:"));
    assert_eq!(authorization.calls.get(), 0);
}
