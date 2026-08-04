use super::*;

#[test]
fn current_source_progress_failure_is_latched_when_driver_suppresses_it() {
    let mut route = fixture_route(CaptureProvider::Gemini, GEMINI_CLI_SOURCE_FORMAT, 1);
    let original = route.driver.take().unwrap();
    let scan = Arc::clone(&original.scan);
    let owns = Arc::clone(&original.owns_source);
    let revalidate = Arc::clone(&original.revalidate);
    route.driver = Some(SourceBackedRouteDriver::new_fallible(
        move |sink| {
            let progress = SourceBackedCurrentSourceProgress::new(
                SourceBackedCurrentSourceProgressStage::LogicalScan,
            );
            assert!(sink.report_current_source_progress(progress).is_err());
            assert!(sink.report_current_source_progress(progress).is_err());
            scan(sink)
        },
        move |source| owns(source),
        move |target| revalidate(target),
    ));
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let callbacks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&callbacks);

    let error = refresh_source_backed_generation_with_detailed_progress(
        tempdir().unwrap().path(),
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
        move |update| {
            if update.current_source_progress.is_some() {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "fixture progress failure",
                ));
            }
            Ok(())
        },
    )
    .expect_err("latched progress failure remains systemic");

    assert!(matches!(error, SourceBackedCoordinatorError::Progress(_)));
    assert_eq!(callbacks.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn committed_progress_failure_does_not_hide_visible_publication() {
    let (route, certificate) = revisioned_receipt_route(7);
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let temp = tempdir().unwrap();

    let receipt = refresh_source_backed_generation_with_progress(
        temp.path(),
        &registry,
        WriterOptions::default(),
        |progress| {
            if progress.phase == "committed" {
                Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "injected committed status failure",
                ))
            } else {
                Ok(())
            }
        },
    )
    .expect("commit visibility is irreversible success");

    assert_eq!(receipt.sources, vec![certificate]);
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        receipt.commit.generation_id
    );
}

#[test]
fn source_record_progress_resets_per_route_and_is_absent_outside_scans() {
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        1,
    ));
    registry.register(fixture_route(
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
        2,
    ));
    let temp = tempdir().unwrap();
    let initial =
        refresh_source_backed_generation(temp.path(), &registry, WriterOptions::default()).unwrap();
    let mut updates = Vec::new();

    let replay = refresh_source_backed_generation_with_progress(
        temp.path(),
        &registry,
        WriterOptions::default(),
        |progress| {
            updates.push((
                progress.phase,
                progress.current_source,
                progress.completed_records,
                progress.completed_bytes,
            ));
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(replay.commit.generation_id, initial.commit.generation_id);
    assert!(replay
        .successful_route_outcomes
        .iter()
        .all(|outcome| !outcome.changed));

    let active = updates
        .iter()
        .filter(|(_, source, _, _)| source.is_some())
        .map(|(_, _, completed_records, completed_bytes)| (*completed_records, *completed_bytes))
        .collect::<Vec<_>>();
    assert_eq!(
        active,
        vec![
            (Some(0), Some(0)),
            (Some(0), Some(1)),
            (Some(1), Some(1)),
            (Some(0), Some(0)),
            (Some(0), Some(1)),
            (Some(1), Some(1)),
        ]
    );
    assert!(updates
        .iter()
        .filter(|(_, source, _, _)| source.is_none())
        .all(|(_, _, completed_records, completed_bytes)| {
            completed_records.is_none() && completed_bytes.is_none()
        }));
}

#[test]
fn accepted_record_progress_failure_stays_typed_and_prevents_publication() {
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        1,
    ));
    let temp = tempdir().unwrap();

    let error = refresh_source_backed_generation_with_progress(
        temp.path(),
        &registry,
        WriterOptions::default(),
        |progress| {
            if progress.completed_records == Some(1) {
                Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "injected source-record progress failure",
                ))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        SourceBackedCoordinatorError::Progress(SourceBackedRouteError { detail, .. })
            if detail == "injected source-record progress failure"
    ));
    assert!(VerifiedIndex::open(temp.path()).is_err());
}
