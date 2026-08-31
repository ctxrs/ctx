use super::*;
use std::{cell::RefCell, error::Error as StdError, fmt, rc::Rc};

use ctx_history_index::test_support::{
    publication_io_error, AtomicPublicationStage, AtomicReplacementFailureProbe,
    PublicationIoProbe, PublicationIoProbeGuard,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixturePublicationAttempt {
    Initial,
    Incremental,
}

impl FixturePublicationAttempt {
    const fn label(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Incremental => "incremental",
        }
    }
}

struct FixturePublicationFailure {
    attempt: FixturePublicationAttempt,
    last_stage: Option<PublicationIoProbe>,
    source: anyhow::Error,
    #[cfg(windows)]
    active_pointer_probe: Option<String>,
}

impl FixturePublicationFailure {
    fn new(
        attempt: FixturePublicationAttempt,
        last_stage: Option<PublicationIoProbe>,
        source: anyhow::Error,
        root: &Path,
        replacement_failure: Option<AtomicReplacementFailureProbe>,
    ) -> Self {
        #[cfg(not(windows))]
        let _ = (root, replacement_failure);
        Self {
            attempt,
            last_stage,
            #[cfg(windows)]
            active_pointer_probe: matches!(
                last_stage,
                Some(PublicationIoProbe::ActivePointer(
                    AtomicPublicationStage::Replacement
                ))
            )
            .then(|| capture_active_pointer_failure(root, &source, replacement_failure)),
            source,
        }
    }
}

impl fmt::Display for FixturePublicationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Core publication failed: attempt={} last_stage=",
            self.attempt.label()
        )?;
        match self.last_stage {
            Some(stage) => write!(formatter, "{stage:?}")?,
            None => formatter.write_str("none")?,
        }
        #[cfg(windows)]
        if let Some(probe) = &self.active_pointer_probe {
            write!(formatter, "{probe}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for FixturePublicationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl StdError for FixturePublicationFailure {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

struct ObservedFixturePublication<T> {
    result: std::result::Result<T, FixturePublicationFailure>,
    stages: Vec<PublicationIoProbe>,
}

impl<T> ObservedFixturePublication<T> {
    fn into_value_or_panic(self) -> T {
        match self.result {
            Ok(value) => value,
            Err(error) => panic!("{error}"),
        }
    }
}

fn observe_fixture_publication<T>(
    attempt: FixturePublicationAttempt,
    root: &Path,
    forced: Option<(PublicationIoProbe, usize)>,
    publish: impl FnOnce() -> Result<T>,
) -> ObservedFixturePublication<T> {
    let stages = Rc::new(RefCell::new(Vec::new()));
    let hook_stages = Rc::clone(&stages);
    let replacement_failure = Rc::new(RefCell::new(None));
    let hook_replacement_failure = Rc::clone(&replacement_failure);
    let mut matching_occurrences = 0;
    let guard = PublicationIoProbeGuard::set(move |stage| {
        if let PublicationIoProbe::AtomicReplacementFailure(probe) = stage {
            hook_replacement_failure.replace(Some(probe));
            return Ok(());
        }
        hook_stages.borrow_mut().push(stage);
        if let Some((forced_stage, forced_occurrence)) = forced {
            if stage == forced_stage {
                matching_occurrences += 1;
                if matching_occurrences == forced_occurrence {
                    return Err(std::io::Error::from_raw_os_error(5));
                }
            }
        }
        Ok(())
    });

    let published = publish();
    drop(guard);
    let stages = Rc::try_unwrap(stages)
        .expect("publication probe stages remained shared")
        .into_inner();
    let replacement_failure = Rc::try_unwrap(replacement_failure)
        .expect("replacement failure probe remained shared")
        .into_inner();
    let last_stage = stages.last().copied();
    ObservedFixturePublication {
        result: published.map_err(|source| {
            FixturePublicationFailure::new(attempt, last_stage, source, root, replacement_failure)
        }),
        stages,
    }
}

fn retained_raw_os_error(source: &anyhow::Error) -> Option<i32> {
    source
        .chain()
        .find_map(|cause| cause.downcast_ref::<ctx_history_index::IndexError>())
        .and_then(publication_io_error)
        .and_then(std::io::Error::raw_os_error)
}

fn format_active_pointer_probe(
    raw_os_error: Option<i32>,
    readonly: Option<bool>,
    replacement: Option<AtomicReplacementFailureProbe>,
) -> String {
    let raw_os_error = raw_os_error
        .or_else(|| replacement.and_then(|probe| probe.move_error))
        .map_or_else(|| "unknown".to_owned(), |raw| raw.to_string());
    let readonly = readonly.map_or_else(|| "unknown".to_owned(), |value| value.to_string());
    let result = |value| match value {
        Some(Ok(())) => "success".to_owned(),
        Some(Err(raw)) => format!("error({raw})"),
        None => "unknown".to_owned(),
    };
    let replacement = replacement.unwrap_or(AtomicReplacementFailureProbe {
        move_error: None,
        source_readonly: None,
        source_delete_open: None,
        parent_delete_child_open: None,
        target_delete_open: None,
        source_cleanup: None,
    });
    let source_readonly = replacement
        .source_readonly
        .map_or_else(|| "unknown".to_owned(), |value| value.to_string());
    let source_delete_open = result(replacement.source_delete_open);
    let parent_delete_child_open = result(replacement.parent_delete_child_open);
    let target_delete_open = result(replacement.target_delete_open);
    let source_cleanup = result(replacement.source_cleanup);
    format!(
        " raw_os_error={raw_os_error} readonly={readonly} source_readonly={source_readonly} \
         source_delete_open={source_delete_open} parent_delete_child_open={parent_delete_child_open} \
         target_delete_open={target_delete_open} source_cleanup={source_cleanup}"
    )
}

#[cfg(windows)]
fn capture_active_pointer_failure(
    root: &Path,
    source: &anyhow::Error,
    replacement: Option<AtomicReplacementFailureProbe>,
) -> String {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;

    let raw_os_error = retained_raw_os_error(source);
    let readonly = root
        .join("active-generation.json")
        .symlink_metadata()
        .ok()
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_READONLY != 0);
    format_active_pointer_probe(raw_os_error, readonly, replacement)
}

#[test]
fn incremental_delta_reopens_catches_up_and_acknowledges_verified_generation() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let root = fixture.data_root.join("index-delta-manifest-identity");
    let initial =
        observe_fixture_publication(FixturePublicationAttempt::Initial, &root, None, || {
            fixture.publish_to_root(&root, "delta-base", &[(0, bodies("base", 1))])
        })
        .into_value_or_panic();
    let initial_generation_id = initial.generation_id().to_owned();
    assert_eq!(initial.manifest().generation_id()?, initial_generation_id);

    let mut store = SemanticVectorStore::open(&fixture.semantic_path, semantic_model_contract())?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    assert!(reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?.ready);
    drop(initial);

    let mut appended = bodies("base", 1);
    appended.push("appended event".to_owned());
    let published =
        observe_fixture_publication(FixturePublicationAttempt::Incremental, &root, None, || {
            fixture.publish_to_root(&root, "delta-next", &[(0, appended)])
        })
        .into_value_or_panic();
    let published_generation_id = published.generation_id().to_owned();
    assert_ne!(
        published.manifest().generation_id()?,
        published_generation_id
    );
    drop(published);

    let reopened = VerifiedIndex::open_pinned(&root)?;
    assert_eq!(reopened.generation_id(), published_generation_id);
    assert_ne!(
        reopened.manifest().generation_id()?,
        reopened.generation_id()
    );
    let generation =
        SourceBackedSemanticGeneration::from_verified_index(&reopened, semantic_model_contract())?;
    assert_eq!(generation.core_generation_id, reopened.generation_id());

    let mut stale = generation.clone();
    stale.core_generation_id = initial_generation_id.clone();
    let error = store
        .reconcile_source_backed_generation(&reopened, &stale, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("source-backed semantic target does not match its pinned Core index"));

    let outcome = reconcile_generation(
        &mut store,
        &reopened,
        &generation,
        &mut builder,
        &mut embedder,
    )?;
    assert!(outcome.ready);
    assert_eq!(outcome.records_decoded, 2);
    assert_eq!(outcome.records_reused, 1);
    assert_eq!(outcome.records_embedded, 1);
    assert_eq!(
        store
            .source_acknowledgement()?
            .expect("incremental semantic acknowledgement")
            .core_generation_id,
        published_generation_id
    );
    assert!(matches!(
        store.source_backed_generation_pin_exact(&initial_generation_id, 1)?,
        SourceBackedGenerationPin::NotReady
    ));
    assert!(matches!(
        store.source_backed_generation_pin_exact(&published_generation_id, 2)?,
        SourceBackedGenerationPin::Ready(_)
    ));
    Ok(())
}

fn atomic_stages(
    artifact: fn(AtomicPublicationStage) -> PublicationIoProbe,
) -> [PublicationIoProbe; 4] {
    [
        artifact(AtomicPublicationStage::Preparation),
        artifact(AtomicPublicationStage::Validation),
        artifact(AtomicPublicationStage::Replacement),
        artifact(AtomicPublicationStage::Synchronization),
    ]
}

fn common_publication_stages() -> Vec<PublicationIoProbe> {
    let stages = vec![
        PublicationIoProbe::CandidateGenerationSync,
        PublicationIoProbe::CertificationSidecar(AtomicPublicationStage::Preparation),
        PublicationIoProbe::CertificationSidecar(AtomicPublicationStage::Validation),
        PublicationIoProbe::CertificationSidecar(AtomicPublicationStage::Replacement),
        PublicationIoProbe::CertificationSidecar(AtomicPublicationStage::Synchronization),
        PublicationIoProbe::ActivePointer(AtomicPublicationStage::Preparation),
        PublicationIoProbe::ActivePointer(AtomicPublicationStage::Validation),
        PublicationIoProbe::ActivePointer(AtomicPublicationStage::Replacement),
        PublicationIoProbe::ActivePointer(AtomicPublicationStage::Synchronization),
    ];
    #[cfg(windows)]
    let stages = {
        let mut stages = stages;
        stages.insert(1, PublicationIoProbe::TerminalSealOpen);
        stages
    };
    stages
}

fn expected_publication_route(attempt: FixturePublicationAttempt) -> Vec<PublicationIoProbe> {
    let other = atomic_stages(PublicationIoProbe::OtherAtomicPublication);
    let mut route = Vec::new();
    route.extend(other);
    if attempt == FixturePublicationAttempt::Initial {
        route.extend(atomic_stages(PublicationIoProbe::CandidateMetadata));
        route.extend(other);
    }
    route.extend(common_publication_stages());
    route
}

fn publication_records(attempt: FixturePublicationAttempt) -> Vec<String> {
    match attempt {
        FixturePublicationAttempt::Initial => bodies("base", 1),
        FixturePublicationAttempt::Incremental => {
            let mut appended = bodies("base", 1);
            appended.push("appended event".to_owned());
            appended
        }
    }
}

fn seed_incremental_attempt(fixture: &Fixture, root: &Path) {
    let initial =
        observe_fixture_publication(FixturePublicationAttempt::Initial, root, None, || {
            fixture.publish_to_root(root, "forced-base", &[(0, bodies("base", 1))])
        })
        .into_value_or_panic();
    drop(initial);
}

fn assert_raw_access_denied(error: &std::io::Error) {
    assert_eq!(error.raw_os_error(), Some(5));
    assert_eq!(error.kind(), std::io::Error::from_raw_os_error(5).kind());
    assert!(StdError::source(error).is_none());
}

fn assert_forced_publication_stages(attempt: FixturePublicationAttempt) -> Result<()> {
    let expected_route = expected_publication_route(attempt);
    let route_fixture = Fixture::new(1)?;
    let route_root = route_fixture
        .data_root
        .join("index-publication-diagnostic-route");
    if attempt == FixturePublicationAttempt::Incremental {
        seed_incremental_attempt(&route_fixture, &route_root);
    }
    let route = observe_fixture_publication(attempt, &route_root, None, || {
        route_fixture.publish_to_root(
            &route_root,
            "route-attempt",
            &[(0, publication_records(attempt))],
        )
    });
    if let Err(error) = &route.result {
        panic!("{error}");
    }
    assert_eq!(route.stages, expected_route);
    if attempt == FixturePublicationAttempt::Incremental {
        assert!(!route
            .stages
            .iter()
            .any(|stage| matches!(stage, PublicationIoProbe::CandidateMetadata(_))));
    }

    for (stage_index, forced_stage) in expected_route.iter().copied().enumerate() {
        let forced_occurrence = expected_route[..=stage_index]
            .iter()
            .filter(|stage| **stage == forced_stage)
            .count();
        let fixture = Fixture::new(1)?;
        let root = fixture
            .data_root
            .join("index-forced-publication-diagnostic");
        if attempt == FixturePublicationAttempt::Incremental {
            seed_incremental_attempt(&fixture, &root);
        }
        let records = publication_records(attempt);
        let observed = observe_fixture_publication(
            attempt,
            &root,
            Some((forced_stage, forced_occurrence)),
            || fixture.publish_to_root(&root, "forced-attempt", &[(0, records)]),
        );

        assert_eq!(observed.stages, expected_route[..=stage_index]);
        let failure = match observed.result {
            Ok(_) => panic!(
                "stage not reached: {forced_stage:?}; observed={:?}",
                observed.stages
            ),
            Err(failure) => failure,
        };
        assert_eq!(failure.attempt, attempt);
        assert_eq!(failure.last_stage, Some(forced_stage));
        let expected_without_probe = format!(
            "Core publication failed: attempt={} last_stage={forced_stage:?}",
            attempt.label()
        );
        #[cfg(windows)]
        let expected = match &failure.active_pointer_probe {
            Some(probe) => format!("{expected_without_probe}{probe}"),
            None => expected_without_probe,
        };
        #[cfg(not(windows))]
        let expected = expected_without_probe;
        assert_eq!(failure.to_string(), expected);
        assert_eq!(format!("{failure:?}"), expected);
        assert!(!expected.contains(&root.display().to_string()));
        assert!(!expected.contains("forced-attempt"));
        assert!(!expected.contains("active-generation.json"));
        #[cfg(windows)]
        assert_eq!(
            failure.active_pointer_probe.is_some(),
            forced_stage == PublicationIoProbe::ActivePointer(AtomicPublicationStage::Replacement)
        );

        if attempt == FixturePublicationAttempt::Initial
            && forced_stage
                == PublicationIoProbe::ActivePointer(AtomicPublicationStage::Replacement)
        {
            let mut replacement = AtomicReplacementFailureProbe {
                move_error: Some(5),
                source_readonly: Some(false),
                source_delete_open: Some(Err(32)),
                parent_delete_child_open: Some(Ok(())),
                target_delete_open: Some(Err(5)),
                source_cleanup: Some(Err(32)),
            };
            assert_eq!(
                format_active_pointer_probe(Some(5), Some(true), Some(replacement)),
                " raw_os_error=5 readonly=true source_readonly=false \
                 source_delete_open=error(32) parent_delete_child_open=success \
                 target_delete_open=error(5) source_cleanup=error(32)"
            );
            assert_eq!(
                format_active_pointer_probe(None, None, None),
                " raw_os_error=unknown readonly=unknown source_readonly=unknown \
                 source_delete_open=unknown parent_delete_child_open=unknown \
                 target_delete_open=unknown source_cleanup=unknown"
            );
            replacement.source_delete_open = Some(Ok(()));
            replacement.target_delete_open = Some(Ok(()));
            replacement.source_cleanup = Some(Ok(()));
            assert!(format_active_pointer_probe(None, None, Some(replacement))
                .ends_with("target_delete_open=success source_cleanup=success"));
        }

        let source = StdError::source(&failure).expect("original production error source");
        assert!(std::ptr::eq(source, failure.source.as_ref()));
        let production = failure
            .source
            .downcast_ref::<ctx_history_index::IndexError>()
            .expect("fixture must retain its typed production error");
        if let Some(error) = publication_io_error(production) {
            assert_raw_access_denied(error);
            assert_eq!(retained_raw_os_error(&failure.source), Some(5));
        } else {
            let ctx_history_index::IndexError::CommittedGenerationNeedsRecovery {
                stage,
                detail,
                ..
            } = production
            else {
                panic!("forced stage returned an unexpected production error kind");
            };
            assert_eq!(
                forced_stage,
                PublicationIoProbe::ActivePointer(AtomicPublicationStage::Synchronization)
            );
            assert_eq!(*stage, "active generation pointer durability");
            assert_eq!(detail, &std::io::Error::from_raw_os_error(5).to_string());
            assert!(StdError::source(production).is_none());
        }
    }
    Ok(())
}

#[test]
fn real_fixture_forced_publication_stages_report_initial_attempt() -> Result<()> {
    assert_forced_publication_stages(FixturePublicationAttempt::Initial)
}

#[test]
fn real_fixture_forced_publication_stages_report_incremental_attempt() -> Result<()> {
    assert_forced_publication_stages(FixturePublicationAttempt::Incremental)
}
