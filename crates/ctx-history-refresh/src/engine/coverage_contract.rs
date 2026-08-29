use super::*;
pub struct SourceBackedRefreshRun {
    pub job: Value,
    pub did_work: bool,
    pub failed: bool,
    pub terminal_persistence_pending: bool,
    pub scope: SourceBackedRefreshScope,
    pub(super) coverage_certificate: Option<SourceBackedRefreshCoverageCertificate>,
    pub(super) route_finalization_performed: bool,
}

/// Coordinator-minted proof that exact routes were admitted before capture,
/// included in one verified Core publication, and acknowledged without a
/// newer watcher event crossing the admitted boundary.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedRefreshCoverageCertificate {
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
pub struct VerifiedSourceRefreshRouteBoundary<'a> {
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

    pub fn route(&self) -> &SourceRouteIdentity {
        self.route
    }

    pub fn covered_through(&self) -> EventWatermark {
        self.covered_through
    }

    #[cfg(test)]
    pub fn for_test(route: &'a SourceRouteIdentity, covered_through: EventWatermark) -> Self {
        Self {
            _request_id: "test-request",
            _published_generation: "test-generation",
            route,
            covered_through,
            _observation: "test-observation",
        }
    }
}

pub struct PostPublicationRouteCoverageFence {
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

    pub fn certified_boundary(
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
    pub fn coverage_certificate(&self) -> Option<&SourceBackedRefreshCoverageCertificate> {
        self.coverage_certificate.as_ref()
    }
}

#[allow(dead_code)] // Public integration seam consumed by #282.
impl SourceBackedRefreshCoverageCertificate {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn published_generation(&self) -> &str {
        &self.published_generation
    }

    /// Exact route/event boundaries safe for an acknowledge-through update.
    /// A consumer must clear only through each returned watermark, never
    /// through a later globally observed watcher position.
    pub fn exact_route_boundaries(
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
