use super::*;

#[test]
fn daemon_generation_wait_interrupts_after_core_before_semantic_preflight() -> Result<()> {
    let temp = semantic_tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let (index, _) = semantic_index_revision_at(&index_root, 1, true)?;
    reconcile_ready_nonempty_generation(&index, temp.path())?;
    let checkpoints = Cell::new(0_u32);
    let continued_to_query_or_output = Cell::new(false);

    let result = (|| -> Result<()> {
        let _ = wait_for_daemon_semantic_generation_with(
            temp.path(),
            PinnedSourceBackedGeneration::from_index(index),
            Duration::from_secs(1),
            || crate::pin_active_verified_generation(temp.path()),
            || {
                checkpoints.set(checkpoints.get() + 1);
                if checkpoints.get() == 3 {
                    return Err(anyhow::Error::new(crate::FiniteWorkerInterrupted));
                }
                Ok(())
            },
            |_| panic!("interruption before ready preflight must not pause"),
        )?;
        continued_to_query_or_output.set(true);
        Ok(())
    })();

    let error = result.expect_err("post-Core interruption must stop semantic completion");
    assert!(crate::finite_worker_interrupted(&error));
    assert_eq!(checkpoints.get(), 3);
    assert!(!continued_to_query_or_output.get());
    Ok(())
}

#[test]
fn daemon_generation_wait_interrupts_after_pause_before_another_repin() -> Result<()> {
    let temp = semantic_tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let (index, _) = semantic_index_revision_at(&index_root, 1, true)?;
    let interrupted = Cell::new(false);
    let repins = Cell::new(0_u32);
    let pauses = Cell::new(0_u32);
    let continued_to_query_or_output = Cell::new(false);

    let result = (|| -> Result<()> {
        let _ = wait_for_daemon_semantic_generation_with(
            temp.path(),
            PinnedSourceBackedGeneration::from_index(index),
            Duration::from_secs(1),
            || {
                repins.set(repins.get() + 1);
                crate::pin_active_verified_generation(temp.path())
            },
            || {
                if interrupted.get() {
                    Err(anyhow::Error::new(crate::FiniteWorkerInterrupted))
                } else {
                    Ok(())
                }
            },
            |_| {
                pauses.set(pauses.get() + 1);
                interrupted.set(true);
            },
        )?;
        continued_to_query_or_output.set(true);
        Ok(())
    })();

    let error = result.expect_err("interruption during semantic pause must stop the wait");
    assert!(crate::finite_worker_interrupted(&error));
    assert_eq!(repins.get(), 1);
    assert_eq!(pauses.get(), 1);
    assert!(!continued_to_query_or_output.get());
    Ok(())
}

#[test]
fn daemon_generation_wait_checks_interruption_before_ready_success() -> Result<()> {
    let temp = semantic_tempdir()?;
    let index_root = ctx_history_refresh::source_backed_index_root(temp.path());
    let (index, _) = semantic_index_revision_at(&index_root, 1, true)?;
    reconcile_ready_nonempty_generation(&index, temp.path())?;
    let checkpoints = Cell::new(0_u32);
    let continued_to_query_or_output = Cell::new(false);

    let result = (|| -> Result<()> {
        let _ = wait_for_daemon_semantic_generation_with(
            temp.path(),
            PinnedSourceBackedGeneration::from_index(index),
            Duration::from_secs(1),
            || crate::pin_active_verified_generation(temp.path()),
            || {
                checkpoints.set(checkpoints.get() + 1);
                if checkpoints.get() == 4 {
                    return Err(anyhow::Error::new(crate::FiniteWorkerInterrupted));
                }
                Ok(())
            },
            |_| panic!("ready semantic generation must not pause"),
        )?;
        continued_to_query_or_output.set(true);
        Ok(())
    })();

    let error = result.expect_err("interruption before success must remain typed");
    assert!(crate::finite_worker_interrupted(&error));
    assert_eq!(checkpoints.get(), 4);
    assert!(!continued_to_query_or_output.get());
    Ok(())
}
