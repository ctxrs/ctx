use super::*;

#[test]
fn v8_frontier_and_v11_parser_generation_is_rebuilt_to_current_authority() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000204";
    write_session(&sessions, native_session_id, &[]);

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let current = VerifiedIndex::open(&index).unwrap();
    let current_certificate = current.manifest().sources.first().unwrap().clone();
    let current_frontier = current_certificate.frontier().unwrap();
    let TypedKey::Bytes(checkpoint_bytes) = current_frontier.checkpoint() else {
        panic!("Codex checkpoint must be byte keyed");
    };
    let mut v8_checkpoint = serde_json::from_slice::<serde_json::Value>(checkpoint_bytes).unwrap();
    v8_checkpoint["version"] = serde_json::json!(8);
    let v8_frontier = SourceFrontier::new(
        "codex-nativepath-checkpoint-v8",
        TypedKey::bytes(serde_json::to_vec(&v8_checkpoint).unwrap()).unwrap(),
        current_frontier.certified_prefix_bytes(),
        *current_frontier.certified_prefix_digest(),
    )
    .unwrap();
    let old_certificate = CertifiedSource::certify_with_frontier(
        current_certificate.observation().clone(),
        current_certificate.observation().clone(),
        "codex-nativepath-core-record-v11",
        *current_certificate.content_digest(),
        current_certificate.counts(),
        Some(v8_frontier),
    )
    .unwrap();

    let source = old_certificate.observation().source().clone();
    let mut downgrade = GenerationWriter::open(&index, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    downgrade.begin_source(source).unwrap();
    downgrade.certify_source(old_certificate.clone()).unwrap();
    let downgraded = downgrade
        .commit(|target| {
            matches!(target, RevalidationTarget::Source(actual) if actual == &old_certificate)
        })
        .unwrap();
    let old = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        old.manifest().sources[0].parser_revision(),
        "codex-nativepath-core-record-v11"
    );
    assert_eq!(
        old.manifest().sources[0]
            .frontier()
            .unwrap()
            .checkpoint_kind(),
        "codex-nativepath-checkpoint-v8"
    );

    let rebuilt = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(rebuilt.counters.replaced_sources, 1);
    assert_ne!(rebuilt.commit.generation_id, downgraded.generation_id);
    let verified = VerifiedIndex::open(&index).unwrap();
    let certificate = &verified.manifest().sources[0];
    assert_eq!(certificate.parser_revision(), CODEX_PARSER_REVISION);
    assert_eq!(
        certificate.frontier().unwrap().checkpoint_kind(),
        CODEX_FRONTIER_KIND
    );
    let TypedKey::Bytes(bytes) = certificate.frontier().unwrap().checkpoint() else {
        panic!("rebuilt Codex checkpoint must be byte keyed");
    };
    let checkpoint = serde_json::from_slice::<serde_json::Value>(bytes).unwrap();
    assert_eq!(checkpoint["version"], 9);
}

#[test]
fn lineage_evidence_authority_rebuilds_v17_parser_generation() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000216";
    write_session(&sessions, native_session_id, &[]);

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let current = VerifiedIndex::open(&index).unwrap();
    let current_certificate = current.manifest().sources.first().unwrap().clone();
    assert_eq!(current_certificate.parser_revision(), CODEX_PARSER_REVISION);
    assert_eq!(
        current_certificate.frontier().unwrap().checkpoint_kind(),
        CODEX_FRONTIER_KIND
    );
    let old_certificate = CertifiedSource::certify_with_frontier(
        current_certificate.observation().clone(),
        current_certificate.observation().clone(),
        "codex-nativepath-core-record-v17-session-copy-lineage",
        *current_certificate.content_digest(),
        current_certificate.counts(),
        current_certificate.frontier().cloned(),
    )
    .unwrap();

    let source = old_certificate.observation().source().clone();
    let mut downgrade = GenerationWriter::open(&index, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    downgrade.begin_source(source).unwrap();
    downgrade.certify_source(old_certificate.clone()).unwrap();
    let downgraded = downgrade
        .commit(|target| {
            matches!(target, RevalidationTarget::Source(actual) if actual == &old_certificate)
        })
        .unwrap();
    let old = VerifiedIndex::open(&index).unwrap();
    let certificate = &old.manifest().sources[0];
    assert_eq!(
        certificate.parser_revision(),
        "codex-nativepath-core-record-v17-session-copy-lineage"
    );
    assert_eq!(
        certificate.frontier().unwrap().checkpoint_kind(),
        CODEX_FRONTIER_KIND
    );

    let rebuilt = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(rebuilt.counters.replaced_sources, 1);
    assert_ne!(rebuilt.commit.generation_id, downgraded.generation_id);
    let verified = VerifiedIndex::open(&index).unwrap();
    let certificate = &verified.manifest().sources[0];
    assert_eq!(certificate.parser_revision(), CODEX_PARSER_REVISION);
    assert_eq!(
        certificate.frontier().unwrap().checkpoint_kind(),
        CODEX_FRONTIER_KIND
    );
}

#[test]
fn v8_frontier_is_rebuilt_when_parser_revision_is_already_current() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000215";
    write_session(&sessions, native_session_id, &[]);

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let current = VerifiedIndex::open(&index).unwrap();
    let current_certificate = current.manifest().sources.first().unwrap().clone();
    assert_eq!(current_certificate.parser_revision(), CODEX_PARSER_REVISION);
    let current_frontier = current_certificate.frontier().unwrap();
    let TypedKey::Bytes(checkpoint_bytes) = current_frontier.checkpoint() else {
        panic!("Codex checkpoint must be byte keyed");
    };
    let mut v8_checkpoint = serde_json::from_slice::<serde_json::Value>(checkpoint_bytes).unwrap();
    v8_checkpoint["version"] = serde_json::json!(8);
    let v8_frontier = SourceFrontier::new(
        "codex-nativepath-checkpoint-v8",
        TypedKey::bytes(serde_json::to_vec(&v8_checkpoint).unwrap()).unwrap(),
        current_frontier.certified_prefix_bytes(),
        *current_frontier.certified_prefix_digest(),
    )
    .unwrap();
    let old_certificate = CertifiedSource::certify_with_frontier(
        current_certificate.observation().clone(),
        current_certificate.observation().clone(),
        CODEX_PARSER_REVISION,
        *current_certificate.content_digest(),
        current_certificate.counts(),
        Some(v8_frontier),
    )
    .unwrap();

    let source = old_certificate.observation().source().clone();
    let mut downgrade = GenerationWriter::open(&index, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    downgrade.begin_source(source).unwrap();
    downgrade.certify_source(old_certificate.clone()).unwrap();
    let downgraded = downgrade
        .commit(|target| {
            matches!(target, RevalidationTarget::Source(actual) if actual == &old_certificate)
        })
        .unwrap();
    let old = VerifiedIndex::open(&index).unwrap();
    let certificate = &old.manifest().sources[0];
    assert_eq!(certificate.parser_revision(), CODEX_PARSER_REVISION);
    assert_eq!(
        certificate.frontier().unwrap().checkpoint_kind(),
        "codex-nativepath-checkpoint-v8"
    );

    let rebuilt = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(rebuilt.counters.replaced_sources, 1);
    assert_ne!(rebuilt.commit.generation_id, downgraded.generation_id);
    let verified = VerifiedIndex::open(&index).unwrap();
    let certificate = &verified.manifest().sources[0];
    assert_eq!(certificate.parser_revision(), CODEX_PARSER_REVISION);
    assert_eq!(
        certificate.frontier().unwrap().checkpoint_kind(),
        CODEX_FRONTIER_KIND
    );
}
