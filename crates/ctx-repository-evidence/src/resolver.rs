use std::path::PathBuf;

use crate::model::{MISSING_ACTIVITY_TIME_UNIX_MS, RepositoryEvaluation};

use super::{
    NeutralRepositoryFacts,
    evaluation::{Candidate, evaluate_with_resolver},
    git::{
        self, CandidateKind, CertifiedCandidate, EventProbeBudget, GitCertifier, ProbeFailure,
        negative_route_geometry_state,
    },
};

const MAX_POSITIVE_CERTIFICATION_CACHE_ENTRIES: usize = 32;
const MAX_NEGATIVE_CERTIFICATION_CACHE_ENTRIES: usize = 64;
const MAX_SOURCE_UNSAFE_CACHE_ENTRIES: usize = 64;
const MAX_EVENT_TIME_CERTIFICATION_ENTRIES: usize = 256;

#[derive(Debug, Default)]
pub struct RepositoryEvidenceResolver {
    pub(super) certifier: GitCertifier,
    positive_cache: Vec<CachedPositiveCertificate>,
    negative_cache: Vec<CachedNegativeCertificate>,
    source_unsafe_cache: Vec<CachedSourceUnsafeOutcome>,
    event_time_cache: Vec<CachedEventTimeCertificate>,
    cache_clock: u64,
}

#[derive(Debug, Clone)]
struct CachedPositiveCertificate {
    certificate: CertifiedCandidate,
    last_used: u64,
}

#[derive(Debug, Clone)]
struct CachedNegativeCertificate {
    path: PathBuf,
    kind: CandidateKind,
    route_geometry_state: [u8; 32],
    failure: ProbeFailure,
    last_used: u64,
}

#[derive(Debug, Clone)]
struct CachedSourceUnsafeOutcome {
    path: PathBuf,
    kind: CandidateKind,
    detail: &'static str,
    last_used: u64,
}

#[derive(Debug, Clone)]
struct CachedEventTimeCertificate {
    path: PathBuf,
    kind: CandidateKind,
    observed_at_unix_ms: i64,
    certificate: CertifiedCandidate,
    certified_move_at_unix_ms: Option<i64>,
    last_used: u64,
}

impl RepositoryEvidenceResolver {
    /// Starts an independent provider source while retaining only the
    /// revalidated positive and negative certification caches. Historical
    /// event-time bindings are source-semantic state and must not cross a
    /// shared scanner worker's leaf boundary. Unsafe-route outcomes are also
    /// leaf-scoped: repeatedly walking an already-rejected symlink or
    /// automount route once per historical event adds no source evidence.
    pub fn begin_source(&mut self) {
        self.source_unsafe_cache.clear();
        self.event_time_cache.clear();
    }

    #[doc(hidden)]
    pub fn event_time_cache_len(&self) -> usize {
        self.event_time_cache.len()
    }

    pub fn evaluate(&mut self, input: NeutralRepositoryFacts) -> RepositoryEvaluation {
        // Positive certificates authorize the route observed during this event
        // only. Unsafe routes remain rejected through the current immutable
        // provider source and revalidate at the next source boundary.
        evaluate_with_resolver(input, self)
    }

    pub(super) fn certify(
        &mut self,
        candidate: &Candidate,
        observed_at_unix_ms: i64,
        budget: &mut EventProbeBudget,
    ) -> Result<CertifiedCandidate, ProbeFailure> {
        self.cache_clock = self.cache_clock.saturating_add(1);
        let now = self.cache_clock;
        if let Some(cached) = self
            .source_unsafe_cache
            .iter_mut()
            .find(|cached| cached.path == candidate.path && cached.kind == candidate.kind)
        {
            budget.start_cached_full_certification()?;
            cached.last_used = now;
            return Err(ProbeFailure::Unsafe(cached.detail));
        }
        let positive = self
            .positive_cache
            .iter()
            .enumerate()
            .filter(|(_, cached)| cached.certificate.lexical_root_contains(&candidate.path))
            .max_by_key(|(_, cached)| cached.certificate.repository_root.components().count())
            .map(|(index, _)| index);
        if let Some(index) = positive {
            let cached = self.positive_cache[index].certificate.clone();
            match cached.try_reuse(
                &candidate.path,
                candidate.kind,
                candidate.evidence_kind,
                observed_at_unix_ms,
            ) {
                Ok(Some(reused)) => {
                    self.record_live_event_time_certificate(candidate, &reused, now)?;
                    self.positive_cache[index].last_used = now;
                    return Ok(reused);
                }
                Ok(None) => {
                    self.positive_cache.remove(index);
                }
                Err(ProbeFailure::Missing) => {
                    if let Some(reused) =
                        self.try_reuse_moved_event_time_certificate(candidate, now)?
                    {
                        return Ok(reused);
                    }
                    return Err(ProbeFailure::Missing);
                }
                Err(failure) => {
                    if let ProbeFailure::Unsafe(detail) = &failure {
                        self.remember_source_unsafe(candidate, detail, now);
                    }
                    return Err(failure);
                }
            }
        }

        if let Some(state) = negative_route_geometry_state(&candidate.path, candidate.kind) {
            if let Some(index) = self.negative_cache.iter().position(|cached| {
                cached.path == candidate.path
                    && cached.kind == candidate.kind
                    && cached.route_geometry_state == state
            }) {
                self.negative_cache[index].last_used = now;
                let failure = self.negative_cache[index].failure.clone();
                if failure == ProbeFailure::Missing
                    && let Some(reused) =
                        self.try_reuse_moved_event_time_certificate(candidate, now)?
                {
                    return Ok(reused);
                }
                return Err(failure);
            }
            self.negative_cache
                .retain(|cached| cached.path != candidate.path || cached.kind != candidate.kind);
        }

        let result = self.certifier.certify_at_with_budget(
            &candidate.path,
            candidate.kind,
            candidate.evidence_kind,
            observed_at_unix_ms,
            budget,
        );
        let result = match result {
            Ok(certificate) => {
                self.record_live_event_time_certificate(candidate, &certificate, now)?;
                Ok(certificate)
            }
            Err(ProbeFailure::Missing) => {
                match self.try_reuse_moved_event_time_certificate(candidate, now)? {
                    Some(reused) => Ok(reused),
                    None => Err(ProbeFailure::Missing),
                }
            }
            Err(failure) => Err(failure),
        };
        match &result {
            Ok(certificate) => {
                self.negative_cache.retain(|cached| {
                    cached.path != candidate.path || cached.kind != candidate.kind
                });
                self.positive_cache.retain(|cached| {
                    cached.certificate.repository_root != certificate.repository_root
                });
                self.positive_cache.push(CachedPositiveCertificate {
                    certificate: certificate.clone(),
                    last_used: now,
                });
                evict_oldest_positive(&mut self.positive_cache);
            }
            Err(failure) if cacheable_negative(failure) => {
                if let Some(state) = negative_route_geometry_state(&candidate.path, candidate.kind)
                {
                    self.negative_cache.push(CachedNegativeCertificate {
                        path: candidate.path.clone(),
                        kind: candidate.kind,
                        route_geometry_state: state,
                        failure: failure.clone(),
                        last_used: now,
                    });
                    evict_oldest_negative(&mut self.negative_cache);
                }
            }
            Err(ProbeFailure::Unsafe(detail)) => {
                self.remember_source_unsafe(candidate, detail, now);
            }
            Err(_) => {}
        }
        result
    }

    fn remember_source_unsafe(&mut self, candidate: &Candidate, detail: &'static str, now: u64) {
        self.source_unsafe_cache
            .retain(|cached| cached.path != candidate.path || cached.kind != candidate.kind);
        self.source_unsafe_cache.push(CachedSourceUnsafeOutcome {
            path: candidate.path.clone(),
            kind: candidate.kind,
            detail,
            last_used: now,
        });
        evict_oldest_source_unsafe(&mut self.source_unsafe_cache);
    }

    fn record_live_event_time_certificate(
        &mut self,
        candidate: &Candidate,
        certificate: &CertifiedCandidate,
        now: u64,
    ) -> Result<(), ProbeFailure> {
        let observed_at_unix_ms = certificate.observed_at_unix_ms();
        if observed_at_unix_ms == MISSING_ACTIVITY_TIME_UNIX_MS {
            return Ok(());
        }

        if self.event_time_cache.iter().any(|cached| {
            cached.path == candidate.path
                && cached.kind == candidate.kind
                && cached.observed_at_unix_ms == observed_at_unix_ms
                && !cached.certificate.same_binding_identity(certificate)
        }) {
            return Err(ProbeFailure::ConflictingEventTimeIdentity);
        }

        for cached in &mut self.event_time_cache {
            if cached.certificate.repository_root == certificate.repository_root
                || cached.observed_at_unix_ms >= observed_at_unix_ms
                || !cached.certificate.same_binding_identity(certificate)
                || !cached
                    .certificate
                    .same_local_root_authorization_identity(certificate)
            {
                continue;
            }
            if matches!(
                git::validate_candidate_route(
                    &cached.certificate.repository_root,
                    CandidateKind::Directory,
                ),
                Err(ProbeFailure::Missing)
            ) {
                cached.certified_move_at_unix_ms = Some(
                    cached
                        .certified_move_at_unix_ms
                        .map_or(observed_at_unix_ms, |existing| {
                            existing.min(observed_at_unix_ms)
                        }),
                );
            }
        }

        if let Some(cached) = self.event_time_cache.iter_mut().find(|cached| {
            cached.path == candidate.path
                && cached.kind == candidate.kind
                && cached.observed_at_unix_ms == observed_at_unix_ms
                && cached.certificate.same_binding_identity(certificate)
        }) {
            cached.certificate = certificate.clone();
            cached.last_used = now;
            return Ok(());
        }
        self.event_time_cache.push(CachedEventTimeCertificate {
            path: candidate.path.clone(),
            kind: candidate.kind,
            observed_at_unix_ms,
            certificate: certificate.clone(),
            certified_move_at_unix_ms: None,
            last_used: now,
        });
        evict_oldest_event_time(&mut self.event_time_cache);
        Ok(())
    }

    fn try_reuse_moved_event_time_certificate(
        &mut self,
        candidate: &Candidate,
        now: u64,
    ) -> Result<Option<CertifiedCandidate>, ProbeFailure> {
        let observed_at_unix_ms = candidate.observed_at_unix_ms;
        if observed_at_unix_ms == MISSING_ACTIVITY_TIME_UNIX_MS {
            return Ok(None);
        }
        let matching = self
            .event_time_cache
            .iter()
            .enumerate()
            .filter(|(_, cached)| {
                cached.path == candidate.path
                    && cached.kind == candidate.kind
                    && cached.observed_at_unix_ms == observed_at_unix_ms
                    && cached
                        .certified_move_at_unix_ms
                        .is_some_and(|moved_at| moved_at > observed_at_unix_ms)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let Some((&first, rest)) = matching.split_first() else {
            return Ok(None);
        };
        if rest.iter().any(|index| {
            !self.event_time_cache[*index]
                .certificate
                .same_binding_identity(&self.event_time_cache[first].certificate)
        }) {
            return Err(ProbeFailure::ConflictingEventTimeIdentity);
        }
        let mut reused = self.event_time_cache[first]
            .certificate
            .for_event(candidate.evidence_kind, observed_at_unix_ms);
        // The historical certificate proves stable repository identity at the
        // event's timestamp. It does not re-authorize a route that is missing
        // now, so never project its former local-root authorization.
        reused.binding.local_root_authorization = None;
        for index in matching {
            self.event_time_cache[index].last_used = now;
        }
        Ok(Some(reused))
    }

    pub fn full_certification_probe_count(&self) -> usize {
        self.certifier.full_certification_probe_count()
    }

    #[cfg(test)]
    pub(crate) fn git_subprocess_count(&self) -> usize {
        self.certifier.git_subprocess_count()
    }
}

fn cacheable_negative(failure: &ProbeFailure) -> bool {
    matches!(
        failure,
        ProbeFailure::Missing
            | ProbeFailure::Failed(
                "git_command_failed" | "unexpected_git_geometry" | "unsupported_git_object_format"
            )
    )
}

fn evict_oldest_positive(cache: &mut Vec<CachedPositiveCertificate>) {
    if cache.len() <= MAX_POSITIVE_CERTIFICATION_CACHE_ENTRIES {
        return;
    }
    if let Some(index) = cache
        .iter()
        .enumerate()
        .min_by_key(|(_, cached)| {
            (
                cached.last_used,
                cached.certificate.repository_root.as_os_str(),
            )
        })
        .map(|(index, _)| index)
    {
        cache.remove(index);
    }
}

fn evict_oldest_negative(cache: &mut Vec<CachedNegativeCertificate>) {
    if cache.len() <= MAX_NEGATIVE_CERTIFICATION_CACHE_ENTRIES {
        return;
    }
    if let Some(index) = cache
        .iter()
        .enumerate()
        .min_by_key(|(_, cached)| (cached.last_used, cached.path.as_os_str()))
        .map(|(index, _)| index)
    {
        cache.remove(index);
    }
}

fn evict_oldest_source_unsafe(cache: &mut Vec<CachedSourceUnsafeOutcome>) {
    if cache.len() <= MAX_SOURCE_UNSAFE_CACHE_ENTRIES {
        return;
    }
    if let Some(index) = cache
        .iter()
        .enumerate()
        .min_by_key(|(_, cached)| (cached.last_used, cached.path.as_os_str()))
        .map(|(index, _)| index)
    {
        cache.remove(index);
    }
}

fn evict_oldest_event_time(cache: &mut Vec<CachedEventTimeCertificate>) {
    if cache.len() <= MAX_EVENT_TIME_CERTIFICATION_ENTRIES {
        return;
    }
    if let Some(index) = cache
        .iter()
        .enumerate()
        .min_by_key(|(_, cached)| (cached.last_used, cached.path.as_os_str()))
        .map(|(index, _)| index)
    {
        cache.remove(index);
    }
}
