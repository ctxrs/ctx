use super::*;

fn assert_released_payload_is_rebuilt(publication_metadata: Option<&str>) {
    additional::assert_active_meta_incompatibility_is_rebuilt(
        "released-payload.jsonl",
        |meta| {
            let payload: serde_json::Value =
                serde_json::from_str(meta["payload"].as_str().unwrap()).unwrap();
            // v1.3.1 serialized these three fields in this order, including None.
            // The metadata was opaque standard unpadded base64 at this boundary.
            let released = format!(
                "{{\"version\":2,\"generation_id\":{},\"publication_metadata\":{}}}",
                payload["generation_id"],
                serde_json::to_string(&publication_metadata).unwrap(),
            );
            if publication_metadata.is_some() {
                assert!(released.len() > 256);
                assert!(released.len() <= 65_792);
            } else {
                assert!(released.len() <= 256);
            }
            meta["payload"] = serde_json::Value::String(released);
        },
        |error| {
            assert!(
                matches!(error, IndexError::UnsupportedCommitPayload(2)),
                "released envelope must be incompatible, got {error:?}"
            );
            assert!(generation_incompatibility_requires_rebuild(error));
            assert!(generation_incompatibility_requires_recovery_rebuild(error));
        },
    );
}

#[test]
fn released_v2_null_metadata_rebuilds_from_source() {
    assert_released_payload_is_rebuilt(None);
}

#[test]
fn released_v2_non_null_metadata_over_current_bound_rebuilds_from_source() {
    // "YWJj" is standard unpadded base64 for b"abc". The released writer
    // accepted arbitrary opaque bytes, including this 192-byte metadata body.
    let encoded_metadata = "YWJj".repeat(64);
    assert_released_payload_is_rebuilt(Some(&encoded_metadata));
}

fn assert_current_payload_is_rejected(
    encode_payload: impl FnOnce(&str) -> String,
    assert_error: impl Fn(&IndexError),
) {
    let temp = tempdir().unwrap();
    let source = source("current-payload-corruption.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "retained source body"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_path = temp.path().join("active-generation.json");
    let pointer_before = fs::read(&pointer_path).unwrap();
    let generation_path = active_generation_path(temp.path());
    let meta_path = generation_path.join("meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    meta["payload"] = serde_json::Value::String(encode_payload(&baseline.generation_id));
    let corrupt_meta = serde_json::to_vec(&meta).unwrap();
    fs::write(&meta_path, &corrupt_meta).unwrap();

    let reader_error = match VerifiedIndex::open(temp.path()) {
        Ok(_) => panic!("corrupt current payload unexpectedly opened"),
        Err(error) => error,
    };
    assert_error(&reader_error);
    assert!(!generation_incompatibility_requires_rebuild(&reader_error));
    assert!(!generation_incompatibility_requires_recovery_rebuild(
        &reader_error
    ));
    let writer_error = match GenerationWriter::open(temp.path(), WriterOptions::default()) {
        Ok(_) => panic!("corrupt current payload unexpectedly started a replacement"),
        Err(error) => error,
    };
    assert_error(&writer_error);
    assert!(!generation_incompatibility_requires_rebuild(&writer_error));
    assert!(!generation_incompatibility_requires_recovery_rebuild(
        &writer_error
    ));
    assert_eq!(fs::read(&pointer_path).unwrap(), pointer_before);
    assert_eq!(active_generation_path(temp.path()), generation_path);
    assert_eq!(fs::read(&meta_path).unwrap(), corrupt_meta);
}

#[test]
fn current_payload_with_removed_metadata_field_remains_a_json_error() {
    assert_current_payload_is_rejected(
        |generation_id| {
            format!(
                "{{\"version\":{COMMIT_PAYLOAD_VERSION},\"generation_id\":\"{generation_id}\",\"publication_metadata\":null}}"
            )
        },
        |error| assert!(matches!(error, IndexError::Json(_)), "{error:?}"),
    );
}

#[test]
fn truncated_current_payload_remains_a_json_error() {
    assert_current_payload_is_rejected(
        |generation_id| {
            format!("{{\"version\":{COMMIT_PAYLOAD_VERSION},\"generation_id\":\"{generation_id}\"")
        },
        |error| assert!(matches!(error, IndexError::Json(_)), "{error:?}"),
    );
}

#[test]
fn current_payload_with_invalid_generation_id_remains_rejected() {
    assert_current_payload_is_rejected(
        |_| format!("{{\"version\":{COMMIT_PAYLOAD_VERSION},\"generation_id\":\"invalid\"}}"),
        |error| {
            assert!(
                matches!(error, IndexError::InvalidGenerationId),
                "{error:?}"
            )
        },
    );
}

#[test]
fn noncanonical_current_payload_remains_rejected() {
    assert_current_payload_is_rejected(
        |generation_id| {
            format!(
                " {{\"version\":{COMMIT_PAYLOAD_VERSION},\"generation_id\":\"{generation_id}\"}}"
            )
        },
        |error| {
            assert!(
                matches!(error, IndexError::NonCanonicalCommitPayload),
                "{error:?}"
            );
        },
    );
}

#[test]
fn oversized_current_payload_keeps_the_current_size_limit() {
    assert_current_payload_is_rejected(
        |generation_id| {
            let payload = format!(
                "{{\"version\":{COMMIT_PAYLOAD_VERSION},\"generation_id\":\"{generation_id}\"}}{}",
                " ".repeat(256),
            );
            assert!(payload.len() > 256);
            assert!(payload.len() < 65_792);
            payload
        },
        |error| {
            assert!(
                matches!(error, IndexError::CommitPayloadTooLarge { actual, maximum: 256 } if *actual > 256),
                "{error:?}"
            );
        },
    );
}
