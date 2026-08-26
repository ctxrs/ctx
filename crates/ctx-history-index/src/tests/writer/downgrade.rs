use serde::{Deserialize, Serialize};

const OLD_V9_MANIFEST_VERSION: u32 = 9;
const OLD_FLAT_DELTA_STORAGE: &str = "ctx-manifest-flat-delta-v1";
const OLD_FLAT_DELTA_PREFIX: &[u8] = br#"{"storage_format":"ctx-manifest-flat-delta-v1","#;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CanonicalOldV9Manifest {
    manifest_version: u32,
    identity_version: u16,
    core_record_version: u32,
    core_record_contract_fingerprint: String,
    lexical_schema_version: u32,
    lexical_analyzer_version: u32,
    policy_schema_hash: String,
    indexed_documents: u64,
    certified_source_bytes: u64,
    sources: Vec<CertifiedSource>,
    core_record_aggregates: Vec<SourceCoreRecordAggregate>,
    source_routes: Vec<SourceRouteSnapshot>,
    automatic_provider_discovery: bool,
    provider_root_config_digest: String,
    provider_roots: Vec<serde_json::Value>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OldFlatDelta {
    storage_format: String,
    base_generation_id: String,
    indexed_documents: u64,
    certified_source_bytes: u64,
    source_count: usize,
    changes: Vec<OldSourceChange>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OldSourceChange {
    source_identity: [u8; 32],
    source: CertifiedSource,
    aggregate: SourceCoreRecordAggregate,
}

fn canonical_old_v9_bytes(manifest: &GenerationManifest) -> Vec<u8> {
    assert!(manifest.provider_roots().is_empty());
    serde_json::to_vec(&CanonicalOldV9Manifest {
        manifest_version: OLD_V9_MANIFEST_VERSION,
        identity_version: manifest.identity_version,
        core_record_version: manifest.core_record_version,
        core_record_contract_fingerprint: manifest.core_record_contract_fingerprint.clone(),
        lexical_schema_version: manifest.lexical_schema_version,
        lexical_analyzer_version: manifest.lexical_analyzer_version,
        policy_schema_hash: manifest.policy_schema_hash.clone(),
        indexed_documents: manifest.indexed_documents,
        certified_source_bytes: manifest.certified_source_bytes,
        sources: manifest.sources.clone(),
        core_record_aggregates: manifest.core_record_aggregates.clone(),
        source_routes: manifest.source_routes().to_vec(),
        automatic_provider_discovery: manifest.automatic_provider_discovery(),
        provider_root_config_digest: manifest.provider_root_config_digest().to_owned(),
        provider_roots: Vec::new(),
    })
    .unwrap()
}

fn install_literal_old_v9_publication(root: &Path, manifest: &GenerationManifest) -> String {
    let bytes = canonical_old_v9_bytes(manifest);
    let generation_id = ctx_history_index_format::sha256_hex(&bytes);
    ctx_history_index_generation::write_manifest_bytes(root, &generation_id, &bytes).unwrap();
    assert_eq!(
        fs::read(manifest_path(root, &generation_id)).unwrap(),
        bytes
    );

    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    let generation_path = active_generation_path(root);
    let directory = DurableMmapDirectory::open(&generation_path).unwrap();
    let index = Index::open(directory).unwrap();
    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    writer.set_merge_policy(Box::<NoMergePolicy>::default());
    let mut prepared = writer.prepare_commit().unwrap();
    prepared.set_payload(&canonical_commit_payload(&generation_id, None).unwrap());
    prepared.commit().unwrap();
    writer.wait_merging_threads().unwrap();

    let physical_integrity_digest =
        physical_integrity_digest(&index, &generation_path, Some(&pointer)).unwrap();
    let active = GenerationSlot::new(
        generation_id.clone(),
        pointer.active().directory().to_owned(),
        physical_integrity_digest,
    )
    .unwrap();
    publish_active_generation_pointer(root, &ActiveGenerationPointer::new(active, None).unwrap())
        .unwrap();
    generation_id
}

fn old_v9_reader_accepts(root: &Path, generation_id: &str) -> bool {
    fn accepts(root: &Path, generation_id: &str, depth: usize) -> bool {
        if depth > 8 {
            return false;
        }
        let Ok(bytes) = fs::read(manifest_path(root, generation_id)) else {
            return false;
        };
        if bytes.starts_with(OLD_FLAT_DELTA_PREFIX) {
            let Ok(delta) = serde_json::from_slice::<OldFlatDelta>(&bytes) else {
                return false;
            };
            return serde_json::to_vec(&delta).ok().as_deref() == Some(bytes.as_slice())
                && delta.storage_format == OLD_FLAT_DELTA_STORAGE
                && !delta.changes.is_empty()
                && accepts(root, &delta.base_generation_id, depth + 1);
        }
        let Ok(manifest) = serde_json::from_slice::<CanonicalOldV9Manifest>(&bytes) else {
            return false;
        };
        manifest.manifest_version == OLD_V9_MANIFEST_VERSION
            && serde_json::to_vec(&manifest).ok().as_deref() == Some(bytes.as_slice())
    }

    accepts(root, generation_id, 0)
}

#[test]
fn source_only_successor_of_literal_v9_publishes_full_v10_anchor() {
    let temp = tempdir().unwrap();
    let source = source("v9-source-only-boundary.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "literal v9 base"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let initial = initial.commit(|_| true).unwrap();
    let v9_generation_id = install_literal_old_v9_publication(temp.path(), initial.manifest());
    assert!(old_v9_reader_accepts(temp.path(), &v9_generation_id));

    let mut successor = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert_eq!(
        successor.base_generation_id(),
        Some(v9_generation_id.as_str())
    );
    successor.begin_source(source.clone()).unwrap();
    successor
        .add_core_record(document(&source, 2, "candidate source replacement"))
        .unwrap();
    successor
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    let anchored = successor.commit(|_| true).unwrap();
    let anchored_bytes = fs::read(manifest_path(temp.path(), &anchored.generation_id)).unwrap();
    assert!(!anchored_bytes.starts_with(OLD_FLAT_DELTA_PREFIX));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&anchored_bytes).unwrap()["manifest_version"],
        GENERATION_MANIFEST_VERSION
    );
    assert!(!old_v9_reader_accepts(temp.path(), &anchored.generation_id));

    let mut next = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    next.begin_source(source.clone()).unwrap();
    next.add_core_record(document(&source, 3, "v10 delta"))
        .unwrap();
    next.certify_source(certificate(&source, 3, 1)).unwrap();
    let delta = next.commit(|_| true).unwrap();
    let delta_bytes = fs::read(manifest_path(temp.path(), &delta.generation_id)).unwrap();
    let delta_json: serde_json::Value = serde_json::from_slice(&delta_bytes).unwrap();
    assert_eq!(delta_json["storage_format"], OLD_FLAT_DELTA_STORAGE);
    assert_eq!(delta_json["base_generation_id"], anchored.generation_id);
    assert!(!old_v9_reader_accepts(temp.path(), &delta.generation_id));
}

#[test]
fn exact_no_op_of_literal_v9_publishes_v10_once_then_reuses_it() {
    let temp = tempdir().unwrap();
    let source = source("v9-no-op-boundary.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "literal v9 no-op base"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let initial = initial.commit(|_| true).unwrap();
    let v9_generation_id = install_literal_old_v9_publication(temp.path(), initial.manifest());
    assert!(old_v9_reader_accepts(temp.path(), &v9_generation_id));

    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    let mut candidate = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    candidate
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    stage_exact_replay(&mut candidate, &source);
    let constructions = Arc::clone(&candidate.index_writer_constructions);
    let anchored = candidate
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap();
    assert_eq!(constructions.load(Ordering::SeqCst), 1);
    assert_ne!(anchored.generation_id, v9_generation_id);
    assert!(!old_v9_reader_accepts(temp.path(), &anchored.generation_id));

    let mut reusable = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    reusable
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    stage_exact_replay(&mut reusable, &source);
    let constructions = Arc::clone(&reusable.index_writer_constructions);
    let reused = reusable
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap();
    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(reused.generation_id, anchored.generation_id);
}

#[test]
fn metadata_only_republish_of_literal_v9_anchors_v10_once() {
    let temp = tempdir().unwrap();
    let source = source("v9-metadata-republish-boundary.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "literal v9 metadata base"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let initial = initial.commit(|_| true).unwrap();
    let v9_generation_id = install_literal_old_v9_publication(temp.path(), initial.manifest());
    assert!(old_v9_reader_accepts(temp.path(), &v9_generation_id));

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let anchored = writer
        .republish_current_publication_metadata(&v9_generation_id, b"v10-owner".to_vec())
        .unwrap();
    assert_ne!(anchored.generation_id(), v9_generation_id);
    assert_eq!(
        anchored.publication_metadata(),
        Some(b"v10-owner".as_slice())
    );
    assert!(!old_v9_reader_accepts(
        temp.path(),
        anchored.generation_id()
    ));

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let reused = writer
        .republish_current_publication_metadata(
            anchored.generation_id(),
            b"replacement-owner".to_vec(),
        )
        .unwrap();
    assert_eq!(reused.generation_id(), anchored.generation_id());
    assert_eq!(
        reused.publication_metadata(),
        Some(b"replacement-owner".as_slice())
    );
}
