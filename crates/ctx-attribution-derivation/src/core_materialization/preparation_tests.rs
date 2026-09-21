use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use super::preparation::{
    PreparedOutputBudget, checked_prepared_output_bytes, collect_prepared_page_units,
    collect_prepared_units, configured_core_preparation_workers,
    ordered_parallel_for_each_with_budget, ordered_parallel_map, ordered_parallel_map_owned,
};
use super::*;

fn assert_sync<T: Sync>() {}

fn unit(origin_event_id: &str) -> PreparedCoreUnit {
    PreparedCoreUnit {
        origin_event_id: origin_event_id.to_owned(),
        producer_authority_disposition: ProducerAuthorityDisposition::EligibleUnique,
        stable_entities: Vec::new(),
        facts: Vec::new(),
        evidence: None,
        coverage: CoreProjectionCoverage::default(),
    }
}

#[test]
fn projection_preparer_is_sync_and_parallelism_is_bounded() -> Result<(), String> {
    assert_sync::<CoreProjectionPreparer>();
    for parallelism in [1, 4, MAX_CORE_PREPARATION_PARALLELISM] {
        let preparer = CoreProjectionPreparer::with_parallelism(parallelism)
            .map_err(|error| error.message.clone())?;
        assert_eq!(preparer.parallelism(), parallelism);
    }

    let zero = CoreProjectionPreparer::with_parallelism(0)
        .err()
        .ok_or_else(|| "zero parallelism unexpectedly succeeded".to_owned())?;
    assert_eq!(zero.class, ErrorClass::InvalidRequest);
    let excessive = CoreProjectionPreparer::with_parallelism(MAX_CORE_PREPARATION_PARALLELISM + 1)
        .err()
        .ok_or_else(|| "excessive parallelism unexpectedly succeeded".to_owned())?;
    assert_eq!(excessive.class, ErrorClass::Bounds);
    Ok(())
}

#[test]
fn concurrent_default_initialization_returns_one_bounded_pool() -> Result<(), String> {
    const CALLERS: usize = 32;
    let barrier = Arc::new(Barrier::new(CALLERS));
    let handles = (0..CALLERS)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let preparer =
                    CoreProjectionPreparer::new().map_err(|error| error.message.clone())?;
                Ok::<_, String>((
                    Arc::as_ptr(&preparer.inner) as usize,
                    preparer.parallelism(),
                ))
            })
        })
        .collect::<Vec<_>>();
    let initialized = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .map_err(|_| "default preparer initialization panicked".to_owned())?
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (first_pointer, first_parallelism) = initialized[0];
    assert!((1..=MAX_CONFIGURED_CORE_PREPARATION_WORKERS).contains(&first_parallelism));
    assert!(initialized.iter().all(
        |(pointer, parallelism)| *pointer == first_pointer && *parallelism == first_parallelism
    ));
    Ok(())
}

#[test]
fn configured_worker_budget_is_canonical_clamped_and_zero_safe() {
    assert_eq!(configured_core_preparation_workers(None, 64).unwrap(), 1);
    for (requested, expected) in [
        ("0", 1),
        ("1", 1),
        ("2", 2),
        ("4", 4),
        ("8", 8),
        ("16", 16),
        ("32", 16),
    ] {
        assert_eq!(
            configured_core_preparation_workers(Some(requested.into()), 64).unwrap(),
            expected,
            "requested={requested}"
        );
    }
    assert_eq!(
        configured_core_preparation_workers(Some("16".into()), 3).unwrap(),
        3
    );

    let overflow = format!("{}0", usize::MAX);
    for invalid in ["", "+1", "-1", " 1", "1 ", "01", "00", &overflow] {
        let error = configured_core_preparation_workers(Some(invalid.into()), 64)
            .expect_err("noncanonical worker budget");
        assert_eq!(error.class, ErrorClass::InvalidRequest, "value={invalid:?}");
    }
}

#[cfg(unix)]
#[test]
fn configured_worker_budget_rejects_non_utf8() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let error = configured_core_preparation_workers(Some(OsString::from_vec(vec![b'1', 0xff])), 64)
        .expect_err("non-UTF-8 worker budget");
    assert_eq!(error.class, ErrorClass::InvalidRequest);
    assert!(error.message.contains("valid UTF-8"));
}

#[test]
fn measured_worker_peak_obeys_every_requested_helper_budget() -> Result<(), String> {
    for requested in [1, 2, 4, 8, 16, 32] {
        let effective = configured_core_preparation_workers(Some(requested.to_string().into()), 64)
            .map_err(|error| error.message)?;
        let preparer =
            CoreProjectionPreparer::with_parallelism(effective).map_err(|error| error.message)?;
        let materialization_id = format!("measurement-{requested}");
        preparer
            .start_measurement(materialization_id.clone())
            .map_err(|error| error.message)?;
        let barrier = Arc::new(Barrier::new(effective));
        let input = (0..effective).collect::<Vec<_>>();
        let output = ordered_parallel_map(&preparer.inner.pool, &input, |value| {
            let _worker = preparer.inner.workers.acquire().expect("worker permit");
            let _active = preparer
                .inner
                .budget
                .enter_preparation()
                .expect("activity guard");
            barrier.wait();
            *value
        });
        assert_eq!(output, input);
        let finish_phase = preparer
            .finish_measurement(&materialization_id)
            .map_err(|error| error.message)?;
        let peak = preparer.peak_workers().map_err(|error| error.message)?;
        assert_eq!(usize::from(peak), effective, "requested={requested}");
        assert!(usize::from(peak) <= effective, "requested={requested}");
        drop(finish_phase);
        preparer
            .start_measurement(format!("next-{requested}"))
            .map_err(|error| error.message)?;
        assert_eq!(preparer.peak_workers().map_err(|error| error.message)?, 0);
    }
    Ok(())
}

#[test]
fn parallel_map_runs_concurrently_and_preserves_input_order() -> Result<(), String> {
    let preparer =
        CoreProjectionPreparer::with_parallelism(4).map_err(|error| error.message.clone())?;
    let input = [0_u64, 1, 2, 3];
    let barrier = Arc::new(Barrier::new(input.len()));
    let output = ordered_parallel_map(&preparer.inner.pool, &input, |value| {
        barrier.wait();
        *value
    });
    assert_eq!(output, input);
    Ok(())
}

#[test]
fn canonical_page_parallel_map_obeys_worker_page_and_protocol_caps() -> Result<(), String> {
    for (workers, pages, expected_peak) in [(4, 16, 4), (32, 3, 3), (32, 16, 16)] {
        let preparer = CoreProjectionPreparer::with_parallelism(workers)
            .map_err(|error| error.message.clone())?;
        let active = AtomicUsize::new(0);
        let maximum_active = AtomicUsize::new(0);
        let barrier = Barrier::new(expected_peak);
        let input = (0..pages).collect::<Vec<_>>();
        let output = ordered_parallel_map_owned(
            &preparer,
            input.clone(),
            crate::protocol::MAX_CORE_EVENT_DELTA_PAGES,
            |value| {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum_active.fetch_max(current, Ordering::SeqCst);
                barrier.wait();
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(value)
            },
        )
        .map_err(|error| error.message)?;
        assert_eq!(output, input, "workers={workers}, pages={pages}");
        assert_eq!(
            maximum_active.load(Ordering::SeqCst),
            expected_peak,
            "workers={workers}, pages={pages}"
        );
        assert_eq!(
            usize::from(preparer.peak_workers().map_err(|error| error.message)?),
            expected_peak,
            "workers={workers}, pages={pages}"
        );
    }
    Ok(())
}

#[test]
fn canonical_page_parallel_map_selects_the_lowest_index_error() -> Result<(), String> {
    for workers in [1, 2, 4, 8, 16, 32] {
        let preparer = CoreProjectionPreparer::with_parallelism(workers)
            .map_err(|error| error.message.clone())?;
        let later_error_completed = AtomicBool::new(false);
        let barrier = Barrier::new(2);
        let error = ordered_parallel_map_owned(
            &preparer,
            vec![0_usize, 1],
            crate::protocol::MAX_CORE_EVENT_DELTA_PAGES,
            |value| -> Result<usize, ProtocolError> {
                if value == 0 {
                    if workers > 1 {
                        barrier.wait();
                        while !later_error_completed.load(Ordering::Acquire) {
                            std::thread::yield_now();
                        }
                    }
                    Err(ProtocolError::new(
                        ErrorClass::Internal,
                        "lowest-index failure",
                    ))
                } else {
                    if workers > 1 {
                        barrier.wait();
                    }
                    later_error_completed.store(true, Ordering::Release);
                    Err(ProtocolError::new(
                        ErrorClass::Sequence,
                        "earlier-completing higher-index failure",
                    ))
                }
            },
        )
        .expect_err("canonical page map should fail");
        assert_eq!(error.class, ErrorClass::Internal, "workers={workers}");
        assert_eq!(error.message, "lowest-index failure", "workers={workers}");
    }
    Ok(())
}

#[test]
fn page_errors_follow_delta_order_including_duplicate_detection() -> Result<(), String> {
    let later_preparation_error = ProtocolError::new(ErrorClass::Internal, "later failure");
    let duplicate_first = collect_prepared_units(vec![
        Ok(Some(unit("duplicate"))),
        Ok(Some(unit("duplicate"))),
        Err(later_preparation_error),
    ])
    .err()
    .ok_or_else(|| "duplicate page unexpectedly succeeded".to_owned())?;
    assert_eq!(duplicate_first.class, ErrorClass::Sequence);

    let earlier_preparation_error = ProtocolError::new(ErrorClass::Internal, "earlier failure");
    let preparation_first = collect_prepared_units(vec![
        Err(earlier_preparation_error),
        Ok(Some(unit("duplicate"))),
        Ok(Some(unit("duplicate"))),
    ])
    .err()
    .ok_or_else(|| "failed page unexpectedly succeeded".to_owned())?;
    assert_eq!(preparation_first.class, ErrorClass::Internal);
    assert_eq!(preparation_first.message, "earlier failure");
    Ok(())
}

#[test]
fn credited_parallel_map_preserves_order_and_caps_active_work() -> Result<(), String> {
    let preparer =
        CoreProjectionPreparer::with_parallelism(32).map_err(|error| error.message.clone())?;
    let input = (0_usize..64).collect::<Vec<_>>();
    let active = AtomicUsize::new(0);
    let maximum_active = AtomicUsize::new(0);
    let mut output = Vec::new();
    let mut budget = PreparedOutputBudget::new(0).map_err(|error| error.message.clone())?;
    ordered_parallel_for_each_with_budget(
        &preparer.inner.pool,
        &input,
        &preparer.inner.credits,
        &mut budget,
        |value| {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum_active.fetch_max(current, Ordering::SeqCst);
            if *value == 0 {
                std::thread::sleep(Duration::from_millis(25));
            } else {
                std::thread::yield_now();
            }
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(*value)
        },
        |_, value, budget| {
            budget.retain(1)?;
            output.push(value);
            Ok(())
        },
    )
    .map_err(|error| error.message)?;
    assert_eq!(output, input);
    assert!(maximum_active.load(Ordering::SeqCst) > 1);
    assert!(
        maximum_active.load(Ordering::SeqCst) <= MAX_CORE_PREPARATION_CREDITS,
        "active work exceeded worst-case credits"
    );
    Ok(())
}

#[test]
fn delayed_low_index_error_across_waves_wins_in_every_worker_pool() -> Result<(), String> {
    let input = (0_usize..34).collect::<Vec<_>>();
    for workers in [1, 2, 4, 8, 16, 32] {
        let preparer = CoreProjectionPreparer::with_parallelism(workers)
            .map_err(|error| error.message.clone())?;
        let operations = AtomicUsize::new(0);
        let mut budget = PreparedOutputBudget::new(0).map_err(|error| error.message.clone())?;
        let error = ordered_parallel_for_each_with_budget(
            &preparer.inner.pool,
            &input,
            &preparer.inner.credits,
            &mut budget,
            |value| {
                operations.fetch_add(1, Ordering::SeqCst);
                if *value == 16 {
                    std::thread::sleep(Duration::from_millis(10));
                    Err(ProtocolError::new(ErrorClass::Internal, "first failure"))
                } else if *value == 17 {
                    Err(ProtocolError::new(ErrorClass::Sequence, "later failure"))
                } else {
                    Ok(*value)
                }
            },
            |_, _, budget| budget.retain(1),
        )
        .expect_err("delayed low-index failure");
        assert_eq!(error.class, ErrorClass::Internal, "workers={workers}");
        assert_eq!(error.message, "first failure", "workers={workers}");
        // The first wave retains 16 bytes, so conservative 8 MiB slots limit
        // the failing second wave to 15 jobs. No third wave may launch.
        assert_eq!(
            operations.load(Ordering::SeqCst),
            31,
            "work after the failing wave launched with {workers} workers"
        );
    }
    Ok(())
}

#[test]
fn four_thousand_ninety_six_maximum_units_never_exceed_one_bounded_wave() {
    let preparer =
        CoreProjectionPreparer::with_parallelism(32).expect("maximum-worker preparation pool");
    let input = (0_usize..4_096).collect::<Vec<_>>();
    let operations = AtomicUsize::new(0);
    let mut budget = PreparedOutputBudget::new(0).expect("empty prepared-output budget");
    let error = ordered_parallel_for_each_with_budget(
        &preparer.inner.pool,
        &input,
        &preparer.inner.credits,
        &mut budget,
        |_| {
            operations.fetch_add(1, Ordering::SeqCst);
            Ok(MAX_CORE_PREPARED_UNIT_BYTES)
        },
        |_, bytes, budget| budget.retain(bytes),
    )
    .expect_err("seventeenth maximum-sized unit must exceed the aggregate bound");
    assert_eq!(error.class, ErrorClass::Bounds);
    assert_eq!(operations.load(Ordering::SeqCst), 16);
    assert_eq!(budget.maximum_wave_width, 16);
    assert_eq!(
        budget.maximum_wave_width * MAX_CORE_PREPARED_UNIT_BYTES,
        MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES
    );
    assert_eq!(
        budget.peak_reserved_bytes,
        MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES
    );
    assert_eq!(
        budget.peak_retained_bytes,
        MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES
    );
}

#[test]
fn prepared_output_bound_is_exact() -> Result<(), String> {
    assert_eq!(
        checked_prepared_output_bytes(0, MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES,)
            .map_err(|error| error.message.clone())?,
        MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES
    );
    let over = checked_prepared_output_bytes(MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES, 1)
        .expect_err("one byte over prepared output bound");
    assert_eq!(over.class, ErrorClass::Bounds);
    let overflow =
        checked_prepared_output_bytes(usize::MAX, 1).expect_err("prepared output byte overflow");
    assert_eq!(overflow.class, ErrorClass::Bounds);

    let wrapper_bytes =
        MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES - 2 * MAX_CORE_PREPARED_UNIT_BYTES;
    let mut budget =
        PreparedOutputBudget::new(wrapper_bytes).map_err(|error| error.message.clone())?;
    assert_eq!(
        budget
            .next_wave_width(3, MAX_CORE_PREPARATION_CREDITS)
            .map_err(|error| error.message.clone())?,
        2
    );
    budget
        .retain(MAX_CORE_PREPARED_UNIT_BYTES)
        .map_err(|error| error.message.clone())?;
    budget
        .retain(MAX_CORE_PREPARED_UNIT_BYTES)
        .map_err(|error| error.message.clone())?;
    assert_eq!(
        budget.retained_bytes,
        MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES
    );
    assert_eq!(
        budget.peak_reserved_bytes,
        MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES
    );
    assert_eq!(
        budget
            .next_wave_width(1, MAX_CORE_PREPARATION_CREDITS)
            .expect_err("metadata plus another active unit exceeds the bound")
            .class,
        ErrorClass::Bounds
    );
    Ok(())
}

#[test]
fn batch_errors_and_duplicates_follow_flattened_page_delta_order() -> Result<(), String> {
    let duplicate_before_later_error = collect_prepared_page_units(
        2,
        [
            (0, Ok(unit("duplicate"))),
            (0, Ok(unit("duplicate"))),
            (
                1,
                Err(ProtocolError::new(ErrorClass::Internal, "later failure")),
            ),
        ],
    )
    .expect_err("earlier duplicate conflict");
    assert_eq!(duplicate_before_later_error.class, ErrorClass::Sequence);

    let error_before_later_duplicate = collect_prepared_page_units(
        2,
        [
            (
                0,
                Err(ProtocolError::new(ErrorClass::Internal, "first failure")),
            ),
            (1, Ok(unit("duplicate"))),
            (1, Ok(unit("duplicate"))),
        ],
    )
    .expect_err("earlier preparation failure");
    assert_eq!(error_before_later_duplicate.class, ErrorClass::Internal);
    assert_eq!(error_before_later_duplicate.message, "first failure");
    Ok(())
}
