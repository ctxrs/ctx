use std::collections::HashSet;

use super::*;

#[derive(Clone, Default)]
struct FakeLookup {
    event_ids: HashSet<Uuid>,
}

#[derive(Debug, thiserror::Error)]
#[error("fake lookup failed")]
struct FakeLookupError;

#[derive(Clone)]
struct FakePreparationPort;

#[derive(Debug)]
struct FakePrepared {
    encoded_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("fake preparation failed")]
struct FakePreparationError;

impl CorePreparationPort for FakePreparationPort {
    type Prepared = FakePrepared;
    type Draft = ();
    type Failure = FakePreparationError;

    fn prepare(&self, _record: CoreRecord) -> Result<Self::Prepared, Self::Failure> {
        Err(FakePreparationError)
    }

    fn prepare_draft(&self, _record: CoreRecord) -> Result<Self::Draft, Self::Failure> {
        Err(FakePreparationError)
    }

    fn materialize_draft(
        &self,
        _draft: Self::Draft,
        _maximum_encoded_bytes: usize,
    ) -> Result<CoreMaterialization<Self::Prepared, Self::Draft>, Self::Failure> {
        Err(FakePreparationError)
    }

    fn prepared_source<'a>(&self, _prepared: &'a Self::Prepared) -> &'a SourceKey {
        panic!("the fake batch test does not inspect source identity")
    }

    fn encoded_bytes(&self, prepared: &Self::Prepared) -> usize {
        prepared.encoded_bytes
    }

    fn failure_kind(&self, _failure: &Self::Failure) -> CorePreparationFailureKind {
        CorePreparationFailureKind::InvalidSource
    }
}

impl BaseEventLookup for FakeLookup {
    type Error = FakeLookupError;

    fn contains(&self, event_id: Uuid) -> Result<bool, Self::Error> {
        Ok(self.event_ids.contains(&event_id))
    }
}

fn lookup_contains<L: BaseEventLookup>(lookup: &L, event_id: Uuid) -> Result<bool, L::Error> {
    lookup.contains(event_id)
}

#[test]
fn fake_lookup_is_static_generic_and_exact() {
    let present = Uuid::new_v4();
    let absent = Uuid::new_v4();
    let lookup = FakeLookup {
        event_ids: HashSet::from([present]),
    };

    assert!(lookup_contains(&lookup, present).unwrap());
    assert!(!lookup_contains(&lookup, absent).unwrap());
}

#[test]
fn neutral_commit_debug_does_not_require_a_debug_verified_pin() {
    struct OpaqueVerifiedPin;

    let receipt = CaptureCommitReceipt::new("generation".to_owned(), 7, 3, 2, 11, ());
    let outcome = CaptureCommitOutcome::new(
        receipt,
        CapturePublicationDisposition::Reused,
        VerifiedCapture::new(OpaqueVerifiedPin),
    );

    let debug = format!("{outcome:?}");
    assert!(debug.contains("CaptureCommitOutcome"));
    assert!(debug.contains("VerifiedCapture(..)"));

    let (receipt, disposition, verified) = outcome.into_parts();
    assert_eq!(disposition, CapturePublicationDisposition::Reused);
    assert_eq!(receipt.into_parts().0, "generation");
    let _opaque = verified.into_inner();
}

#[test]
fn route_output_budget_is_live_without_a_cumulative_cap() {
    let resources = CoreRouteResources::for_test(2, 9, 20);
    let first = resources
        .reserve(CoreRouteResourceKind::CoreOutput, 5)
        .unwrap();
    assert_eq!(resources.live_bytes(CoreRouteResourceKind::CoreOutput), 5);
    drop(first);
    let second = resources
        .reserve(CoreRouteResourceKind::CoreOutput, 5)
        .unwrap();
    drop(second);
    assert_eq!(resources.live_bytes(CoreRouteResourceKind::CoreOutput), 0);
}

#[test]
fn cloned_workers_share_one_live_output_budget_exactly_one_over() {
    let resources = CoreRouteResources::for_test(4, 9, 20);
    let first = resources
        .reserve(CoreRouteResourceKind::CoreOutput, 5)
        .unwrap();
    let error = resources
        .clone()
        .reserve(CoreRouteResourceKind::CoreOutput, 5)
        .unwrap_err();
    assert_eq!(
        error,
        CoreRouteResourceError::Unavailable {
            kind: CoreRouteResourceKind::CoreOutput,
            maximum: 9,
            observed: 10,
        }
    );
    drop(first);
    resources
        .reserve(CoreRouteResourceKind::CoreOutput, 5)
        .unwrap();
}

#[test]
fn physical_scratch_has_a_separate_exact_aggregate_limit() {
    let resources = CoreRouteResources::for_test(4, 3, 9);
    let first = resources
        .reserve(CoreRouteResourceKind::LogicalSourceScratch, 5)
        .unwrap();
    let error = resources
        .clone()
        .reserve(CoreRouteResourceKind::LogicalSourceScratch, 5)
        .unwrap_err();
    assert_eq!(
        error,
        CoreRouteResourceError::Unavailable {
            kind: CoreRouteResourceKind::LogicalSourceScratch,
            maximum: 9,
            observed: 10,
        }
    );
    assert_eq!(resources.live_bytes(CoreRouteResourceKind::CoreOutput), 0);
    drop(first);
    assert_eq!(
        resources.live_bytes(CoreRouteResourceKind::LogicalSourceScratch),
        0
    );
}

#[test]
fn generic_batch_uses_one_vec_and_releases_its_shared_lease() {
    let resources = CoreRouteResources::for_test(1, 9, 1);
    let port = FakePreparationPort;
    let mut builder = CorePreparedBatchBuilder::<FakePreparationPort>::default();
    builder.reserve_bytes(9, &resources).unwrap();
    builder
        .push(FakePrepared { encoded_bytes: 4 }, &port)
        .unwrap();
    builder
        .push(FakePrepared { encoded_bytes: 5 }, &port)
        .unwrap();
    assert_eq!(resources.live_bytes(CoreRouteResourceKind::CoreOutput), 9);

    let batch = builder.take_batch().unwrap().unwrap();
    assert_eq!(batch.len(), 2);
    drop(batch);
    assert_eq!(resources.live_bytes(CoreRouteResourceKind::CoreOutput), 0);
}
