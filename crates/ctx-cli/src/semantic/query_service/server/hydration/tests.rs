use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, BatchHydrationResult, EventIdentityInput,
    HydratedProviderRecord, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
};

use crate::semantic::query_service::hydration_budget::SOURCE_HYDRATION_MAX_ITEM_BYTES;

use super::*;

const GENERATION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct FixtureLocator {
    event_id: StableEntityId,
    locator: SourceRecordLocator,
}

fn fixture(lineage: u8, sequence: u64) -> FixtureLocator {
    fixture_with_coordinate(
        lineage,
        sequence,
        "fixture_jsonl",
        NativeRecordCoordinate::ProviderNative {
            namespace: "fixture".to_owned(),
            coordinate: TypedKey::U64(sequence),
        },
    )
}

fn jsonl_fixture(lineage: u8, sequence: u64, byte_length: usize) -> FixtureLocator {
    fixture_with_coordinate(
        lineage,
        sequence,
        "fixture_jsonl",
        NativeRecordCoordinate::Jsonl {
            byte_offset: sequence.saturating_mul(SOURCE_HYDRATION_MAX_ITEM_BYTES as u64),
            byte_length: byte_length as u64,
            physical_ordinal: sequence,
            native_session_key: None,
            native_event_key: None,
        },
    )
}

fn sqlite_fixture(lineage: u8, sequence: u64) -> FixtureLocator {
    fixture_with_coordinate(
        lineage,
        sequence,
        "fixture_sqlite",
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: "fixture_messages".to_owned(),
            primary_key: TypedKey::U64(sequence),
            row_version: Some(TypedKey::U64(sequence)),
        },
    )
}

fn fixture_with_coordinate(
    lineage: u8,
    sequence: u64,
    source_format: &str,
    coordinate: NativeRecordCoordinate,
) -> FixtureLocator {
    let source = SourceKey::derive(
        "codex",
        source_format,
        "fixture",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap();
    let native_session_key = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8(format!("session-{lineage}")).unwrap(),
    )
    .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let locator = SourceRecordLocator::new(
        source,
        coordinate,
        LocatorRevisionPolicy::ExactSourceRevision,
        Some([lineage; 32]),
        [sequence as u8; 32],
    )
    .unwrap();
    FixtureLocator { event_id, locator }
}

fn request(items: &[&FixtureLocator], mode: &str, max_chars: Option<usize>) -> Value {
    json!({
        "schema_version": 1,
        "op": "source_hydrate_batch",
        "generation_id": GENERATION,
        "mode": mode,
        "max_chars": max_chars,
        "items": items.iter().map(|item| json!({
            "event_identity": item.event_id,
            "locator": item.locator,
        })).collect::<Vec<_>>(),
    })
}

fn assert_typed_budget_failure(response: &Value, snapshot: HydrationBudgetSnapshot) {
    assert_eq!(response["ok"], false);
    assert_eq!(response["code"], "hydration_budget_exceeded");
    assert_eq!(response["failure_kind"], "content_too_large");
    assert_eq!(
        response["detail"],
        "source hydration request exceeds the daemon byte budget"
    );
    assert_eq!(response["retryable"], false);
    assert_eq!(response["refresh_scheduled"], false);
    assert!(response.get("generation_id").is_none());
    assert!(response.get("items").is_none());
    assert_eq!(snapshot.in_flight_bytes, 0);
    assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
    assert!(snapshot.cancelled);
    assert!(snapshot.exhausted);
}

fn assert_preflight_budget_failure(response: &Value, snapshot: HydrationBudgetSnapshot) {
    assert_typed_budget_failure(response, snapshot);
    assert_eq!(snapshot.retained_bytes, 0);
    assert_eq!(snapshot.committed_items, 0);
    assert_eq!(snapshot.reservations, 0);
}

#[derive(Default)]
struct MockResolver {
    bodies: HashMap<StableEntityId, Vec<u8>>,
    body_sizes: HashMap<StableEntityId, usize>,
    batch_calls: Mutex<Vec<Vec<StableEntityId>>>,
    failure: Option<HydrationFailure>,
    delayed: bool,
    active: AtomicUsize,
    max_active: AtomicUsize,
    allocated_items: AtomicUsize,
    allocated_bytes: AtomicUsize,
}

impl MockResolver {
    fn with_body(mut self, item: &FixtureLocator, text: impl Into<Vec<u8>>) -> Self {
        self.bodies.insert(item.event_id, text.into());
        self
    }

    fn with_body_size(mut self, item: &FixtureLocator, bytes: usize) -> Self {
        self.body_sizes.insert(item.event_id, bytes);
        self
    }

    fn with_failure(mut self, kind: HydrationFailureKind, detail: &str) -> Self {
        self.failure = Some(HydrationFailure {
            kind,
            detail: detail.to_owned(),
        });
        self
    }

    fn with_delay(mut self) -> Self {
        self.delayed = true;
        self
    }
}

impl ContentSourceResolver for MockResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        if let Some(failure) = self.failure.as_ref() {
            return Err(failure.clone());
        }
        let provider_bytes = if let Some(body) = self.bodies.get(&request.event_id()) {
            body.clone()
        } else if let Some(bytes) = self.body_sizes.get(&request.event_id()).copied() {
            self.allocated_items.fetch_add(1, Ordering::SeqCst);
            self.allocated_bytes.fetch_add(bytes, Ordering::SeqCst);
            vec![b'x'; bytes]
        } else {
            return Err(HydrationFailure {
                kind: HydrationFailureKind::MissingRecord,
                detail: "fixture body is absent".to_owned(),
            });
        };
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes,
        })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
        if self.delayed {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
        }
        self.batch_calls.lock().unwrap().push(
            request
                .events()
                .iter()
                .map(|event| event.event_id())
                .collect(),
        );
        let result = (|| {
            let records = request
                .events()
                .iter()
                .map(|event| self.hydrate_event(event))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let result = BatchHydrationResult::new(records).map_err(|error| HydrationFailure {
                kind: HydrationFailureKind::InvalidLocator,
                detail: error.to_string(),
            })?;
            result.validate_for_request(request)?;
            Ok(result)
        })();
        if self.delayed {
            self.active.fetch_sub(1, Ordering::SeqCst);
        }
        result
    }
}

fn assert_tiny_unknown_batch_succeeds(items: Vec<FixtureLocator>, body_bytes: usize) {
    let expected_ids = items
        .iter()
        .map(|item| item.event_id.as_uuid().to_string())
        .collect::<Vec<_>>();
    let resolver = items
        .iter()
        .fold(MockResolver::default(), |resolver, item| {
            resolver.with_body_size(item, body_bytes)
        });
    let references = items.iter().collect::<Vec<_>>();
    let (response, snapshot) = handle_source_hydration_batch_with_budget(
        &request(&references, "complete", None),
        GENERATION,
        &resolver,
        |_| false,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    );

    assert_eq!(response["ok"], true);
    assert_eq!(
        response["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["event_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected_ids
    );
    assert!(serde_json::to_vec(&response).unwrap().len() <= snapshot.retained_bytes);
    assert_eq!(resolver.allocated_items.load(Ordering::SeqCst), items.len());
    assert_eq!(
        resolver.allocated_bytes.load(Ordering::SeqCst),
        items.len() * body_bytes
    );
    let call_sizes = resolver
        .batch_calls
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|call| call.len())
        .collect::<Vec<_>>();
    assert_eq!(call_sizes.iter().sum::<usize>(), items.len());
    assert!(call_sizes.iter().all(|size| (1..=3).contains(size)));
    assert_eq!(call_sizes.len(), items.len().div_ceil(3));
    assert_eq!(snapshot.reservations, call_sizes.len() + 1);
    assert_eq!(snapshot.committed_items, items.len());
    assert_eq!(snapshot.in_flight_bytes, 0);
    assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
    assert!(!snapshot.cancelled);
    assert!(!snapshot.exhausted);
    eprintln!(
        "tiny unknown-size hydration evidence: items={}, calls={}, snapshot={snapshot:?}",
        items.len(),
        call_sizes.len(),
    );
}

#[test]
fn source_hydration_groups_by_exact_source_and_restores_request_order() {
    let first = fixture(1, 1);
    let second_source = fixture(2, 2);
    let third = fixture(1, 3);
    let resolver = MockResolver::default()
        .with_body(&first, "first")
        .with_body(&second_source, "second")
        .with_body(&third, "third");

    let response = handle_source_hydration_batch_with(
        &request(&[&second_source, &first, &third], "complete", None),
        GENERATION,
        &resolver,
        |_| false,
    );

    assert_eq!(response["ok"], true);
    assert_eq!(
        response["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["event_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            second_source.event_id.as_uuid().to_string(),
            first.event_id.as_uuid().to_string(),
            third.event_id.as_uuid().to_string(),
        ]
    );
    assert_eq!(
        response["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["second", "first", "third"]
    );
    let mut call_sizes = resolver
        .batch_calls
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|call| call.len())
        .collect::<Vec<_>>();
    call_sizes.sort_unstable();
    assert_eq!(call_sizes, vec![1, 2]);
}

#[test]
fn source_search_hydration_truncates_exact_content_by_character() {
    let item = fixture(3, 1);
    let resolver = MockResolver::default().with_body(&item, "αβγδ");
    let response = handle_source_hydration_batch_with(
        &request(&[&item], "search_display", Some(3)),
        GENERATION,
        &resolver,
        |_| false,
    );

    assert_eq!(response["ok"], true);
    assert_eq!(response["items"][0]["text"], "αβγ");
}

#[test]
fn source_hydration_mixed_sqlite_jsonl_workers_use_bounded_phases() {
    let mut items = (1..=3)
        .map(|lineage| sqlite_fixture(lineage, 1))
        .collect::<Vec<_>>();
    items.extend((4..=9).map(|lineage| jsonl_fixture(lineage, 1, 64)));
    let resolver = items
        .iter()
        .fold(MockResolver::default().with_delay(), |resolver, item| {
            resolver.with_body(item, format!("source {}", item.event_id))
        });
    let references = items.iter().collect::<Vec<_>>();
    let (response, snapshot) = handle_source_hydration_batch_with_budget(
        &request(&references, "complete", None),
        GENERATION,
        &resolver,
        |_| false,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    );

    assert_eq!(response["ok"], true);
    assert!((2..=DAEMON_SOURCE_HYDRATION_MAX_WORKERS)
        .contains(&resolver.max_active.load(Ordering::SeqCst)));
    assert_eq!(resolver.batch_calls.into_inner().unwrap().len(), 9);
    assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
    assert_eq!(snapshot.committed_items, 9);
    assert_eq!(snapshot.reservations, 2);
}

#[test]
fn source_hydration_unknown_size_workers_share_three_item_waves() {
    let items = (1..=9)
        .map(|lineage| fixture(lineage, 1))
        .collect::<Vec<_>>();
    let resolver = items
        .iter()
        .fold(MockResolver::default().with_delay(), |resolver, item| {
            resolver.with_body(item, format!("source {}", item.event_id))
        });
    let references = items.iter().collect::<Vec<_>>();
    let (response, snapshot) = handle_source_hydration_batch_with_budget(
        &request(&references, "complete", None),
        GENERATION,
        &resolver,
        |_| false,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    );

    assert_eq!(response["ok"], true);
    assert!((2..=3).contains(&resolver.max_active.load(Ordering::SeqCst)));
    assert_eq!(resolver.batch_calls.into_inner().unwrap().len(), 9);
    assert_eq!(snapshot.reservations, 4);
    assert_eq!(snapshot.committed_items, 9);
    assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
}

#[test]
fn source_hydration_20_tiny_native_and_sqlite_items_succeed_in_bounded_waves() {
    let native = (0..20)
        .map(|sequence| fixture(30, sequence))
        .collect::<Vec<_>>();
    assert_tiny_unknown_batch_succeeds(native, 32);

    let sqlite = (0..20)
        .map(|sequence| sqlite_fixture(31, sequence))
        .collect::<Vec<_>>();
    assert_tiny_unknown_batch_succeeds(sqlite, 32);
}

#[test]
fn source_hydration_128_tiny_native_and_sqlite_items_succeed_in_bounded_waves() {
    let native = (0..DAEMON_SOURCE_HYDRATION_MAX_ITEMS as u64)
        .map(|sequence| fixture(32, sequence))
        .collect::<Vec<_>>();
    assert_tiny_unknown_batch_succeeds(native, 32);

    let sqlite = (0..DAEMON_SOURCE_HYDRATION_MAX_ITEMS as u64)
        .map(|sequence| sqlite_fixture(33, sequence))
        .collect::<Vec<_>>();
    assert_tiny_unknown_batch_succeeds(sqlite, 32);
}

#[test]
fn source_hydration_without_resident_generation_is_typed_and_queues_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let item = fixture(10, 1);
    let coordinator = SourceBackedRefreshCoordinator::new();
    let response = handle_source_hydration_batch(
        temp.path(),
        &coordinator,
        &request(&[&item], "complete", None),
    );

    assert_eq!(response["ok"], false);
    assert_eq!(response["code"], "resolver_generation_unavailable");
    assert_eq!(response["failure_kind"], "temporarily_unavailable");
    assert_eq!(response["refresh_scheduled"], true);
    assert!(coordinator.has_pending_request());
}

#[test]
fn source_hydration_preserves_typed_stale_failure_and_refresh_signal() {
    let item = fixture(4, 1);
    let refresh_called = AtomicBool::new(false);
    let resolver = MockResolver::default().with_failure(
        HydrationFailureKind::StaleSourceEvidence,
        "fixture source revision changed",
    );
    let response = handle_source_hydration_batch_with(
        &request(&[&item], "complete", None),
        GENERATION,
        &resolver,
        |_| {
            refresh_called.store(true, Ordering::Relaxed);
            true
        },
    );

    assert_eq!(response["ok"], false);
    assert_eq!(response["code"], "source_hydration_failed");
    assert_eq!(response["failure_kind"], "stale_source_evidence");
    assert_eq!(response["refresh_scheduled"], true);
    assert!(refresh_called.load(Ordering::Relaxed));
}

#[test]
fn source_hydration_rejects_generation_mismatch_before_resolver_access() {
    let item = fixture(5, 1);
    let resolver = MockResolver::default().with_body(&item, "body");
    let response = handle_source_hydration_batch_with(
        &request(&[&item], "complete", None),
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        &resolver,
        |_| false,
    );

    assert_eq!(response["ok"], false);
    assert_eq!(response["code"], "resolver_generation_mismatch");
    assert_eq!(response["failure_kind"], "stale_source_evidence");
    assert!(resolver.batch_calls.into_inner().unwrap().is_empty());
}

#[test]
fn source_hydration_rejects_empty_content_instead_of_emitting_a_placeholder() {
    let item = fixture(6, 1);
    let resolver = MockResolver::default().with_body(&item, Vec::new());
    let response = handle_source_hydration_batch_with(
        &request(&[&item], "complete", None),
        GENERATION,
        &resolver,
        |_| false,
    );

    assert_eq!(response["ok"], false);
    assert_eq!(response["failure_kind"], "missing_record");
}

#[test]
fn source_hydration_ordinary_jsonl_and_sqlite_preserves_results_with_wave_counter() {
    let jsonl = (0..64)
        .map(|sequence| jsonl_fixture(20, sequence, 64))
        .collect::<Vec<_>>();
    let sqlite = (0..64)
        .map(|sequence| sqlite_fixture(21, sequence))
        .collect::<Vec<_>>();
    let mut ordered = Vec::with_capacity(128);
    for index in 0..64 {
        ordered.push(&jsonl[index]);
        ordered.push(&sqlite[index]);
    }
    let resolver = ordered
        .iter()
        .enumerate()
        .fold(MockResolver::default(), |resolver, (index, item)| {
            resolver.with_body(item, format!("ordinary-{index}"))
        });
    let (response, snapshot) = handle_source_hydration_batch_with_budget(
        &request(&ordered, "complete", None),
        GENERATION,
        &resolver,
        |_| false,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    );

    assert_eq!(response["ok"], true);
    assert_eq!(response["items"].as_array().unwrap().len(), 128);
    assert!(serde_json::to_vec(&response).unwrap().len() <= snapshot.retained_bytes);
    assert_eq!(
        response["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        (0..128)
            .map(|index| format!("ordinary-{index}"))
            .collect::<Vec<_>>()
    );
    let call_sizes = resolver
        .batch_calls
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|call| call.len())
        .collect::<Vec<_>>();
    assert!(call_sizes.contains(&64));
    let sqlite_call_sizes = call_sizes
        .iter()
        .copied()
        .filter(|size| *size <= 3)
        .collect::<Vec<_>>();
    assert_eq!(sqlite_call_sizes.iter().sum::<usize>(), 64);
    assert_eq!(sqlite_call_sizes.len(), 64_usize.div_ceil(3));
    assert!(sqlite_call_sizes.iter().all(|size| (1..=3).contains(size)));
    assert_eq!(snapshot.committed_items, 128);
    assert_eq!(snapshot.reservations, 23);
    assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
    assert_eq!(snapshot.in_flight_bytes, 0);
    eprintln!(
        "same-source SQLite wave receipt: items=64, resolver_calls={}, call_sizes={sqlite_call_sizes:?}, snapshot={snapshot:?}",
        sqlite_call_sizes.len(),
    );
}

#[test]
fn source_hydration_jsonl_reservation_allows_certified_record_framing() {
    let item = jsonl_fixture(25, 0, SOURCE_HYDRATION_MAX_ITEM_BYTES + 2);
    let resolver = MockResolver::default().with_body(&item, "framed");
    let (response, snapshot) = handle_source_hydration_batch_with_budget(
        &request(&[&item], "complete", None),
        GENERATION,
        &resolver,
        |_| false,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    );

    assert_eq!(response["ok"], true);
    assert_eq!(response["items"][0]["text"], "framed");
    assert_eq!(snapshot.committed_items, 1);
    assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
}

#[test]
fn source_hydration_exact_read_boundary_passes_and_next_byte_never_reaches_provider() {
    let exact = jsonl_fixture(26, 0, 100);
    let exact_request = EventHydrationRequest::new(exact.event_id, exact.locator.clone()).unwrap();
    let budget_limit = successful_response_envelope_charge(GENERATION).unwrap()
        + retained_response_items_metadata_charge(1).unwrap()
        + provider_read_reservation_bytes(&exact_request, 0).unwrap();
    let exact_resolver = MockResolver::default().with_body(&exact, "x");
    let (exact_response, exact_snapshot) = handle_source_hydration_batch_with_budget(
        &request(&[&exact], "complete", None),
        GENERATION,
        &exact_resolver,
        |_| false,
        budget_limit,
    );

    assert_eq!(exact_response["ok"], true);
    assert_eq!(exact_snapshot.peak_bytes, budget_limit);
    assert_eq!(exact_snapshot.reservations, 1);
    assert_eq!(exact_snapshot.committed_items, 1);
    assert_eq!(exact_resolver.batch_calls.into_inner().unwrap().len(), 1);

    let over = jsonl_fixture(27, 0, 101);
    let over_resolver = MockResolver::default().with_body_size(&over, 1);
    let (over_response, over_snapshot) = handle_source_hydration_batch_with_budget(
        &request(&[&over], "complete", None),
        GENERATION,
        &over_resolver,
        |_| false,
        budget_limit,
    );

    assert_preflight_budget_failure(&over_response, over_snapshot);
    assert!(over_resolver.batch_calls.into_inner().unwrap().is_empty());
    assert_eq!(over_resolver.allocated_items.load(Ordering::SeqCst), 0);
    assert_eq!(over_resolver.allocated_bytes.load(Ordering::SeqCst), 0);
}

#[test]
fn source_hydration_valid_policy_near_limit_bounds_value_and_transport_coexistence() {
    let probe = jsonl_fixture(28, 0, 1);
    let probe_request = EventHydrationRequest::new(probe.event_id, probe.locator.clone()).unwrap();
    let read_overhead = provider_read_reservation_bytes(&probe_request, 0).unwrap() - 1;
    let envelope_charge = successful_response_envelope_charge(GENERATION).unwrap();
    let metadata_charge = retained_response_items_metadata_charge(4).unwrap();
    let body_bytes =
        (DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES - envelope_charge - metadata_charge) / 4
            - read_overhead;
    let items = (0..4)
        .map(|sequence| jsonl_fixture(28, sequence, body_bytes))
        .collect::<Vec<_>>();
    let resolver = items
        .iter()
        .fold(MockResolver::default(), |resolver, item| {
            resolver.with_body_size(item, body_bytes)
        });
    let references = items.iter().collect::<Vec<_>>();
    let expected_reservation =
        items
            .iter()
            .try_fold(envelope_charge + metadata_charge, |total, item| {
                let request =
                    EventHydrationRequest::new(item.event_id, item.locator.clone()).unwrap();
                total.checked_add(provider_read_reservation_bytes(&request, 0).unwrap())
            });
    let expected_reservation = expected_reservation.unwrap();
    let (response, snapshot) = handle_source_hydration_batch_with_budget(
        &request(&references, "complete", None),
        GENERATION,
        &resolver,
        |_| false,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    );

    assert_eq!(response["ok"], true);
    assert_eq!(snapshot.peak_bytes, expected_reservation);
    assert!(
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES - snapshot.peak_bytes < 4,
        "{snapshot:?}"
    );
    assert_eq!(snapshot.reservations, 1);
    assert_eq!(snapshot.committed_items, 4);
    assert_eq!(resolver.allocated_items.load(Ordering::SeqCst), 4);
    assert_eq!(
        resolver.allocated_bytes.load(Ordering::SeqCst),
        body_bytes * 4
    );
    assert_eq!(resolver.batch_calls.into_inner().unwrap().len(), 1);

    let transport = serde_json::to_string(&response).unwrap();
    assert!(transport.len() <= DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES);
    assert!(transport.capacity() <= DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES);
    assert!(transport.len() <= snapshot.retained_bytes);
    let coexistence_peak = snapshot
        .retained_bytes
        .checked_add(transport.capacity())
        .unwrap();
    assert!(coexistence_peak <= DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES.saturating_mul(2));
    eprintln!(
        "near-limit serialization evidence: hydration={snapshot:?}, transport_len={}, transport_capacity={}, coexistence_peak={coexistence_peak}",
        transport.len(),
        transport.capacity(),
    );
}

#[test]
fn source_hydration_128_near_limit_jsonl_items_fail_before_resolver_work() {
    let body_bytes = SOURCE_HYDRATION_MAX_ITEM_BYTES - 4 * 1024;
    let items = (0..DAEMON_SOURCE_HYDRATION_MAX_ITEMS as u64)
        .map(|sequence| jsonl_fixture(22, sequence, body_bytes))
        .collect::<Vec<_>>();
    let resolver = items
        .iter()
        .fold(MockResolver::default(), |resolver, item| {
            resolver.with_body_size(item, body_bytes)
        });
    let references = items.iter().collect::<Vec<_>>();
    let (response, snapshot) = handle_source_hydration_batch_with_budget(
        &request(&references, "complete", None),
        GENERATION,
        &resolver,
        |_| false,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    );

    assert_preflight_budget_failure(&response, snapshot);
    assert_eq!(resolver.allocated_items.load(Ordering::SeqCst), 0);
    assert_eq!(resolver.allocated_bytes.load(Ordering::SeqCst), 0);
    assert!(resolver.batch_calls.into_inner().unwrap().is_empty());
    eprintln!("jsonl near-limit budget evidence: {snapshot:?}");
}

#[test]
fn source_hydration_unknown_huge_sqlite_items_stop_before_the_next_wave() {
    let body_bytes = 10 * 1024 * 1024;
    let items = (0..DAEMON_SOURCE_HYDRATION_MAX_ITEMS as u64)
        .map(|sequence| sqlite_fixture(23, sequence))
        .collect::<Vec<_>>();
    let resolver = items
        .iter()
        .fold(MockResolver::default(), |resolver, item| {
            resolver.with_body_size(item, body_bytes)
        });
    let references = items.iter().collect::<Vec<_>>();
    let (response, snapshot) = handle_source_hydration_batch_with_budget(
        &request(&references, "complete", None),
        GENERATION,
        &resolver,
        |_| false,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    );

    assert_typed_budget_failure(&response, snapshot);
    assert_eq!(resolver.allocated_items.load(Ordering::SeqCst), 5);
    assert_eq!(
        resolver.allocated_bytes.load(Ordering::SeqCst),
        body_bytes * 5
    );
    let calls = resolver.batch_calls.into_inner().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].len(), 3);
    assert_eq!(calls[1].len(), 2);
    assert_eq!(snapshot.committed_items, 5);
    assert_eq!(snapshot.reservations, 3);
    assert!(snapshot.retained_bytes > body_bytes * 4);
    eprintln!("unknown huge-item budget evidence: {snapshot:?}");
}

#[test]
fn source_hydration_mixed_small_and_huge_items_fail_before_resolver_work() {
    let huge_bytes = SOURCE_HYDRATION_MAX_ITEM_BYTES - 4 * 1024;
    let mut items = Vec::with_capacity(DAEMON_SOURCE_HYDRATION_MAX_ITEMS);
    items.push(jsonl_fixture(24, 0, 32));
    for sequence in 1..=4 {
        items.push(jsonl_fixture(24, sequence, huge_bytes));
    }
    for sequence in 5..DAEMON_SOURCE_HYDRATION_MAX_ITEMS as u64 {
        items.push(jsonl_fixture(24, sequence, 32));
    }
    let resolver =
        items
            .iter()
            .enumerate()
            .fold(MockResolver::default(), |resolver, (index, item)| {
                resolver.with_body_size(
                    item,
                    if (1..=4).contains(&index) {
                        huge_bytes
                    } else {
                        32
                    },
                )
            });
    let references = items.iter().collect::<Vec<_>>();
    let (response, snapshot) = handle_source_hydration_batch_with_budget(
        &request(&references, "complete", None),
        GENERATION,
        &resolver,
        |_| false,
        DAEMON_SOURCE_HYDRATION_MAX_RESPONSE_BYTES,
    );

    assert_preflight_budget_failure(&response, snapshot);
    assert_eq!(resolver.allocated_items.load(Ordering::SeqCst), 0);
    assert_eq!(resolver.allocated_bytes.load(Ordering::SeqCst), 0);
    assert!(resolver.batch_calls.into_inner().unwrap().is_empty());
    eprintln!("mixed-size budget evidence: {snapshot:?}");
}
