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
