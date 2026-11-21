use super::*;
use crate::{
    provider_source_config_digest, source_token, AppliedProviderRoot,
    AppliedProviderRootSourceMembership, DetachedReleasedProviderRootAuthority,
    ProviderRootDefinition, ProviderRootSourceIdentity, SourceCoreRecordAggregate,
    SourceRouteIdentity, SourceRouteSnapshot,
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

<<<<<<< HEAD
fn certified_at(source: SourceKey, revision: u8, docs: u64, bytes: u64) -> CertifiedSource {
    let observation = SourceObservation::new(source, "fixture-revision", vec![revision]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: docs,
            retained_records: docs,
            indexed_documents: docs,
            certified_bytes: bytes,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn manifest_with_routes(
    route_entries: Vec<(SourceRouteIdentity, Vec<CertifiedSource>)>,
    state: Option<crate::GenerationStateEnvelope>,
) -> GenerationManifest {
    let mut sources = Vec::new();
    let mut aggregates = Vec::new();
    let mut routes = Vec::new();
    for (route_id, route_sources) in route_entries {
        let mut keys = Vec::with_capacity(route_sources.len());
        for src in route_sources {
            let key = src.observation().source().clone();
            aggregates.push(
                SourceCoreRecordAggregate::new(
                    source_token(&key),
                    src.counts().indexed_documents,
                    "00".repeat(32),
                )
                .unwrap(),
            );
            keys.push(key);
            sources.push(src);
        }
        routes.push(SourceRouteSnapshot::present(route_id, keys).unwrap());
    }
    let manifest =
        GenerationManifest::from_parts_with_record_aggregates(sources, aggregates, routes).unwrap();
    match state {
        Some(state) => manifest.with_generation_state(state).unwrap(),
        None => manifest,
    }
}

#[test]
fn flat_delta_carries_added_sources_updates_and_generation_state_across_cold_reopens() {
    let temp = tempfile::tempdir().unwrap();
    let route_a = route("1");
    let route_b = route("2");
    let s1 = fixture_source("session-1");
    let s2 = fixture_source("session-2");
    let s3 = fixture_source("session-3");
    let s4 = fixture_source("session-4");

    let state_0 = crate::GenerationStateEnvelope::new("ctx.test-state.v1", b"s0".to_vec()).unwrap();
    let state_1 = crate::GenerationStateEnvelope::new("ctx.test-state.v1", b"s1".to_vec()).unwrap();
    let state_2 = crate::GenerationStateEnvelope::new("ctx.test-state.v1", b"s2".to_vec()).unwrap();

    // Anchor with s1 in route_a, s2 in route_b.
    let anchor = manifest_with_routes(
        vec![
            (route_a.clone(), vec![certified_at(s1.clone(), 1, 2, 100)]),
            (route_b.clone(), vec![certified_at(s2.clone(), 1, 3, 200)]),
        ],
        Some(state_0),
    );
    let anchor_id = anchor.generation_id().unwrap();
    write_manifest(temp.path(), &anchor_id, &anchor).unwrap();

    // Publication 1: add new source s3 to route_a and update generation_state -> flat delta.
    let gen1 = manifest_with_routes(
        vec![
            (
                route_a.clone(),
                vec![
                    certified_at(s1.clone(), 1, 2, 100),
                    certified_at(s3.clone(), 1, 4, 300),
                ],
            ),
            (route_b.clone(), vec![certified_at(s2.clone(), 1, 3, 200)]),
        ],
        Some(state_1.clone()),
    );
    let expected_gen1 = serde_json::to_vec(&gen1).unwrap();
    let prep1 =
        prepare_successor_manifest(temp.path(), Arc::new(gen1), Some((&anchor_id, &anchor)))
            .unwrap();
    assert!(prep1.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
    let delta1: StoredManifestFlatDeltaV1 = serde_json::from_slice(&prep1.bytes).unwrap();
    assert_eq!(delta1.base_generation_id, anchor_id);
    assert!(delta1.changes.is_empty());
    assert_eq!(delta1.added.len(), 1);
    assert_eq!(delta1.route_additions.len(), 1);
    assert_eq!(delta1.generation_state.as_ref(), Some(&state_1));
    let (gen1_id, reopened1) = persist_and_cold_reopen(temp.path(), prep1);
    assert_eq!(
        serde_json::to_vec(reopened1.as_ref()).unwrap(),
        expected_gen1
    );

    // Publication 2: modify s3 (previously added!) + modify base s1 + add s4 to route_b.
    // s3 MUST remain in `added` (never moved to `changes`), flattened directly against anchor_id!
    let gen2 = manifest_with_routes(
        vec![
            (
                route_a.clone(),
                vec![
                    certified_at(s1.clone(), 2, 5, 250),
                    certified_at(s3.clone(), 2, 6, 450),
                ],
            ),
            (
                route_b.clone(),
                vec![
                    certified_at(s2.clone(), 1, 3, 200),
                    certified_at(s4.clone(), 1, 1, 80),
                ],
            ),
        ],
        Some(state_1.clone()),
    );
    let expected_gen2 = serde_json::to_vec(&gen2).unwrap();
    let prep2 =
        prepare_successor_manifest(temp.path(), Arc::new(gen2), Some((&gen1_id, &reopened1)))
            .unwrap();
    assert!(prep2.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
    let delta2: StoredManifestFlatDeltaV1 = serde_json::from_slice(&prep2.bytes).unwrap();
    assert_eq!(delta2.base_generation_id, anchor_id);
    assert_eq!(delta2.changes.len(), 1);
    assert_eq!(delta2.changes[0].source_identity, s1.identity().digest());
    assert_eq!(delta2.added.len(), 2);
    assert_eq!(delta2.route_additions.len(), 2);
    assert_eq!(delta2.generation_state.as_ref(), Some(&state_1));
    let (gen2_id, reopened2) = persist_and_cold_reopen(temp.path(), prep2);
    assert_eq!(
        serde_json::to_vec(reopened2.as_ref()).unwrap(),
        expected_gen2
    );

    // Publication 3: generation_state-only update on non-empty delta base stays a flat delta.
    let gen3 = manifest_with_routes(
        vec![
            (
                route_a.clone(),
                vec![
                    certified_at(s1.clone(), 2, 5, 250),
                    certified_at(s3.clone(), 2, 6, 450),
                ],
            ),
            (
                route_b.clone(),
                vec![
                    certified_at(s2.clone(), 1, 3, 200),
                    certified_at(s4.clone(), 1, 1, 80),
                ],
            ),
        ],
        Some(state_2.clone()),
    );
    let expected_gen3 = serde_json::to_vec(&gen3).unwrap();
    let prep3 =
        prepare_successor_manifest(temp.path(), Arc::new(gen3), Some((&gen2_id, &reopened2)))
            .unwrap();
    assert!(prep3.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
    let delta3: StoredManifestFlatDeltaV1 = serde_json::from_slice(&prep3.bytes).unwrap();
    assert_eq!(delta3.base_generation_id, anchor_id);
    assert_eq!(delta3.generation_state.as_ref(), Some(&state_2));
    let (gen3_id, reopened3) = persist_and_cold_reopen(temp.path(), prep3);
    assert_eq!(
        serde_json::to_vec(reopened3.as_ref()).unwrap(),
        expected_gen3
    );

    // Publication 4: removing a previously added source falls back to a full manifest.
    let gen4 = manifest_with_routes(
        vec![
            (route_a.clone(), vec![certified_at(s1.clone(), 2, 5, 250)]),
            (
                route_b.clone(),
                vec![
                    certified_at(s2.clone(), 1, 3, 200),
                    certified_at(s4.clone(), 1, 1, 80),
                ],
            ),
        ],
        Some(state_2),
    );
    let expected_gen4 = serde_json::to_vec(&gen4).unwrap();
    let prep4 =
        prepare_successor_manifest(temp.path(), Arc::new(gen4), Some((&gen3_id, &reopened3)))
            .unwrap();
    assert!(!prep4.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
    let (_, reopened4) = persist_and_cold_reopen(temp.path(), prep4);
    assert_eq!(
        serde_json::to_vec(reopened4.as_ref()).unwrap(),
        expected_gen4
    );
}

#[test]
fn old_delta_encoding_omits_empty_additive_fields() {
    let temp = tempfile::tempdir().unwrap();
    let s1 = fixture_source("legacy-compat");
    let base = manifest_at_revision(s1.clone(), 1);
    let base_id = base.generation_id().unwrap();
    write_manifest(temp.path(), &base_id, &base).unwrap();
    let successor = manifest_at_revision(s1, 2);
    let prepared =
        prepare_successor_manifest(temp.path(), Arc::new(successor), Some((&base_id, &base)))
            .unwrap();
    let encoded = std::str::from_utf8(&prepared.bytes).unwrap();
    assert!(encoded.starts_with(r#"{"storage_format":"ctx-manifest-flat-delta-v1","#));
    assert!(!encoded.contains("\"added\""));
    assert!(!encoded.contains("\"route_additions\""));
    assert!(!encoded.contains("\"generation_state\""));
}

#[test]
fn flat_delta_falls_back_to_full_when_added_plus_changed_exceeds_bound() {
    let temp = tempfile::tempdir().unwrap();
    let route_a = route("9");
    let base_source = fixture_source("base-0");
    let anchor = manifest_with_routes(
        vec![(
            route_a.clone(),
            vec![certified_at(base_source.clone(), 1, 1, 10)],
        )],
        None,
    );
    let anchor_id = anchor.generation_id().unwrap();
    write_manifest(temp.path(), &anchor_id, &anchor).unwrap();

    // Adding exactly MAX_MANIFEST_DELTA_CHANGES stays a delta.
    let mut at_limit_sources = vec![certified_at(base_source.clone(), 1, 1, 10)];
    for i in 0..MAX_MANIFEST_DELTA_CHANGES {
        at_limit_sources.push(certified_at(fixture_source(&format!("add-{i}")), 1, 1, 10));
    }
    let at_limit = manifest_with_routes(vec![(route_a.clone(), at_limit_sources.clone())], None);
    let prep_at_limit =
        prepare_successor_manifest(temp.path(), Arc::new(at_limit), Some((&anchor_id, &anchor)))
            .unwrap();
    assert!(prep_at_limit.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));

    // Adding one more (MAX_MANIFEST_DELTA_CHANGES + 1) falls back to a full manifest.
    at_limit_sources.push(certified_at(
        fixture_source(&format!("add-{MAX_MANIFEST_DELTA_CHANGES}")),
        1,
        1,
        10,
    ));
    let over_limit = manifest_with_routes(vec![(route_a, at_limit_sources)], None);
    let prep_over = prepare_successor_manifest(
        temp.path(),
        Arc::new(over_limit),
        Some((&anchor_id, &anchor)),
    )
    .unwrap();
    assert!(!prep_over.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
}

#[test]
fn malformed_additive_deltas_are_rejected_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let route_a = route("a");
    let route_b = route("b");
    let s1 = fixture_source("s1");
    let s2 = fixture_source("s2");
    let s3 = fixture_source("s3");
    let base = manifest_with_routes(
        vec![
            (route_a.clone(), vec![certified_at(s1.clone(), 1, 2, 100)]),
            (route_b.clone(), vec![certified_at(s2.clone(), 1, 1, 50)]),
        ],
        None,
    );
    let base_id = base.generation_id().unwrap();
    write_manifest(temp.path(), &base_id, &base).unwrap();

    let s3_cert = certified_at(s3.clone(), 1, 3, 75);
    let s3_agg = SourceCoreRecordAggregate::new(source_token(&s3), 3, "00".repeat(32)).unwrap();
    let s3_entry = StoredManifestSourceChangeV1 {
        source_identity: s3.identity().digest(),
        source: s3_cert.clone(),
        aggregate: s3_agg.clone(),
    };
    let s1_cert = certified_at(s1.clone(), 2, 2, 100);
    let s1_agg = SourceCoreRecordAggregate::new(source_token(&s1), 2, "00".repeat(32)).unwrap();

    let try_load = |delta: StoredManifestFlatDeltaV1| {
        let bytes = serde_json::to_vec(&delta).unwrap();
        let id = sha256_hex(&bytes);
        write_manifest_bytes(temp.path(), &id, &bytes).unwrap();
        clear_manifest_cache_for_root(temp.path()).unwrap();
        load_materialized_manifest(temp.path(), &id, 0)
    };

    // 1. Empty delta (no changes, no additions, no generation_state).
    assert!(matches!(
        try_load(StoredManifestFlatDeltaV1 {
            storage_format: MANIFEST_FLAT_DELTA_STORAGE.to_owned(),
            base_generation_id: base_id.clone(),
            indexed_documents: 3,
            certified_source_bytes: 150,
            source_count: 2,
            changes: Vec::new(),
            added: Vec::new(),
            route_additions: Vec::new(),
            generation_state: None,
        }),
        Err(IndexError::NonCanonicalManifest)
    ));

    // 2. Added source without route_additions.
    assert!(matches!(
        try_load(StoredManifestFlatDeltaV1 {
            storage_format: MANIFEST_FLAT_DELTA_STORAGE.to_owned(),
            base_generation_id: base_id.clone(),
            indexed_documents: 6,
            certified_source_bytes: 225,
            source_count: 3,
            changes: Vec::new(),
            added: vec![StoredManifestSourceChangeV1 {
                source_identity: s3.identity().digest(),
                source: s3_cert.clone(),
                aggregate: s3_agg.clone(),
            }],
            route_additions: Vec::new(),
            generation_state: None,
        }),
        Err(IndexError::NonCanonicalManifest)
    ));

    // 3. Added source claimed by two routes.
    assert!(matches!(
        try_load(StoredManifestFlatDeltaV1 {
            storage_format: MANIFEST_FLAT_DELTA_STORAGE.to_owned(),
            base_generation_id: base_id.clone(),
            indexed_documents: 6,
            certified_source_bytes: 225,
            source_count: 3,
            changes: Vec::new(),
            added: vec![StoredManifestSourceChangeV1 {
                source_identity: s3.identity().digest(),
                source: s3_cert.clone(),
                aggregate: s3_agg.clone(),
            }],
            route_additions: vec![
                StoredManifestRouteAdditionV1 {
                    route_identity: route_a.clone(),
                    sources: vec![s3.clone()],
                },
                StoredManifestRouteAdditionV1 {
                    route_identity: route_b.clone(),
                    sources: vec![s3.clone()],
                },
            ],
            generation_state: None,
        }),
        Err(IndexError::SourceOwnedByMultipleRoutes(_))
    ));

    // 4. Added source already present in base (s1).
    assert!(matches!(
        try_load(StoredManifestFlatDeltaV1 {
            storage_format: MANIFEST_FLAT_DELTA_STORAGE.to_owned(),
            base_generation_id: base_id.clone(),
            indexed_documents: 5,
            certified_source_bytes: 250,
            source_count: 3,
            changes: Vec::new(),
            added: vec![StoredManifestSourceChangeV1 {
                source_identity: s1.identity().digest(),
                source: s1_cert.clone(),
                aggregate: s1_agg.clone(),
            }],
            route_additions: vec![StoredManifestRouteAdditionV1 {
                route_identity: route_a.clone(),
                sources: vec![s1.clone()],
            }],
            generation_state: None,
        }),
        Err(IndexError::NonCanonicalManifest)
    ));

    // 5. Wrong totals (indexed_documents mismatch).
    assert!(matches!(
        try_load(StoredManifestFlatDeltaV1 {
            storage_format: MANIFEST_FLAT_DELTA_STORAGE.to_owned(),
            base_generation_id: base_id.clone(),
            indexed_documents: 999,
            certified_source_bytes: 225,
            source_count: 3,
            changes: Vec::new(),
            added: vec![s3_entry],
            route_additions: vec![StoredManifestRouteAdditionV1 {
                route_identity: route_a,
                sources: vec![s3.clone()],
            }],
            generation_state: None,
        }),
        Err(IndexError::NonCanonicalManifest)
    ));

    // 6. Explicit empty `"added":[]` is non-canonical JSON.
    let mut raw = serde_json::to_value(StoredManifestFlatDeltaV1 {
        storage_format: MANIFEST_FLAT_DELTA_STORAGE.to_owned(),
        base_generation_id: base_id,
        indexed_documents: 3,
        certified_source_bytes: 150,
        source_count: 2,
        changes: vec![StoredManifestSourceChangeV1 {
            source_identity: s1.identity().digest(),
            source: s1_cert,
            aggregate: s1_agg,
        }],
        added: Vec::new(),
        route_additions: Vec::new(),
        generation_state: None,
    })
    .unwrap();
    raw.as_object_mut()
        .unwrap()
        .insert("added".to_owned(), serde_json::json!([]));
    let non_canonical = serde_json::to_vec(&raw).unwrap();
    assert!(matches!(
        decode_validated_delta(&non_canonical),
        Err(IndexError::NonCanonicalManifest)
    ));
    let _ = MAX_MANIFEST_DELTA_BYTES;
}

#[test]
fn adding_one_source_to_large_manifest_cuts_written_bytes_by_orders_of_magnitude() {
    let temp = tempfile::tempdir().unwrap();
    let route_a = route("c");
    let mut sources = (0..2_000)
        .map(|i| certified_at(fixture_source(&format!("bulk-session-{i:05}")), 1, 5, 512))
        .collect::<Vec<_>>();
    let anchor = manifest_with_routes(vec![(route_a.clone(), sources.clone())], None);
    let anchor_bytes = serde_json::to_vec(&anchor).unwrap();
    let anchor_id = sha256_hex(&anchor_bytes);
    write_manifest_bytes(temp.path(), &anchor_id, &anchor_bytes).unwrap();

    sources.push(certified_at(fixture_source("bulk-session-new"), 1, 5, 512));
    let successor = manifest_with_routes(vec![(route_a, sources)], None);
    let full_successor_bytes = serde_json::to_vec(&successor).unwrap();
    let prepared = prepare_successor_manifest(
        temp.path(),
        Arc::new(successor),
        Some((&anchor_id, &anchor)),
    )
    .unwrap();
    assert!(prepared.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
    // Full manifest is > 2 MB for 2,000 sources; delta for 1 added source is < 3 KB (> 500x smaller).
    assert!(
        prepared.bytes.len() * 200 < full_successor_bytes.len(),
        "expected flat delta ({} B) to be >200x smaller than full manifest ({} B)",
        prepared.bytes.len(),
        full_successor_bytes.len()
    );
    let (_, reopened) = persist_and_cold_reopen(temp.path(), prepared);
    assert_eq!(reopened.sources.len(), 2_001);
}

#[test]
fn manifest_summary_sidecar_loads_without_reading_full_manifest_and_falls_back_safely() {
    let temp = tempfile::tempdir().unwrap();
    let source = fixture_source("summary-sidecar");
    let manifest = manifest_at_revision(source, 1);
    let generation_id = manifest.generation_id().unwrap();
    write_manifest(temp.path(), &generation_id, &manifest).unwrap();

    let summary_path =
        ctx_history_index_generation::manifest_summary_path(temp.path(), &generation_id);
    assert!(summary_path.is_file());

    clear_manifest_cache_for_root(temp.path()).unwrap();
    ctx_history_index_generation::read_metrics::reset_metadata_read_activity();
    let summary = summary::try_load_validated_manifest_summary(temp.path(), &generation_id)
        .unwrap()
        .expect("valid summary sidecar should load");
    let reads = ctx_history_index_generation::read_metrics::metadata_read_activity();
    assert_eq!(reads.manifest.operations, 0);
    assert_eq!(reads.manifest.bytes, 0);
    assert_eq!(summary.generation_id, generation_id);
    assert_eq!(summary.indexed_documents, manifest.indexed_documents);
    assert_eq!(
        summary.certified_source_bytes,
        manifest.certified_source_bytes
    );
    assert_eq!(summary.source_count, manifest.sources.len() as u64);

    // Non-canonical whitespace in sidecar falls back cleanly (returns None).
    let raw_summary = std::fs::read(&summary_path).unwrap();
    let mut padded = raw_summary.clone();
    padded.push(b'\n');
    std::fs::write(&summary_path, &padded).unwrap();
    assert!(
        summary::try_load_validated_manifest_summary(temp.path(), &generation_id)
            .unwrap()
            .is_none()
    );

    // Corrupt JSON in sidecar falls back cleanly.
    std::fs::write(&summary_path, b"{not json").unwrap();
    assert!(
        summary::try_load_validated_manifest_summary(temp.path(), &generation_id)
            .unwrap()
            .is_none()
    );

    // Length-mismatched summary falls back cleanly.
    let mut mismatched = summary.clone();
    mismatched.manifest_bytes_len += 1;
    std::fs::write(&summary_path, serde_json::to_vec(&mismatched).unwrap()).unwrap();
    assert!(
        summary::try_load_validated_manifest_summary(temp.path(), &generation_id)
            .unwrap()
            .is_none()
    );

    // Wrong generation_id in summary falls back cleanly.
    let mut wrong_id = summary.clone();
    wrong_id.generation_id = "0".repeat(64);
    std::fs::write(&summary_path, serde_json::to_vec(&wrong_id).unwrap()).unwrap();
    assert!(
        summary::try_load_validated_manifest_summary(temp.path(), &generation_id)
            .unwrap()
            .is_none()
    );

    // Missing sidecar falls back cleanly and is backfilled by idempotent write_manifest.
    std::fs::remove_file(&summary_path).unwrap();
    assert!(
        summary::try_load_validated_manifest_summary(temp.path(), &generation_id)
            .unwrap()
            .is_none()
    );
    write_manifest(temp.path(), &generation_id, &manifest).unwrap();
    assert!(summary_path.is_file());
    assert!(
        summary::try_load_validated_manifest_summary(temp.path(), &generation_id)
            .unwrap()
            .is_some()
    );

    // Deleting the authoritative manifest MUST fail closed even when the sidecar exists!
    std::fs::remove_file(manifest_path(temp.path(), &generation_id)).unwrap();
    assert!(matches!(
        summary::try_load_validated_manifest_summary(temp.path(), &generation_id),
        Err(IndexError::MissingManifest(missing)) if missing == generation_id
    ));
}

#[test]
fn manifest_summary_avoids_reading_megabytes_on_large_corpus() {
    let temp = tempfile::tempdir().unwrap();
    let sources = (0..2_000)
        .map(|i| {
            let source = fixture_source(&format!("summary-scale-{i:05}"));
            let obs = SourceObservation::new(source, "fixture-revision", vec![1]).unwrap();
            CertifiedSource::certify(
                obs.clone(),
                obs,
                "fixture-parser",
                [1; 32],
                ScannedSourceCounts {
                    complete_records: 5,
                    retained_records: 5,
                    indexed_documents: 5,
                    certified_bytes: 512,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let manifest = GenerationManifest::from_sources(sources).unwrap();
    let generation_id = manifest.generation_id().unwrap();
    write_manifest(temp.path(), &generation_id, &manifest).unwrap();

    let full_bytes = std::fs::metadata(manifest_path(temp.path(), &generation_id))
        .unwrap()
        .len();
    let summary_bytes = std::fs::metadata(ctx_history_index_generation::manifest_summary_path(
        temp.path(),
        &generation_id,
    ))
    .unwrap()
    .len();
    assert!(
        full_bytes > 2_000_000 && summary_bytes < 1_024,
        "expected >2 MB manifest ({full_bytes} B) and <1 KB summary ({summary_bytes} B)"
    );

    clear_manifest_cache_for_root(temp.path()).unwrap();
    ctx_history_index_generation::read_metrics::reset_metadata_read_activity();
    let start_summary = std::time::Instant::now();
    let summary = summary::try_load_validated_manifest_summary(temp.path(), &generation_id)
        .unwrap()
        .expect("summary should load");
    let summary_elapsed = start_summary.elapsed();
    let summary_reads = ctx_history_index_generation::read_metrics::metadata_read_activity();
    assert_eq!(summary_reads.manifest.operations, 0);
    assert_eq!(summary_reads.manifest.bytes, 0);
    assert_eq!(summary.source_count, 2_000);
    assert_eq!(summary.indexed_documents, 10_000);

    clear_manifest_cache_for_root(temp.path()).unwrap();
    ctx_history_index_generation::read_metrics::reset_metadata_read_activity();
    let start_full = std::time::Instant::now();
    let loaded = load_materialized_manifest(temp.path(), &generation_id, 0).unwrap();
    let full_elapsed = start_full.elapsed();
    let full_reads = ctx_history_index_generation::read_metrics::metadata_read_activity();
    assert_eq!(full_reads.manifest.operations, 1);
    assert_eq!(full_reads.manifest.bytes, full_bytes);
    assert_eq!(loaded.sources.len(), 2_000);
    eprintln!(
        "summary_scale_measurement: sources=2000 full_bytes={full_bytes} summary_bytes={summary_bytes} full_ms={:.2} summary_ms={:.2}",
        full_elapsed.as_secs_f64() * 1000.0,
        summary_elapsed.as_secs_f64() * 1000.0
    );
}
