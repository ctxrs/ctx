use std::{collections::BTreeSet, fs};

use ctx_history_capture::complete_content::{
    structured::{StructuredCompleteContentResolver, STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND},
    verified_content_profile, verified_content_route_supported, AuthorizedSourceRoute,
    CompleteContentBodyDigest, CompleteContentHashAuthority, CompleteContentResolver,
    CompleteContentSourceFamily, CompleteContentSourceLocator, CompleteMessageRequest,
    SourceAccessBroker, SourceSnapshot, VerifiedContentRole, VERIFIED_CONTENT_ROUTES,
};
use ctx_history_core::{CaptureProvider, ContentRef};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

const CAPABILITY_MATRIX: &str =
    include_str!("../../../docs/complete-content-provider-capabilities.json");
const PROVIDER_MATRIX: &str = include_str!("../../../docs/provider-support-matrix.json");

fn structured_locator(
    provider: CaptureProvider,
    ordinal: u64,
    subrecord: u32,
    native: &str,
) -> Vec<u8> {
    let provider = provider.as_str().as_bytes();
    let mut value = Vec::new();
    value.extend_from_slice(b"SC\0\x01");
    value.push(provider.len() as u8);
    value.extend_from_slice(provider);
    value.extend_from_slice(&ordinal.to_be_bytes());
    value.extend_from_slice(&subrecord.to_be_bytes());
    value.extend_from_slice(&(native.len() as u16).to_be_bytes());
    value.extend_from_slice(native.as_bytes());
    value
}

#[test]
fn public_matrix_covers_every_public_provider_exactly_once() {
    let capability: Value = serde_json::from_str(CAPABILITY_MATRIX).unwrap();
    let provider: Value = serde_json::from_str(PROVIDER_MATRIX).unwrap();
    let ids = |document: &Value| {
        document["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&capability), ids(&provider));
    assert_eq!(ids(&capability).len(), 41);
    assert_eq!(
        VERIFIED_CONTENT_ROUTES
            .iter()
            .filter(|route| {
                route.role == VerifiedContentRole::MessageBody
                    && verified_content_route_supported(
                        route.provider,
                        route.source_format,
                        CompleteContentSourceFamily::Structured,
                        route.role,
                    )
            })
            .count(),
        7
    );
    assert!(capability["providers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| {
            matches!(
                entry["status"].as_str(),
                Some("supported" | "not_needed" | "unsupported")
            )
        }));
}

#[test]
fn public_resolver_recovers_verified_rovo_body() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("session_context.json");
    let text = "complete body beyond preview λ";
    let native = "rovo-public-1";
    let bytes = serde_json::to_vec(&json!({
        "message_history": [{"id": native, "content": text}]
    }))
    .unwrap();
    fs::write(&path, &bytes).unwrap();
    let record_digest = format!("{:x}", Sha256::digest(&bytes));
    let event_id = Uuid::new_v4();
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::RovoDev,
                source_format: "rovodev_session_json_tree".to_owned(),
                family: CompleteContentSourceFamily::Structured,
                raw_source_path: path,
                source_root: None,
                source_identity: Some("test:rovo".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event_id,
        )
        .unwrap();
    let request = CompleteMessageRequest {
        event_id,
        provider: CaptureProvider::RovoDev,
        source_format: "rovodev_session_json_tree".to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Structured),
        content_profile: verified_content_profile(
            CaptureProvider::RovoDev,
            "rovodev_session_json_tree",
            CompleteContentSourceFamily::Structured,
            VerifiedContentRole::MessageBody,
        )
        .unwrap()
        .to_owned(),
        source_locator: CompleteContentSourceLocator::new(
            STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
            structured_locator(CaptureProvider::RovoDev, 0, 0, native),
        ),
        provider_session_id: Some("session".to_owned()),
        source_record_ordinal: 0,
        source_record_subrecord_index: 0,
        expected_provider_event_hash: native.to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(native.to_owned()),
        expected_record_digest: CompleteContentBodyDigest::parse(record_digest),
        expected_content_ref: ContentRef::from_bytes(text.as_bytes()),
        indexed_text: text.chars().take(8).collect(),
        indexed_limit_chars: 8,
    };
    let messages = StructuredCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages[0].text, text);
    assert!(messages[0].verification.is_verified());
}
