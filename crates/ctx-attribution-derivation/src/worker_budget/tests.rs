use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;

use super::*;

#[test]
fn canonical_budget_maps_preparation_and_publication_headroom() {
    assert_eq!(
        configured_provider_worker_limits(None, 64).unwrap(),
        ProviderWorkerLimits {
            preparation_workers: 1,
            finish_workers: 0,
        }
    );
    for (helper, preparation, finish) in [
        ("0", 1, 0),
        ("1", 1, 0),
        ("2", 2, 1),
        ("4", 4, 2),
        ("8", 8, 4),
        ("16", 16, 8),
        ("32", 16, 8),
    ] {
        assert_eq!(
            configured_provider_worker_limits(Some(helper.into()), 64).unwrap(),
            ProviderWorkerLimits {
                preparation_workers: preparation,
                finish_workers: finish,
            }
        );
    }

    let overflow = format!("{}0", usize::MAX);
    for invalid in ["", "+1", "-1", " 1", "1 ", "01", "00", &overflow] {
        assert_eq!(
            configured_provider_worker_limits(Some(invalid.into()), 64)
                .unwrap_err()
                .class,
            ErrorClass::InvalidRequest
        );
    }
}

#[cfg(unix)]
#[test]
fn canonical_budget_rejects_non_utf8() {
    use std::os::unix::ffi::OsStringExt as _;

    let error = configured_provider_worker_limits(Some(OsString::from_vec(vec![b'1', 0xff])), 64)
        .unwrap_err();
    assert_eq!(error.class, ErrorClass::InvalidRequest);
}

#[test]
fn finish_waits_for_preparation_and_blocks_new_preparation() {
    let budget = ProviderWorkerBudget::isolated(2);
    budget
        .begin_preparation()
        .unwrap()
        .commit("candidate-a".to_owned())
        .unwrap();
    let worker = budget.enter_preparation().unwrap();
    let (finish_sender, finish_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let finish_budget = Arc::clone(&budget);
    let finisher = std::thread::spawn(move || {
        let finish = finish_budget.close_preparation("candidate-a").unwrap();
        finish_sender.send(()).unwrap();
        release_receiver.recv().unwrap();
        drop(finish);
    });
    assert!(
        finish_receiver
            .recv_timeout(Duration::from_millis(20))
            .is_err()
    );
    drop(worker);
    finish_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();

    let (preparation_sender, preparation_receiver) = mpsc::channel();
    let preparation_budget = Arc::clone(&budget);
    let waiter = std::thread::spawn(move || {
        let worker = preparation_budget.enter_preparation().unwrap();
        preparation_sender.send(()).unwrap();
        drop(worker);
    });
    assert!(
        preparation_receiver
            .recv_timeout(Duration::from_millis(20))
            .is_err()
    );
    release_sender.send(()).unwrap();
    finisher.join().unwrap();
    preparation_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    waiter.join().unwrap();
}

#[test]
fn dropped_phase_leases_restore_admission() {
    let budget = ProviderWorkerBudget::isolated(2);
    drop(budget.begin_preparation().unwrap());
    budget
        .begin_preparation()
        .unwrap()
        .commit("candidate".to_owned())
        .unwrap();
    drop(budget.close_preparation("candidate").unwrap());
    let worker = budget.enter_preparation().unwrap();
    drop(worker);
    assert_eq!(budget.preparation_peak().unwrap(), 1);
}
