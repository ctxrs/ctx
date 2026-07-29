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
    let generation_id = super::initialize_generation_only_sql_projection(data_root);
    assert!(!generation_id.is_empty());
    assert!(
        !data_root.join("work.sqlite").exists(),
        "Pro query fixtures must use only the fresh source-backed epoch"
    );
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
pub(crate) fn write_blame_helper(path: &Path) {
    write_blame_helper_with_oversized_page(path, false);
}

#[cfg(unix)]
pub(crate) fn write_oversized_blame_helper(path: &Path) {
    write_blame_helper_with_oversized_page(path, true);
}

#[cfg(unix)]
fn write_blame_helper_with_oversized_page(path: &Path, oversized_page: bool) {
    const HELPER: &str = r#"#!/usr/bin/python3
import base64, json, os, struct, sys

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
capabilities = [
    capability for capability in hello['message']['body']['capabilities']
    if capability in ('query', 'git_read')
]
send({
  'sequence': hello['sequence'],
  'request_id': hello['request_id'],
  'message': {'kind':'hello','body':{
    'protocol_version':1,
    'protocol_fingerprint':'__PROTOCOL_FINGERPRINT__',
    'helper_version':'fake-blame-v1',
    'authorization_challenge_base64url':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    'capabilities':capabilities
  }}
})
request = receive()
body = request['message']['body']
if request['message']['kind'] != 'blame':
    sys.exit(22)
target = body['target']
if body['limit'] < 1 or body['limit'] > 100:
    sys.exit(23)
repository = target.get('repository') or 'ctxrs/ctx'
repository_ref = {'id':'repository:' + repository, 'kind':'repository', 'display':repository}
evidence = [{
  'number':1,
  'citation':{
    'event_id':'00000000-0000-0000-0000-000000000001',
    'event_seq':1
  }
}]
if __OVERSIZED_BLAME__:
    locator_payload = base64.b64encode(b'x' * (64 * 1024)).decode()
    evidence = [{
      'number':number,
      'citation':{
        'provider_output':{
          'source_id':'oversized-source',
          'source_epoch':1,
          'locator':{
            'version':1,
            'kind':'native',
            'payload_base64':locator_payload
          },
          'coordinate':{
            'unit_key':'unit',
            'native_sequence':number,
            'native_record_id':None,
            'source_record_ordinal':None,
            'source_record_subrecord_index':None,
            'byte_start':None,
            'byte_end_exclusive':None
          },
          'availability':'available'
        }
      }
    } for number in range(1, 17)]
evidence_numbers = [item['number'] for item in evidence]
kind = target['kind']
if kind == 'commit':
    oid = target['oid']
    commit = {'id':'commit:' + oid, 'kind':'commit', 'display':oid}
    resolved = {'kind':'commit', 'commit':commit, 'repository':repository_ref}
    matches = [{
      'kind':'commit',
      'value':{
        'fact_id':'fact:produced',
        'fact_type':'git.commit.produced',
        'predicate':'produced_by',
        'subject':commit,
        'object':{'id':'session:producer', 'kind':'session', 'display':'session-producer'},
        'fact_occurred_at_ms':None,
        'confidence':'explicit',
        'state':'asserted',
        'direct_actor':None,
        'owning_root':None,
        'evidence_numbers':evidence_numbers
      }
    }]
    snapshot = None
elif kind == 'file':
    if 'git_read' not in capabilities or not os.environ.get('CTX_PRO_GIT_EXECUTABLE'):
        sys.exit(24)
    path = target['path']
    lines = target.get('lines') or {'start':1, 'end':1}
    commit = {'id':'commit:deadbeef', 'kind':'commit', 'display':'deadbeef'}
    resolved = {
      'kind':'file',
      'path':path,
      'repository':repository_ref,
      'requested_lines':target.get('lines')
    }
    matches = [{
      'kind':'file',
      'value':{
        'id':'file-match:1',
        'lines':lines,
        'commit':commit,
        'line_evidence_numbers':[1],
        'production':[]
      }
    }]
    snapshot = {'head_oid':'deadbeef', 'worktree_status':'clean'}
elif kind == 'pull_request':
    selector = target['selector']
    pull_request = {'id':'pull_request:' + selector, 'kind':'pull_request', 'display':selector}
    resolved = {
      'kind':'pull_request',
      'selector':selector,
      'pull_request':pull_request,
      'repository':repository_ref
    }
    matches = [{
      'kind':'pull_request',
      'value':{
        'pull_request':pull_request,
        'relationship':{
          'kind':'activity',
          'value':{
            'fact_id':'fact:reviewed',
            'action':'reviewed',
            'session':{'id':'session:reviewer', 'kind':'session', 'display':'session-reviewer'},
            'direct_actor':None,
            'owning_root':None,
            'fact_occurred_at_ms':None,
            'confidence':'explicit',
            'state':'asserted',
            'evidence_numbers':[1]
          }
        }
      }
    }]
    snapshot = None
else:
    sys.exit(25)
send({
  'sequence': request['sequence'],
  'request_id': request['request_id'],
  'message': {'kind':'blame','body':{
    'target':resolved,
    'git_snapshot':snapshot,
    'matches':matches,
    'evidence':evidence,
    'next':None
  }}
})
"#;
    let helper = HELPER
        .replace(
            "__PROTOCOL_FINGERPRINT__",
            ctx_pro_host_protocol::PROTOCOL_FINGERPRINT,
        )
        .replace(
            "__OVERSIZED_BLAME__",
            if oversized_page { "True" } else { "False" },
        );
    write_python_helper(path, &helper);
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
