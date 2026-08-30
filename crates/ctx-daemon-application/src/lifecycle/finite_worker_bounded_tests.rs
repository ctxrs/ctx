use super::*;

#[cfg(unix)]
#[test]
fn readiness_candidate_cleanup_retries_kill_and_authoritatively_reaps() -> Result<()> {
    let mut child = std::process::Command::new("sh")
        .args(["-c", "exec sleep 30"])
        .spawn()?;
    let pid = child.id();
    let mut kill_attempts = 0;

    let error = super::finite_worker::reap_owned_candidate_with_actions_for_test(
        &mut child,
        |_| Err(std::io::Error::other("injected candidate delivery failure")),
        |child| {
            kill_attempts += 1;
            if kill_attempts == 1 {
                Err(std::io::Error::other("injected candidate kill failure"))
            } else {
                child.kill()
            }
        },
    )
    .expect_err("delivery failure remains diagnostic after candidate reap");

    assert_eq!(error.to_string(), "injected candidate delivery failure");
    assert!(kill_attempts >= 2);
    assert!(child.try_wait()?.is_some());
    assert_eq!(
        ctx_daemon_runtime::process_state(pid),
        ctx_daemon_runtime::ProcessState::NotRunning
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn successful_hard_kill_with_delayed_exit_is_bounded_for_a_losing_candidate() -> Result<()> {
    let mut child = std::process::Command::new("sh")
        .args(["-c", "exec sleep 30"])
        .spawn()?;

    let kill_succeeded = Cell::new(false);
    let kill_attempts = Cell::new(0);
    let probes_after_kill = Cell::new(0);
    let started = Instant::now();
    let error = super::finite_worker::reap_owned_candidate_with_probe_for_test(
        &mut child,
        |_| Ok(()),
        |_| {
            kill_attempts.set(kill_attempts.get() + 1);
            kill_succeeded.set(true);
            std::thread::sleep(Duration::from_millis(1_100));
            Ok(())
        },
        |child| {
            if kill_succeeded.get() {
                probes_after_kill.set(probes_after_kill.get() + 1);
            }
            child.try_wait()
        },
    )
    .expect_err("a successful candidate kill must not make delayed exit unbounded");

    assert_eq!(
        kill_attempts.get(),
        1,
        "successful kill must not be retried"
    );
    assert_eq!(
        probes_after_kill.get(),
        1,
        "deadline expiry must retain one final post-kill status probe"
    );
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        error.to_string(),
        "finite worker candidate did not exit after bounded kill escalation"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "delayed candidate exit exceeded its escalation bound"
    );
    assert!(child.try_wait()?.is_none());

    child.kill()?;
    child.wait()?;
    Ok(())
}
