use super::*;
use ctx_history_capture_model::{
    ProviderRootDefinition, ProviderRootKind, ProviderRootSourceIdentity, ProviderRouteRole,
    ReleasedProviderRootAutomaticRole,
};
use ctx_history_core::CaptureProvider;
use ctx_history_core::{
    CertifiedSource, ScannedSourceCounts, SourceAnchor, SourceKey, SourceObservation, TypedKey,
};

fn source(name: &str, format: &str) -> SourceKey {
    SourceKey::derive(
        "fixture",
        format,
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

fn root(temp: &std::path::Path, id: &str) -> ProviderRootDefinition {
    ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::Codex,
        path: temp.join(id),
        group: Some(format!("{id}-group")),
        kind: None,
    }
}

#[test]
fn source_route_snapshot_and_generation_wire_contract_remain_stable() {
    let route_identity = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let snapshot = SourceRouteSnapshot::present(route_identity, Vec::new()).unwrap();

    assert_eq!(
        serde_json::to_string(&snapshot).unwrap(),
        format!(
            "{{\"route_identity\":\"{}\",\"sources\":[],\"missing\":null}}",
            "ab".repeat(32)
        )
    );

    let manifest = GenerationManifest::from_parts(Vec::new(), vec![snapshot]).unwrap();
    assert_eq!(
        serde_json::to_string(&manifest).unwrap(),
        "{\"manifest_version\":10,\"identity_version\":1,\"core_record_version\":3,\"core_record_contract_fingerprint\":\"ebb5c9b638de184824a6ce141ebf9b70941fb293fc113d29e2851565bad4371e\",\"lexical_schema_version\":22,\"lexical_analyzer_version\":2,\"policy_schema_hash\":\"84d58ff1dbcfbf524845eea78162e013e76cc000b275393711b6617764da3ae9\",\"indexed_documents\":0,\"certified_source_bytes\":0,\"sources\":[],\"core_record_aggregates\":[],\"source_routes\":[{\"route_identity\":\"abababababababababababababababababababababababababababababababab\",\"sources\":[],\"missing\":null}],\"automatic_provider_discovery\":true,\"provider_root_config_digest\":\"4bfe780cf41a834d4bd7c58d54498cc96b6a5a1d6b20c37f212af31aaa674064\",\"provider_roots\":[]}",
    );
    assert_eq!(
        manifest.generation_id().unwrap(),
        "6bc6b9995692fe7302983e5fdf387310344a7f61ccfcc8d6755acfd0d3c4bc35"
    );
}

#[test]
fn released_provider_root_retains_immutable_connector_authority_across_moves() {
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("original-hermes-home");
    let moved = ProviderRootDefinition {
        id: "hermes".to_owned(),
        provider: CaptureProvider::Hermes,
        path: temp.path().join("moved-hermes-home"),
        group: None,
        kind: None,
    };
    let applied = AppliedProviderRoot::with_source_identity_and_connector_binding(
        moved.clone(),
        ProviderRootSourceIdentity::Released,
        Some(ProviderRootConnectorBinding::released_rooted_v1(
            original.clone(),
        )),
        Vec::new(),
    )
    .unwrap();
    let manifest = GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        true,
        provider_source_config_digest(true, std::slice::from_ref(&moved)),
        vec![applied],
    )
    .unwrap();

    let retained = &manifest.provider_roots()[0];
    assert_eq!(retained.definition().path, moved.path);
    assert_eq!(
        retained
            .connector_binding()
            .unwrap()
            .identity_root()
            .unwrap(),
        original
    );

    let mut moved_again = moved;
    moved_again.path = temp.path().join("moved-again-hermes-home");
    let reconstructed = AppliedProviderRoot::with_retained_authority(
        moved_again.clone(),
        retained.retained_authority().unwrap(),
        Vec::new(),
    )
    .unwrap();
    assert_eq!(reconstructed.definition(), &moved_again);
    assert_eq!(
        reconstructed
            .connector_binding()
            .unwrap()
            .identity_root()
            .unwrap(),
        original
    );
}

#[test]
fn detached_released_authority_is_bounded_and_platform_independent() {
    let temp = tempfile::tempdir().unwrap();
    let authorities = (0..MAX_DETACHED_RELEASED_PROVIDER_ROOTS)
        .map(|index| {
            let root = AppliedProviderRoot::with_source_identity(
                ProviderRootDefinition {
                    id: format!("released-{index}"),
                    provider: CaptureProvider::Codex,
                    path: temp.path().join(format!("root-{index}")),
                    group: None,
                    kind: None,
                },
                ProviderRootSourceIdentity::Released,
                Vec::new(),
            )
            .unwrap();
            DetachedReleasedProviderRootAuthority::from_applied(&root)
                .unwrap()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let exact_bound = GenerationManifest::from_parts_with_record_aggregates_and_provider_roots_and_detached_authorities(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        true,
        provider_source_config_digest(true, &[]),
        Vec::new(),
        authorities.clone(),
    )
    .unwrap();
    assert_eq!(
        exact_bound.detached_released_provider_roots().len(),
        MAX_DETACHED_RELEASED_PROVIDER_ROOTS
    );

    let overflow_root = AppliedProviderRoot::with_source_identity(
        ProviderRootDefinition {
            id: "released-overflow".to_owned(),
            provider: CaptureProvider::Codex,
            path: temp.path().join("root-overflow"),
            group: None,
            kind: None,
        },
        ProviderRootSourceIdentity::Released,
        Vec::new(),
    )
    .unwrap();
    let mut overflow = authorities;
    overflow.push(
        DetachedReleasedProviderRootAuthority::from_applied(&overflow_root)
            .unwrap()
            .unwrap(),
    );
    assert!(matches!(
        GenerationManifest::from_parts_with_record_aggregates_and_provider_roots_and_detached_authorities(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            true,
            provider_source_config_digest(true, &[]),
            Vec::new(),
            overflow,
        ),
        Err(IndexError::InvalidProviderRoots(detail))
            if detail.contains("detached released root authorities")
    ));
}

#[test]
fn released_connector_automatic_roles_are_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let roles = (0..=256_u64)
        .map(|index| {
            let component = index.to_be_bytes();
            let configured_role =
                ProviderRouteRole::from_dynamic([b"configured".as_slice(), component.as_slice()])
                    .unwrap();
            let automatic_role =
                ProviderRouteRole::from_dynamic([b"automatic".as_slice(), component.as_slice()])
                    .unwrap();
            ReleasedProviderRootAutomaticRole::new(
                format!("fixture_{index}"),
                configured_role.as_bytes().to_vec(),
                automatic_role.as_bytes().to_vec(),
            )
        })
        .collect();
    let binding = ProviderRootConnectorBinding::released_rooted_v1(temp.path().join("identity"))
        .with_automatic_route_roles(roles);
    assert!(matches!(
        AppliedProviderRoot::with_source_identity_and_connector_binding(
            ProviderRootDefinition {
                id: "bounded".to_owned(),
                provider: CaptureProvider::Hermes,
                path: temp.path().join("current"),
                group: None,
                kind: None,
            },
            ProviderRootSourceIdentity::Released,
            Some(binding),
            Vec::new(),
        ),
        Err(IndexError::InvalidProviderRoots(detail))
            if detail.contains("automatic route roles are not bounded")
    ));
}

#[test]
fn provider_root_connector_binding_matches_released_identity_contract() {
    let temp = tempfile::tempdir().unwrap();
    let definition = ProviderRootDefinition {
        id: "codex".to_owned(),
        provider: CaptureProvider::Codex,
        path: temp.path().join("codex-home"),
        group: None,
        kind: None,
    };
    let binding = ProviderRootConnectorBinding::released_path_independent_v1();
    assert_eq!(
        serde_json::to_string(&binding).unwrap(),
        "{\"kind\":\"released_path_independent_v1\"}"
    );
    assert_eq!(
        serde_json::to_string(&ProviderRootConnectorBinding::released_rooted_v1(
            definition.path.clone()
        ))
        .unwrap(),
        format!(
            "{{\"kind\":\"released_rooted_v1\",\"identity_root\":{}}}",
            serde_json::to_string(&definition.path).unwrap()
        )
    );

    assert!(matches!(
        AppliedProviderRoot::with_source_identity_and_connector_binding(
            definition.clone(),
            ProviderRootSourceIdentity::NamedV1,
            Some(binding),
            Vec::new(),
        ),
        Err(IndexError::InvalidProviderRoots(_))
    ));
    assert!(matches!(
        AppliedProviderRoot::with_source_identity_and_connector_binding(
            definition.clone(),
            ProviderRootSourceIdentity::Released,
            None,
            Vec::new(),
        ),
        Err(IndexError::InvalidProviderRoots(_))
    ));
    assert!(matches!(
        AppliedProviderRoot::with_source_identity_and_connector_binding(
            definition.clone(),
            ProviderRootSourceIdentity::Released,
            Some(ProviderRootConnectorBinding::released_rooted_v1(
                temp.path().join("wrong-rooted-codex-home"),
            )),
            Vec::new(),
        ),
        Err(IndexError::InvalidProviderRoots(_))
    ));

    let hermes = ProviderRootDefinition {
        id: "hermes".to_owned(),
        provider: CaptureProvider::Hermes,
        path: temp.path().join("hermes-home"),
        group: None,
        kind: None,
    };
    assert!(matches!(
        AppliedProviderRoot::with_source_identity_and_connector_binding(
            hermes,
            ProviderRootSourceIdentity::Released,
            Some(ProviderRootConnectorBinding::released_rooted_v1(
                "relative-home",
            )),
            Vec::new(),
        ),
        Err(IndexError::InvalidProviderRoots(_))
    ));

    let released = AppliedProviderRoot::with_source_identity(
        definition,
        ProviderRootSourceIdentity::Released,
        Vec::new(),
    )
    .unwrap();
    assert!(released
        .connector_binding()
        .unwrap()
        .identity_root()
        .is_none());
}

#[test]
fn provider_root_aliases_are_bounded_and_generation_local() {
    let temp = tempfile::tempdir().unwrap();
    let route_identity = SourceRouteIdentity::from_sha256("cd".repeat(32)).unwrap();
    let definition = ProviderRootDefinition {
        id: "personal".to_owned(),
        provider: CaptureProvider::Claude,
        path: temp.path().join("claude-personal"),
        group: Some("personal".to_owned()),
        kind: None,
    };
    let manifest = GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
        Vec::new(),
        Vec::new(),
        vec![SourceRouteSnapshot::present(route_identity.clone(), Vec::new()).unwrap()],
        true,
        provider_source_config_digest(true, std::slice::from_ref(&definition)),
        vec![AppliedProviderRoot::new(definition, vec![route_identity]).unwrap()],
    )
    .unwrap();

    assert_eq!(manifest.provider_roots().len(), 1);
    assert_eq!(
        manifest
            .provider_root_source_tokens(&["personal".to_owned()], &[])
            .unwrap(),
        Vec::<String>::new()
    );
    assert!(matches!(
        manifest.provider_root_source_tokens(&["work".to_owned()], &[]),
        Err(IndexError::UnknownProviderRootSelector(selector)) if selector == "work"
    ));
    assert!(matches!(
        manifest.provider_root_source_tokens(&[], &["work".to_owned()]),
        Err(IndexError::UnknownProviderRootGroup(group)) if group == "work"
    ));
}

#[test]
fn provider_root_manifest_validation_is_provider_generic() {
    let temp = tempfile::tempdir().unwrap();
    let definition = ProviderRootDefinition {
        id: "future-provider".to_owned(),
        provider: CaptureProvider::Cursor,
        path: temp.path().join("cursor-root"),
        group: None,
        kind: None,
    };

    let applied = AppliedProviderRoot::new(definition.clone(), Vec::new()).unwrap();
    assert_eq!(applied.definition(), &definition);
    assert_eq!(
        applied.source_identity(),
        ProviderRootSourceIdentity::NamedV1
    );
}

#[test]
fn provider_root_manifest_rejects_paths_over_the_shared_encoded_bound() {
    let temp = tempfile::tempdir().unwrap();
    let invalid = ProviderRootDefinition {
        id: "oversized".to_owned(),
        provider: CaptureProvider::Claude,
        path: temp
            .path()
            .join("x".repeat(ctx_history_capture_model::MAX_PROVIDER_ROOT_ENCODED_PATH_BYTES + 1)),
        group: None,
        kind: None,
    };

    assert!(matches!(
        AppliedProviderRoot::new(invalid, Vec::new()),
        Err(IndexError::InvalidProviderRoots(detail))
            if detail.contains("bounded normalized absolute")
    ));
}

#[test]
fn provider_root_manifest_validates_openhands_kind_at_the_persisted_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let invalid = ProviderRootDefinition {
        id: "openhands".to_owned(),
        provider: CaptureProvider::OpenHands,
        path: temp.path().join("openhands"),
        group: None,
        kind: None,
    };
    assert!(matches!(
        AppliedProviderRoot::new(invalid, Vec::new()),
        Err(IndexError::InvalidProviderRoots(_))
    ));

    let invalid_old_provider = ProviderRootDefinition {
        id: "claude".to_owned(),
        provider: CaptureProvider::Claude,
        path: temp.path().join("claude"),
        group: None,
        kind: Some(ProviderRootKind::OpenHandsLegacyPersistence),
    };
    assert!(matches!(
        AppliedProviderRoot::new(invalid_old_provider, Vec::new()),
        Err(IndexError::InvalidProviderRoots(_))
    ));
}

#[test]
fn provider_root_manifest_rejects_openhands_ancestor_overlap_at_the_index_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let current = ProviderRootDefinition {
        id: "current".to_owned(),
        provider: CaptureProvider::OpenHands,
        path: temp.path().join("openhands"),
        group: None,
        kind: Some(ProviderRootKind::OpenHandsCurrentConversations),
    };
    let legacy = ProviderRootDefinition {
        id: "legacy".to_owned(),
        provider: CaptureProvider::OpenHands,
        path: current.path.join("legacy-persistence"),
        group: None,
        kind: Some(ProviderRootKind::OpenHandsLegacyPersistence),
    };
    let definitions = vec![current.clone(), legacy.clone()];

    assert!(matches!(
        GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            true,
            provider_source_config_digest(true, &definitions),
            vec![
                AppliedProviderRoot::new(current, Vec::new()).unwrap(),
                AppliedProviderRoot::new(legacy, Vec::new()).unwrap(),
            ],
        ),
        Err(IndexError::InvalidProviderRoots(detail)) if detail.contains("overlapping legacy/current")
    ));
}

#[test]
fn provider_root_manifest_prunes_unretained_routes_and_rejects_shared_routes() {
    let temp = tempfile::tempdir().unwrap();
    let route_identity = SourceRouteIdentity::from_sha256("ef".repeat(32)).unwrap();
    let definition = |id: &str| ProviderRootDefinition {
        id: id.to_owned(),
        provider: CaptureProvider::Codex,
        path: temp.path().join(format!("codex-{id}")),
        group: None,
        kind: None,
    };
    let first = definition("first");
    let pruned = GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        true,
        provider_source_config_digest(true, std::slice::from_ref(&first)),
        vec![AppliedProviderRoot::new(first, vec![route_identity.clone()]).unwrap()],
    )
    .unwrap();
    assert!(pruned.provider_roots()[0].routes().is_empty());

    let mut persisted = serde_json::to_value(&pruned).unwrap();
    persisted["provider_roots"][0]["routes"] = serde_json::json!([route_identity.as_str()]);
    let dangling: GenerationManifest = serde_json::from_value(persisted).unwrap();
    assert!(matches!(
        dangling.validate_contract(),
        Err(IndexError::ProviderRootRouteNotRetained { .. })
    ));

    let definitions = vec![definition("first"), definition("second")];
    assert!(matches!(
        GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
            Vec::new(),
            Vec::new(),
            vec![SourceRouteSnapshot::present(route_identity.clone(), Vec::new()).unwrap()],
            true,
            provider_source_config_digest(true, &definitions),
            definitions
                .into_iter()
                .map(|root| AppliedProviderRoot::new(root, vec![route_identity.clone()]).unwrap())
                .collect(),
        ),
        Err(IndexError::SourceRouteOwnedByMultipleProviderRoots { .. })
    ));
}

#[test]
fn malformed_deserialized_route_identity_reaches_complete_manifest_validation() {
    let route_identity = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let manifest = GenerationManifest::from_parts(
        Vec::new(),
        vec![SourceRouteSnapshot::present(route_identity, Vec::new()).unwrap()],
    )
    .unwrap();
    let mut persisted = serde_json::to_value(manifest).unwrap();
    persisted["source_routes"][0]["route_identity"] = serde_json::json!("AB".repeat(32));
    let loaded: GenerationManifest = serde_json::from_value(persisted).unwrap();

    assert!(matches!(
        loaded.validate_contract(),
        Err(IndexError::InvalidSourceRouteIdentity)
    ));
}

#[test]
fn disjoint_exact_roots_share_one_route_without_claiming_automatic_peers() {
    let temp = tempfile::tempdir().unwrap();
    let route = SourceRouteIdentity::from_sha256("61".repeat(32)).unwrap();
    let alpha = source("alpha", "old-format");
    let beta = source("beta", "old-format");
    let automatic = source("automatic", "old-format");
    let definitions = vec![root(temp.path(), "alpha"), root(temp.path(), "beta")];
    let applied = definitions
        .iter()
        .zip([&alpha, &beta])
        .map(|(definition, source)| {
            AppliedProviderRoot::new(definition.clone(), vec![route.clone()])
                .unwrap()
                .with_exact_source_memberships(vec![AppliedProviderRootSourceMembership::exact(
                    route.clone(),
                    vec![source_token(source)],
                )
                .unwrap()])
                .unwrap()
        })
        .collect();
    let sources = vec![
        certified(alpha.clone()),
        certified(beta.clone()),
        certified(automatic.clone()),
    ];
    let aggregates = test_aggregates(&sources).unwrap();
    let manifest = GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
        sources,
        aggregates,
        vec![SourceRouteSnapshot::present(
            route,
            vec![alpha.clone(), beta.clone(), automatic.clone()],
        )
        .unwrap()],
        true,
        provider_source_config_digest(true, &definitions),
        applied,
    )
    .unwrap();

    assert_eq!(
        manifest
            .provider_root_source_tokens(&["alpha".to_owned()], &[])
            .unwrap(),
        vec![source_token(&alpha)]
    );
    assert_eq!(
        manifest
            .provider_root_source_tokens(&[], &["beta-group".to_owned()])
            .unwrap(),
        vec![source_token(&beta)]
    );
    assert_eq!(manifest.source_routes()[0].sources().len(), 3);
    assert!(!manifest
        .provider_root_source_tokens(&["alpha".to_owned(), "beta".to_owned()], &[])
        .unwrap()
        .contains(&source_token(&automatic)));
}

#[test]
fn exact_membership_intersects_by_identity_across_descriptor_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let route = SourceRouteIdentity::from_sha256("62".repeat(32)).unwrap();
    let old = source("stable-lineage", "old-format");
    let replacement = source("stable-lineage", "new-format");
    let absent = source("absent", "old-format");
    assert!(old.is_same_lineage_descriptor_replacement(&replacement));
    let definition = root(temp.path(), "stable");
    let applied = AppliedProviderRoot::new(definition.clone(), vec![route.clone()])
        .unwrap()
        .with_exact_source_memberships(vec![AppliedProviderRootSourceMembership::exact(
            route.clone(),
            vec![source_token(&old), source_token(&absent)],
        )
        .unwrap()])
        .unwrap();
    let sources = vec![certified(replacement.clone())];
    let aggregates = test_aggregates(&sources).unwrap();
    let manifest = GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
        sources,
        aggregates,
        vec![SourceRouteSnapshot::present(route.clone(), vec![replacement.clone()]).unwrap()],
        true,
        provider_source_config_digest(true, std::slice::from_ref(&definition)),
        vec![applied],
    )
    .unwrap();

    assert_eq!(
        manifest.provider_roots()[0].exact_source_memberships()[0].source_tokens(),
        &[source_token(&replacement)]
    );
    assert_eq!(
        manifest
            .provider_root_source_tokens(&["stable".to_owned()], &[])
            .unwrap(),
        vec![source_token(&replacement)]
    );
    manifest.validate_contract().unwrap();
}

#[test]
fn persisted_exact_membership_is_strict_and_shared_route_sets_must_be_disjoint() {
    let temp = tempfile::tempdir().unwrap();
    let route = SourceRouteIdentity::from_sha256("63".repeat(32)).unwrap();
    let alpha = source("strict-alpha", "fixture");
    let beta = source("strict-beta", "fixture");
    let definitions = vec![root(temp.path(), "first"), root(temp.path(), "second")];
    let exact_root = |definition: &ProviderRootDefinition, tokens: Vec<String>| {
        AppliedProviderRoot::new(definition.clone(), vec![route.clone()])
            .unwrap()
            .with_exact_source_memberships(vec![AppliedProviderRootSourceMembership::exact(
                route.clone(),
                tokens,
            )
            .unwrap()])
            .unwrap()
    };
    let build = |roots| {
        let sources = vec![certified(alpha.clone()), certified(beta.clone())];
        let aggregates = test_aggregates(&sources).unwrap();
        GenerationManifest::from_parts_with_record_aggregates_and_provider_roots(
            sources,
            aggregates,
            vec![
                SourceRouteSnapshot::present(route.clone(), vec![alpha.clone(), beta.clone()])
                    .unwrap(),
            ],
            true,
            provider_source_config_digest(true, &definitions),
            roots,
        )
    };

    assert!(build(vec![
        exact_root(&definitions[0], vec![source_token(&alpha)]),
        exact_root(&definitions[1], vec![source_token(&alpha)]),
    ])
    .is_err());
    assert!(build(vec![
        AppliedProviderRoot::new(definitions[0].clone(), vec![route.clone()]).unwrap(),
        exact_root(&definitions[1], vec![source_token(&beta)]),
    ])
    .is_err());
    build(vec![
        exact_root(&definitions[0], Vec::new()),
        exact_root(&definitions[1], vec![source_token(&beta)]),
    ])
    .unwrap();

    let valid = build(vec![
        exact_root(&definitions[0], vec![source_token(&alpha)]),
        exact_root(&definitions[1], vec![source_token(&beta)]),
    ])
    .unwrap();
    let valid_value = serde_json::to_value(&valid).unwrap();
    let mut cross_route = valid_value.clone();
    cross_route["provider_roots"][0]["exact_source_memberships"][0]["source_tokens"] =
        serde_json::json!(["ff".repeat(32)]);
    let cross_route: GenerationManifest = serde_json::from_value(cross_route).unwrap();
    assert!(matches!(
        cross_route.validate_contract(),
        Err(IndexError::InvalidProviderRoots(_))
    ));

    let mut duplicate = valid_value.clone();
    let token = source_token(&alpha);
    duplicate["provider_roots"][0]["exact_source_memberships"][0]["source_tokens"] =
        serde_json::json!([token, source_token(&alpha)]);
    let duplicate: GenerationManifest = serde_json::from_value(duplicate).unwrap();
    assert!(matches!(
        duplicate.validate_contract(),
        Err(IndexError::InvalidProviderRoots(_))
    ));

    let mut unsorted = valid_value.clone();
    unsorted["provider_roots"][0]["exact_source_memberships"][0]["source_tokens"] =
        serde_json::json!(["ff".repeat(32), "00".repeat(32)]);
    let unsorted: GenerationManifest = serde_json::from_value(unsorted).unwrap();
    assert!(matches!(
        unsorted.validate_contract(),
        Err(IndexError::InvalidProviderRoots(_))
    ));

    let mut dangling = valid_value.clone();
    dangling["provider_roots"][0]["exact_source_memberships"][0]["route_identity"] =
        serde_json::json!("64".repeat(32));
    let dangling: GenerationManifest = serde_json::from_value(dangling).unwrap();
    assert!(matches!(
        dangling.validate_contract(),
        Err(IndexError::InvalidProviderRoots(_))
    ));

    let mut transient_v10 = valid_value;
    transient_v10["provider_roots"][0]
        .as_object_mut()
        .unwrap()
        .remove("exact_source_memberships");
    assert!(serde_json::from_value::<GenerationManifest>(transient_v10).is_err());
}
