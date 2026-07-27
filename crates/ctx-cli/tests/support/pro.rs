#[cfg(unix)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[cfg(unix)]
const PYTHON3_SHEBANG: &str = "#!/usr/bin/python3\n";

#[cfg(unix)]
fn python3_candidates(target_os: &str) -> &'static [&'static str] {
    match target_os {
        "linux" => &["/usr/bin/python3", "/usr/local/bin/python3"],
        "freebsd" => &[
            "/usr/local/bin/python3",
            "/usr/local/bin/python3.14",
            "/usr/local/bin/python3.13",
            "/usr/local/bin/python3.12",
            "/usr/local/bin/python3.11",
            "/usr/local/bin/python3.10",
            "/usr/bin/python3",
            "/usr/bin/python3.14",
            "/usr/bin/python3.13",
            "/usr/bin/python3.12",
            "/usr/bin/python3.11",
            "/usr/bin/python3.10",
        ],
        "macos" => &[
            "/usr/bin/python3",
            "/opt/homebrew/bin/python3",
            "/usr/local/bin/python3",
        ],
        _ => &["/usr/bin/python3", "/usr/local/bin/python3"],
    }
}

#[cfg(unix)]
pub(crate) fn select_python3_interpreter(
    target_os: &str,
    mut canonicalize: impl FnMut(&Path) -> Option<PathBuf>,
    mut usable: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    python3_candidates(target_os)
        .iter()
        .copied()
        .filter_map(|candidate| {
            let candidate = Path::new(candidate);
            candidate.is_absolute().then(|| canonicalize(candidate))?
        })
        .find(|canonical| canonical.is_absolute() && usable(canonical))
}

#[cfg(unix)]
pub(crate) fn write_python_helper(path: &Path, body: &str) {
    let interpreter = select_python3_interpreter(
        std::env::consts::OS,
        |candidate| candidate.canonicalize().ok(),
        |canonical| {
            fs::symlink_metadata(canonical)
                .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        },
    )
    .unwrap_or_else(|| {
        panic!(
            "no executable Python 3 interpreter found in bounded {} test candidates",
            std::env::consts::OS
        )
    });
    let body = body
        .strip_prefix(PYTHON3_SHEBANG)
        .expect("Python helper fixture must use the canonical test shebang");
    fs::write(path, format!("#!{}\n{body}", interpreter.display())).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
pub(crate) fn initialize_current_query_store(data_root: &Path) {
    initialize_pro_installation_identity(data_root);
    let store = ctx_history_store::Store::open(data_root.join("work.sqlite")).unwrap();
    let checkpoint = store
        .activate_projection_journal(ctx_pro_host_protocol::PROTOCOL_FINGERPRINT)
        .unwrap();
    assert_eq!(checkpoint.position.sequence, 0);
}

#[cfg(unix)]
pub(crate) fn initialize_pro_installation_identity(data_root: &Path) {
    fs::create_dir_all(data_root).unwrap();
    fs::set_permissions(data_root, fs::Permissions::from_mode(0o700)).unwrap();
    let path = ctx_pro_host_protocol::ProFilesystemLayout::new(data_root).installation_id_path();
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1,
        "install_id": uuid::Uuid::new_v4().to_string(),
        "created_at": "2026-07-22T00:00:00Z",
    }))
    .unwrap();
    fs::write(&path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(unix)]
pub(crate) fn write_locate_helper(path: &Path) {
    const HELPER: &str = r#"#!/usr/bin/python3
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

hello = receive()
if 'query' not in hello['message']['body']['capabilities']:
    sys.exit(21)
send({
  'sequence': hello['sequence'],
  'request_id': hello['request_id'],
  'message': {'kind':'hello','body':{
    'protocol_version':1,
    'protocol_fingerprint':'f9c77c0df491f276dd3d8c2cdb7f6c95daf8ebb9a216b2ca9a158ff0be1024c9',
    'helper_version':'fake-locate-v1',
    'authorization_challenge_base64url':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    'capabilities':['query']
  }}
})
request = receive()
body = request['message']['body']
if request['message']['kind'] != 'query' or body['kind'] != 'locate':
    sys.exit(22)
target = body['target']
send({
  'sequence': request['sequence'],
  'request_id': request['request_id'],
  'message': {'kind':'query','body':{'records':[{
    'resource': {'id':target['kind'] + ':' + target['value'],'kind':target['kind'],'display':target['value']},
    'summary': 'Exact canonical evidence location',
    'occurred_at_ms': 1,
    'facts': [],
    'citations': [{'event_id':'00000000-0000-0000-0000-000000000001','event_seq':1}]
  }],'next_cursor':None,'truncated':False,'stale':False}}
})
"#;
    write_python_helper(path, HELPER);
}

#[cfg(unix)]
pub(crate) fn write_startup_error_helper(path: &Path, error_class: &str) {
    assert!(matches!(
        error_class,
        "key_store_unavailable" | "key_store_locked" | "entitlement_expired"
    ));
    let helper = format!(
        r#"#!/usr/bin/python3
import json, struct, sys

header = sys.stdin.buffer.read(12)
if len(header) != 12 or header[:6] != b'CTXPRO':
    sys.exit(20)
size = struct.unpack('>I', header[8:12])[0]
hello = json.loads(sys.stdin.buffer.read(size))
response = {{
  'sequence': hello['sequence'],
  'request_id': hello['request_id'],
  'message': {{'kind':'error','body':{{
    'class':'{error_class}',
    'message':'private helper detail at /secret/key-store/path',
    'retryable':False
  }}}}
}}
payload = json.dumps(response, separators=(',', ':')).encode()
sys.stdout.buffer.write(b'CTXPRO' + struct.pack('>H', 1) + struct.pack('>I', len(payload)) + payload)
sys.stdout.buffer.flush()
"#
    );
    write_python_helper(path, &helper);
}
