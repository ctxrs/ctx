use super::*;
use crate::{
    source_token, AppliedProviderRoot, AppliedProviderRootSourceMembership,
    DetachedReleasedProviderRootAuthority,
};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, ScannedSourceCounts, SourceAnchor, SourceKey,
    SourceObservation, TypedKey,
};

const V8_EMPTY_FIXTURE: &[u8] = br#"{"manifest_version":8,"identity_version":1,"core_record_version":3,"core_record_contract_fingerprint":"ebb5c9b638de184824a6ce141ebf9b70941fb293fc113d29e2851565bad4371e","lexical_schema_version":22,"lexical_analyzer_version":2,"policy_schema_hash":"98a522ab684f09534a71628117e182f3559d7094880609a74e81041d00361475","indexed_documents":0,"certified_source_bytes":0,"sources":[],"core_record_aggregates":[],"source_routes":[]}"#;
const V9_CODEX_FIXTURE: &[u8] = br#"{"manifest_version":9,"identity_version":1,"core_record_version":3,"core_record_contract_fingerprint":"ebb5c9b638de184824a6ce141ebf9b70941fb293fc113d29e2851565bad4371e","lexical_schema_version":22,"lexical_analyzer_version":2,"policy_schema_hash":"98a522ab684f09534a71628117e182f3559d7094880609a74e81041d00361475","indexed_documents":0,"certified_source_bytes":0,"sources":[],"core_record_aggregates":[],"source_routes":[],"automatic_provider_discovery":true,"provider_root_config_digest":"655246d699705d7c3bee11f277332db40cd54fda6a8e75a0ea10eec60306d3c2","provider_roots":[{"definition":{"id":"codex","provider":"codex","path":"/fixtures/codex"},"source_identity":"released","routes":[]}]}"#;

fn v9_value() -> serde_json::Value {
    serde_json::from_slice(V9_CODEX_FIXTURE).unwrap()
}

fn canonical_v9_bytes(value: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::from_value::<PreviousGenerationManifestV9>(value).unwrap())
        .unwrap()
}

fn route(byte: &str) -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256(byte.repeat(64)).unwrap()
}

fn previous_root(
    id: &str,
    provider: &str,
    source_identity: &str,
    routes: Vec<SourceRouteIdentity>,
) -> serde_json::Value {
    serde_json::json!({
        "definition": {"id": id, "provider": provider, "path": format!("/fixtures/{id}")},
        "source_identity": source_identity,
        "routes": routes,
    })
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

fn write_literal_manifest(root: &Path, bytes: &[u8]) -> String {
    let generation_id = sha256_hex(bytes);
    write_manifest_bytes(root, &generation_id, bytes).unwrap();
    generation_id
}

fn current_as_v9(manifest: GenerationManifest) -> Vec<u8> {
    let mut value = serde_json::to_value(manifest).unwrap();
    value["manifest_version"] = serde_json::json!(PREVIOUS_GENERATION_MANIFEST_VERSION);
    for root in value["provider_roots"].as_array_mut().unwrap() {
        root.as_object_mut().unwrap().remove("connector_binding");
        root.as_object_mut()
            .unwrap()
            .remove("exact_source_memberships");
    }
    canonical_v9_bytes(value)
}

#[test]
fn literal_v8_and_v9_fixtures_migrate_directly_to_final_v10() {
    let v8 = migrate_previous_manifest_v8(V8_EMPTY_FIXTURE).unwrap();
    assert_eq!(v8.manifest_version, GENERATION_MANIFEST_VERSION);
    assert!(v8.automatic_provider_discovery());
    assert!(v8.provider_roots().is_empty());

    let definition = ProviderRootDefinition {
        id: "codex".to_owned(),
        provider: CaptureProvider::Codex,
        path: "/fixtures/codex".into(),
        group: None,
        kind: None,
    };
    assert_eq!(
        provider_source_config_digest(true, std::slice::from_ref(&definition)),
        "655246d699705d7c3bee11f277332db40cd54fda6a8e75a0ea10eec60306d3c2"
    );
    let v9 = migrate_previous_manifest_v9(V9_CODEX_FIXTURE).unwrap();
    let root = &v9.provider_roots()[0];
    assert_eq!(root.definition(), &definition);
    assert!(root.connector_binding().unwrap().identity_root().is_none());
    assert!(root.exact_source_memberships().is_empty());
}

#[test]
fn v9_root_dto_rejects_kind_and_v10_only_fields() {
    let mut kind = v9_value();
    kind["provider_roots"][0]["definition"]["kind"] = serde_json::json!("legacy-persistence");
    assert!(migrate_previous_manifest_v9(&serde_json::to_vec(&kind).unwrap()).is_err());

    for field in ["connector_binding", "exact_source_memberships"] {
        let mut value = v9_value();
        value["provider_roots"][0][field] = serde_json::json!([]);
        assert!(migrate_previous_manifest_v9(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

#[test]
fn v9_rejects_every_non_public_provider_for_both_source_identities() {
    for source_identity in ["named_v1", "released"] {
        let mut value = v9_value();
        value["provider_roots"][0]["definition"]["provider"] = serde_json::json!("crush");
        value["provider_roots"][0]["source_identity"] = serde_json::json!(source_identity);
        assert!(matches!(
            migrate_previous_manifest_v9(&canonical_v9_bytes(value)),
            Err(IndexError::InvalidProviderRoots(detail))
                if detail.contains("outside the public v9 contract")
        ));
    }
}

#[test]
fn v9_validates_source_route_root_and_ownership_canonicality_before_conversion() {
    let first = route("1");
    let second = route("2");
    let snapshot = |route| SourceRouteSnapshot::present(route, Vec::new()).unwrap();

    let mut source_route_order = v9_value();
    source_route_order["source_routes"] =
        serde_json::to_value([snapshot(second.clone()), snapshot(first.clone())]).unwrap();
    assert!(matches!(
        migrate_previous_manifest_v9(&canonical_v9_bytes(source_route_order)),
        Err(IndexError::NonCanonicalSourceRoutes)
    ));

    let mut root_order = v9_value();
    root_order["provider_roots"] = serde_json::json!([
        previous_root("codex", "codex", "released", Vec::new()),
        previous_root("alpha", "claude", "named_v1", Vec::new()),
    ]);
    assert!(matches!(
        migrate_previous_manifest_v9(&canonical_v9_bytes(root_order)),
        Err(IndexError::InvalidProviderRoots(detail)) if detail.contains("root definitions")
    ));

    let mut route_order = v9_value();
    route_order["source_routes"] =
        serde_json::to_value([snapshot(first.clone()), snapshot(second.clone())]).unwrap();
    route_order["provider_roots"][0]["routes"] =
        serde_json::to_value([second.clone(), first.clone()]).unwrap();
    assert!(matches!(
        migrate_previous_manifest_v9(&canonical_v9_bytes(route_order)),
        Err(IndexError::InvalidProviderRoots(detail)) if detail.contains("routes are not")
    ));

    let mut dangling = v9_value();
    dangling["provider_roots"][0]["routes"] = serde_json::to_value([first.clone()]).unwrap();
    assert!(matches!(
        migrate_previous_manifest_v9(&canonical_v9_bytes(dangling)),
        Err(IndexError::ProviderRootRouteNotRetained { .. })
    ));

    let mut shared = v9_value();
    shared["source_routes"] = serde_json::to_value([snapshot(first.clone())]).unwrap();
    shared["provider_roots"] = serde_json::json!([
        previous_root("alpha", "claude", "named_v1", vec![first.clone()]),
        previous_root("codex", "codex", "released", vec![first]),
    ]);
    assert!(matches!(
        migrate_previous_manifest_v9(&canonical_v9_bytes(shared)),
        Err(IndexError::SourceRouteOwnedByMultipleProviderRoots { .. })
    ));
}

#[test]
fn v9_root_filter_migration_preserves_whole_route_semantics() {
    let source = fixture_source("v9-filter");
    let token = source_token(&source);
    let certified = certified(source.clone());
    let route = route("3");
    let definition = ProviderRootDefinition {
        id: "claude".to_owned(),
        provider: CaptureProvider::Claude,
        path: "/fixtures/claude".into(),
        group: Some("work".to_owned()),
        kind: None,
    };
    let manifest = GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
        vec![certified],
        vec![SourceCoreRecordAggregate::new(token.clone(), 0, "00".repeat(32)).unwrap()],
        vec![SourceRouteSnapshot::present(route.clone(), vec![source]).unwrap()],
        true,
        provider_source_config_digest(true, std::slice::from_ref(&definition)),
        vec![AppliedProviderRoot::new(definition, vec![route]).unwrap()],
    )
    .unwrap();
    let migrated = migrate_previous_manifest_v9(&current_as_v9(manifest)).unwrap();

    assert_eq!(
        migrated
            .provider_root_source_tokens(&["claude".to_owned()], &[])
            .unwrap(),
        vec![token.clone()]
    );
    assert_eq!(
        migrated
            .provider_root_source_tokens(&[], &["work".to_owned()])
            .unwrap(),
        vec![token]
    );
}

#[test]
fn membership_only_successor_uses_a_full_v10_manifest() {
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
        reopened.manifest.detached_released_provider_roots(),
        successor.detached_released_provider_roots()
    );
}

#[test]
fn migrated_v8_and_v9_anchors_are_rewritten_once_before_reuse() {
    let temp = tempfile::tempdir().unwrap();
    for bytes in [V8_EMPTY_FIXTURE, V9_CODEX_FIXTURE] {
        let generation_id = write_literal_manifest(temp.path(), bytes);
        let loaded = load_materialized_manifest(temp.path(), &generation_id, 0).unwrap();
        assert!(loaded.requires_current_anchor);

        let prepared = prepare_successor_manifest(
            temp.path(),
            Arc::clone(&loaded.manifest),
            Some((&generation_id, loaded.manifest.as_ref())),
        )
        .unwrap();
        assert_ne!(prepared.generation_id(), generation_id);
        assert!(!prepared.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&prepared.bytes).unwrap()
                ["manifest_version"],
            GENERATION_MANIFEST_VERSION
        );

        write_prepared_manifest(temp.path(), &prepared).unwrap();
        let anchored_id = prepared.generation_id().to_owned();
        let reused = prepare_successor_manifest(
            temp.path(),
            Arc::clone(&loaded.manifest),
            Some((&anchored_id, loaded.manifest.as_ref())),
        )
        .unwrap();
        assert_eq!(reused.generation_id(), anchored_id);
    }
}

#[test]
fn flat_delta_inherits_the_v9_anchors_rewrite_requirement() {
    let temp = tempfile::tempdir().unwrap();
    let source = fixture_source("v9-delta-anchor");
    let base = GenerationManifest::from_sources(vec![certified(source.clone())]).unwrap();
    let base_generation_id = write_literal_manifest(temp.path(), &current_as_v9(base));
    let observation = SourceObservation::new(source.clone(), "fixture-revision", vec![2]).unwrap();
    let successor = CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser",
        [1; 32],
        ScannedSourceCounts::default(),
    )
    .unwrap();
    let delta = StoredManifestFlatDeltaV1 {
        storage_format: MANIFEST_FLAT_DELTA_STORAGE.to_owned(),
        base_generation_id,
        indexed_documents: 0,
        certified_source_bytes: 0,
        source_count: 1,
        changes: vec![StoredManifestSourceChangeV1 {
            source_identity: source.identity().digest(),
            source: successor,
            aggregate: SourceCoreRecordAggregate::new(source_token(&source), 0, "00".repeat(32))
                .unwrap(),
        }],
    };
    let delta_generation_id =
        write_literal_manifest(temp.path(), &serde_json::to_vec(&delta).unwrap());
    let loaded = load_materialized_manifest(temp.path(), &delta_generation_id, 0).unwrap();
    assert!(loaded.requires_current_anchor);

    let prepared = prepare_successor_manifest(
        temp.path(),
        Arc::clone(&loaded.manifest),
        Some((&delta_generation_id, loaded.manifest.as_ref())),
    )
    .unwrap();
    assert!(!prepared.bytes.starts_with(MANIFEST_FLAT_DELTA_PREFIX));
    assert_ne!(prepared.generation_id(), delta_generation_id);
}
