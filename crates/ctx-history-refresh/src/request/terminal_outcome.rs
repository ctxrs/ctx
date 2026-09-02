use super::*;

impl RefreshTerminalOutcome {
    pub fn validate(&self) -> Result<()> {
        Self::validate_components(
            self.code,
            self.class,
            self.retryable,
            &self.affected_routes,
            &self.retryable_routes,
            &self.blocked_routes,
            self.retry_advice,
        )
    }

    pub(crate) fn validate_components(
        code: RefreshOutcomeCode,
        class: RefreshOutcomeClass,
        retryable: bool,
        affected_routes: &BTreeSet<SourceRouteIdentity>,
        retryable_routes: &BTreeSet<SourceRouteIdentity>,
        blocked_routes: &BTreeSet<SourceRouteIdentity>,
        retry_advice: Option<RefreshRetryAdvice>,
    ) -> Result<()> {
        if affected_routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT
            || retryable_routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT
            || blocked_routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT
            || !retryable_routes.is_disjoint(blocked_routes)
            || !retryable_routes.is_subset(affected_routes)
            || !blocked_routes.is_subset(affected_routes)
            || (code.is_failure()
                && retryable_routes
                    .union(blocked_routes)
                    .ne(affected_routes.iter()))
            || (!affected_routes.is_empty() && retryable == retryable_routes.is_empty())
        {
            bail!("source refresh structured outcome has inconsistent route dispositions");
        }
        if code == RefreshOutcomeCode::SourceUnclaimed
            && (class != RefreshOutcomeClass::Coverage
                || blocked_routes.is_empty()
                || retry_advice
                    != Some(if retryable {
                        RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked
                    } else {
                        RefreshRetryAdvice::InspectSources
                    }))
        {
            bail!("source refresh source-unclaimed outcome is inconsistent");
        }
        if !valid_code_class(code, class) {
            bail!("source refresh structured outcome has inconsistent code and class");
        }
        if !valid_retry_contract(code, class, retryable, retry_advice) {
            bail!("source refresh structured outcome has inconsistent retry disposition");
        }
        Ok(())
    }

    pub(crate) const fn automatic_retry_disposition(
        code: RefreshOutcomeCode,
        class: RefreshOutcomeClass,
        retryable_routes_present: bool,
    ) -> Option<(bool, RefreshRetryAdvice)> {
        if !matches!(
            (code, class),
            (
                RefreshOutcomeCode::SourceRefreshFailed,
                RefreshOutcomeClass::Internal,
            ) | (
                RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable,
                RefreshOutcomeClass::Coverage,
            )
        ) {
            return None;
        }
        Some(if retryable_routes_present {
            (true, RefreshRetryAdvice::RetryAffectedRoutes)
        } else {
            (false, RefreshRetryAdvice::InspectSources)
        })
    }
}

fn valid_retry_contract(
    code: RefreshOutcomeCode,
    class: RefreshOutcomeClass,
    retryable: bool,
    retry_advice: Option<RefreshRetryAdvice>,
) -> bool {
    if let Some(paused) = RefreshTerminalOutcome::automatic_retry_disposition(code, class, false) {
        if Some(paused) == retry_advice.map(|advice| (retryable, advice)) {
            return true;
        }
    }
    match code {
        RefreshOutcomeCode::Completed => !retryable && retry_advice.is_none(),
        RefreshOutcomeCode::CompletedWithRejections
        | RefreshOutcomeCode::CompletedWithSourceFailures
        | RefreshOutcomeCode::CompletedWithRejectionsAndSourceFailures => matches!(
            (class, retryable, retry_advice),
            (RefreshOutcomeClass::CompletedWithDiagnostics, false, None)
                | (
                    RefreshOutcomeClass::CompletedWithRetryableFailures,
                    true,
                    None,
                )
                | (
                    RefreshOutcomeClass::CompletedWithRetryableFailures,
                    true,
                    Some(RefreshRetryAdvice::RetryAffectedRoutes),
                )
        ),
        RefreshOutcomeCode::SourceUnavailable | RefreshOutcomeCode::SourceChanged => {
            retryable
                && retry_advice
                    .is_none_or(|advice| advice == RefreshRetryAdvice::RetryAffectedRoutes)
        }
        RefreshOutcomeCode::ExplicitSourcePathMissing => {
            retryable
                && retry_advice.is_none_or(|advice| advice == RefreshRetryAdvice::InspectSources)
        }
        RefreshOutcomeCode::MalformedSource => {
            !retryable
                && retry_advice.is_none_or(|advice| advice == RefreshRetryAdvice::InspectSources)
        }
        RefreshOutcomeCode::UnsupportedSchema => {
            !retryable
                && retry_advice.is_none_or(|advice| {
                    matches!(
                        advice,
                        RefreshRetryAdvice::InspectSources
                            | RefreshRetryAdvice::UpgradeOrReconfigure
                    )
                })
        }
        RefreshOutcomeCode::SourceFailures | RefreshOutcomeCode::LogicalSourceFailures => {
            matches!(
                (retryable, retry_advice),
                (true, None)
                    | (true, Some(RefreshRetryAdvice::RetryAffectedRoutes))
                    | (false, None)
                    | (false, Some(RefreshRetryAdvice::InspectSources))
            )
        }
        RefreshOutcomeCode::SourceUnclaimed => matches!(
            (retryable, retry_advice),
            (
                true,
                Some(RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked),
            ) | (false, Some(RefreshRetryAdvice::InspectSources))
        ),
        RefreshOutcomeCode::SourceRefreshFailed => matches!(
            (retryable, retry_advice),
            (true, None)
                | (true, Some(RefreshRetryAdvice::RetryRequest))
                | (true, Some(RefreshRetryAdvice::RetryAffectedRoutes))
        ),
        RefreshOutcomeCode::SourceRefreshInternal | RefreshOutcomeCode::ResourceUnavailable => {
            matches!(
                (retryable, retry_advice),
                (true, None)
                    | (true, Some(RefreshRetryAdvice::RetryRequest))
                    | (true, Some(RefreshRetryAdvice::RetryAffectedRoutes))
            )
        }
        RefreshOutcomeCode::IndexIncompatible | RefreshOutcomeCode::IndexCorruption => {
            !retryable
                && retry_advice.is_none_or(|advice| advice == RefreshRetryAdvice::RebuildIndex)
        }
        RefreshOutcomeCode::SourceRefreshAdmissionFailed => {
            retryable
                && retry_advice.is_none_or(|advice| advice == RefreshRetryAdvice::RetryAdmission)
        }
        RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable => matches!(
            (retryable, retry_advice),
            (true, None)
                | (true, Some(RefreshRetryAdvice::RetryRequest))
                | (true, Some(RefreshRetryAdvice::RetryAffectedRoutes))
        ),
    }
}

const fn valid_code_class(code: RefreshOutcomeCode, class: RefreshOutcomeClass) -> bool {
    match code {
        RefreshOutcomeCode::Completed => matches!(class, RefreshOutcomeClass::Completed),
        RefreshOutcomeCode::CompletedWithRejections
        | RefreshOutcomeCode::CompletedWithSourceFailures
        | RefreshOutcomeCode::CompletedWithRejectionsAndSourceFailures => matches!(
            class,
            RefreshOutcomeClass::CompletedWithRetryableFailures
                | RefreshOutcomeClass::CompletedWithDiagnostics
        ),
        RefreshOutcomeCode::SourceUnavailable | RefreshOutcomeCode::ExplicitSourcePathMissing => {
            matches!(class, RefreshOutcomeClass::Unavailable)
        }
        RefreshOutcomeCode::SourceChanged => matches!(class, RefreshOutcomeClass::SourceChanged),
        RefreshOutcomeCode::MalformedSource => matches!(class, RefreshOutcomeClass::Unreadable),
        RefreshOutcomeCode::UnsupportedSchema | RefreshOutcomeCode::IndexIncompatible => {
            matches!(class, RefreshOutcomeClass::Incompatible)
        }
        RefreshOutcomeCode::SourceFailures | RefreshOutcomeCode::LogicalSourceFailures => {
            matches!(class, RefreshOutcomeClass::Mixed)
        }
        RefreshOutcomeCode::SourceUnclaimed
        | RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable => {
            matches!(class, RefreshOutcomeClass::Coverage)
        }
        RefreshOutcomeCode::SourceRefreshFailed | RefreshOutcomeCode::SourceRefreshInternal => {
            matches!(class, RefreshOutcomeClass::Internal)
        }
        RefreshOutcomeCode::ResourceUnavailable => {
            matches!(class, RefreshOutcomeClass::ResourceUnavailable)
        }
        RefreshOutcomeCode::IndexCorruption => matches!(class, RefreshOutcomeClass::Corruption),
        RefreshOutcomeCode::SourceRefreshAdmissionFailed => {
            matches!(class, RefreshOutcomeClass::ControlPlane)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nonretryable_outcome(
        code: RefreshOutcomeCode,
        class: RefreshOutcomeClass,
    ) -> RefreshTerminalOutcome {
        let route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
        RefreshTerminalOutcome {
            code,
            class,
            retryable: false,
            affected_routes: BTreeSet::from([route.clone()]),
            retryable_routes: BTreeSet::new(),
            blocked_routes: BTreeSet::from([route]),
            physical_attempt_id: "physical-attempt".to_owned(),
            retained_generation: None,
            published_generation: None,
            retry_advice: Some(RefreshRetryAdvice::InspectSources),
            detail: None,
        }
    }

    #[test]
    fn automatic_retry_pause_contract_is_closed() {
        for (code, class) in [
            (
                RefreshOutcomeCode::SourceRefreshFailed,
                RefreshOutcomeClass::Internal,
            ),
            (
                RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable,
                RefreshOutcomeClass::Coverage,
            ),
        ] {
            assert_eq!(
                RefreshTerminalOutcome::automatic_retry_disposition(code, class, false),
                Some((false, RefreshRetryAdvice::InspectSources)),
            );
            assert_eq!(
                RefreshTerminalOutcome::automatic_retry_disposition(code, class, true),
                Some((true, RefreshRetryAdvice::RetryAffectedRoutes)),
            );
            assert!(nonretryable_outcome(code, class).validate().is_ok());
        }

        assert_eq!(
            RefreshTerminalOutcome::automatic_retry_disposition(
                RefreshOutcomeCode::SourceRefreshInternal,
                RefreshOutcomeClass::Internal,
                false,
            ),
            None,
        );
        assert!(nonretryable_outcome(
            RefreshOutcomeCode::SourceRefreshInternal,
            RefreshOutcomeClass::Internal,
        )
        .validate()
        .is_err());
    }

    #[test]
    fn unsupported_schema_accepts_direct_and_aggregate_advice() {
        let mut outcome = nonretryable_outcome(
            RefreshOutcomeCode::UnsupportedSchema,
            RefreshOutcomeClass::Incompatible,
        );
        for advice in [
            RefreshRetryAdvice::UpgradeOrReconfigure,
            RefreshRetryAdvice::InspectSources,
        ] {
            outcome.retry_advice = Some(advice);
            assert!(outcome.validate().is_ok(), "{advice:?}");
        }
    }
}
