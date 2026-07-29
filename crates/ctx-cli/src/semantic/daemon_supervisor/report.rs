use super::*;

pub(in crate::semantic) fn daemon_supervisor_report(data_root: &Path) -> Value {
    revalidated_supervisor_report_with(data_root, &PlatformNativeSupervisor)
}

pub(super) fn revalidated_supervisor_report_with(
    data_root: &Path,
    backend: &dyn NativeSupervisorBackend,
) -> Value {
    let mut report = stored_supervisor_report(data_root);
    append_forced_termination_identity_report(&mut report);
    if native_supervisor_product_authority_blocker()
        || report.get("kind").and_then(Value::as_str) != Some(native_supervisor_kind())
        || matches!(
            report.get("status").and_then(Value::as_str),
            Some("disabled" | "degraded")
        )
    {
        return report;
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
