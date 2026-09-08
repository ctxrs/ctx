//! Logical receipt admission coverage owned by the retained refresh engine.

use super::*;

#[test]
fn mismatched_request_overlay_is_not_recorded_as_verified() {
    let temp = tempfile::tempdir().unwrap();
    let requested = test_catalog_authority(1, 0x11);
    let published = test_catalog_authority(2, 0x22);
    let coordinator = CoreRefreshEngine::new();
    let response = coordinator
        .handle_ipc_request_with_admission_fence_for_test(
            temp.path(),
            &json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "refresh_intent": {
                    "kind": "selected_import",
                    "selection": {
                        "kind": "exact_source",
                        "authority": requested.to_json(),
                    },
                },
            }),
            BTreeMap::new(),
        )
        .unwrap()
        .unwrap();
    let request_id = request_id(&response);
    let run = coordinator
        .run_next_with(
            |_, _| {
                let mut publication = test_publication("catalog-generation");
                publication.published_explicit_source_catalog = Some(published);
                Ok(publication)
            },
            || Ok(Some("catalog-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(run.failed);
    assert!(coordinator.status(&request_id).unwrap()["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("different from the requested authority")));
}
