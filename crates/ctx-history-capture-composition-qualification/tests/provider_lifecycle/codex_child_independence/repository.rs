use super::*;

#[test]
fn destructive_precommit_truncate_and_replacement_preserve_last_good_generation_atomically() {
    // Same-object rewrites are excluded by the Codex append-only provider
    // contract and are covered by the explicit trust-boundary test in shared
    // JSONL. Observable truncation and object replacement must still fail the
    // terminal fence atomically.
    for mutation in ["truncate", "replacement"] {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let index_root = temp.path().join("index");
        fs::create_dir_all(&sessions).unwrap();
        let native_session_id = "019fb000-0000-7000-8000-000000000041";
        let path = session_path(&sessions, native_session_id);
        write_session(
            &sessions,
            native_session_id,
            ProviderNativeSessionRelationship::Root,
            None,
            [message("lastgooduniquetoken")],
        );
        let registry = register_tree(&[&sessions]);
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        let before = VerifiedIndex::open_pinned(&index_root).unwrap();
        let generation = before.generation_id().to_owned();
        let snapshot = source_snapshot(&before, native_session_id, "lastgooduniquetoken");
        drop(before);

        let mutate = path.clone();
        let replacement = path.with_extension("replacement");
        if mutation == "replacement" {
            fs::write(
                &replacement,
                jsonl_bytes([
                    session_meta(
                        native_session_id,
                        ProviderNativeSessionRelationship::Root,
                        None,
                    ),
                    message("replacementuniquetoken"),
                ]),
            )
            .unwrap();
        }
        set_before_jsonl_terminal_physical_revalidation_hook(sessions.clone(), move || {
            destructively_mutate_session(&mutate, &replacement, mutation);
        });
        match refresh_source_backed_generation(&index_root, &registry, writer_options()) {
            Ok(failed) => {
                assert_eq!(failed.failed_routes.len(), 1, "{mutation}");
                assert!(failed.failed_routes[0].carried_forward, "{mutation}");
            }
            Err(SourceBackedCoordinatorError::RouteScan { source, .. }) => {
                assert_eq!(
                    source.kind,
                    SourceBackedRouteErrorKind::InvalidSource,
                    "{mutation}"
                );
            }
            Err(error) => panic!("unexpected {mutation} failure: {error:?}"),
        }
        let retained = VerifiedIndex::open_pinned(&index_root).unwrap();
        assert_eq!(retained.generation_id(), generation, "{mutation}");
        assert_eq!(
            source_snapshot(&retained, native_session_id, "lastgooduniquetoken"),
            snapshot,
            "{mutation}"
        );
        assert!(search_event_candidates(&retained, "replacementuniquetoken", 8).is_empty());
    }
}

#[test]
fn current_authority_reappearance_before_publication_carries_last_good_generation() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let retained_session = "019fb000-0000-7000-8000-000000000043";
    let deleted_session = "019fb000-0000-7000-8000-000000000044";
    let retained_marker = "currentauthorityretainedmarker";
    let deleted_marker = "currentauthoritydeletedmarker";
    write_session(
        &sessions,
        retained_session,
        ProviderNativeSessionRelationship::Root,
        None,
        [message(retained_marker)],
    );
    let deleted_path = session_path(&sessions, deleted_session);
    write_session(
        &sessions,
        deleted_session,
        ProviderNativeSessionRelationship::Root,
        None,
        [message(deleted_marker)],
    );
    let registry = register_tree(&[&sessions]);
    let route_identity = route_identity(&registry, &sessions);
    let initial =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(initial.failed_routes.is_empty());
    let before = VerifiedIndex::open_pinned(&index_root).unwrap();
    let generation = before.generation_id().to_owned();
    let retained_snapshot = source_snapshot(&before, retained_session, retained_marker);
    let deleted_snapshot = source_snapshot(&before, deleted_session, deleted_marker);
    drop(before);

    fs::remove_file(&deleted_path).unwrap();
    let hook_sessions = sessions.clone();
    set_before_jsonl_terminal_physical_revalidation_hook(sessions.clone(), move || {
        write_session(
            &hook_sessions,
            deleted_session,
            ProviderNativeSessionRelationship::Root,
            None,
            [message(deleted_marker)],
        );
    });

    let failed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(matches!(
        failed.failed_routes.as_slice(),
        [failure]
            if failure.route_identity == route_identity
                && failure.class == SourceBackedSourceFailureClass::SourceChanged
                && failure.carried_forward
    ));
    assert!(failed.removals.is_empty());
    let retained = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(retained.generation_id(), generation);
    assert_eq!(
        source_snapshot(&retained, retained_session, retained_marker),
        retained_snapshot
    );
    assert_eq!(
        source_snapshot(&retained, deleted_session, deleted_marker),
        deleted_snapshot
    );
}
