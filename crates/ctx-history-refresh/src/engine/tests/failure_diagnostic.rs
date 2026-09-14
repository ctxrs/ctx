use super::*;

#[test]
fn failure_diagnostic_distinguishes_execution_probe_and_verification() {
    for (case, expected) in [
        ("execution", json!(["execution", "index"])),
        ("probe", json!(["verification", "io"])),
        ("mismatch", json!(["verification", "unknown"])),
        ("receipt", json!(["verification", "unknown"])),
    ] {
        let coordinator = CoreRefreshEngine::new();
        let request = coordinator.enqueue(None);
        let request_id = request_id(&request);
        let run = coordinator
            .run_next_with(
                |_, _| {
                    if case == "execution" {
                        return Err(IndexError::WriterInvariant("private writer detail").into());
                    }
                    let mut publication = test_publication("generation-1");
                    if case == "receipt" {
                        publication.route_results =
                            vec![
                                SourceBackedRefreshRouteResult::succeeded("aa".repeat(32), false);
                                2
                            ];
                    }
                    Ok(publication)
                },
                || {
                    if case == "probe" || case == "execution" {
                        // An execution failure keeps priority over a secondary probe failure.
                        Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "/private/index",
                        )
                        .into())
                    } else if case == "mismatch" {
                        Ok(None)
                    } else {
                        Ok(Some("generation-1".to_owned()))
                    }
                },
                |_| Ok(()),
                |_| {
                    Err(anyhow!(
                        "secondary failure must not replace primary diagnosis"
                    ))
                },
            )
            .unwrap();
        assert!(run.failed, "{case}: {:?}", run.job);
        assert_eq!(run.job["error_code"], "source_refresh_failed");
        assert_eq!(run.job["structured_outcome"]["retryable"], true);
        assert_eq!(diagnostic_pair(&run.job), expected, "{case}");
        assert_eq!(
            diagnostic_pair(&coordinator.status(&request_id).unwrap()),
            expected
        );
    }
}

#[test]
fn failure_diagnostic_identifies_finalization() {
    let coordinator = CoreRefreshEngine::new();
    coordinator.enqueue(None);
    let run = coordinator
        .run_next_with_terminal_success(
            |_, _| Ok(test_publication("generation-1")),
            || Ok(Some("generation-1".to_owned())),
            |_, _| {
                Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "/private/control").into())
            },
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(run.failed);
    assert_eq!(run.job["error_code"], "source_refresh_failed");
    assert_eq!(diagnostic_pair(&run.job), json!(["finalization", "io"]));
}

#[test]
fn failure_diagnostic_survives_terminal_retry_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let coordinator = CoreRefreshEngine::new();
    let request = coordinator.enqueue(None);
    let request_id = request_id(&request);
    let first = coordinator
        .run_next_with(
            |_, _| Err(IndexError::WriterInvariant("private writer detail").into()),
            || Ok(None),
            |_| Err(anyhow!("terminal persistence unavailable")),
            |_| Ok(()),
        )
        .unwrap();
    let expected = json!(["execution", "index"]);
    assert!(first.terminal_persistence_pending);
    assert_eq!(diagnostic_pair(&first.job), expected);
    assert!(coordinator
        .status(&request_id)
        .unwrap()
        .get("refresh_failure_stage")
        .is_none());

    let retry = coordinator
        .run_next_with(
            |_, _| panic!("must not recapture"),
            || panic!("must not reopen Core"),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(!retry.terminal_persistence_pending);
    assert_eq!(diagnostic_pair(&retry.job), expected);

    for diagnostic in [
        Some(expected.clone()),
        None,
        Some(json!(["unrecognized", "unknown"])),
    ] {
        let mut job = retry.job.clone();
        job.as_object_mut().unwrap().remove("refresh_failure_stage");
        job.as_object_mut().unwrap().remove("refresh_failure_kind");
        if let Some(value) = &diagnostic {
            job["refresh_failure_stage"] = value[0].clone();
            job["refresh_failure_kind"] = value[1].clone();
        }
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root), &job).unwrap();
        let recovered = CoreRefreshEngine::new();
        assert!(!recovered.recover(&data_root).unwrap());
        let status = recovered.status(&request_id).unwrap();
        assert_eq!(status["error_code"], "source_refresh_failed");
        assert_eq!(
            diagnostic_pair(&status),
            diagnostic
                .filter(|value| value == &expected)
                .unwrap_or(json!([null, null]))
        );
    }
}

fn diagnostic_pair(job: &Value) -> Value {
    json!([
        job.get("refresh_failure_stage"),
        job.get("refresh_failure_kind")
    ])
}

#[test]
fn coverage_reason_survives_terminal_persistence_and_restart_without_changing_policy() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let coordinator = CoreRefreshEngine::new();
    let request_id = request_id(&coordinator.enqueue(None));
    let first = coordinator
        .run_next_with(
            |_, _| {
                Err(ZeroSourcePublicationBlocked::with_reason(
                    ZeroSourcePublicationBlockReason::MissingTerminalAuthority,
                    "private source path token=secret",
                )
                .into())
            },
            || Ok(None),
            |_| Err(anyhow!("terminal persistence unavailable")),
            |_| Ok(()),
        )
        .unwrap();
    assert!(first.terminal_persistence_pending);
    assert_eq!(
        first.job["refresh_coverage_reason"],
        "missing_terminal_authority"
    );
    assert!(coordinator
        .status(&request_id)
        .unwrap()
        .get("refresh_coverage_reason")
        .is_none());
    let retry = coordinator
        .run_next_with(
            |_, _| panic!("must not recapture"),
            || panic!("must not probe"),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(
        retry.job["refresh_coverage_reason"],
        "missing_terminal_authority"
    );
    for reason in [
        Some(json!("missing_terminal_authority")),
        None,
        Some(json!("/private/future")),
        Some(json!(null)),
    ] {
        let mut job = retry.job.clone();
        job.as_object_mut()
            .unwrap()
            .remove("refresh_coverage_reason");
        if let Some(reason) = &reason {
            job["refresh_coverage_reason"] = reason.clone();
        }
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root), &job).unwrap();
        let recovered = CoreRefreshEngine::new();
        assert!(!recovered.recover(&data_root).unwrap());
        let status = recovered.status(&request_id).unwrap();
        assert_eq!(
            status["structured_outcome"],
            retry.job["structured_outcome"]
        );
        assert_eq!(
            status.get("refresh_coverage_reason"),
            reason
                .as_ref()
                .filter(|value| **value == json!("missing_terminal_authority"))
        );
    }
}
