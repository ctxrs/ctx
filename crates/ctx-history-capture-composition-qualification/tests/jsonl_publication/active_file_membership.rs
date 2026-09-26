use super::*;

fn alternate_path(root: &Path, provider: CaptureProvider, session: &str) -> std::path::PathBuf {
    match provider {
        CaptureProvider::Claude => root.join("other").join(format!("{session}.jsonl")),
        CaptureProvider::Cursor => root
            .join("other/agent-transcripts")
            .join(session)
            .join(format!("{session}.jsonl")),
        _ => unreachable!(),
    }
}

#[test]
fn independent_directory_additions_are_deferred_without_invalidating_existing_proofs() {
    for provider in [CaptureProvider::Claude, CaptureProvider::Cursor] {
        for warm in [false, true] {
            for at in [RaceAt::Discovery, RaceAt::Terminal] {
                let temp = crate::test_support_paths::tempdir().unwrap();
                // Also cover Cursor's provider-root entry point.
                let root = temp.path().join(if provider == CaptureProvider::Cursor {
                    "cursor-data"
                } else {
                    "projects"
                });
                let tree = if provider == CaptureProvider::Cursor {
                    root.join("projects")
                } else {
                    root.clone()
                };
                let index = temp.path().join("index");
                let race = transcript_path(&tree, provider, "racing");
                let peer = transcript_path(&tree, provider, "peer");
                for (path, session) in [(&race, "racing"), (&peer, "peer")] {
                    write_transcript(path, &[row(provider, session, "first", "first")]);
                }
                // The unselected copy remains exact while unrelated directories change.
                if provider == CaptureProvider::Cursor {
                    let stable = row(provider, "shared", "first", "shared first");
                    write_transcript(
                        &transcript_path(&tree, provider, "shared"),
                        std::slice::from_ref(&stable),
                    );
                    write_transcript(&alternate_path(&tree, provider, "shared"), &[stable]);
                }
                let registry = registry(provider, source_format(provider), &root);
                if warm {
                    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
                }
                append_transcript(&race, &row(provider, "racing", "second", "second"));
                append_transcript(&peer, &row(provider, "peer", "second", "peer new"));
                let added = match provider {
                    CaptureProvider::Claude => tree.join("new-project/added.jsonl"),
                    CaptureProvider::Cursor => {
                        tree.join("new-project/agent-transcripts/added/added.jsonl")
                    }
                    _ => unreachable!(),
                };
                let changed = race.clone();
                inject(at, &root, &race, move || {
                    write_transcript(&added, &[row(provider, "added", "first", "added")]);
                    append_transcript(&changed, &row(provider, "racing", "third", "third"));
                });
                let receipt = refresh_source_backed_generation(&index, &registry, writer_options())
                    .unwrap_or_else(|error| {
                        panic!("provider={provider:?}, warm={warm}, at={at:?}: {error:?}")
                    });
                assert!(
                    receipt.failed_routes.is_empty(),
                    "provider={provider:?}, warm={warm}, at={at:?}: {receipt:?}"
                );
                assert_literal_bodies(
                    &indexed_records(&index, provider, "peer"),
                    &["first", "peer new"],
                );
                assert!(indexed_records(&index, provider, "added").is_empty());
                let recovered =
                    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
                assert!(recovered.failed_routes.is_empty(), "{recovered:?}");
                assert_literal_bodies(
                    &indexed_records(&index, provider, "racing"),
                    &["first", "second", "third"],
                );
                assert_literal_bodies(&indexed_records(&index, provider, "added"), &["added"]);
            }
        }
    }
}

#[test]
fn new_aliases_cannot_change_selection_or_hide_a_reappearing_deletion_candidate() {
    for provider in [CaptureProvider::Claude, CaptureProvider::Cursor] {
        for warm in [false, true] {
            for reappearing in [false, true] {
                if reappearing && !warm {
                    continue;
                }
                for at in [RaceAt::Discovery, RaceAt::Terminal] {
                    let temp = crate::test_support_paths::tempdir().unwrap();
                    let root = temp.path().join("projects");
                    let index = temp.path().join("index");
                    let race = transcript_path(&root, provider, "racing");
                    let peer = transcript_path(&root, provider, "peer");
                    let missing = transcript_path(&root, provider, "missing");
                    for (path, session) in
                        [(&race, "racing"), (&peer, "peer"), (&missing, "missing")]
                    {
                        write_transcript(path, &[row(provider, session, "first", "first")]);
                    }
                    let registry = registry(provider, source_format(provider), &root);
                    // Keep the generation-state envelope identical to the
                    // workset refresh below: it participates in generation identity.
                    let base = warm.then(|| {
                        refresh_member(&index, &registry, &race)
                            .unwrap()
                            .commit
                            .generation_id
                    });
                    append_transcript(&race, &row(provider, "racing", "second", "second"));
                    append_transcript(&peer, &row(provider, "peer", "second", "peer new"));
                    if reappearing {
                        fs::remove_file(&missing).unwrap();
                    }
                    let session = if reappearing { "missing" } else { "racing" };
                    let added = alternate_path(&root, provider, session);
                    inject(at, &root, &race, move || {
                        write_transcript(
                            &added,
                            &[
                                row(provider, session, "first", "divergent first"),
                                row(provider, session, "second", "divergent second"),
                                row(provider, session, "third", "divergent third"),
                            ],
                        );
                    });
                    // A member workset must still discover global native aliases.
                    assert_safe_failure(
                        refresh_member(&index, &registry, &race),
                        &index,
                        base.as_deref(),
                        &format!("provider={provider:?}, warm={warm}, at={at:?}, reappearing={reappearing}"),
                    );
                    if warm {
                        assert_literal_bodies(
                            &indexed_records(&index, provider, "peer"),
                            &["first"],
                        );
                        assert_literal_bodies(
                            &indexed_records(&index, provider, "missing"),
                            &["first"],
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn cursor_selected_copy_can_append_while_alternate_copy_stays_exact() {
    for warm in [false, true] {
        for at in [RaceAt::Discovery, RaceAt::Preflight, RaceAt::Terminal] {
            let temp = crate::test_support_paths::tempdir().unwrap();
            let root = temp.path().join("projects");
            let index = temp.path().join("index");
            // The lexically first copy is selected when the snapshots agree.
            let selected = root.join("a/agent-transcripts/shared/shared.jsonl");
            let alternate = root.join("z/agent-transcripts/shared/shared.jsonl");
            let peer = transcript_path(&root, CaptureProvider::Cursor, "peer");
            let first = cursor_message("user", "2026-08-16T00:00:00Z", "first");
            for path in [&selected, &alternate] {
                write_transcript(path, std::slice::from_ref(&first));
            }
            write_transcript(
                &peer,
                &[row(CaptureProvider::Cursor, "peer", "first", "peer old")],
            );
            let registry = registry(CaptureProvider::Cursor, CURSOR_SOURCE_FORMAT, &root);
            if warm {
                refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
            }
            let second = cursor_message("assistant", "2026-08-16T00:00:01Z", "second");
            for path in [&selected, &alternate] {
                append_transcript(path, &second);
            }
            append_transcript(
                &peer,
                &row(CaptureProvider::Cursor, "peer", "second", "peer new"),
            );
            let changed = selected.clone();
            inject(at, &root, &selected, move || {
                append_transcript(
                    &changed,
                    &cursor_message("assistant", "2026-08-16T00:00:02Z", "third"),
                )
            });
            let receipt =
                refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
            assert!(
                receipt.failed_routes.is_empty(),
                "warm={warm}, at={at:?}: {receipt:?}"
            );
            let expected: &[&str] = if at == RaceAt::Discovery {
                &["first", "second", "third"]
            } else {
                &["first", "second"]
            };
            assert_literal_bodies(
                &indexed_records(&index, CaptureProvider::Cursor, "shared"),
                expected,
            );
            assert_literal_bodies(
                &indexed_records(&index, CaptureProvider::Cursor, "peer"),
                &["peer old", "peer new"],
            );
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
            assert_literal_bodies(
                &indexed_records(&index, CaptureProvider::Cursor, "shared"),
                &["first", "second", "third"],
            );
        }
    }
}

#[test]
fn cursor_partial_capture_keeps_alternate_copy_dependencies_through_publication() {
    for warm in [false, true] {
        for at in [RaceAt::Preflight, RaceAt::Terminal] {
            let temp = crate::test_support_paths::tempdir().unwrap();
            let root = temp.path().join("projects");
            let index = temp.path().join("index");
            let selected = root.join("a/agent-transcripts/shared/shared.jsonl");
            let alternate = root.join("z/agent-transcripts/shared/shared.jsonl");
            let pending = transcript_path(&root, CaptureProvider::Cursor, "pending");
            let first = cursor_message("user", "2026-08-16T00:00:00Z", "first");
            for path in [&selected, &alternate, &pending] {
                write_transcript(path, std::slice::from_ref(&first));
            }
            let registry = registry(CaptureProvider::Cursor, CURSOR_SOURCE_FORMAT, &root);
            let base = warm.then(|| {
                refresh_source_backed_generation(&index, &registry, writer_options())
                    .unwrap()
                    .commit
                    .generation_id
            });
            fs::write(&pending, serde_json::to_vec(&first).unwrap()).unwrap();
            let second = cursor_message("assistant", "2026-08-16T00:00:01Z", "second");
            for path in [&selected, &alternate] {
                append_transcript(path, &second);
            }
            inject(at, &root, &selected, move || {
                write_transcript(
                    &alternate,
                    &[
                        cursor_message("user", "2026-08-16T00:00:00Z", "divergent first"),
                        cursor_message("assistant", "2026-08-16T00:00:01Z", "divergent second"),
                        cursor_message("assistant", "2026-08-16T00:00:02Z", "divergent third"),
                    ],
                );
            });
            let result = refresh_source_backed_generation(&index, &registry, writer_options());
            if warm && at == RaceAt::Terminal {
                assert!(
                    matches!(
                        &result,
                        Err(crate::SourceBackedCoordinatorError::Index(
                            ctx_history_index::IndexError::SourceInvalidated(_)
                        ))
                    ),
                    "{result:?}"
                );
            }
            assert_safe_failure(
                result,
                &index,
                base.as_deref(),
                &format!("Cursor alternate dependency: warm={warm}, at={at:?}"),
            );
            if warm {
                assert_literal_bodies(
                    &indexed_records(&index, CaptureProvider::Cursor, "shared"),
                    &["first"],
                );
            }
        }
    }
}

#[test]
fn cursor_pending_duplicate_preserves_aliases_and_peer_progress_until_completion() {
    for warm in [false, true] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("projects");
        let index = temp.path().join("index");
        let selected = root.join("a/agent-transcripts/shared/shared.jsonl");
        let alternate = root.join("z/agent-transcripts/shared/shared.jsonl");
        let peer = transcript_path(&root, CaptureProvider::Cursor, "peer");
        let first = cursor_message("user", "2026-08-16T00:00:00Z", "first");
        for path in [&selected, &alternate] {
            write_transcript(path, std::slice::from_ref(&first));
        }
        write_transcript(
            &peer,
            &[row(CaptureProvider::Cursor, "peer", "first", "peer old")],
        );
        let registry = registry(CaptureProvider::Cursor, CURSOR_SOURCE_FORMAT, &root);
        if warm {
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        }
        // Both copies are unchanged throughout capture, with an unfinished
        // first record. Classification must retain their admitted alias proof.
        for path in [&selected, &alternate] {
            fs::write(path, serde_json::to_vec(&first).unwrap()).unwrap();
        }
        append_transcript(
            &peer,
            &row(CaptureProvider::Cursor, "peer", "second", "peer new"),
        );
        let receipt = refresh_source_backed_generation(&index, &registry, writer_options())
            .unwrap_or_else(|error| panic!("warm={warm}: {error:?}"));
        assert!(receipt.failed_routes.is_empty(), "warm={warm}: {receipt:?}");
        assert!(receipt.logical_source_failures.is_empty(), "{receipt:?}");
        assert!(receipt.removals.is_empty(), "{receipt:?}");
        assert_literal_bodies(
            &indexed_records(&index, CaptureProvider::Cursor, "peer"),
            &["peer old", "peer new"],
        );
        let expected: &[&str] = if warm { &["first"] } else { &[] };
        assert_literal_bodies(
            &indexed_records(&index, CaptureProvider::Cursor, "shared"),
            expected,
        );
        for path in [&selected, &alternate] {
            append_bytes(path, b"\n");
            append_transcript(
                path,
                &cursor_message("assistant", "2026-08-16T00:00:01Z", "second"),
            );
        }
        let recovered =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert!(recovered.failed_routes.is_empty(), "{recovered:?}");
        assert!(
            recovered.logical_source_failures.is_empty(),
            "{recovered:?}"
        );
        assert_literal_bodies(
            &indexed_records(&index, CaptureProvider::Cursor, "shared"),
            &["first", "second"],
        );
        assert_literal_bodies(
            &indexed_records(&index, CaptureProvider::Cursor, "peer"),
            &["peer old", "peer new"],
        );
    }
}
