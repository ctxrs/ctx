#[cfg(unix)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures)
))]
use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};
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

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures)
))]
pub(crate) fn initialize_current_query_store(data_root: &Path) {
    initialize_pro_installation_identity(data_root);
    let generation_id = initialize_provider_neutral_core_projection(data_root);
    assert!(!generation_id.is_empty());
    assert!(
        !data_root.join("work.sqlite").exists(),
        "Pro query fixtures must use only the fresh source-backed epoch"
    );
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures)
))]
pub(crate) fn initialize_empty_current_query_store(data_root: &Path) -> String {
    initialize_pro_installation_identity(data_root);
    let index_root = data_root.join("search").join("lexical");
    let receipt = GenerationWriter::open(
        &index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap()
    .commit(|_| true)
    .unwrap();
    let verified = VerifiedIndex::open(index_root).unwrap();
    assert_eq!(verified.generation_id(), receipt.generation_id);
    receipt.generation_id
}

#[cfg(unix)]
pub(crate) fn write_core_materialization_helper(
    path: &Path,
    materializer_revision: &str,
    state_path: &Path,
    log_path: &Path,
) {
    const HELPER: &str = r#"#!/usr/bin/python3
import hashlib, json, pathlib, struct, sys

REVISION = __REVISION__
STATE = pathlib.Path(__STATE_PATH__)
LOG = pathlib.Path(__LOG_PATH__)

def receive():
    header = sys.stdin.buffer.read(12)
    if not header:
        return None
    if len(header) != 12 or header[:6] != b'CTXPRO':
        sys.exit(20)
    size = struct.unpack('>I', header[8:12])[0]
    return json.loads(sys.stdin.buffer.read(size))

def send(request, kind, body):
    value = {
      'sequence': request['sequence'],
      'request_id': request['request_id'],
      'message': {'kind': kind, 'body': body}
    }
    payload = json.dumps(value, separators=(',', ':')).encode()
    sys.stdout.buffer.write(b'CTXPRO' + struct.pack('>H', 1) + struct.pack('>I', len(payload)) + payload)
    sys.stdout.buffer.flush()

def stored_receipt():
    try:
        return json.loads(STATE.read_text())
    except FileNotFoundError:
        return None

def status_body(requested_generation):
    receipt = stored_receipt()
    current = receipt is not None and receipt['core_generation_id'] == requested_generation and receipt['materializer_revision'] == REVISION
    return {
      'currentness': 'current' if current else 'not_materialized',
      'requested_core_generation_id': requested_generation,
      'core_receipt': receipt if current else None,
      'coverage': 'empty' if current else 'not_materialized',
      'repository_coverage': {
        'repository_candidate_events': 0,
        'logical_binding_events': 0,
        'certified_live_root_access_events': 0,
        'file_evidence_events': 0,
        'exact_commit_evidence_events': 0,
        'exact_pull_request_evidence_events': 0
      },
      'core_preparation_peak_workers': 0,
      'access': {
        'entitlement': 'available',
        'graph_key': 'available',
        'local_repository': 'unavailable'
      },
      'supported_operations': [],
      'available_operations': [],
      'storage_evidence': {
        'graph_manifest_schema': 3,
        'flat_format_version': 2,
        'materializer_checkpoint_version': 3,
        'journal_pack_format_version': 3,
        'legacy_journals_written': 0,
        'journal_pages_written': 1,
        'journal_packs_written': 1,
        'journal_finish_activity': {
          'worker_limit': 1,
          'peak_workers': 1,
          'started_after_preparation': True
        }
      } if current else None
    }

hello = receive()
if hello is None or hello['message']['kind'] != 'hello':
    sys.exit(21)
with LOG.open('a') as stream:
    stream.write('start:' + REVISION + '\n')
send(hello, 'hello', {
  'protocol_version': 1,
  'protocol_fingerprint': '__PROTOCOL_FINGERPRINT__',
  'helper_version': 'same-generation-fixture-' + REVISION,
  'authorization_challenge_base64url': 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
  'capabilities': ['status', 'core_materialization']
})

while True:
    request = receive()
    if request is None:
        break
    kind = request['message']['kind']
    body = request['message']['body']
    with LOG.open('a') as stream:
        stream.write('request:' + REVISION + ':' + kind + '\n')
    if kind == 'status':
        send(request, 'status', status_body(body.get('requested_core_generation_id')))
    elif kind == 'begin_core_materialization':
        encoded = json.dumps(body, separators=(',', ':')).encode()
        encoded_revision = json.dumps(REVISION, separators=(',', ':')).encode()
        materialization_id = hashlib.sha256(b'[' + encoded + b',' + encoded_revision + b']').hexdigest()
        send(request, 'core_materialization_began', {
          'materialization_id': materialization_id,
          'core_generation_id': body['head']['core_generation_id'],
          'materializer_revision': REVISION,
          'expected_prior_receipt': body['expected_prior_receipt'],
          'replayed': False
        })
    elif kind == 'apply_core_source_delta_page':
        page = body['page']
        send(request, 'core_source_delta_page_applied', {
          'materialization_id': page['materialization_id'],
          'core_generation_id': page['core_generation_id'],
          'page_index': page['page_index'],
          'acknowledgement_page_index': body['acknowledgement_page_index'],
          'acknowledgement_terminal': True,
          'changed_sources': 0,
          'removed_sources': 0,
          'reconcile_sources': [],
          'replayed': False
        })
    elif kind == 'finish_core_materialization':
        head = body['head']
        receipt = {
          'core_generation_id': head['core_generation_id'],
          'core_record_contract_fingerprint': head['core_record_contract_fingerprint'],
          'source_snapshot_sha256': head['source_snapshot_sha256'],
          'materializer_revision': REVISION,
          'source_count': head['source_count'],
          'event_count': head['event_count']
        }
        STATE.write_text(json.dumps(receipt, separators=(',', ':')))
        with LOG.open('a') as stream:
            stream.write('finish:' + REVISION + '\n')
        send(request, 'core_materialization_finished', {'receipt': receipt, 'replayed': False})
    else:
        sys.exit(22)
"#;
    let helper = HELPER
        .replace(
            "__REVISION__",
            &serde_json::to_string(materializer_revision).unwrap(),
        )
        .replace(
            "__STATE_PATH__",
            &serde_json::to_string(&state_path.to_string_lossy()).unwrap(),
        )
        .replace(
            "__LOG_PATH__",
            &serde_json::to_string(&log_path.to_string_lossy()).unwrap(),
        )
        .replace(
            "__PROTOCOL_FINGERPRINT__",
            ctx_pro_host_protocol::PROTOCOL_FINGERPRINT,
        );
    write_python_helper(path, &helper);
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures)
))]
fn initialize_provider_neutral_core_projection(data_root: &Path) -> String {
    let source_digest = [
        175, 208, 244, 36, 180, 188, 63, 218, 129, 170, 29, 64, 65, 216, 117, 181, 87, 2, 144, 20,
        105, 113, 110, 148, 100, 116, 65, 93, 202, 224, 111, 187,
    ];
    let descriptor_digest = [
        228, 136, 202, 181, 26, 51, 96, 50, 4, 21, 154, 120, 18, 194, 115, 94, 26, 61, 151, 30,
        190, 204, 19, 116, 24, 34, 103, 109, 63, 77, 114, 212,
    ];
    let source = serde_json::json!({
        "provider": "golden",
        "source_format": "golden_jsonl",
        "schema_variant": "golden-v1",
        "provider_identity_version": 1,
        "anchor": {"CatalogLineage": vec![1_u8; 32]},
        "identity": {
            "contract_version": 1,
            "entity_kind": "Source",
            "digest": source_digest,
            "source_digest": source_digest,
            "source_descriptor_digest": vec![0_u8; 32],
            "uuid": "afd0f424-b4bc-8fda-81aa-1d4041d875b5",
        },
    });
    let session_id = serde_json::json!({
        "contract_version": 1,
        "entity_kind": "Session",
        "digest": [
            197, 33, 206, 47, 175, 208, 90, 191, 45, 157, 209, 244, 53, 79, 81, 8, 122,
            251, 196, 109, 217, 48, 148, 110, 61, 131, 195, 254, 61, 124, 40, 84,
        ],
        "source_digest": source_digest,
        "source_descriptor_digest": descriptor_digest,
        "uuid": "c521ce2f-afd0-8abf-ad9d-d1f4354f5108",
    });
    let event_id = serde_json::json!({
        "contract_version": 1,
        "entity_kind": "Event",
        "digest": [
            216, 99, 203, 132, 107, 211, 192, 113, 235, 219, 83, 38, 196, 76, 137, 106,
            44, 136, 49, 16, 137, 199, 221, 179, 12, 16, 95, 74, 24, 15, 80, 210,
        ],
        "source_digest": source_digest,
        "source_descriptor_digest": descriptor_digest,
        "uuid": "d863cb84-6bd3-8071-abdb-5326c44c896a",
    });
    let record: ctx_pro_host_protocol::CoreRecord = serde_json::from_value(serde_json::json!({
        "record_version": 1,
        "event_id": event_id,
        "session_id": session_id,
        "parent_session_id": null,
        "root_session_id": session_id,
        "source": source,
        "provider_session_id": "golden-session",
        "native_event_id": null,
        "event_sequence": 1,
        "occurred_at_unix_ms": 1700000000000_i64,
        "event_type": "message",
        "role": "assistant",
        "agent_type": "primary",
        "is_primary": true,
        "workspace": null,
        "branch": null,
        "cwd": null,
        "parser_revision": "golden-parser-v1",
        "normalization_revision": 1,
        "content": {
            "policy_revision": 2,
            "policy_status": "selected",
            "normalized_body": "provider-neutral Pro query fixture",
            "structured_content": null,
        },
        "metadata": {},
        "repository_candidate_evidence": {
            "repository_observation_revision": 2,
            "bounded_shell_subset_revision": 1,
            "association_policy_revision": 4,
            "outcome_capture_revision": 2,
            "candidates": [
                {"kind": "session_cwd", "path": "/fixture/repository"},
                {"kind": "file_activity_path", "path": "/fixture/repository/src/lib.rs"},
            ],
        },
        "repository_bindings": [],
        "repository_abstentions": [],
        "repository_file_observations": [],
        "repository_vcs_observations": [],
    }))
    .unwrap();
    let source = record.source.clone();
    let observation = serde_json::json!({
        "source": source,
        "revision_kind": "fixture-v1",
        "revision": [1],
    });
    let certificate = serde_json::json!({
        "observation": observation,
        "parser_revision": "golden-parser-v1",
        "content_digest": vec![1_u8; 32],
        "counts": {
            "complete_records": 1,
            "retained_records": 1,
            "rejected_records": 0,
            "ignored_records": 0,
            "indexed_documents": 1,
            "certified_bytes": 1,
        },
        "frontier": null,
    });

    let index_root = data_root.join("search").join("lexical");
    let mut writer = GenerationWriter::open(
        &index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap();
    writer.begin_source(source).unwrap();
    writer.add_core_record(record).unwrap();
    writer
        .certify_source(serde_json::from_value(certificate).unwrap())
        .unwrap();
    let core_receipt = writer.commit(|_| true).unwrap();
    let verified = VerifiedIndex::open(index_root).unwrap();
    assert_eq!(verified.generation_id(), core_receipt.generation_id);
    core_receipt.generation_id
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

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures)
))]
pub(crate) fn write_blame_helper(path: &Path) {
    write_blame_helper_with_options(path, false, None);
}

#[cfg(unix)]
pub(crate) fn write_status_helper(path: &Path) {
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
if 'status' not in hello['message']['body']['capabilities']:
    sys.exit(21)
send({
  'sequence': hello['sequence'],
  'request_id': hello['request_id'],
  'message': {'kind':'hello','body':{
    'protocol_version':1,
    'protocol_fingerprint':'__PROTOCOL_FINGERPRINT__',
    'helper_version':'fake-status-v1',
    'authorization_challenge_base64url':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    'capabilities':['status']
  }}
})
request = receive()
if request['message']['kind'] != 'status':
    sys.exit(22)
requested_generation = request['message']['body'].get('requested_core_generation_id')
current = requested_generation is not None
receipt = {
  'core_generation_id':requested_generation,
  'core_record_contract_fingerprint':'__CORE_RECORD_CONTRACT_FINGERPRINT__',
  'source_snapshot_sha256':'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
  'materializer_revision':'status-fixture-v1',
  'source_count':1,
  'event_count':1
} if current else None
storage_evidence = {
  'graph_manifest_schema':3,
  'flat_format_version':2,
  'materializer_checkpoint_version':3,
  'journal_pack_format_version':3,
  'legacy_journals_written':0,
  'journal_pages_written':2,
  'journal_packs_written':1,
  'journal_finish_activity':{
    'worker_limit':1,
    'peak_workers':1,
    'started_after_preparation':True
  }
} if current else None
send({
  'sequence': request['sequence'],
  'request_id': request['request_id'],
  'message': {'kind':'status','body':{
    'currentness':'current' if current else 'not_materialized',
    'requested_core_generation_id':requested_generation,
    'core_receipt':receipt,
    'coverage':'abstained' if current else 'not_materialized',
    'repository_coverage':{
      'repository_candidate_events':0,
      'logical_binding_events':0,
      'certified_live_root_access_events':0,
      'file_evidence_events':0,
      'exact_commit_evidence_events':0,
      'exact_pull_request_evidence_events':0
    },
    'core_preparation_peak_workers':0,
    'access':{
      'entitlement':'available',
      'graph_key':'available',
      'local_repository':'unavailable'
    },
    'supported_operations':['file_blame','commit_blame','pull_request_blame'],
    'available_operations':[],
    'storage_evidence':storage_evidence
  }}
})
"#;
    let helper = HELPER
        .replace(
            "__PROTOCOL_FINGERPRINT__",
            ctx_pro_host_protocol::PROTOCOL_FINGERPRINT,
        )
        .replace(
            "__CORE_RECORD_CONTRACT_FINGERPRINT__",
            &ctx_history_core::core_record_contract_fingerprint(),
        );
    write_python_helper(path, &helper);
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures)
))]
pub(crate) fn write_oversized_blame_helper(path: &Path) {
    write_blame_helper_with_options(path, true, None);
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures)
))]
pub(crate) fn write_blame_error_helper(path: &Path, error_class: &str) {
    assert!(matches!(
        error_class,
        "resource_not_found" | "missing_repository" | "ambiguous" | "invalid_request"
    ));
    write_blame_helper_with_options(path, false, Some(error_class));
}

#[cfg(all(
    unix,
    any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures)
))]
fn write_blame_helper_with_options(
    path: &Path,
    oversized_page: bool,
    blame_error_class: Option<&str>,
) {
    let data_root = path
        .parent()
        .expect("fake Pro blame helper must live directly under its data root");
    let active_generation =
        ctx_history_index::VerifiedIndex::open(data_root.join("search").join("lexical"))
            .expect("fake Pro blame helper requires an active verified Core generation");
    let manifest = active_generation.manifest();
    let source_states = manifest
        .sources
        .iter()
        .zip(&manifest.core_record_aggregates)
        .map(
            |(source, aggregate)| ctx_pro_host_protocol::CoreSourceState {
                source: source.observation().source().clone(),
                core_record_accumulator: aggregate.core_record_accumulator().to_owned(),
                event_count: source.counts().indexed_documents,
            },
        )
        .collect::<Vec<_>>();
    let core_head = ctx_pro_host_protocol::CoreGenerationHead::new(
        active_generation.generation_id(),
        manifest.manifest_version,
        manifest.identity_version,
        manifest.core_record_contract_fingerprint.clone(),
        manifest.lexical_schema_version,
        manifest.lexical_analyzer_version,
        manifest.policy_schema_hash.clone(),
        &source_states,
    )
    .expect("fake Pro blame helper requires an exact provider-neutral Core generation head");
    let evidence_page = active_generation
        .core_source_event_page(&source_states[0].source, None, 1)
        .expect("fake Pro blame helper requires a readable provider-neutral Core event");
    let evidence_record = &evidence_page.items[0].core_record;
    let evidence_source = serde_json::to_string(&evidence_record.source).unwrap();
    let evidence_session_id = serde_json::to_string(&evidence_record.session_id).unwrap();
    let evidence_event_id = serde_json::to_string(&evidence_record.event_id).unwrap();
    let evidence_event_sequence = evidence_record.event_sequence.to_string();

    const HELPER: &str = r#"#!/usr/bin/python3
import json, os, struct, sys

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

def status_body(request):
    return {
      'currentness':'current',
      'requested_core_generation_id':request['message']['body'].get('requested_core_generation_id'),
      'core_receipt':{
        'core_generation_id':'__CORE_GENERATION_ID__',
        'core_record_contract_fingerprint':'__CORE_RECORD_CONTRACT_FINGERPRINT__',
        'source_snapshot_sha256':'__CORE_SOURCE_SNAPSHOT_SHA256__',
        'materializer_revision':'pro-query-fixture-v1',
        'source_count':__CORE_SOURCE_COUNT__,
        'event_count':__CORE_EVENT_COUNT__
      },
      'coverage':'complete',
      'repository_coverage':{
        'repository_candidate_events':1,
        'logical_binding_events':1,
        'certified_live_root_access_events':1,
        'file_evidence_events':1,
        'exact_commit_evidence_events':1,
        'exact_pull_request_evidence_events':1
      },
      'core_preparation_peak_workers':0,
      'access':{
        'entitlement':'available',
        'graph_key':'available',
        'local_repository':'available'
      },
      'supported_operations':['file_blame','commit_blame','pull_request_blame'],
      'available_operations':['file_blame','commit_blame','pull_request_blame'],
      'storage_evidence':{
        'graph_manifest_schema':3,
        'flat_format_version':2,
        'materializer_checkpoint_version':3,
        'journal_pack_format_version':3,
        'legacy_journals_written':0,
        'journal_pages_written':2,
        'journal_packs_written':1,
        'journal_finish_activity':{
          'worker_limit':1,
          'peak_workers':1,
          'started_after_preparation':True
        }
      }
    }

hello = receive()
if 'query' not in hello['message']['body']['capabilities']:
    sys.exit(21)
capabilities = [
    capability for capability in hello['message']['body']['capabilities']
    if capability in ('status', 'query', 'git_read')
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
if request['message']['kind'] != 'status':
    sys.exit(22)
send({
  'sequence': request['sequence'],
  'request_id': request['request_id'],
  'message': {'kind':'status','body':status_body(request)}
})
request = receive()
body = request['message']['body']
if request['message']['kind'] != 'blame':
    sys.exit(26)
if __BLAME_ERROR_CLASS__ is not None:
    send({
      'sequence': request['sequence'],
      'request_id': request['request_id'],
      'message': {'kind':'error','body':{
        'class':__BLAME_ERROR_CLASS__,
        'message':'untrusted helper detail at /secret/graph/path',
        'retryable':False
      }}
    })
    sys.exit(0)
target = body['target']
if body['limit'] < 1 or body['limit'] > 100:
    sys.exit(23)
repository = target.get('repository') or 'ctxrs/ctx'
repository_ref = {'id':'repository:' + repository, 'kind':'repository', 'display':repository}
field_padding = 'x' * 8000 if __OVERSIZED_BLAME__ else ''
evidence_citation = {
  'core_generation_id':'__CORE_GENERATION_ID__',
  'source':__EVIDENCE_SOURCE__,
  'session_id':__EVIDENCE_SESSION_ID__,
  'event_id':__EVIDENCE_EVENT_ID__,
  'event_sequence':__EVIDENCE_EVENT_SEQUENCE__,
  'byte_range':None,
  'evidence_sha256':None
}
evidence = [{
  'number':1,
  'citation':evidence_citation
}]
if __OVERSIZED_BLAME__:
    evidence = [{
      'number':number,
      'citation':evidence_citation
    } for number in range(1, 8 * 32 + 1)]
evidence_numbers = [item['number'] for item in evidence]
kind = target['kind']
if kind == 'commit':
    oid = target['oid']
    commit = {'id':'commit:' + oid, 'kind':'commit', 'display':oid}
    resolved = {'kind':'commit', 'commit':commit, 'repository':repository_ref}
    match_count = 8 if __OVERSIZED_BLAME__ else 1
    matches = [{
      'kind':'commit',
      'value':{
        'fact_id':'fact:produced' + (':' + str(index + 1) if __OVERSIZED_BLAME__ else '') + field_padding,
        'fact_type':'git.commit.produced',
        'predicate':'produced_by',
        'subject':commit,
        'object':{
          'id':'session:producer' + field_padding,
          'kind':'session',
          'display':'session-producer' + field_padding
        },
        'fact_occurred_at_ms':None,
        'confidence':'explicit',
        'state':'asserted',
        'direct_actor':None,
        'owning_root':None,
        'evidence_numbers':(
          list(range(index * 32 + 1, (index + 1) * 32 + 1))
          if __OVERSIZED_BLAME__ else evidence_numbers
        )
      }
    } for index in range(match_count)]
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
    'snapshot':body['expected_snapshot'],
    'target':resolved,
    'git_snapshot':snapshot,
    'matches':matches,
    'evidence':evidence,
    'next':None
  }}
})
request = receive()
if request['message']['kind'] != 'status':
    sys.exit(27)
send({
  'sequence': request['sequence'],
  'request_id': request['request_id'],
  'message': {'kind':'status','body':status_body(request)}
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
        )
        .replace(
            "__BLAME_ERROR_CLASS__",
            &blame_error_class.map_or_else(|| "None".to_owned(), |class| format!("'{class}'")),
        )
        .replace("__CORE_GENERATION_ID__", &core_head.core_generation_id)
        .replace(
            "__CORE_RECORD_CONTRACT_FINGERPRINT__",
            &core_head.core_record_contract_fingerprint,
        )
        .replace(
            "__CORE_SOURCE_SNAPSHOT_SHA256__",
            &core_head.source_snapshot_sha256,
        )
        .replace("__CORE_SOURCE_COUNT__", &core_head.source_count.to_string())
        .replace("__CORE_EVENT_COUNT__", &core_head.event_count.to_string())
        .replace("__EVIDENCE_SOURCE__", &evidence_source)
        .replace("__EVIDENCE_SESSION_ID__", &evidence_session_id)
        .replace("__EVIDENCE_EVENT_ID__", &evidence_event_id)
        .replace("__EVIDENCE_EVENT_SEQUENCE__", &evidence_event_sequence);
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
