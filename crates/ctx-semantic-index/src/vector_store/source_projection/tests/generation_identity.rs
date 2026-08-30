use super::*;
use std::{cell::RefCell, error::Error as StdError, fmt, rc::Rc};

use ctx_history_index::test_support::{
    publication_io_error, AtomicPublicationStage, PublicationIoProbe, PublicationIoProbeGuard,
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
}

impl fmt::Display for FixturePublicationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Core publication failed: attempt={} last_stage=",
            self.attempt.label()
        )?;
        match self.last_stage {
            Some(stage) => write!(formatter, "{stage:?}"),
            None => formatter.write_str("none"),
        }
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
    forced: Option<(PublicationIoProbe, usize)>,
    publish: impl FnOnce() -> Result<T>,
) -> ObservedFixturePublication<T> {
    let stages = Rc::new(RefCell::new(Vec::new()));
    let hook_stages = Rc::clone(&stages);
    let mut matching_occurrences = 0;
    let guard = PublicationIoProbeGuard::set(move |stage| {
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
    let last_stage = stages.last().copied();
    ObservedFixturePublication {
        result: published.map_err(|source| FixturePublicationFailure {
            attempt,
            last_stage,
            source,
        }),
        stages,
    }
}

#[test]
fn incremental_delta_reopens_catches_up_and_acknowledges_verified_generation() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let root = fixture.data_root.join("index-delta-manifest-identity");
    let initial = observe_fixture_publication(FixturePublicationAttempt::Initial, None, || {
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
        observe_fixture_publication(FixturePublicationAttempt::Incremental, None, || {
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
    let initial = observe_fixture_publication(FixturePublicationAttempt::Initial, None, || {
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
    let route = observe_fixture_publication(attempt, None, || {
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
        let observed =
            observe_fixture_publication(attempt, Some((forced_stage, forced_occurrence)), || {
                fixture.publish_to_root(&root, "forced-attempt", &[(0, records)])
            });

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
        let expected = format!(
            "Core publication failed: attempt={} last_stage={forced_stage:?}",
            attempt.label()
        );
        assert_eq!(failure.to_string(), expected);
        assert_eq!(format!("{failure:?}"), expected);
        assert!(!expected.contains(&root.display().to_string()));
        assert!(!expected.contains("forced-attempt"));

        let source = StdError::source(&failure).expect("original production error source");
        assert!(std::ptr::eq(source, failure.source.as_ref()));
        let production = failure
            .source
            .downcast_ref::<ctx_history_index::IndexError>()
            .expect("fixture must retain its typed production error");
        if let Some(error) = publication_io_error(production) {
            assert_raw_access_denied(error);
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
