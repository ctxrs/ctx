use super::*;

const SESSION: &str = "019fb100-0000-7000-8000-000000000001";

// Authored filesystem cases; these do not establish a native producer format.
#[test]
fn unchanged_inventory_preserves_observations_and_session_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let day = sessions.join("2026/09/29");
    std::fs::create_dir_all(&day).unwrap();
    for ordinal in 0..16 {
        let session = format!("019fb100-0000-7000-8000-{ordinal:012x}");
        std::fs::write(
            day.join(format!("{session}.jsonl")),
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{session}\"}}}}\n"),
        )
        .unwrap();
    }
    let discover = || {
        super::super::catalog::discover_codex_deferred_session_tree_inventory_v0(
            std::slice::from_ref(&sessions),
        )
        .unwrap()
    };
    let first = discover();
    let repeated = discover();
    assert!(first.rejected_leaves.is_empty());
    assert!(repeated.rejected_leaves.is_empty());
    assert_eq!(first.sources.len(), 16);
    assert_eq!(repeated.sources.len(), 16);
    for (before, after) in first.sources.iter().zip(&repeated.sources) {
        assert!(before.1.exact_descriptor_eq(&after.1));
        assert_eq!(before.2, after.2);
        assert_eq!(before.0.catalog_observation, after.0.catalog_observation);
        assert_eq!(
            before.0.carried_jsonl_observation,
            after.0.carried_jsonl_observation
        );
        assert!(generation_source_owner_is_admissible_v0(&before.0, &before.2).unwrap());
        assert!(generation_source_owner_is_admissible_v0(&after.0, &after.2).unwrap());
    }
}

#[test]
fn ownership_inventory_distinguishes_incomplete_and_conflicting_sources() {
    for (body, admissible) in [
        (String::new(), true),
        ("{\"type\":\"session_meta\",\"payload\":".to_owned(), true),
        ("{\"type\":\"response_item\"}\n".to_owned(), false),
        ("malformed complete record\n".to_owned(), false),
        (
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{SESSION}\"}}}}\n"),
            true,
        ),
        (
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"another-session\"}}\n".to_owned(),
            false,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(format!("{SESSION}.jsonl"));
        std::fs::write(&path, body).unwrap();
        let inventory =
            super::super::catalog::discover_codex_deferred_session_tree_inventory_v0(&[temp
                .path()
                .to_path_buf()])
            .unwrap();
        let (source, _, session) = &inventory.sources[0];
        assert_eq!(session, SESSION);
        assert_eq!(
            generation_source_owner_is_admissible_v0(source, session).unwrap(),
            admissible
        );
    }
}
