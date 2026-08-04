use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    core_record_contract_fingerprint, derive_event_id, derive_session_id, CertifiedSource,
    CoreRecord, EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use sha2::{Digest, Sha256};

const EXPECTED_FINGERPRINT: &str =
    "c5ad8c7bce69d5fd3f12d3b57e8e49403233db4a74f91882ed649a2bb117b19a";

fn main() {
    let fixture_root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: predecessor_fixture_generator FIXTURE_ROOT");
    assert!(!fixture_root.exists(), "fixture output must not exist");
    assert_eq!(core_record_contract_fingerprint(), EXPECTED_FINGERPRINT);
    fs::create_dir_all(&fixture_root).unwrap();
    let index_root = fixture_root.join("index");
    let source = source();
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 1..=3 {
        writer
            .add_core_record(document(&source, sequence))
            .unwrap();
    }
    writer.certify_source(certificate(&source)).unwrap();
    let receipt = writer
        .commit_with_publication_metadata(
            |_| true,
            |_| Ok(b"source-catalog-frontier-receipt-v1".to_vec()),
        )
        .unwrap()
        .into_parts()
        .0;

    remove_lock_files(&index_root);
    let pointer: serde_json::Value =
        serde_json::from_slice(&fs::read(index_root.join("active-generation.json")).unwrap())
            .unwrap();
    let physical_integrity_digest = pointer["active"]["physical_integrity_digest"]
        .as_str()
        .unwrap();
    let files = fixture_files(&index_root)
        .into_iter()
        .map(|path| {
            let relative = path.strip_prefix(&fixture_root).unwrap();
            serde_json::json!({
                "path": relative.to_str().unwrap(),
                "bytes": path.metadata().unwrap().len(),
                "sha256": sha256_file(&path),
            })
        })
        .collect::<Vec<_>>();
    let provenance = serde_json::json!({
        "version": 1,
        "source_commit": std::env::var("CTX_PREDECESSOR_SOURCE_COMMIT").unwrap(),
        "core_record_contract_fingerprint": EXPECTED_FINGERPRINT,
        "generation_id": receipt.generation_id,
        "physical_integrity_digest": physical_integrity_digest,
        "generator": "predecessor_fixture_generator.rs",
        "command": "testdata/core-predecessor-c5ad8c7b/regenerate.sh",
        "sanitization": "synthetic source and records; no user or provider files",
        "files": files,
    });
    fs::write(
        fixture_root.join("PROVENANCE.json"),
        serde_json::to_vec_pretty(&provenance).unwrap(),
    )
    .unwrap();
}

fn source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("golden-predecessor.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn document(source: &SourceKey, sequence: u64) -> CoreRecord {
    let session_key = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8("golden-predecessor-session").unwrap(),
    )
    .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id(
        "message",
        TypedKey::utf8(format!("event-{sequence}")).unwrap(),
    )
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        "primary",
        true,
        "predecessor-fixture-v1",
        format!("golden predecessor migration evidence {sequence}"),
    )
    .unwrap();
    record.provider_session_id = Some("golden-predecessor-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.occurred_at_unix_ms = Some(1_700_000_000_000 + sequence as i64);
    record.role = Some("user".to_owned());
    record.workspace = Some("sanitized-fixture".to_owned());
    record
}

fn certificate(source: &SourceKey) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "predecessor-fixture-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 3,
            retained_records: 3,
            indexed_documents: 3,
            certified_bytes: 30,
            ..ScannedSourceCounts::default()
        },
        Some(
            SourceFrontier::new("jsonl-byte-offset", TypedKey::U64(30), 30, [1; 32]).unwrap(),
        ),
    )
    .unwrap()
}

fn remove_lock_files(index_root: &Path) {
    let _ = fs::remove_file(index_root.join(".ctx-generation-writer.lock"));
    for entry in fs::read_dir(index_root.join("index-generations")).unwrap() {
        let generation = entry.unwrap().path();
        for lock in [".tantivy-meta.lock", ".tantivy-writer.lock"] {
            let _ = fs::remove_file(generation.join(lock));
        }
    }
}

fn fixture_files(root: &Path) -> Vec<PathBuf> {
    fn visit(path: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(&entry.path(), files);
            } else {
                files.push(entry.path());
            }
        }
    }
    let mut files = Vec::new();
    visit(root, &mut files);
    files.sort();
    files
}

fn sha256_file(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    format!("{:x}", digest.finalize())
}
