use super::*;
use crate::{
    provider_source_config_digest, source_token, AppliedProviderRoot,
    AppliedProviderRootSourceMembership, DetachedReleasedProviderRootAuthority,
    ProviderRootDefinition, ProviderRootSourceIdentity, SourceRouteIdentity, SourceRouteSnapshot,
};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, ScannedSourceCounts, SourceAnchor, SourceKey,
    SourceObservation, TypedKey,
};

fn released_v2_payload(metadata_json: &str) -> String {
    format!(
        r#"{{"version":2,"generation_id":"{}","publication_metadata":{metadata_json}}}"#,
        "a".repeat(64)
    )
}

#[test]
fn released_v2_envelopes_are_typed_incompatible() {
    // Literal envelopes reproduce the released writer's field order and null
    // handling, independently of either current or historical serializers.
    for metadata in [
        "null".to_owned(),
        r#""""#.to_owned(),
        r#""AQID""#.to_owned(),
        format!("\"{}\"", "AAAA".repeat(64)),
        format!("\"{}\"", "AAAA".repeat(16 * 1024)),
    ] {
        let encoded = released_v2_payload(&metadata);
        assert!(matches!(
            decode_commit_payload(&encoded),
            Err(IndexError::UnsupportedCommitPayload(2))
        ));
    }
    let encoded = released_v2_payload(&format!("\"{}\"", "AAAA".repeat(64)));
    assert!(encoded.len() > 256);
}

#[test]
fn malformed_v2_envelopes_do_not_gain_rebuild_classification() {
    let canonical = released_v2_payload("null");
    for encoded in [
        released_v2_payload("42"),
        released_v2_payload("{}"),
        released_v2_payload("[]"),
        canonical.replace("\"version\":2", "\"version\":2,\"version\":3"),
        canonical.replace("\"publication_metadata\":null", "\"unknown\":null"),
        canonical.replace(
            "\"publication_metadata\":null",
            "\"publication_metadata\":null,\"publication_metadata\":null",
        ),
        canonical.replace(&"a".repeat(64), &"z".repeat(64)),
        format!("{canonical}{{}}"),
        canonical[..canonical.len() - 1].to_owned(),
    ] {
        assert!(
            matches!(decode_commit_payload(&encoded), Err(IndexError::Json(_))),
            "unexpected classification for {encoded}"
        );
    }
}

#[test]
fn oversized_v2_envelopes_keep_the_size_error() {
    // One byte beyond the old base64 budget still fits the envelope budget.
    // Beyond either bound, this is not a recognized released envelope.
    for metadata_bytes in [64 * 1024 + 1, 64 * 1024 + 256] {
        let encoded = released_v2_payload(&format!("\"{}\"", "A".repeat(metadata_bytes)));
        assert!(matches!(
            decode_commit_payload(&encoded),
            Err(IndexError::CommitPayloadTooLarge { actual, maximum: 256 })
                if actual == encoded.len()
        ));
    }
}

#[test]
fn current_commit_payload_stays_strict_and_canonical() {
    let generation_id = "a".repeat(64);
    let canonical = format!(r#"{{"version":3,"generation_id":"{generation_id}"}}"#);
    assert_eq!(
        decode_commit_payload(&canonical).unwrap().generation_id,
        generation_id
    );
    assert_eq!(canonical_commit_payload(&generation_id).unwrap(), canonical);

    for encoded in [
        format!(" {canonical}"),
        format!("{canonical}\n"),
        format!(r#"{{"generation_id":"{generation_id}","version":3}}"#),
    ] {
        assert!(matches!(
            decode_commit_payload(&encoded),
            Err(IndexError::NonCanonicalCommitPayload)
        ));
    }
    assert!(matches!(
        decode_commit_payload(&canonical.replace(&generation_id, &"z".repeat(64))),
        Err(IndexError::InvalidGenerationId)
    ));
}

#[test]
fn malformed_current_and_unknown_envelopes_stay_json_errors() {
    let canonical = format!(r#"{{"version":3,"generation_id":"{}"}}"#, "a".repeat(64));
    for encoded in [
        "{".to_owned(),
        canonical.replace("\"version\":3", "\"version\":\"3\""),
        canonical.replace("\"version\":3", "\"version\":null"),
        canonical.replace("\"version\":3,", ""),
        canonical.replace("\"version\":3", "\"version\":3,\"version\":2"),
        canonical.replace("\"version\":3", "\"version\":3,\"unknown\":true"),
        format!("{canonical}{{}}"),
        released_v2_payload("null").replace("\"version\":2", "\"version\":3"),
        released_v2_payload("null").replace("\"version\":2", "\"version\":4"),
    ] {
        assert!(
            matches!(decode_commit_payload(&encoded), Err(IndexError::Json(_))),
            "unexpected classification for {encoded}"
        );
    }
}

#[test]
fn oversized_current_payloads_keep_the_256_byte_limit() {
    let canonical = format!(r#"{{"version":3,"generation_id":"{}"}}"#, "a".repeat(64));
    for total_bytes in [257, 64 * 1024 + 256, 64 * 1024 + 257] {
        let encoded = format!("{canonical}{}", " ".repeat(total_bytes - canonical.len()));
        assert!(matches!(
            decode_commit_payload(&encoded),
            Err(IndexError::CommitPayloadTooLarge { actual, maximum: 256 })
                if actual == total_bytes
        ));
    }
    let encoded = released_v2_payload(&format!("\"{}\"", "AAAA".repeat(64)))
        .replace("\"version\":2", "\"version\":3");
    assert!(matches!(
        decode_commit_payload(&encoded),
        Err(IndexError::CommitPayloadTooLarge { maximum: 256, .. })
    ));
}

fn route(byte: &str) -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256(byte.repeat(64)).unwrap()
}

fn fixture_source(name: &str) -> SourceKey {
    SourceKey::derive(
        "fixture",
        "fixture-format",
        "fixture-v1",
        1,
        SourceAnchor::provider_native("fixture.source", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn certified(source: SourceKey) -> CertifiedSource {
    let observation = SourceObservation::new(source, "fixture-revision", vec![1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser",
        [0; 32],
        ScannedSourceCounts::default(),
    )
    .unwrap()
}

fn manifest_at_revision(source: SourceKey, revision: u8) -> GenerationManifest {
    let observation = SourceObservation::new(source, "fixture-revision", vec![revision]).unwrap();
    GenerationManifest::from_sources(vec![CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 16,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()])
    .unwrap()
}

fn persist_and_cold_reopen(
    root: &Path,
    prepared: PreparedManifest,
) -> (String, Arc<GenerationManifest>) {
    let generation_id = prepared.generation_id().to_owned();
    write_prepared_manifest(root, &prepared).unwrap();
    drop(prepared);
    clear_manifest_cache_for_root(root).unwrap();
    let reopened = load_materialized_manifest(root, &generation_id, 0).unwrap();
    (generation_id, reopened)
}

#[test]
fn descriptor_replacements_reset_flat_delta_base_and_survive_cold_reopen() {
    for (format, variant, identity_version) in [
        ("fixture-format-v2", "fixture-v1", 1),
        ("fixture-format", "fixture-v2", 1),
        ("fixture-format", "fixture-v1", 2),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let original = fixture_source("descriptor-replacement");
        let replacement = SourceKey::derive(
            "fixture",
            format,
            variant,
            identity_version,
            original.anchor().clone(),
        )
        .unwrap();
        assert_eq!(original.identity(), replacement.identity());
        assert!(!original.exact_descriptor_eq(&replacement));

        let base = manifest_at_revision(original.clone(), 1);
        let base_id = base.generation_id().unwrap();
        write_manifest(temp.path(), &base_id, &base).unwrap();
        let changed = manifest_at_revision(original, 2);
        let prepared =
            prepare_successor_manifest(temp.path(), Arc::new(changed), Some((&base_id, &base)))
                .unwrap();
        assert!(prepared.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
        drop(base);
        let (delta_id, delta_base) = persist_and_cold_reopen(temp.path(), prepared);

        let migrated = manifest_at_revision(replacement.clone(), 3);
        let expected_migrated = serde_json::to_vec(&migrated).unwrap();
        let prepared = prepare_successor_manifest(
            temp.path(),
            Arc::new(migrated),
            Some((&delta_id, &delta_base)),
        )
        .unwrap();
        let is_full = !prepared.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX);
        drop(delta_base);
        let (migrated_id, migrated_base) = persist_and_cold_reopen(temp.path(), prepared);
        assert!(is_full);
        assert_eq!(
            serde_json::to_vec(migrated_base.as_ref()).unwrap(),
            expected_migrated
        );
        assert!(migrated_base.sources[0]
            .observation()
            .source()
            .exact_descriptor_eq(&replacement));

        let updated = manifest_at_revision(replacement, 4);
        let expected_updated = serde_json::to_vec(&updated).unwrap();
        let prepared = prepare_successor_manifest(
            temp.path(),
            Arc::new(updated),
            Some((&migrated_id, &migrated_base)),
        )
        .unwrap();
        let delta: StoredManifestFlatDeltaV1 = serde_json::from_slice(&prepared.bytes).unwrap();
        assert_eq!(delta.base_generation_id, migrated_id);
        drop(migrated_base);
        let (_, reopened) = persist_and_cold_reopen(temp.path(), prepared);
        assert_eq!(
            serde_json::to_vec(reopened.as_ref()).unwrap(),
            expected_updated
        );
    }
}

#[test]
fn descriptor_only_replacements_survive_cold_reopen_from_full_and_delta_bases() {
    for delta_base in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let original = fixture_source("descriptor-only-replacement");
        let replacement = SourceKey::derive(
            "fixture",
            "fixture-format",
            "fixture-v2",
            1,
            original.anchor().clone(),
        )
        .unwrap();
        let base = manifest_at_revision(original.clone(), 1);
        let prepared = prepare_successor_manifest(temp.path(), Arc::new(base), None).unwrap();
        let (mut base_id, mut base) = persist_and_cold_reopen(temp.path(), prepared);
        let revision = if delta_base { 2 } else { 1 };
        if delta_base {
            let successor = manifest_at_revision(original, revision);
            let prepared = prepare_successor_manifest(
                temp.path(),
                Arc::new(successor),
                Some((&base_id, &base)),
            )
            .unwrap();
            assert!(prepared.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
            drop(base);
            (base_id, base) = persist_and_cold_reopen(temp.path(), prepared);
        }

        let successor = manifest_at_revision(replacement.clone(), revision);
        let expected = serde_json::to_vec(&successor).unwrap();
        // Lineage-based equality alone cannot detect this descriptor-only change.
        assert_eq!(base.sources, successor.sources);
        assert_eq!(
            base.core_record_aggregates,
            successor.core_record_aggregates
        );
        assert_ne!(serde_json::to_vec(base.as_ref()).unwrap(), expected);
        let prepared =
            prepare_successor_manifest(temp.path(), Arc::new(successor), Some((&base_id, &base)))
                .unwrap();
        let is_full = !prepared.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX);
        drop(base);
        let (successor_id, reopened) = persist_and_cold_reopen(temp.path(), prepared);
        assert_ne!(successor_id, base_id);
        assert!(is_full);
        assert_eq!(serde_json::to_vec(reopened.as_ref()).unwrap(), expected);
        assert!(reopened.sources[0]
            .observation()
            .source()
            .exact_descriptor_eq(&replacement));
    }
}

#[test]
fn unchanged_descriptors_keep_flat_deltas_across_cold_reopens() {
    let temp = tempfile::tempdir().unwrap();
    let source = fixture_source("unchanged-descriptor");
    let base = manifest_at_revision(source.clone(), 1);
    let base_id = base.generation_id().unwrap();
    write_manifest(temp.path(), &base_id, &base).unwrap();
    let mut previous_id = base_id.clone();
    let mut previous = Arc::new(base);
    for revision in [2, 3] {
        let successor = manifest_at_revision(source.clone(), revision);
        let expected = serde_json::to_vec(&successor).unwrap();
        let prepared = prepare_successor_manifest(
            temp.path(),
            Arc::new(successor),
            Some((&previous_id, &previous)),
        )
        .unwrap();
        let delta: StoredManifestFlatDeltaV1 = serde_json::from_slice(&prepared.bytes).unwrap();
        assert_eq!(delta.base_generation_id, base_id);
        assert_eq!(delta.changes.len(), 1);
        drop(previous);
        (previous_id, previous) = persist_and_cold_reopen(temp.path(), prepared);
        assert_eq!(serde_json::to_vec(previous.as_ref()).unwrap(), expected);
    }
    let replay = prepare_successor_manifest(
        temp.path(),
        Arc::clone(&previous),
        Some((&previous_id, &previous)),
    )
    .unwrap();
    assert_eq!(replay.generation_id(), previous_id);
    assert_eq!(
        replay.bytes,
        load_manifest_bytes(temp.path(), &previous_id).unwrap()
    );
}

#[test]
fn membership_only_successor_uses_a_full_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let route = route("4");
    let alpha = fixture_source("alpha");
    let beta = fixture_source("beta");
    let sources = vec![certified(alpha.clone()), certified(beta.clone())];
    let aggregates = [&alpha, &beta]
        .into_iter()
        .map(|source| {
            SourceCoreRecordAggregate::new(source_token(source), 0, "00".repeat(32)).unwrap()
        })
        .collect::<Vec<_>>();
    let definition = ProviderRootDefinition {
        id: "codex".to_owned(),
        provider: CaptureProvider::Codex,
        path: "/fixtures/codex".into(),
        group: None,
        kind: None,
    };
    let build = |token| {
        GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
            sources.clone(),
            aggregates.clone(),
            vec![
                SourceRouteSnapshot::present(route.clone(), vec![alpha.clone(), beta.clone()])
                    .unwrap(),
            ],
            true,
            provider_source_config_digest(true, std::slice::from_ref(&definition)),
            vec![
                AppliedProviderRoot::new(definition.clone(), vec![route.clone()])
                    .unwrap()
                    .with_exact_source_memberships(vec![
                        AppliedProviderRootSourceMembership::exact(route.clone(), vec![token])
                            .unwrap(),
                    ])
                    .unwrap(),
            ],
        )
        .unwrap()
    };
    let base = build(source_token(&alpha));
    let base_id = base.generation_id().unwrap();
    write_manifest(temp.path(), &base_id, &base).unwrap();
    let no_op =
        prepare_successor_manifest(temp.path(), Arc::new(base.clone()), Some((&base_id, &base)))
            .unwrap();
    assert_eq!(no_op.generation_id(), base_id);
    assert_eq!(no_op.bytes, serde_json::to_vec(&base).unwrap());
    let successor = build(source_token(&beta));
    let prepared =
        prepare_successor_manifest(temp.path(), Arc::new(successor), Some((&base_id, &base)))
            .unwrap();

    assert!(!prepared.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
    let persisted: GenerationManifest = serde_json::from_slice(&prepared.bytes).unwrap();
    persisted.validate_contract().unwrap();
}

#[test]
fn generation_state_only_successor_uses_a_full_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let base = GenerationManifest::from_sources(Vec::new())
        .unwrap()
        .with_generation_state(
            crate::GenerationStateEnvelope::new("ctx.test-state.v1", b"one".to_vec()).unwrap(),
        )
        .unwrap();
    let base_id = base.generation_id().unwrap();
    write_manifest(temp.path(), &base_id, &base).unwrap();
    let successor = base
        .clone()
        .with_generation_state(
            crate::GenerationStateEnvelope::new("ctx.test-state.v1", b"two".to_vec()).unwrap(),
        )
        .unwrap();

    let prepared =
        prepare_successor_manifest(temp.path(), Arc::new(successor), Some((&base_id, &base)))
            .unwrap();

    assert_ne!(prepared.generation_id(), base_id);
    assert!(!prepared.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
}

#[test]
fn detached_authority_change_survives_a_cold_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let source = fixture_source("detached-authority-delta");
    let source_route = route("5");
    let released = AppliedProviderRoot::with_source_identity(
        ProviderRootDefinition {
            id: "codex".to_owned(),
            provider: CaptureProvider::Codex,
            path: temp.path().join("codex"),
            group: None,
            kind: None,
        },
        ProviderRootSourceIdentity::Released,
        Vec::new(),
    )
    .unwrap();
    let authority = DetachedReleasedProviderRootAuthority::from_applied(&released)
        .unwrap()
        .unwrap();
    let build = |certified_source, authorities| {
        GenerationManifest::from_parts_with_record_aggregates_and_provider_roots_and_detached_authorities(
            vec![certified_source],
            vec![
                SourceCoreRecordAggregate::new(source_token(&source), 0, "00".repeat(32)).unwrap(),
            ],
            vec![SourceRouteSnapshot::present(source_route.clone(), vec![source.clone()]).unwrap()],
            true,
            provider_source_config_digest(true, &[]),
            Vec::new(),
            authorities,
        )
        .unwrap()
    };
    let base = build(certified(source.clone()), Vec::new());
    let base_id = base.generation_id().unwrap();
    write_manifest(temp.path(), &base_id, &base).unwrap();
    let successor_observation =
        SourceObservation::new(source.clone(), "fixture-revision", vec![2]).unwrap();
    let successor_source = CertifiedSource::certify(
        successor_observation.clone(),
        successor_observation,
        "fixture-parser",
        [1; 32],
        ScannedSourceCounts::default(),
    )
    .unwrap();
    let successor = build(successor_source, vec![authority]);
    let prepared = prepare_successor_manifest(
        temp.path(),
        Arc::new(successor.clone()),
        Some((&base_id, &base)),
    )
    .unwrap();

    assert!(!prepared.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
    write_prepared_manifest(temp.path(), &prepared).unwrap();
    clear_manifest_cache_for_root(temp.path()).unwrap();
    let reopened = load_materialized_manifest(temp.path(), prepared.generation_id(), 0).unwrap();
    assert_eq!(
        reopened.detached_released_provider_roots(),
        successor.detached_released_provider_roots()
    );
}
