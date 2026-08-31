use std::cell::{Cell, RefCell};

use super::*;

#[test]
fn owner_wait_rechecks_the_lock_after_a_finite_owner_retires() -> Result<()> {
    let events = RefCell::new(Vec::new());
    let outcome = classify_daemon_owner_wait_with(
        || {
            events.borrow_mut().push("owner_wait");
            Ok(None)
        },
        || {
            events.borrow_mut().push("lock_recheck");
            false
        },
    )?;

    assert_eq!(outcome, DaemonOwnerWaitOutcome::Released);
    assert_eq!(events.borrow().as_slice(), &["owner_wait", "lock_recheck"]);
    assert!(existing_daemon_request_after_owner_wait(outcome)?.is_none());
    Ok(())
}

#[test]
fn owner_wait_retains_a_typed_error_while_the_lock_remains_active() -> Result<()> {
    let events = RefCell::new(Vec::new());
    let outcome = classify_daemon_owner_wait_with(
        || {
            events.borrow_mut().push("owner_wait");
            Ok(None)
        },
        || {
            events.borrow_mut().push("lock_recheck");
            true
        },
    )?;

    assert_eq!(
        outcome,
        DaemonOwnerWaitOutcome::StillActiveWithoutStableOwner
    );
    assert_eq!(events.borrow().as_slice(), &["owner_wait", "lock_recheck"]);
    let error = match existing_daemon_request_after_owner_wait(outcome) {
        Ok(_) => panic!("an active unstable owner unexpectedly permitted startup"),
        Err(error) => error,
    };
    assert!(error.is::<ActiveDaemonOwnerIdentityError>());
    assert_eq!(
        error.to_string(),
        "active ctx daemon lock has no stable owner identity"
    );
    Ok(())
}

#[test]
fn owner_wait_uses_a_stable_owner_without_a_second_lock_classification() -> Result<()> {
    let owner = test_daemon_owner("stable-wait-owner", 51);
    let lock_rechecked = Cell::new(false);
    let outcome = classify_daemon_owner_wait_with(
        || Ok(Some(owner.clone())),
        || {
            lock_rechecked.set(true);
            false
        },
    )?;

    assert_eq!(outcome, DaemonOwnerWaitOutcome::Owner(owner.clone()));
    assert!(!lock_rechecked.get());
    match existing_daemon_request_after_owner_wait(outcome)? {
        Some(DaemonAutostartRequest::Existing(observed)) => assert_eq!(observed, owner),
        _ => panic!("stable owner was not reused"),
    }
    Ok(())
}
