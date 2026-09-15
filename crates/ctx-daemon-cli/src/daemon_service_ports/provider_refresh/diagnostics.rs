use super::*;
use ctx_history_refresh::ZeroSourcePublicationBlockReason;

pub(super) fn coverage_reason(
    job: &Value,
    code: RefreshOutcomeCode,
) -> Option<ProviderRefreshCoverageReason> {
    if code != RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable
        || !matches!(
            refresh_failure_diagnostic(job),
            Some((_, ProviderRefreshFailureKind::Provider))
        )
    {
        return None;
    }
    let reason =
        ZeroSourcePublicationBlockReason::parse(job.get("refresh_coverage_reason")?.as_str()?)?;
    Some(match reason {
        ZeroSourcePublicationBlockReason::CatalogUnavailable => {
            ProviderRefreshCoverageReason::CatalogUnavailable
        }
        ZeroSourcePublicationBlockReason::UnsafeRoot => ProviderRefreshCoverageReason::UnsafeRoot,
        ZeroSourcePublicationBlockReason::MissingTerminalAuthority => {
            ProviderRefreshCoverageReason::MissingTerminalAuthority
        }
        ZeroSourcePublicationBlockReason::RouteFailed => ProviderRefreshCoverageReason::RouteFailed,
        ZeroSourcePublicationBlockReason::InvalidRouteIdentity => {
            ProviderRefreshCoverageReason::InvalidRouteIdentity
        }
        ZeroSourcePublicationBlockReason::MissingEmptyAuthority => {
            ProviderRefreshCoverageReason::MissingEmptyAuthority
        }
    })
}

pub(super) fn source_failure_class(
    receipt: &SourceBackedRefreshReceipt,
) -> Option<ProviderRefreshSourceFailureClass> {
    let mut classes = Vec::new();
    for route in &receipt.route_results {
        if route.source_failures.len() == route.source_failure_total {
            classes.extend(
                route
                    .source_failures
                    .iter()
                    .map(|failure| failure.class.as_str()),
            );
        } else if route.source_failure_total == 1 && route.source_failures.is_empty() {
            classes.push(route.outcome.failure_class()?);
        } else {
            // A bounded prefix cannot establish the class of omitted failures.
            return None;
        }
    }
    classes.sort_unstable();
    classes.dedup();
    let classes = classes
        .into_iter()
        .map(ProviderRefreshSourceFailureClass::parse)
        .collect::<Option<Vec<_>>>()?;
    match classes.as_slice() {
        [class] => Some(*class),
        [] => None,
        _ => Some(ProviderRefreshSourceFailureClass::Mixed),
    }
}

pub(super) fn failure_reason(
    job: &Value,
) -> Option<ctx_client_observability::analytics::ProviderRefreshFailureReason> {
    let (_, kind) = refresh_failure_diagnostic(job)?;
    let reason = ctx_client_observability::analytics::ProviderRefreshFailureReason::parse(
        job.get("refresh_failure_reason")?.as_str()?,
    )?;
    reason.permits(kind).then_some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reason_projection_requires_closed_kind_and_ignores_local_text() {
        for (reason, kind) in [
            ("io_not_found", "io"),
            ("io_permission_denied", "io"),
            ("io_storage_full", "io"),
            ("io_read_only_filesystem", "io"),
            ("io_out_of_memory", "io"),
            ("io_timed_out", "io"),
            ("route_output_limit", "provider"),
            ("route_scratch_limit", "provider"),
            ("index_memory_limit", "index"),
            ("index_scratch_limit", "index"),
            ("index_writer_invariant", "index"),
        ] {
            let mut job = json!({"refresh_failure_stage": "admission", "refresh_failure_kind": kind, "refresh_failure_reason": reason, "last_error": "/private/token"});
            assert_eq!(failure_reason(&job).unwrap().as_str(), reason);
            job["refresh_failure_kind"] = json!("unknown");
            assert!(failure_reason(&job).is_none());
            job["refresh_failure_kind"] = json!(kind);
            for value in [
                json!(null),
                json!("future"),
                json!("/private/token"),
                json!(12),
            ] {
                job["refresh_failure_reason"] = value;
                assert!(failure_reason(&job).is_none());
            }
        }
    }

    #[test]
    fn coverage_projection_requires_the_exact_closed_context() {
        for reason in [
            "catalog_unavailable",
            "unsafe_root",
            "missing_terminal_authority",
            "route_failed",
            "invalid_route_identity",
            "missing_empty_authority",
        ] {
            let mut job = json!({
                "refresh_failure_stage": "execution", "refresh_failure_kind": "provider",
                "refresh_coverage_reason": reason, "last_error": "/private/source token=secret",
            });
            let code = RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable;
            assert_eq!(coverage_reason(&job, code).unwrap().as_str(), reason);
            assert!(coverage_reason(&job, RefreshOutcomeCode::SourceRefreshFailed).is_none());
            for value in [
                json!(null),
                json!("future"),
                json!("/private/path"),
                json!(1),
            ] {
                job["refresh_coverage_reason"] = value;
                assert!(coverage_reason(&job, code).is_none());
            }
            job["refresh_coverage_reason"] = json!(reason);
            job["refresh_failure_kind"] = json!("io");
            assert!(coverage_reason(&job, code).is_none());
            job.as_object_mut().unwrap().remove("refresh_failure_stage");
            assert!(coverage_reason(&job, code).is_none());
        }
    }
}
