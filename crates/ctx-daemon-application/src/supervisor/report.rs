use super::*;

pub fn daemon_supervisor_report(host: &dyn DaemonApplicationHost, data_root: &Path) -> Value {
    let normalized = supervisor_environment_snapshot_for_registration(host, data_root).and_then(
        |daemon_environment| {
            supervisor_manager_environment(host)
                .map(|manager_environment| (daemon_environment, manager_environment))
        },
    );
    daemon_supervisor_report_with_normalized_environment(host, data_root, normalized)
}

fn supervisor_environment_snapshot_for_registration(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
) -> Result<SupervisorEnvironmentSnapshot> {
    let mut snapshot = supervisor_environment_snapshot(host)?;
    let config = host.daemon_config(data_root)?;
    if !config.semantic_enabled || config.semantic_executor == "builtin" {
        snapshot = snapshot.without_semantic_embedding_auth();
    }
    let persisted_loop_interval_seconds = persisted_supervisor_loop_interval_seconds(data_root);
    let loop_interval_seconds =
        persisted_loop_interval_seconds.or(snapshot.loop_interval_seconds());
    snapshot.with_loop_interval_seconds(loop_interval_seconds)
}

pub(super) fn daemon_supervisor_report_with_normalized_environment(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    normalized: Result<(SupervisorEnvironmentSnapshot, SupervisorManagerEnvironment)>,
) -> Value {
    let Ok((daemon_environment, manager_environment)) = normalized else {
        let mut report = stored_supervisor_report(data_root);
        invalidate_supervisor_claims_for_environment_failure(&mut report);
        append_forced_termination_identity_report(&mut report);
        append_supervisor_environment_report(host, data_root, &mut report);
        return report;
    };
    let Ok(backend) = PlatformNativeSupervisor::new(
        host,
        data_root,
        Some(&daemon_environment),
        &manager_environment,
    ) else {
        let mut report = stored_supervisor_report(data_root);
        invalidate_supervisor_claims_for_environment_failure(&mut report);
        append_forced_termination_identity_report(&mut report);
        append_supervisor_environment_report(host, data_root, &mut report);
        return report;
    };
    revalidated_supervisor_report_with(host, data_root, &backend)
}

fn invalidate_supervisor_claims_for_environment_failure(report: &mut Value) {
    if let Some(object) = report.as_object_mut() {
        object.insert("revalidated".to_owned(), Value::Bool(true));
        object.insert("registration_verified".to_owned(), Value::Bool(false));
        object.insert("live_owner_verified".to_owned(), Value::Bool(false));
        object.insert("owner_pid".to_owned(), Value::Null);
        object.insert(
            "status".to_owned(),
            Value::String("environment_invalid".to_owned()),
        );
        object.insert(
            "revalidation_error".to_owned(),
            Value::String(
                "current supervisor environment cannot be normalized; native registration and live-owner claims are not trusted"
                    .to_owned(),
            ),
        );
    }
}

pub(super) fn revalidated_supervisor_report_with(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    backend: &dyn NativeSupervisorBackend<SupervisorEnvironmentSnapshot>,
) -> Value {
    let mut report = stored_supervisor_report(data_root);
    append_forced_termination_identity_report(&mut report);
    append_supervisor_environment_report(host, data_root, &mut report);
    if native_supervisor_product_authority_blocker()
        || report.get("kind").and_then(Value::as_str) != Some(native_supervisor_kind())
        || matches!(
            report.get("status").and_then(Value::as_str),
            Some("disabled" | "degraded")
        )
    {
        return report;
    }

    match backend.probe_manager(data_root) {
        Ok(SupervisorManagerOperability::Operational) => {}
        Ok(SupervisorManagerOperability::Unavailable { reason }) => {
            mark_supervisor_manager_unavailable(&mut report, reason);
            return report;
        }
        Err(error) => {
            mark_supervisor_manager_probe_failed(&mut report, format!("{error:#}"));
            return report;
        }
    }

    let installation_lock = SupervisorInstallationLock::acquire(data_root);
    let executable = report
        .get("executable_path")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let (registration_verified, live_owner, error) = match (installation_lock, executable) {
        (Ok(_installation_lock), Some(executable)) => {
            match backend.verify_registration(data_root, &executable) {
                Ok(()) => match backend.verify_live_owner(data_root, &executable) {
                    Ok(owner_pid) => (true, Some(owner_pid), None),
                    Err(error) => (true, None, Some(format!("{error:#}"))),
                },
                Err(error) => (false, None, Some(format!("{error:#}"))),
            }
        }
        (Err(error), _) => (false, None, Some(format!("{error:#}"))),
        (Ok(_installation_lock), None) => (
            false,
            None,
            Some("supervisor receipt has no installed executable identity".to_owned()),
        ),
    };
    let live_owner_verified = live_owner.is_some();
    if let Some(object) = report.as_object_mut() {
        object.insert("revalidated".to_owned(), Value::Bool(true));
        object.insert(
            "registration_verified".to_owned(),
            Value::Bool(registration_verified),
        );
        object.insert(
            "live_owner_verified".to_owned(),
            Value::Bool(live_owner_verified),
        );
        object.insert(
            "owner_pid".to_owned(),
            live_owner.map_or(Value::Null, Value::from),
        );
        object.insert(
            "status".to_owned(),
            Value::String(
                if !registration_verified {
                    "stale_registration"
                } else if live_owner_verified {
                    "installed"
                } else {
                    "registered_not_running"
                }
                .to_owned(),
            ),
        );
        object.insert(
            "revalidation_error".to_owned(),
            error.map_or(Value::Null, Value::String),
        );
    }
    report
}

fn mark_supervisor_manager_unavailable(report: &mut Value, reason: String) {
    suppress_environment_restart_claim(report);
    if let Some(object) = report.as_object_mut() {
        object.insert("revalidated".to_owned(), Value::Bool(true));
        object.insert("registration_verified".to_owned(), Value::Bool(false));
        object.insert("live_owner_verified".to_owned(), Value::Bool(false));
        object.insert("owner_pid".to_owned(), Value::Null);
        object.insert("autostart_supported".to_owned(), Value::Bool(false));
        object.insert("restart_supported".to_owned(), Value::Bool(false));
        object.insert(
            "status".to_owned(),
            Value::String("manager_unavailable".to_owned()),
        );
        object.insert(
            "limitation".to_owned(),
            Value::String(native_supervisor_limitation().to_owned()),
        );
        object.insert("revalidation_error".to_owned(), Value::String(reason));
    }
}

fn mark_supervisor_manager_probe_failed(report: &mut Value, error: String) {
    suppress_environment_restart_claim(report);
    if let Some(object) = report.as_object_mut() {
        object.insert("revalidated".to_owned(), Value::Bool(true));
        object.insert("registration_verified".to_owned(), Value::Bool(false));
        object.insert("live_owner_verified".to_owned(), Value::Bool(false));
        object.insert("owner_pid".to_owned(), Value::Null);
        object.insert(
            "status".to_owned(),
            Value::String("manager_probe_failed".to_owned()),
        );
        object.insert("revalidation_error".to_owned(), Value::String(error));
    }
}

fn suppress_environment_restart_claim(report: &mut Value) {
    if let Some(restart_required) = report.pointer_mut("/environment_snapshot/restart_required") {
        *restart_required = Value::Bool(false);
    }
}

fn append_supervisor_environment_report(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    report: &mut Value,
) {
    let current = supervisor_environment_snapshot_for_registration(host, data_root)
        .map(|snapshot| snapshot.contract_report())
        .unwrap_or_else(|_| supervisor_environment_contract_report(host));
    let stored_sha256 = report
        .pointer("/environment_snapshot/sha256")
        .and_then(Value::as_str);
    let current_sha256 = current.get("sha256").and_then(Value::as_str);
    let native_registration = report.get("kind").and_then(Value::as_str)
        == Some(native_supervisor_kind())
        && !matches!(
            report.get("status").and_then(Value::as_str),
            Some("disabled" | "degraded")
        );
    let restart_required =
        native_registration && (stored_sha256.is_none() || stored_sha256 != current_sha256);
    let mut environment = report
        .get("environment_snapshot")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if let Some(object) = environment.as_object_mut() {
        // The private receipt retains this digest for change detection, but it
        // includes credential values and must never become a status oracle.
        object.remove("sha256");
        object.remove("current_sha256");
        object.insert(
            "current_captured_names".to_owned(),
            current
                .get("captured_names")
                .cloned()
                .unwrap_or_else(|| json!([])),
        );
        object.insert(
            "current_observed_at_ms".to_owned(),
            current
                .get("captured_at_ms")
                .cloned()
                .unwrap_or(Value::Null),
        );
        object.insert(
            "current_error".to_owned(),
            current.get("error").cloned().unwrap_or(Value::Null),
        );
        object.insert("restart_required".to_owned(), Value::Bool(restart_required));
        object.insert("values_exposed".to_owned(), Value::Bool(false));
    }
    if let Some(object) = report.as_object_mut() {
        object.insert("environment_snapshot".to_owned(), environment);
    }
}

fn append_forced_termination_identity_report(report: &mut Value) {
    let detail = if cfg!(target_os = "linux") {
        json!({
            "strategy": "pidfd_when_available",
            "limitation": "Linux kernels or restricted runtimes without usable pidfd support cannot close PID reuse completely; ctx falls back only to an immediately repeated owner-lock, executable, and PID identity check before each signal",
        })
    } else if cfg!(unix) {
        json!({
            "strategy": "reverified_pid",
            "limitation": "this platform exposes no stable process handle used by ctx for signals; ctx minimizes but cannot eliminate the PID-reuse window by repeating owner-lock, executable, and PID identity checks immediately before each signal",
        })
    } else if cfg!(windows) {
        json!({
            "strategy": "process_handle",
            "limitation": Value::Null,
        })
    } else {
        json!({
            "strategy": "unavailable",
            "limitation": "this platform cannot identity-verify residual daemon termination",
        })
    };
    if let Some(object) = report.as_object_mut() {
        object.insert("forced_termination_identity".to_owned(), detail);
    }
}

#[cfg(any(test, target_os = "freebsd"))]
pub(super) fn freebsd_supervisor_authority_blocker() -> &'static str {
    "FreeBSD has no standard current-user service manager with both login/boot registration and identity-verifiable restart ownership; ctx will not mutate the user's crontab or claim rc.d authority, so retrieval commands retain typed CLI self-healing"
}

pub(super) fn native_supervisor_product_authority_blocker() -> bool {
    cfg!(not(any(target_os = "linux", target_os = "macos", windows)))
}
