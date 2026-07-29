use super::*;

#[test]
fn public_projection_serde_forms_remain_stable() {
    let source = source(7);
    let identity = source.identity();
    let identity_json = serde_json::json!({
        "contract_version": IDENTITY_VERSION,
        "entity_kind": "Source",
        "digest": identity.digest(),
        "source_digest": identity.source_digest(),
        "source_descriptor_digest": identity.source_descriptor_digest(),
        "uuid": identity.as_uuid(),
    });
    assert_eq!(serde_json::to_value(identity).unwrap(), identity_json);
    assert_eq!(
        serde_json::to_string(&identity).unwrap(),
        format!(
            r#"{{"contract_version":1,"entity_kind":"Source","digest":{},"source_digest":{},"source_descriptor_digest":{},"uuid":{}}}"#,
            serde_json::to_string(&identity.digest()).unwrap(),
            serde_json::to_string(&identity.source_digest()).unwrap(),
            serde_json::to_string(&identity.source_descriptor_digest()).unwrap(),
            serde_json::to_string(&identity.as_uuid()).unwrap(),
        )
    );

    let source_json = serde_json::json!({
        "provider": "codex",
        "source_format": "codex_session_jsonl",
        "schema_variant": "session",
        "provider_identity_version": 1,
        "anchor": {"CatalogLineage": vec![7_u8; 32]},
        "identity": identity_json,
    });
    assert_eq!(serde_json::to_value(&source).unwrap(), source_json);
    assert_eq!(
        serde_json::to_string(&source).unwrap(),
        format!(
            r#"{{"provider":"codex","source_format":"codex_session_jsonl","schema_variant":"session","provider_identity_version":1,"anchor":{{"CatalogLineage":{}}},"identity":{}}}"#,
            serde_json::to_string(&vec![7_u8; 32]).unwrap(),
            serde_json::to_string(&identity).unwrap(),
        )
    );
    let decoded_source: SourceKey = serde_json::from_value(source_json.clone()).unwrap();
    assert!(source.exact_descriptor_eq(&decoded_source));

    let observation =
        SourceObservation::new(source.clone(), "regular-file-v1", vec![1, 2, 3]).unwrap();
    let observation_json = serde_json::json!({
        "source": source_json,
        "revision_kind": "regular-file-v1",
        "revision": [1, 2, 3],
    });
    assert_eq!(
        serde_json::to_value(&observation).unwrap(),
        observation_json
    );

    let counts = ScannedSourceCounts {
        complete_records: 1,
        retained_records: 1,
        indexed_documents: 1,
        certified_bytes: 3,
        ..ScannedSourceCounts::default()
    };
    let frontier = SourceFrontier::new("jsonl-byte-offset", TypedKey::U64(3), 3, [9; 32]).unwrap();
    let certificate = CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "parser-v1",
        [9; 32],
        counts,
        Some(frontier),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string(&certificate).unwrap(),
        format!(
            r#"{{"observation":{},"parser_revision":"parser-v1","content_digest":{},"counts":{},"frontier":{}}}"#,
            serde_json::to_string(certificate.observation()).unwrap(),
            serde_json::to_string(certificate.content_digest()).unwrap(),
            serde_json::to_string(&certificate.counts()).unwrap(),
            serde_json::to_string(certificate.frontier().unwrap()).unwrap(),
        )
    );
    assert_eq!(
        serde_json::to_value(&certificate).unwrap(),
        serde_json::json!({
            "observation": observation_json,
            "parser_revision": "parser-v1",
            "content_digest": vec![9_u8; 32],
            "counts": {
                "complete_records": 1,
                "retained_records": 1,
                "rejected_records": 0,
                "ignored_records": 0,
                "indexed_documents": 1,
                "certified_bytes": 3,
            },
            "frontier": {
                "checkpoint_kind": "jsonl-byte-offset",
                "checkpoint": {"U64": 3},
                "certified_prefix_bytes": 3,
                "certified_prefix_digest": vec![9_u8; 32],
            },
        })
    );

    let native_session_key = NativeSessionKey::certified_position(
        "array-index",
        TypedKey::U64(2),
        PositionStability::AppendStable,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string(&native_session_key).unwrap(),
        r#"{"CertifiedPosition":{"kind":"array-index","coordinate":{"U64":2},"stability":"AppendStable","revision_scope":null}}"#
    );
    assert_eq!(
        serde_json::to_value(native_session_key).unwrap(),
        serde_json::json!({"CertifiedPosition": {
            "kind": "array-index",
            "coordinate": {"U64": 2},
            "stability": "AppendStable",
            "revision_scope": null,
        }})
    );
    assert_eq!(
        serde_json::to_value(NativeLocator::new("jsonl", vec![4, 5]).unwrap()).unwrap(),
        serde_json::json!({"kind": "jsonl", "value": [4, 5]})
    );
}
