use super::*;

#[test]
fn partial_publication_clears_internal_failure_history_only_for_completed_routes() {
    for retryable in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let executor_calls = Arc::clone(&calls);
        let selected = Arc::new(Mutex::new(None::<SourceRouteIdentity>));
        let executor_selected = Arc::clone(&selected);
        let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            if executor_calls.fetch_add(1, Ordering::SeqCst) != 1 {
                return Err(anyhow!("stable internal refresh fixture failure"));
            }
            let retained = VerifiedIndex::open_pinned(execution.index_root)?
                .manifest()
                .sources
                .clone();
            let mut writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?;
            for source in retained {
                writer.retain_source(source)?;
            }
            let commit = commit_source_backed_test_generation(writer)?;
            let mut publication = empty_test_publication(commit.generation_id.clone());
            publication.current =
                SourceBackedRefreshCurrent::from_sources(&commit.manifest().sources, 0)?;
            publication.certified_source_count = publication.current.source_count;
            publication.certified_source_bytes = publication.current.certified_source_bytes;
            let route = executor_selected.lock().unwrap().clone().unwrap();
            let mut result =
                SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), false);
            result.source_failure_total = 1;
            result.source_retryable_failure_total = usize::from(retryable);
            result.source_failures = vec![SourceBackedRefreshSourceFailure {
                route_identity: route.as_str().to_owned(),
                source_identity: "ef".repeat(32),
                provider: "fixture".to_owned(),
                class: if retryable {
                    "unavailable"
                } else {
                    "unreadable"
                }
                .to_owned(),
                carried_forward: true,
                source_selector: "fixture leaf".to_owned(),
                detail: "fixture leaf failure".to_owned(),
            }];
            publication.route_results = vec![result];
            Ok(publication)
        });
        let (_temp, data_root, _source, engine, _catalog, route, _generation) =
            automatic_retry_fixture(executor);
        *selected.lock().unwrap() = Some(route.clone());
        let first = run_due_failure(&engine, &data_root, &route);
        assert_eq!(first.job["automatic_retry"]["state"], "confirming");
        let unrelated = route_identity(0x79);
        let unrelated_checkpoint = {
            let mut state = engine.lock_state();
            let mut checkpoint = state.automatic_retry_checkpoints[&route].clone();
            checkpoint.pause();
            state
                .automatic_retry_checkpoints
                .insert(unrelated.clone(), checkpoint.clone());
            checkpoint
        };

        let partial = run_due_failure(&engine, &data_root, &route);
        assert!(!partial.failed, "{:#}", partial.job);
        assert_eq!(
            partial.job["structured_outcome"]["code"],
            "completed_with_source_failures"
        );
        {
            let state = engine.lock_state();
            assert!(!state.automatic_retry_checkpoints.contains_key(&route));
            assert_eq!(
                state.automatic_retry_checkpoints.get(&unrelated),
                Some(&unrelated_checkpoint)
            );
        }
        engine.record_watch_routes(
            [(route.clone(), automatic_retry_test_watermark())],
            ledger_now_ms().saturating_sub(1_000),
        );
        let later = run_due_failure(&engine, &data_root, &route);
        assert_eq!(
            later.job["automatic_retry"]["routes"][route.as_str()]["state"],
            "confirming"
        );
        assert_eq!(
            later.job["automatic_retry"]["routes"][route.as_str()]["matching_failures"],
            1
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
