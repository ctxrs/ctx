// Authored fault-injection cases exercise the real provider capture routes.
use super::*;
use ctx_history_provider_runtime::{
    set_after_jsonl_append_observation_route_binding_hook,
    set_before_jsonl_terminal_physical_revalidation_hook,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RaceAt {
    Discovery,
    Preflight,
    Terminal,
}

fn inject(at: RaceAt, root: &Path, leaf: &Path, change: impl FnOnce() + Send + 'static) {
    match at {
        RaceAt::Discovery => {
            set_after_jsonl_append_observation_route_binding_hook(leaf.to_path_buf(), change)
        }
        RaceAt::Preflight => set_after_jsonl_semantic_preflight_hook(leaf.to_path_buf(), change),
        RaceAt::Terminal => {
            set_before_jsonl_terminal_physical_revalidation_hook(root.to_path_buf(), change)
        }
    }
}

fn transcript_path(root: &Path, provider: CaptureProvider, session: &str) -> std::path::PathBuf {
    match provider {
        CaptureProvider::Claude => root.join("project").join(format!("{session}.jsonl")),
        CaptureProvider::Cursor => root
            .join("project/agent-transcripts")
            .join(session)
            .join(format!("{session}.jsonl")),
        _ => unreachable!(),
    }
}

fn row(provider: CaptureProvider, session: &str, uuid: &str, text: &str) -> Value {
    match provider {
        CaptureProvider::Claude => claude_message("user", uuid, session, text),
        CaptureProvider::Cursor => cursor_message("user", "2026-08-16T00:00:00Z", text),
        _ => unreachable!(),
    }
}

fn source_format(provider: CaptureProvider) -> &'static str {
    match provider {
        CaptureProvider::Claude => "claude_projects_jsonl_tree",
        CaptureProvider::Cursor => CURSOR_SOURCE_FORMAT,
        _ => unreachable!(),
    }
}

fn refresh_member(
    index: &Path,
    registry: &SourceBackedProviderRegistry,
    member: &Path,
) -> crate::SourceBackedCoordinatorResult<crate::SourceBackedRefreshReceipt> {
    let route = registry
        .routes()
        .next()
        .and_then(|route| route.route_identity.clone())
        .unwrap();
    crate::SourceBackedRefreshExecutor::new(registry.clone(), writer_options())
        .refresh_physical_scope_with_detailed_progress_generation_state_reconciliation_and_worksets(
            index,
            crate::SourceBackedRefreshScope::All,
            crate::SourceBackedRefreshScope::All,
            crate::SourceBackedReconciliationDemand::Incremental,
            BTreeMap::from([(route, BTreeSet::from([member.to_path_buf()]))]),
            |_| Ok(()),
            |_| ctx_history_index::GenerationStateEnvelope::new("ctx.test.empty.v1", Vec::new()),
        )
}

#[test]
fn frozen_jsonl_append_publishes_new_peers_and_recovers_all_deferred_rows() {
    for provider in [CaptureProvider::Claude, CaptureProvider::Cursor] {
        for warm in [false, true] {
            for at in [RaceAt::Discovery, RaceAt::Preflight, RaceAt::Terminal] {
                for exact_member in [false, true] {
                    let temp = crate::test_support_paths::tempdir().unwrap();
                    // Exercise the literal Cursor projects entry point reported in #1068.
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
                    if warm {
                        refresh_source_backed_generation(&index, &registry, writer_options())
                            .unwrap();
                    }
                    append_transcript(&race, &row(provider, "racing", "second", "second"));
                    append_transcript(&peer, &row(provider, "peer", "second", "peer new"));
                    fs::remove_file(&missing).unwrap();
                    let hook_path = fs::canonicalize(&race).unwrap();
                    let changed = hook_path.clone();
                    inject(at, &root, &hook_path, move || {
                        append_transcript(&changed, &row(provider, "racing", "third", "third"));
                    });
                    let result = if exact_member {
                        refresh_member(&index, &registry, &race)
                    } else {
                        refresh_source_backed_generation(&index, &registry, writer_options())
                    };
                    let receipt = result.unwrap_or_else(|error| {
                        panic!("provider={provider:?}, warm={warm}, at={at:?}, member={exact_member}: {error:?}")
                    });
                    assert!(receipt.failed_routes.is_empty(), "{receipt:?}");
                    assert!(receipt.logical_source_failures.is_empty(), "{receipt:?}");
                    assert_eq!(receipt.complete_inventory_route_ids.len(), 1);
                    let expected: &[&str] = if at == RaceAt::Discovery {
                        &["first", "second", "third"]
                    } else {
                        &["first", "second"]
                    };
                    assert_literal_bodies(&indexed_records(&index, provider, "racing"), expected);
                    assert_literal_bodies(
                        &indexed_records(&index, provider, "peer"),
                        &["first", "peer new"],
                    );
                    assert!(indexed_records(&index, provider, "missing").is_empty());
                    let recovered =
                        refresh_source_backed_generation(&index, &registry, writer_options())
                            .unwrap();
                    assert!(recovered.failed_routes.is_empty(), "{recovered:?}");
                    assert_literal_bodies(
                        &indexed_records(&index, provider, "racing"),
                        &["first", "second", "third"],
                    );
                    assert_literal_bodies(
                        &indexed_records(&index, provider, "peer"),
                        &["first", "peer new"],
                    );
                }
            }
        }
    }
}

fn append_bytes(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn frozen_jsonl_incomplete_tail_is_deferred_until_the_next_refresh() {
    for provider in [CaptureProvider::Claude, CaptureProvider::Cursor] {
        for warm in [false, true] {
            let temp = crate::test_support_paths::tempdir().unwrap();
            let root = temp.path().join("projects");
            let index = temp.path().join("index");
            let race = transcript_path(&root, provider, "racing");
            let peer = transcript_path(&root, provider, "peer");
            write_transcript(&race, &[row(provider, "racing", "first", "first")]);
            write_transcript(&peer, &[row(provider, "peer", "first", "first")]);
            let registry = registry(provider, source_format(provider), &root);
            if warm {
                refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
            }
            append_bytes(
                &race,
                &serde_json::to_vec(&row(provider, "racing", "second", "second")).unwrap(),
            );
            append_transcript(&peer, &row(provider, "peer", "second", "peer new"));
            let changed = race.clone();
            inject(RaceAt::Preflight, &root, &race, move || {
                append_bytes(&changed, b"\n");
                append_transcript(&changed, &row(provider, "racing", "third", "third"));
            });
            let receipt =
                refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
            assert!(
                receipt.failed_routes.is_empty(),
                "provider={provider:?}, warm={warm}: {receipt:?}"
            );
            assert_literal_bodies(&indexed_records(&index, provider, "racing"), &["first"]);
            assert_literal_bodies(
                &indexed_records(&index, provider, "peer"),
                &["first", "peer new"],
            );
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
            assert_literal_bodies(
                &indexed_records(&index, provider, "racing"),
                &["first", "second", "third"],
            );
        }
    }
}

#[test]
fn pending_first_record_can_finish_without_blocking_peer_publication() {
    for provider in [CaptureProvider::Claude, CaptureProvider::Cursor] {
        for warm in [false, true] {
            let temp = crate::test_support_paths::tempdir().unwrap();
            let root = temp.path().join("projects");
            let index = temp.path().join("index");
            let pending = transcript_path(&root, provider, "pending");
            let peer = transcript_path(&root, provider, "peer");
            let first = row(provider, "pending", "first", "first");
            write_transcript(&pending, std::slice::from_ref(&first));
            write_transcript(&peer, &[row(provider, "peer", "first", "first")]);
            let registry = registry(provider, source_format(provider), &root);
            if warm {
                refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
            }
            fs::write(&pending, serde_json::to_vec(&first).unwrap()).unwrap();
            append_transcript(&peer, &row(provider, "peer", "second", "peer new"));
            let changed = pending.clone();
            inject(RaceAt::Preflight, &root, &peer, move || {
                append_bytes(&changed, b"\n");
                append_transcript(&changed, &row(provider, "pending", "second", "second"));
            });
            let receipt =
                refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
            assert!(
                receipt.failed_routes.is_empty(),
                "provider={provider:?}, warm={warm}: {receipt:?}"
            );
            assert_literal_bodies(
                &indexed_records(&index, provider, "peer"),
                &["first", "peer new"],
            );
            let expected: &[&str] = if warm { &["first"] } else { &[] };
            assert_literal_bodies(&indexed_records(&index, provider, "pending"), expected);
            assert!(receipt.removals.is_empty());
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
            assert_literal_bodies(
                &indexed_records(&index, provider, "pending"),
                &["first", "second"],
            );
        }
    }
}

fn assert_safe_failure(
    result: crate::SourceBackedCoordinatorResult<crate::SourceBackedRefreshReceipt>,
    index: &Path,
    base_generation: Option<&str>,
    context: &str,
) {
    match result {
        Ok(receipt) => {
            assert!(
                base_generation.is_some(),
                "{context}: cold failure published: {receipt:?}"
            );
            assert!(
                receipt.successful_route_ids.is_empty(),
                "{context}: {receipt:?}"
            );
            assert!(
                matches!(receipt.failed_routes.as_slice(), [failure]
                if failure.class == SourceBackedSourceFailureClass::SourceChanged && failure.carried_forward),
                "{context}: {receipt:?}"
            );
        }
        Err(crate::SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes }) => {
            assert!(base_generation.is_none(), "{context}: {failed_routes:?}");
            assert!(
                matches!(failed_routes.failures(), [failure]
                if failure.class == SourceBackedSourceFailureClass::SourceChanged),
                "{context}: {failed_routes:?}"
            );
        }
        Err(crate::SourceBackedCoordinatorError::Index(
            ctx_history_index::IndexError::SourceInvalidated(_),
        )) => {}
        other => panic!("{context}: unexpected capture failure: {other:?}"),
    }
    match base_generation {
        Some(expected) => assert_eq!(
            VerifiedIndex::open_pinned(index).unwrap().generation_id(),
            expected,
            "{context}"
        ),
        None => assert!(VerifiedIndex::open_pinned(index).is_err(), "{context}"),
    }
}

#[derive(Clone, Copy, Debug)]
enum Corruption {
    SameLength,
    RewriteAndAppend,
    Truncate,
    Replace,
}

#[test]
fn frozen_jsonl_rejects_rewrite_truncation_and_replacement_after_preflight() {
    for provider in [CaptureProvider::Claude, CaptureProvider::Cursor] {
        for warm in [false, true] {
            for corruption in [
                Corruption::SameLength,
                Corruption::RewriteAndAppend,
                Corruption::Truncate,
                Corruption::Replace,
            ] {
                let temp = crate::test_support_paths::tempdir().unwrap();
                let root = temp.path().join("projects");
                let index = temp.path().join("index");
                let race = transcript_path(&root, provider, "racing");
                let peer = transcript_path(&root, provider, "peer");
                write_transcript(&race, &[row(provider, "racing", "first", "first")]);
                write_transcript(&peer, &[row(provider, "peer", "first", "peer old")]);
                let registry = registry(provider, source_format(provider), &root);
                let base = warm.then(|| {
                    refresh_source_backed_generation(&index, &registry, writer_options())
                        .unwrap()
                        .commit
                        .generation_id
                });
                append_transcript(&race, &row(provider, "racing", "second", "second"));
                append_transcript(&peer, &row(provider, "peer", "second", "peer new"));
                let changed = race.clone();
                inject(RaceAt::Preflight, &root, &race, move || {
                    let bytes = fs::read(&changed).unwrap();
                    match corruption {
                        Corruption::SameLength | Corruption::RewriteAndAppend => {
                            let modified = fs::metadata(&changed).unwrap().modified().unwrap();
                            let rewritten =
                                String::from_utf8(bytes).unwrap().replace("first", "other");
                            fs::write(&changed, rewritten).unwrap();
                            if matches!(corruption, Corruption::SameLength) {
                                OpenOptions::new()
                                    .write(true)
                                    .open(&changed)
                                    .unwrap()
                                    .set_modified(modified)
                                    .unwrap();
                            } else {
                                append_transcript(
                                    &changed,
                                    &row(provider, "racing", "third", "third"),
                                );
                            }
                        }
                        Corruption::Truncate => {
                            fs::write(&changed, b"").unwrap();
                        }
                        Corruption::Replace => {
                            let replacement = changed.with_extension("replacement");
                            fs::write(&replacement, bytes).unwrap();
                            fs::rename(replacement, &changed).unwrap();
                        }
                    }
                });
                let result = refresh_source_backed_generation(&index, &registry, writer_options());
                assert_safe_failure(
                    result,
                    &index,
                    base.as_deref(),
                    &format!("provider={provider:?}, warm={warm}, corruption={corruption:?}"),
                );
                if warm {
                    assert_literal_bodies(
                        &indexed_records(&index, provider, "peer"),
                        &["peer old"],
                    );
                    assert_literal_bodies(&indexed_records(&index, provider, "racing"), &["first"]);
                }
            }
        }
    }
}

#[path = "active_file_membership.rs"]
mod membership;
