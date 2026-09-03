use super::*;

/// The validated terminal result shared by daemon state, recovery, progress,
/// and Core capability failures. Construction is deliberately the only way to
/// assemble one, so no downstream layer can invent a legal-looking outcome.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RefreshTerminalOutcome {
    code: RefreshOutcomeCode,
    retryable: bool,
    affected_routes: BTreeSet<SourceRouteIdentity>,
    retryable_routes: BTreeSet<SourceRouteIdentity>,
    blocked_routes: BTreeSet<SourceRouteIdentity>,
    physical_attempt_id: String,
    retained_generation: Option<String>,
    published_generation: Option<String>,
    retry_advice: Option<RefreshRetryAdvice>,
    detail: Option<String>,
}

impl RefreshTerminalOutcome {
    pub(crate) fn from_published_receipt(
        receipt: &SourceBackedRefreshReceipt,
        physical_attempt_id: &str,
    ) -> Self {
        let code = RefreshOutcomeCode::from_receipt(receipt);
        let (retryable_routes, blocked_routes) = receipt.route_retry_dispositions();
        let retryable = !retryable_routes.is_empty();
        let affected_routes = receipt
            .route_results
            .iter()
            .filter(|result| {
                result.outcome.is_failure()
                    || result.source_failure_total != 0
                    || result.rejected_record_total != 0
            })
            .map(|result| {
                SourceRouteIdentity::from_sha256(result.route_identity.clone())
                    .expect("validated refresh receipt route identity")
            })
            .collect();
        Self::new(
            code,
            retryable,
            affected_routes,
            retryable_routes,
            blocked_routes,
            physical_attempt_id.to_owned(),
            (code != RefreshOutcomeCode::Completed || !receipt.generation_changed)
                .then_some(receipt.published_generation.clone()),
            Some(receipt.published_generation.clone()),
            retryable.then_some(RefreshRetryAdvice::RetryAffectedRoutes),
            None,
        )
        .expect("validated refresh receipt has a valid terminal projection")
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        code: RefreshOutcomeCode,
        retryable: bool,
        affected_routes: BTreeSet<SourceRouteIdentity>,
        retryable_routes: BTreeSet<SourceRouteIdentity>,
        blocked_routes: BTreeSet<SourceRouteIdentity>,
        physical_attempt_id: String,
        retained_generation: Option<String>,
        published_generation: Option<String>,
        retry_advice: Option<RefreshRetryAdvice>,
        detail: Option<String>,
    ) -> Result<Self> {
        validate_components(
            code,
            retryable,
            &affected_routes,
            &retryable_routes,
            &blocked_routes,
            &physical_attempt_id,
            retained_generation.as_deref(),
            published_generation.as_deref(),
            retry_advice,
            detail.as_deref(),
        )?;
        Ok(Self {
            code,
            retryable,
            affected_routes,
            retryable_routes,
            blocked_routes,
            physical_attempt_id,
            retained_generation,
            published_generation,
            retry_advice,
            detail,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_uniform_route_disposition(
        code: RefreshOutcomeCode,
        retryable: bool,
        affected_routes: BTreeSet<SourceRouteIdentity>,
        physical_attempt_id: String,
        retained_generation: Option<String>,
        published_generation: Option<String>,
        retry_advice: Option<RefreshRetryAdvice>,
        detail: Option<String>,
    ) -> Result<Self> {
        let (retryable_routes, blocked_routes) = if retryable {
            (affected_routes.clone(), BTreeSet::new())
        } else {
            (BTreeSet::new(), affected_routes.clone())
        };
        Self::new(
            code,
            retryable,
            affected_routes,
            retryable_routes,
            blocked_routes,
            physical_attempt_id,
            retained_generation,
            published_generation,
            retry_advice,
            detail,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_route_dispositions(
        code: RefreshOutcomeCode,
        retryable: bool,
        retryable_routes: BTreeSet<SourceRouteIdentity>,
        blocked_routes: BTreeSet<SourceRouteIdentity>,
        physical_attempt_id: String,
        retained_generation: Option<String>,
        published_generation: Option<String>,
        retry_advice: Option<RefreshRetryAdvice>,
        detail: Option<String>,
    ) -> Result<Self> {
        let affected_routes = retryable_routes.union(&blocked_routes).cloned().collect();
        Self::new(
            code,
            retryable,
            affected_routes,
            retryable_routes,
            blocked_routes,
            physical_attempt_id,
            retained_generation,
            published_generation,
            retry_advice,
            detail,
        )
    }

    pub(crate) fn validate(&self) -> Result<()> {
        validate_components(
            self.code,
            self.retryable,
            &self.affected_routes,
            &self.retryable_routes,
            &self.blocked_routes,
            &self.physical_attempt_id,
            self.retained_generation.as_deref(),
            self.published_generation.as_deref(),
            self.retry_advice,
            self.detail.as_deref(),
        )
    }

    pub const fn code(&self) -> RefreshOutcomeCode {
        self.code
    }
    pub const fn class(&self) -> RefreshOutcomeClass {
        self.code.class(self.retryable)
    }
    pub(crate) fn validate_declared_class(&self, class: RefreshOutcomeClass) -> Result<()> {
        if class != self.class() {
            bail!("source refresh structured outcome has inconsistent code and class");
        }
        Ok(())
    }
    pub const fn retryable(&self) -> bool {
        self.retryable
    }
    pub fn affected_routes(&self) -> &BTreeSet<SourceRouteIdentity> {
        &self.affected_routes
    }
    pub fn retryable_routes(&self) -> &BTreeSet<SourceRouteIdentity> {
        &self.retryable_routes
    }
    pub fn blocked_routes(&self) -> &BTreeSet<SourceRouteIdentity> {
        &self.blocked_routes
    }
    pub fn physical_attempt_id(&self) -> &str {
        &self.physical_attempt_id
    }
    pub fn retained_generation(&self) -> Option<&str> {
        self.retained_generation.as_deref()
    }
    pub fn published_generation(&self) -> Option<&str> {
        self.published_generation.as_deref()
    }
    pub const fn retry_advice(&self) -> Option<RefreshRetryAdvice> {
        self.retry_advice
    }
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    pub(crate) fn with_failure_context(
        mut self,
        retained_generation: Option<String>,
        detail: Option<String>,
    ) -> Result<Self> {
        self.retained_generation = retained_generation;
        self.detail = detail;
        self.validate()?;
        Ok(self)
    }

    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "code": self.code.as_str(),
            "class": self.class().as_str(),
            "retryable": self.retryable,
            "affected_routes": route_names(&self.affected_routes),
            "retryable_routes": route_names(&self.retryable_routes),
            "blocked_routes": route_names(&self.blocked_routes),
            "physical_attempt_id": self.physical_attempt_id,
            "retained_generation": self.retained_generation,
            "published_generation": self.published_generation,
            "retry_advice": self.retry_advice.map(RefreshRetryAdvice::as_str),
            "detail": self.detail,
        }))
    }

    pub(crate) fn is_automatic_retry_eligible(&self) -> bool {
        automatic_retry_disposition(self.code, false).is_some()
    }

    pub(crate) fn pause_automatic_retry_routes(&mut self, routes: &BTreeSet<SourceRouteIdentity>) {
        self.move_automatic_retry_routes(routes, true);
    }

    pub(crate) fn rearm_automatic_retry_routes(&mut self, routes: &BTreeSet<SourceRouteIdentity>) {
        self.move_automatic_retry_routes(routes, false);
    }

    fn move_automatic_retry_routes(&mut self, routes: &BTreeSet<SourceRouteIdentity>, pause: bool) {
        if !self.is_automatic_retry_eligible() {
            return;
        }
        let (from, to) = if pause {
            (&mut self.retryable_routes, &mut self.blocked_routes)
        } else {
            (&mut self.blocked_routes, &mut self.retryable_routes)
        };
        let mut changed = false;
        for route in routes {
            if from.remove(route) {
                to.insert(route.clone());
                changed = true;
            }
        }
        if !changed {
            return;
        }
        if let Some((retryable, advice)) =
            automatic_retry_disposition(self.code, !self.retryable_routes.is_empty())
        {
            self.retryable = retryable;
            self.retry_advice = Some(advice);
        }
        debug_assert!(self.validate().is_ok());
    }
}

fn route_names(routes: &BTreeSet<SourceRouteIdentity>) -> Vec<String> {
    routes
        .iter()
        .map(|route| route.as_str().to_owned())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn validate_components(
    code: RefreshOutcomeCode,
    retryable: bool,
    affected_routes: &BTreeSet<SourceRouteIdentity>,
    retryable_routes: &BTreeSet<SourceRouteIdentity>,
    blocked_routes: &BTreeSet<SourceRouteIdentity>,
    physical_attempt_id: &str,
    retained_generation: Option<&str>,
    published_generation: Option<&str>,
    retry_advice: Option<RefreshRetryAdvice>,
    detail: Option<&str>,
) -> Result<()> {
    if physical_attempt_id.is_empty()
        || physical_attempt_id.chars().any(char::is_control)
        || retained_generation.is_some_and(invalid_identity)
        || published_generation.is_some_and(invalid_identity)
    {
        bail!("source refresh structured outcome has invalid identity");
    }
    match (code.is_failure(), published_generation.is_some()) {
        (true, true) => bail!("failed source refresh outcome has a published generation"),
        (false, false) => bail!("successful source refresh outcome has no published generation"),
        _ => {}
    }
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
    if code == RefreshOutcomeCode::Completed
        && (retryable
            || !affected_routes.is_empty()
            || !retryable_routes.is_empty()
            || !blocked_routes.is_empty()
            || retry_advice.is_some()
            || detail.is_some())
    {
        bail!("completed source refresh outcome carries failure facts");
    }
    if code == RefreshOutcomeCode::SourceUnclaimed
        && (blocked_routes.is_empty()
            || retry_advice
                != Some(if retryable {
                    RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked
                } else {
                    RefreshRetryAdvice::InspectSources
                }))
    {
        bail!("source refresh source-unclaimed outcome is inconsistent");
    }
    if !valid_retry_contract(code, retryable, retry_advice) {
        bail!("source refresh structured outcome has inconsistent retry disposition");
    }
    Ok(())
}

fn invalid_identity(value: &str) -> bool {
    value.is_empty() || value.chars().any(char::is_control)
}

pub(crate) const fn automatic_retry_disposition(
    code: RefreshOutcomeCode,
    retryable_routes_present: bool,
) -> Option<(bool, RefreshRetryAdvice)> {
    if !matches!(
        code,
        RefreshOutcomeCode::SourceRefreshFailed
            | RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable
    ) {
        return None;
    }
    Some(if retryable_routes_present {
        (true, RefreshRetryAdvice::RetryAffectedRoutes)
    } else {
        (false, RefreshRetryAdvice::InspectSources)
    })
}

fn valid_retry_contract(
    code: RefreshOutcomeCode,
    retryable: bool,
    retry_advice: Option<RefreshRetryAdvice>,
) -> bool {
    if let Some(paused) = automatic_retry_disposition(code, false) {
        if Some(paused) == retry_advice.map(|advice| (retryable, advice)) {
            return true;
        }
    }
    match code {
        RefreshOutcomeCode::Completed => !retryable && retry_advice.is_none(),
        RefreshOutcomeCode::CompletedWithRejections
        | RefreshOutcomeCode::CompletedWithSourceFailures
        | RefreshOutcomeCode::CompletedWithRejectionsAndSourceFailures => matches!(
            (retryable, retry_advice),
            (false, None) | (true, None) | (true, Some(RefreshRetryAdvice::RetryAffectedRoutes))
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
        RefreshOutcomeCode::SourceFailures | RefreshOutcomeCode::LogicalSourceFailures => matches!(
            (retryable, retry_advice),
            (true, None)
                | (true, Some(RefreshRetryAdvice::RetryAffectedRoutes))
                | (false, None)
                | (false, Some(RefreshRetryAdvice::InspectSources))
        ),
        RefreshOutcomeCode::SourceUnclaimed => matches!(
            (retryable, retry_advice),
            (
                true,
                Some(RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked)
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

#[cfg(test)]
#[path = "terminal_outcome/tests.rs"]
mod tests;
