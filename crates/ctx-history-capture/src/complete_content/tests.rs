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
fn locator_wire_has_one_message_role_and_rejects_removed_result_vocabulary() {
    assert_eq!(VERIFIED_CONTENT_LOCATORS_MAX_ENTRIES, 1);
    assert_eq!(
        serde_json::to_value(VerifiedContentRole::MessageBody).unwrap(),
        serde_json::json!("message_body")
    );
    assert!(
        serde_json::from_value::<VerifiedContentRole>(serde_json::json!("result_body")).is_err()
    );

    let mut address = [0_u8; 16];
    address[8..].copy_from_slice(&1_u64.to_be_bytes());
    let locator = VerifiedContentLocatorV1::new(
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
    let mut value = VerifiedContentLocatorsV1::singleton(locator)
        .unwrap()
        .to_metadata_value();
    let duplicate = value["locators"][0].clone();
    value["locators"].as_array_mut().unwrap().push(duplicate);
    assert!(VerifiedContentLocatorsV1::from_metadata_value(&value).is_none());
}

#[test]
fn route_registry_exactly_covers_matrix_formats_with_message_routes() {
    use std::{collections::HashSet, str::FromStr};

    let matrix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../docs/provider-support-matrix.json"
    ))
    .unwrap();
    let expected = matrix["providers"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|provider| {
            let capture_provider =
                CaptureProvider::from_str(provider["capture_provider"].as_str().unwrap()).unwrap();
            provider["implemented_paths"]
                .as_array()
                .unwrap()
                .iter()
                .map(move |path| {
                    (
                        capture_provider,
                        path["source_format"].as_str().unwrap().to_owned(),
                    )
                })
        })
        .collect::<HashSet<_>>();
    let actual = VERIFIED_CONTENT_ROUTES
        .iter()
        .map(|route| {
            assert_eq!(route.role, VerifiedContentRole::MessageBody);
            (route.provider, route.source_format.to_owned())
        })
        .collect::<HashSet<_>>();
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), VERIFIED_CONTENT_ROUTES.len());
}

#[test]
fn active_core_complete_content_sources_forbid_result_hydration_surfaces() {
    const SOURCES: &[(&str, &str)] = &[
        ("locator", include_str!("locator.rs")),
        ("resolver", include_str!("resolver.rs")),
        ("routes", include_str!("registry/routes.rs")),
        ("jsonl", include_str!("jsonl.rs")),
        ("sqlite", include_str!("sqlite.rs")),
        ("structured", include_str!("structured.rs")),
        (
            "warp provider",
            include_str!("../provider/providers/warp.rs"),
        ),
        (
            "openclaw writer",
            include_str!("../provider/providers/openclaw/complete_content.rs"),
        ),
        (
            "mistral writer",
            include_str!("../provider/providers/mistral_vibe/native_path.rs"),
        ),
    ];
    for (name, source) in SOURCES {
        for forbidden in [
            "ResultBody",
            "ResultContentRequest",
            "ResultContentResolver",
            "ResolvedResultContent",
            "\"result_content_ref\"",
            "warp_result_content_at",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} still exposes {forbidden}"
            );
        }
    }
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
