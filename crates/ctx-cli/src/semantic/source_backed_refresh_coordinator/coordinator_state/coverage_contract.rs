use super::*;

#[derive(Debug, Clone)]
pub(super) struct ManualAllContinuation {
    pub(super) predecessor_request_id: String,
    pub(super) predecessor_finished: bool,
    pub(super) admission_route_observations: BTreeMap<SourceRouteIdentity, Option<String>>,
    pub(super) ledger_eligible_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) admission_event_watermarks: BTreeMap<SourceRouteIdentity, EventWatermark>,
    pub(super) predecessor_event_watermarks: BTreeMap<SourceRouteIdentity, EventWatermark>,
    pub(super) invalidated_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) covered_route_results: BTreeMap<SourceRouteIdentity, SourceBackedRefreshRouteResult>,
    pub(super) covered_removed_source_count: usize,
    pub(super) covered_timings: SourceBackedRefreshTimings,
}

impl ManualAllContinuation {
    pub(super) fn new(
        predecessor_request_id: String,
        admission_route_observations: BTreeMap<SourceRouteIdentity, Option<String>>,
        ledger_eligible_routes: BTreeSet<SourceRouteIdentity>,
        admission_event_watermarks: BTreeMap<SourceRouteIdentity, EventWatermark>,
        predecessor_event_watermarks: BTreeMap<SourceRouteIdentity, EventWatermark>,
    ) -> Self {
        Self {
            predecessor_request_id,
            predecessor_finished: false,
            admission_route_observations,
            ledger_eligible_routes,
            admission_event_watermarks,
            predecessor_event_watermarks,
            invalidated_routes: BTreeSet::new(),
            covered_route_results: BTreeMap::new(),
            covered_removed_source_count: 0,
            covered_timings: SourceBackedRefreshTimings::default(),
        }
    }

    pub(super) fn invalidate_route(&mut self, route: &SourceRouteIdentity) {
        if self.admission_route_observations.contains_key(route) {
            self.invalidated_routes.insert(route.clone());
        }
        if self.covered_route_results.remove(route).is_some()
            && self.covered_route_results.is_empty()
        {
            self.covered_removed_source_count = 0;
            self.covered_timings = SourceBackedRefreshTimings::default();
        }
    }

    pub(super) fn covered_publication(&self) -> SourceBackedRefreshCoveredPublication {
        SourceBackedRefreshCoveredPublication {
            route_results: self.covered_route_results.values().cloned().collect(),
            removed_source_count: self.covered_removed_source_count,
            timings: self.covered_timings,
        }
    }

    pub(super) fn is_fully_covered(&self) -> bool {
        self.invalidated_routes.is_empty()
            && self
                .admission_route_observations
                .keys()
                .all(|route| self.covered_route_results.contains_key(route))
    }

    pub(super) fn to_json(&self) -> Value {
        let admission_route_observations = self
            .admission_route_observations
            .iter()
            .map(|(route, observation)| {
                (
                    route.as_str().to_owned(),
                    observation
                        .as_ref()
                        .map_or(Value::Bool(false), |value| json!(value)),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let covered_route_results = self
            .covered_route_results
            .iter()
            .map(|(route, result)| (route.as_str().to_owned(), result.compact_json()))
            .collect::<serde_json::Map<_, _>>();
        compact_json(json!({
            "predecessor_request_id": self.predecessor_request_id,
            "predecessor_finished": self.predecessor_finished,
            "admission_route_observations": admission_route_observations,
            "ledger_eligible_routes": self.ledger_eligible_routes
                .iter()
                .map(SourceRouteIdentity::as_str)
                .collect::<Vec<_>>(),
            "admission_event_watermarks": event_watermarks_json(&self.admission_event_watermarks),
            "predecessor_event_watermarks": event_watermarks_json(&self.predecessor_event_watermarks),
            "invalidated_routes": self.invalidated_routes
                .iter()
                .map(SourceRouteIdentity::as_str)
                .collect::<Vec<_>>(),
            "covered_route_results": covered_route_results,
            "covered_removed_source_count": self.covered_removed_source_count,
            "covered_timings": self.covered_timings.to_json(),
        }))
    }

    pub(super) fn from_json(value: &Value) -> Result<Self> {
        let predecessor_request_id = value
            .get("predecessor_request_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("logical refresh demand has no predecessor request ID"))?
            .to_owned();
        let predecessor_finished = value
            .get("predecessor_finished")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow!("logical refresh demand has no predecessor terminal state"))?;
        let admission = value
            .get("admission_route_observations")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("logical refresh demand has no admission fence"))?;
        if admission.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
            bail!("logical refresh demand admission fence exceeds its route bound");
        }
        let admission_route_observations = admission
            .iter()
            .map(|(route, observation)| {
                let route = SourceRouteIdentity::from_sha256(route.clone())?;
                let observation = if observation.is_null() || observation == &Value::Bool(false) {
                    None
                } else {
                    let value = observation
                        .as_str()
                        .filter(|value| is_sha256_identity(value))
                        .ok_or_else(|| anyhow!("logical refresh demand observation is invalid"))?;
                    Some(value.to_owned())
                };
                Ok((route, observation))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let ledger_eligible_routes = value
            .get("ledger_eligible_routes")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("logical refresh demand has no ledger-eligible route set"))?;
        if ledger_eligible_routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
            bail!("logical refresh demand ledger-eligible route set exceeds its route bound");
        }
        let ledger_eligible_routes = ledger_eligible_routes
            .iter()
            .map(|route| {
                route
                    .as_str()
                    .ok_or_else(|| anyhow!("logical refresh demand ledger route is invalid"))
                    .and_then(|route| {
                        SourceRouteIdentity::from_sha256(route.to_owned()).map_err(Into::into)
                    })
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if ledger_eligible_routes
            .iter()
            .any(|route| !admission_route_observations.contains_key(route))
        {
            bail!("logical refresh demand ledger route is outside its admission fence");
        }
        let admission_event_watermarks = event_watermarks_from_json(
            value.get("admission_event_watermarks"),
            "logical refresh demand admission event watermarks",
        )?;
        let predecessor_event_watermarks = event_watermarks_from_json(
            value.get("predecessor_event_watermarks"),
            "logical refresh demand predecessor event watermarks",
        )?;
        if admission_event_watermarks
            .keys()
            .chain(predecessor_event_watermarks.keys())
            .any(|route| !admission_route_observations.contains_key(route))
        {
            bail!(
                "logical refresh demand event boundary names a route outside its admission fence"
            );
        }
        let invalidated_routes = value
            .get("invalidated_routes")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("logical refresh demand has no invalidated route set"))?;
        if invalidated_routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
            bail!("logical refresh demand invalidated route set exceeds its route bound");
        }
        let invalidated_routes = invalidated_routes
            .iter()
            .map(|route| {
                route
                    .as_str()
                    .ok_or_else(|| anyhow!("logical refresh demand invalidated route is invalid"))
                    .and_then(|route| {
                        SourceRouteIdentity::from_sha256(route.to_owned()).map_err(Into::into)
                    })
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if invalidated_routes
            .iter()
            .any(|route| !admission_route_observations.contains_key(route))
        {
            bail!("logical refresh demand invalidates a route outside its admission fence");
        }
        let covered_value = value
            .get("covered_route_results")
            .ok_or_else(|| anyhow!("logical refresh demand has no covered route results"))?;
        let covered_route_results = required_route_results(Some(covered_value))?
            .into_iter()
            .map(|result| {
                let route = SourceRouteIdentity::from_sha256(result.route_identity.clone())?;
                Ok((route, result))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let covered_outside_fence = covered_route_results
            .keys()
            .filter(|route| !admission_route_observations.contains_key(*route))
            .map(|route| route.as_str())
            .collect::<Vec<_>>();
        if !covered_outside_fence.is_empty() {
            bail!(
                "logical refresh demand covers routes outside its admission fence: {}",
                covered_outside_fence.join(", ")
            );
        }
        let covered_removed_source_count = value
            .get("covered_removed_source_count")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| anyhow!("logical refresh demand removed-source count is invalid"))?;
        let covered_timings = value
            .get("covered_timings")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("logical refresh demand covered timings are invalid"))?;
        let covered_timings = SourceBackedRefreshTimings {
            discovery_us: covered_timings
                .get("discovery")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("logical refresh demand discovery timing is invalid"))?,
            scan_stage_us: covered_timings
                .get("scan_stage")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("logical refresh demand scan timing is invalid"))?,
            commit_us: covered_timings
                .get("commit")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("logical refresh demand commit timing is invalid"))?,
        };
        Ok(Self {
            predecessor_request_id,
            predecessor_finished,
            admission_route_observations,
            ledger_eligible_routes,
            admission_event_watermarks,
            predecessor_event_watermarks,
            invalidated_routes,
            covered_route_results,
            covered_removed_source_count,
            covered_timings,
        })
    }
}

fn event_watermarks_json(
    watermarks: &BTreeMap<SourceRouteIdentity, EventWatermark>,
) -> serde_json::Map<String, Value> {
    watermarks
        .iter()
        .map(|(route, watermark)| {
            (
                route.as_str().to_owned(),
                json!([watermark.watcher_epoch, watermark.sequence]),
            )
        })
        .collect()
}

fn event_watermarks_from_json(
    value: Option<&Value>,
    label: &str,
) -> Result<BTreeMap<SourceRouteIdentity, EventWatermark>> {
    let fields = value
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("{label} are invalid"))?;
    if fields.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
        bail!("{label} exceed the route bound");
    }
    fields
        .iter()
        .map(|(route, watermark)| {
            let route = SourceRouteIdentity::from_sha256(route.clone())?;
            let watermark = watermark
                .as_array()
                .filter(|watermark| watermark.len() == 2)
                .ok_or_else(|| anyhow!("{label} contain an invalid watermark"))?;
            let watcher_epoch = watermark[0]
                .as_u64()
                .ok_or_else(|| anyhow!("{label} contain an invalid watcher epoch"))?;
            let sequence = watermark[1]
                .as_u64()
                .ok_or_else(|| anyhow!("{label} contain an invalid sequence"))?;
            Ok((route, EventWatermark::new(watcher_epoch, sequence)))
        })
        .collect()
}
pub(in crate::semantic) struct SourceBackedRefreshRun {
    pub(in crate::semantic) job: Value,
    pub(in crate::semantic) did_work: bool,
    pub(in crate::semantic) failed: bool,
    pub(in crate::semantic) terminal_persistence_pending: bool,
    pub(in crate::semantic) scope: SourceBackedRefreshScope,
    pub(super) coverage_certificate: Option<SourceBackedRefreshCoverageCertificate>,
}

/// Coordinator-minted proof that exact routes were admitted before capture,
/// included in one verified Core publication, and acknowledged without a
/// newer watcher event crossing the admitted boundary.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(in crate::semantic) struct SourceBackedRefreshCoverageCertificate {
    pub(super) request_id: String,
    pub(super) published_generation: String,
    pub(super) routes: BTreeMap<SourceRouteIdentity, SourceBackedRefreshRouteCoverageCertificate>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct SourceBackedRefreshRouteCoverageCertificate {
    pub(super) observation: String,
    pub(super) admitted_watermark: EventWatermark,
}

/// Opaque authority for one exact route boundary in a verified publication.
///
/// Only the coordinator can construct this value, after binding the terminal
/// request to its retained verified generation and comparing the generation's
/// route observation with a post-publication sample. The dirty-route ledger
/// may inspect the route and watermark, but cannot mint the proof from raw
/// strings or a globally latest watcher position.
#[derive(Debug)]
pub(in crate::semantic) struct VerifiedSourceRefreshRouteBoundary<'a> {
    _request_id: &'a str,
    _published_generation: &'a str,
    route: &'a SourceRouteIdentity,
    covered_through: EventWatermark,
    _observation: &'a str,
}

impl<'a> VerifiedSourceRefreshRouteBoundary<'a> {
    pub(super) fn new(
        request_id: &'a str,
        published_generation: &'a str,
        route: &'a SourceRouteIdentity,
        covered_through: EventWatermark,
        observation: &'a str,
    ) -> Option<Self> {
        (!request_id.is_empty() && !published_generation.is_empty() && !observation.is_empty())
            .then_some(Self {
                _request_id: request_id,
                _published_generation: published_generation,
                route,
                covered_through,
                _observation: observation,
            })
    }

    pub(in crate::semantic) fn route(&self) -> &SourceRouteIdentity {
        self.route
    }

    pub(in crate::semantic) fn covered_through(&self) -> EventWatermark {
        self.covered_through
    }

    #[cfg(test)]
    pub(in crate::semantic) fn for_test(
        route: &'a SourceRouteIdentity,
        covered_through: EventWatermark,
    ) -> Self {
        Self {
            _request_id: "test-request",
            _published_generation: "test-generation",
            route,
            covered_through,
            _observation: "test-observation",
        }
    }
}

pub(in crate::semantic) struct PostPublicationRouteCoverageFence {
    pub(super) seen_watermarks: BTreeMap<SourceRouteIdentity, EventWatermark>,
    pub(super) sampled_observations: BTreeMap<SourceRouteIdentity, Option<String>>,
}

impl PostPublicationRouteCoverageFence {
    pub(super) fn fail_closed() -> Self {
        Self {
            seen_watermarks: BTreeMap::new(),
            sampled_observations: BTreeMap::new(),
        }
    }

    pub(in crate::semantic) fn certified_boundary(
        &self,
        route: &SourceRouteIdentity,
        admitted_watermark: EventWatermark,
        verified_observation: &str,
    ) -> EventWatermark {
        let observed_matches = self
            .sampled_observations
            .get(route)
            .and_then(Option::as_deref)
            == Some(verified_observation);
        if !observed_matches {
            return admitted_watermark;
        }
        self.seen_watermarks
            .get(route)
            .copied()
            .map_or(admitted_watermark, |seen| admitted_watermark.max(seen))
    }
}

#[allow(dead_code)] // Public integration seam consumed by #282.
impl SourceBackedRefreshRun {
    pub(in crate::semantic) fn coverage_certificate(
        &self,
    ) -> Option<&SourceBackedRefreshCoverageCertificate> {
        self.coverage_certificate.as_ref()
    }
}

#[allow(dead_code)] // Public integration seam consumed by #282.
impl SourceBackedRefreshCoverageCertificate {
    pub(in crate::semantic) fn request_id(&self) -> &str {
        &self.request_id
    }

    pub(in crate::semantic) fn published_generation(&self) -> &str {
        &self.published_generation
    }

    /// Exact route/event boundaries safe for an acknowledge-through update.
    /// A consumer must clear only through each returned watermark, never
    /// through a later globally observed watcher position.
    pub(in crate::semantic) fn exact_route_boundaries(
        &self,
    ) -> impl ExactSizeIterator<Item = (&SourceRouteIdentity, EventWatermark, &str)> {
        self.routes.iter().map(|(route, certificate)| {
            (
                route,
                certificate.admitted_watermark,
                certificate.observation.as_str(),
            )
        })
    }
}
#[cfg(test)]
mod coverage_certificate_tests {
    use super::*;

    #[test]
    fn matching_post_publication_observation_covers_through_seen_event() {
        let route = SourceRouteIdentity::from_sha256("81".repeat(32)).unwrap();
        let admitted = EventWatermark::new(4, 1);
        let seen_during_capture = EventWatermark::new(4, 2);
        let observation = "91".repeat(32);
        let fence = PostPublicationRouteCoverageFence {
            seen_watermarks: BTreeMap::from([(route.clone(), seen_during_capture)]),
            sampled_observations: BTreeMap::from([(route.clone(), Some(observation.clone()))]),
        };

        let boundary = fence.certified_boundary(&route, admitted, &observation);
        let certificate = SourceBackedRefreshCoverageCertificate {
            request_id: Uuid::from_u128(0x28107).to_string(),
            published_generation: "verified-generation".to_owned(),
            routes: BTreeMap::from([(
                route.clone(),
                SourceBackedRefreshRouteCoverageCertificate {
                    observation: observation.clone(),
                    admitted_watermark: boundary,
                },
            )]),
        };

        assert_eq!(
            certificate.request_id(),
            Uuid::from_u128(0x28107).to_string()
        );
        assert_eq!(certificate.published_generation(), "verified-generation");
        assert_eq!(
            certificate.exact_route_boundaries().collect::<Vec<_>>(),
            vec![(&route, seen_during_capture, observation.as_str())]
        );
    }

    #[test]
    fn event_after_seen_fence_survives_and_indeterminate_sample_does_not_extend() {
        let route = SourceRouteIdentity::from_sha256("82".repeat(32)).unwrap();
        let admitted = EventWatermark::new(5, 1);
        let seen_fence = EventWatermark::new(5, 2);
        let event_after_fence = EventWatermark::new(5, 3);
        let observation = "92".repeat(32);
        let matching = PostPublicationRouteCoverageFence {
            seen_watermarks: BTreeMap::from([(route.clone(), seen_fence)]),
            sampled_observations: BTreeMap::from([(route.clone(), Some(observation.clone()))]),
        };
        let indeterminate = PostPublicationRouteCoverageFence {
            seen_watermarks: BTreeMap::from([(route.clone(), seen_fence)]),
            sampled_observations: BTreeMap::from([(route.clone(), None)]),
        };

        let certified = matching.certified_boundary(&route, admitted, &observation);
        assert_eq!(certified, seen_fence);
        assert!(event_after_fence > certified);
        assert_eq!(
            indeterminate.certified_boundary(&route, admitted, &observation),
            admitted
        );
    }

    #[test]
    fn verified_boundary_acknowledges_during_capture_event_but_not_post_fence_event() {
        let route = SourceRouteIdentity::from_sha256("83".repeat(32)).unwrap();
        let admitted = EventWatermark::new(6, 1);
        let seen_fence = EventWatermark::new(6, 2);
        let event_after_fence = EventWatermark::new(6, 3);
        let observation = "93".repeat(32);
        let mut ledger = DirtySourceRoutes::default();

        assert!(ledger.record_event(route.clone(), admitted, 0));
        let admission = ledger.admit_next(250).unwrap();
        assert!(ledger.record_event(route.clone(), seen_fence, 300));
        let fence = PostPublicationRouteCoverageFence {
            seen_watermarks: BTreeMap::from([(route.clone(), seen_fence)]),
            sampled_observations: BTreeMap::from([(route.clone(), Some(observation.clone()))]),
        };
        let boundary = VerifiedSourceRefreshRouteBoundary::new(
            "request",
            "generation",
            &route,
            fence.certified_boundary(&route, admitted, &observation),
            &observation,
        )
        .unwrap();

        assert!(ledger.acknowledge_generation_coverage(&admission, &boundary));
        assert!(ledger.is_empty());

        assert!(ledger.record_event(route.clone(), event_after_fence, 350));
        assert_eq!(ledger.next_due_at_ms(), Some(600));
        assert_eq!(
            ledger.admit_next(600).unwrap().watermark(),
            event_after_fence
        );
    }
}
