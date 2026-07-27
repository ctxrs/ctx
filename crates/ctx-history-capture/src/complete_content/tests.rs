use super::*;
use ctx_history_core::{CaptureProvider, ContentRef};
use uuid::Uuid;

fn request() -> CompleteMessageRequest {
    let event_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000010").unwrap();
    CompleteMessageRequest {
        event_id,
        provider: CaptureProvider::Custom,
        source_format: "fixture".to_owned(),
        source_access: BrokeredSourceAccess::fixture(Uuid::new_v4()),
        source_family: Some(CompleteContentSourceFamily::Fixture),
        content_profile: "fixture.message-body.v1".to_owned(),
        source_locator: None,
        provider_session_id: Some("session-1".to_owned()),
        source_record_ordinal: 0,
        source_record_subrecord_index: 0,
        expected_provider_event_hash: "event-1".to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some("event-1".to_owned()),
        expected_record_digest: None,
        expected_content_ref: None,
        indexed_text: "1234".to_owned(),
        indexed_limit_chars: 4,
    }
}

#[test]
fn verified_message_requires_exact_character_prefix() {
    let request = request();
    let message = CompleteMessage::verified(
        &request,
        "1234tail".to_owned(),
        SourceVerification::VERIFIED,
    )
    .unwrap();
    assert_eq!(message.text, "1234tail");

    let error = CompleteMessage::verified(
        &request,
        "123Xtail".to_owned(),
        SourceVerification::VERIFIED,
    )
    .unwrap_err();
    assert_eq!(
        error.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

#[test]
fn errors_are_typed_and_path_free() {
    let error =
        CompleteContentError::new(CompleteContentErrorKind::SourceMissing, request().event_id);
    let rendered = error.to_string();
    assert!(rendered.contains("source_missing"));
    assert!(rendered.contains("ctx locate event"));
    assert!(!rendered.contains("fixture.jsonl"));
}

#[test]
fn persisted_locator_is_versioned_bounded_and_round_trips() {
    let record = CompleteContentBodyDigest::from_text("record");
    let content_ref = ContentRef::from_bytes(b"body").unwrap();
    let mut address = [0_u8; 16];
    address[8..].copy_from_slice(&1_u64.to_be_bytes());
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        "codex.message-body.v1",
        content_ref.clone(),
        CompleteContentSourceFamily::Jsonl,
        "jsonl-range-v1",
        &address,
        "native-event-1",
        record.clone(),
    )
    .unwrap();
    let collection = VerifiedContentLocatorsV1::singleton(locator).unwrap();
    let value = collection.to_metadata_value();
    let wire = &value["locators"][0];
    assert!(wire.get("content_role").is_some());
    assert!(wire.get("source_family").is_some());
    assert!(wire.get("address_kind").is_some());
    assert!(wire.get("address_value").is_some());
    for legacy in ["role", "family", "kind", "value_hex"] {
        assert!(wire.get(legacy).is_none());
    }
    assert!(serde_json::to_vec(&value).unwrap().len() <= VERIFIED_CONTENT_LOCATORS_MAX_BYTES);
    let decoded = VerifiedContentLocatorsV1::from_metadata_value(&value).unwrap();
    let decoded = decoded.locator(VerifiedContentRole::MessageBody).unwrap();
    assert_eq!(decoded.family(), CompleteContentSourceFamily::Jsonl);
    assert_eq!(decoded.content_profile(), "codex.message-body.v1");
    assert_eq!(decoded.content_ref(), &content_ref);
    assert_eq!(decoded.native_record_id(), "native-event-1");
    assert_eq!(decoded.record_sha256(), &record);
    assert_eq!(decoded.source_locator().unwrap().value(), &address);
}

#[test]
fn persisted_locator_rejects_unknown_fields_duplicates_and_invalid_profiles() {
    let mut valid_address = [0_u8; 16];
    valid_address[8..].copy_from_slice(&1_u64.to_be_bytes());
    assert!(VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        "unknown.message-body.v1",
        ContentRef::from_bytes(b"body").unwrap(),
        CompleteContentSourceFamily::Jsonl,
        "jsonl-range-v1",
        &valid_address,
        "native-event-1",
        CompleteContentBodyDigest::from_text("record"),
    )
    .is_none());
    assert!(VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        "codex.message-body.v1",
        ContentRef::new("a".repeat(64), COMPLETE_CONTENT_MAX_BODY_BYTES as u64 + 1,).unwrap(),
        CompleteContentSourceFamily::Jsonl,
        "jsonl-range-v1",
        &valid_address,
        "native-event-1",
        CompleteContentBodyDigest::from_text("record"),
    )
    .is_none());
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        "codex.message-body.v1",
        ContentRef::from_bytes(b"body").unwrap(),
        CompleteContentSourceFamily::Jsonl,
        "jsonl-range-v1",
        &valid_address,
        "native-event-1",
        CompleteContentBodyDigest::from_text("record"),
    )
    .unwrap();
    let mut value = VerifiedContentLocatorsV1::singleton(locator)
        .unwrap()
        .to_metadata_value();
    value["future"] = serde_json::json!(true);
    assert!(VerifiedContentLocatorsV1::from_metadata_value(&value).is_none());

    let oversized = serde_json::json!({
        "version": 1,
        "locators": [],
        "oversized": "x".repeat(VERIFIED_CONTENT_LOCATORS_MAX_BYTES + 1)
    });
    assert!(VerifiedContentLocatorsV1::from_metadata_value(&oversized).is_none());

    let malformed = serde_json::json!({
        "version": 1,
        "locators": [{
            "content_role": "message_body",
            "content_profile": "Future.Profile",
            "content_ref": {"sha256": "a".repeat(64), "byte_len": 1},
            "source_family": "jsonl",
            "address_kind": "jsonl-range-v1",
            "address_value": "00",
            "native_record_id": "id",
            "record_sha256": "b".repeat(64)
        }]
    });
    assert!(VerifiedContentLocatorsV1::from_metadata_value(&malformed).is_none());

    for (field, value) in [
        ("content_role", serde_json::json!("future_body")),
        ("source_family", serde_json::json!("future_store")),
    ] {
        let mut unknown = serde_json::json!({
            "version": 1,
            "locators": [{
                "content_role": "message_body",
                "content_profile": "codex.message-body.v1",
                "content_ref": {"sha256": "a".repeat(64), "byte_len": 1},
                "source_family": "jsonl",
                "address_kind": "jsonl-range-v1",
                "address_value": "00000000000000000000000000000001",
                "native_record_id": "id",
                "record_sha256": "b".repeat(64)
            }]
        });
        unknown["locators"][0][field] = value;
        assert!(VerifiedContentLocatorsV1::from_metadata_value(&unknown).is_none());
    }

    for (field, value) in [
        (
            "content_profile",
            serde_json::json!("unknown.message-body.v1"),
        ),
        (
            "address_value",
            serde_json::json!("0000000000000000000000000000000A"),
        ),
        (
            "content_ref",
            serde_json::json!({
                "sha256": "a".repeat(64),
                "byte_len": COMPLETE_CONTENT_MAX_BODY_BYTES as u64 + 1
            }),
        ),
    ] {
        let mut invalid = serde_json::json!({
            "version": 1,
            "locators": [{
                "content_role": "message_body",
                "content_profile": "codex.message-body.v1",
                "content_ref": {"sha256": "a".repeat(64), "byte_len": 1},
                "source_family": "jsonl",
                "address_kind": "jsonl-range-v1",
                "address_value": "00000000000000000000000000000001",
                "native_record_id": "id",
                "record_sha256": "b".repeat(64)
            }]
        });
        invalid["locators"][0][field] = value;
        assert!(VerifiedContentLocatorsV1::from_metadata_value(&invalid).is_none());
    }

    let mut address = [0_u8; 16];
    address[8..].copy_from_slice(&1_u64.to_be_bytes());
    let duplicate = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        "codex.message-body.v1",
        ContentRef::from_bytes(b"body").unwrap(),
        CompleteContentSourceFamily::Jsonl,
        "jsonl-range-v1",
        &address,
        "id",
        CompleteContentBodyDigest::from_text("record"),
    )
    .unwrap();
    let mut metadata = serde_json::json!({});
    assert!(attach_verified_content_locator(&mut metadata, duplicate.clone()).is_some());
    assert!(attach_verified_content_locator(&mut metadata, duplicate).is_none());
}

#[test]
fn route_registry_exactly_covers_matrix_formats_roles_platforms_and_contracts() {
    use std::{collections::HashSet, str::FromStr};

    let matrix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../docs/provider-support-matrix.json"
    ))
    .unwrap();
    let mut expected = HashSet::new();
    for provider in matrix["providers"].as_array().unwrap() {
        let capture_provider =
            CaptureProvider::from_str(provider["capture_provider"].as_str().unwrap()).unwrap();
        for path in provider["implemented_paths"].as_array().unwrap() {
            let source_format = path["source_format"].as_str().unwrap().to_owned();
            for role in [
                VerifiedContentRole::MessageBody,
                VerifiedContentRole::ResultBody,
            ] {
                assert!(expected.insert((capture_provider, source_format.clone(), role)));
            }
        }
    }
    assert_eq!(expected.len(), 42 * 2);

    let actual = VERIFIED_CONTENT_ROUTES
        .iter()
        .map(|route| (route.provider, route.source_format.to_owned(), route.role))
        .collect::<HashSet<_>>();
    assert_eq!(actual.len(), VERIFIED_CONTENT_ROUTES.len());
    assert_eq!(actual, expected);
    assert!(actual.contains(&(
        CaptureProvider::Codex,
        "codex_history_jsonl".to_owned(),
        VerifiedContentRole::MessageBody
    )));
    assert_eq!(
        VERIFIED_CONTENT_ROUTES
            .iter()
            .filter(|route| {
                route.provider != CaptureProvider::Codex
                    && route.role == VerifiedContentRole::ResultBody
            })
            .count(),
        40
    );

    for route in VERIFIED_CONTENT_ROUTES {
        let platforms = route
            .platform_dispositions
            .iter()
            .map(|disposition| disposition.platform)
            .collect::<HashSet<_>>();
        assert_eq!(
            platforms,
            VERIFIED_CONTENT_RELEASE_PLATFORMS.into_iter().collect()
        );
        let status = route.platform_dispositions[0].status;
        assert!(route
            .platform_dispositions
            .iter()
            .all(|disposition| disposition.status == status));
        if status == VerifiedContentRouteStatus::Supported {
            assert!(!route.contracts.is_empty());
        } else {
            assert!(route.contracts.is_empty());
            assert!(route
                .platform_dispositions
                .iter()
                .all(|disposition| !disposition.reason.is_empty()));
        }
        assert!(route.contracts.iter().all(|contract| {
            !contract.content_profile.is_empty()
                && !contract.locator_kind.is_empty()
                && !contract.fixture_reference.is_empty()
        }));
    }

    let contract_routes = VERIFIED_CONTENT_ROUTES
        .iter()
        .flat_map(|route| {
            route.contracts.iter().map(move |contract| {
                (
                    route.provider,
                    route.source_format,
                    route.role,
                    contract.family,
                    contract.locator_kind,
                )
            })
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        contract_routes.len(),
        VERIFIED_CONTENT_ROUTES
            .iter()
            .map(|route| route.contracts.len())
            .sum::<usize>()
    );
    let content_profiles = VERIFIED_CONTENT_ROUTES
        .iter()
        .flat_map(|route| {
            route
                .contracts
                .iter()
                .map(|contract| contract.content_profile)
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        content_profiles.len(),
        VERIFIED_CONTENT_ROUTES
            .iter()
            .map(|route| route.contracts.len())
            .sum::<usize>()
    );

    const NATIVE_RESULT_FIXTURES: [(&str, &str); 5] = [
        (
            "provider::providers::native_jsonl::native_path::gemini::tests::gemini_production_nativepath_core_first_failure_isolated_and_replay_catches_up_idempotently",
            include_str!("../provider/providers/native_jsonl/native_path/gemini.rs"),
        ),
        (
            "provider::providers::native_jsonl::native_path::tabnine::tests::production_is_core_first_with_independent_pro_replay",
            include_str!("../provider/providers/native_jsonl/native_path/tabnine.rs"),
        ),
        (
            "provider::providers::native_jsonl::native_path::copilot::tests::production_is_core_first_with_independent_pro_replay",
            include_str!("../provider/providers/native_jsonl/native_path/copilot.rs"),
        ),
        (
            "provider::providers::native_jsonl::native_path::factory_ai_droid::tests::production_is_core_first_and_pro_failure_is_independent",
            include_str!("../provider/providers/native_jsonl/native_path/factory_ai_droid.rs"),
        ),
        (
            "provider::providers::native_jsonl::native_path::qwen_code::tests::core_commits_before_failed_pro_and_later_output_replay_is_independent",
            include_str!("../provider/providers/native_jsonl/native_path/qwen_code.rs"),
        ),
    ];
    for (fixture_reference, source) in NATIVE_RESULT_FIXTURES {
        let test_name = fixture_reference.rsplit("::").next().unwrap();
        assert!(source.contains(&format!("fn {test_name}")));
        assert_eq!(
            VERIFIED_CONTENT_ROUTES
                .iter()
                .flat_map(|route| route.contracts)
                .filter(|contract| contract.fixture_reference == fixture_reference)
                .count(),
            1
        );
    }
    assert!(VERIFIED_CONTENT_ROUTES
        .iter()
        .flat_map(|route| route.contracts)
        .all(|contract| !contract.fixture_reference.contains("result_locator_tests")));
}

#[test]
fn no_separate_result_cohort_has_explicit_message_routes_without_result_routes() {
    use std::{collections::HashSet, str::FromStr};

    let expected = [
        CaptureProvider::KiroCli,
        CaptureProvider::Antigravity,
        CaptureProvider::Windsurf,
        CaptureProvider::CodeBuddy,
        CaptureProvider::Auggie,
        CaptureProvider::NanoClaw,
        CaptureProvider::AstrBot,
        CaptureProvider::Lingma,
        CaptureProvider::Trae,
    ]
    .into_iter()
    .collect::<HashSet<_>>();
    assert_eq!(expected.len(), 9);

    let matrix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../docs/provider-support-matrix.json"
    ))
    .unwrap();
    for provider in &expected {
        let matrix_provider = matrix["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| {
                CaptureProvider::from_str(entry["capture_provider"].as_str().unwrap()).unwrap()
                    == *provider
            })
            .unwrap();
        let matrix_formats = matrix_provider["implemented_paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|path| path["source_format"].as_str().unwrap())
            .collect::<HashSet<_>>();

        let message_routes = VERIFIED_CONTENT_ROUTES
            .iter()
            .filter(|route| {
                route.provider == *provider && route.role == VerifiedContentRole::MessageBody
            })
            .collect::<Vec<_>>();
        let result_routes = VERIFIED_CONTENT_ROUTES
            .iter()
            .filter(|route| {
                route.provider == *provider && route.role == VerifiedContentRole::ResultBody
            })
            .collect::<Vec<_>>();

        assert_eq!(
            message_routes
                .iter()
                .map(|route| route.source_format)
                .collect::<HashSet<_>>(),
            matrix_formats
        );
        assert_eq!(
            result_routes
                .iter()
                .map(|route| route.source_format)
                .collect::<HashSet<_>>(),
            matrix_formats
        );
        assert!(message_routes.iter().all(|route| {
            !route.contracts.is_empty()
                && route
                    .platform_dispositions
                    .iter()
                    .all(|disposition| disposition.status == VerifiedContentRouteStatus::Supported)
        }));
        assert!(result_routes.iter().all(|route| {
            route.contracts.is_empty()
                && route
                    .platform_dispositions
                    .iter()
                    .all(|disposition| disposition.status == VerifiedContentRouteStatus::NotNeeded)
        }));
    }

    let actual = VERIFIED_CONTENT_ROUTES
        .iter()
        .filter(|route| route.role == VerifiedContentRole::ResultBody)
        .map(|route| route.provider)
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|provider| {
            VERIFIED_CONTENT_ROUTES.iter().all(|route| {
                route.provider != *provider
                    || route.role != VerifiedContentRole::ResultBody
                    || route.platform_dispositions.iter().all(|disposition| {
                        disposition.status == VerifiedContentRouteStatus::NotNeeded
                    })
            })
        })
        .collect::<HashSet<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn nanoclaw_compound_locator_rejects_unselected_or_nonpositive_rows() {
    let content_ref = ContentRef::from_bytes(b"body").unwrap();
    let digest = CompleteContentBodyDigest::from_text("record");
    let mut locator = vec![0_u8; 17];
    locator[8] = 1;
    assert!(VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        "nanoclaw-project.message-body.v1",
        content_ref.clone(),
        CompleteContentSourceFamily::Sqlite,
        "nanoclaw-project-message-v1",
        &locator,
        "inbound:id",
        digest.clone(),
    )
    .is_none());

    locator[..8].copy_from_slice(&((1_u64) ^ (1_u64 << 63)).to_be_bytes());
    locator[9..].copy_from_slice(&((2_u64) ^ (1_u64 << 63)).to_be_bytes());
    locator[8] = 0;
    assert!(VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        "nanoclaw-project.message-body.v1",
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        "nanoclaw-project-message-v1",
        &locator,
        "inbound:id",
        digest,
    )
    .is_none());
}
