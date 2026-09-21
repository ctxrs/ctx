// Preserve the existing bounded answer size without transport envelopes.
const MAX_BLAME_RESULT_BYTES: usize = 80 * 1024 * 1024;
use std::collections::{BTreeMap, HashMap};

use crate::graph::segment_graph::{SegmentGraph, SegmentGraphError};
use crate::protocol::{
    AgentAttribution, BlameAttribution, BlameContinuation, BlameCoverage, BlameCoverageUnit,
    BlameMatch, BlameOutcome, BlameRequest, BlameResult, CommitBlameMatch, CommitFactType,
    CommitPredicate, ContinuationReason, FactConfidence, FactState, FileBlameMatch,
    NumberedEvidence, PullRequestActivity, PullRequestBlameMatch, PullRequestBlameRelationship,
    PullRequestCommit, QuerySnapshotExpectation, ResolvedBlameTarget,
};
use crate::query::{
    AttributionOutcome, BlameEntry, BlamePage, BlameService, Citation, CommitBlameEntry,
    Confidence, FactState as InternalFactState, ProductionAttribution, QueryBounds, Resource,
    ResourceId, attribution_outcome,
};

mod commit_lineage;
use commit_lineage::{compose_commit_lineage, public_commit_lineage};

impl SegmentGraph {
    /// Executes one typed file, commit, or pull-request blame request.
    ///
    /// # Errors
    ///
    /// Fails closed when the requested snapshot is incomplete or stale, the
    /// target cannot be resolved exactly, or the bounded result cannot be
    /// represented by the frozen protocol.
    pub fn blame_request(&self, request: &BlameRequest) -> Result<BlameResult, SegmentGraphError> {
        self.blame_request_with_interleave(request, || Ok(()))
    }

    fn blame_request_with_interleave<F>(
        &self,
        request: &BlameRequest,
        interleave: F,
    ) -> Result<BlameResult, SegmentGraphError>
    where
        F: FnOnce() -> Result<(), SegmentGraphError>,
    {
        request
            .validate()
            .map_err(|error| SegmentGraphError::InvalidInput(error.message))?;
        let graph = self;
        let graph_generation = graph.graph_generation();
        self.verify_query_snapshot(request)?;
        if request.target.requires_git_read() && !self.git_authority_available() {
            return Err(SegmentGraphError::GitAuthorityUnavailable);
        }
        interleave()?;
        let bounds = QueryBounds {
            max_matches: usize::try_from(request.limit).map_err(|_| {
                SegmentGraphError::InvalidInput("blame limit is invalid".to_owned())
            })?,
            max_attributions_per_match: crate::protocol::MAX_BLAME_ATTRIBUTIONS_PER_MATCH,
        };
        let service = BlameService::new(graph, bounds)?;
        let resolved = service.resolve_target(&request.target)?;
        let resume = request
            .cursor
            .as_deref()
            .map(|cursor| self.decode_blame_cursor(cursor, &resolved, graph_generation))
            .transpose()?;
        let page = service.execute(&request.target, resume.as_ref())?;
        if page.target != resolved {
            return Err(SegmentGraphError::InvalidInput(
                "resolved blame target changed during execution".to_owned(),
            ));
        }
        let lineage = compose_commit_lineage(graph, &page.target)?;
        let result = self.bound_blame_page(request, &page, lineage.as_ref(), graph_generation)?;
        self.verify_query_snapshot(request)?;
        if graph.graph_generation() != graph_generation {
            return Err(SegmentGraphError::StaleCursor);
        }
        result
            .validate_for_request(request)
            .map_err(|error| SegmentGraphError::InvalidInput(error.message))?;
        Ok(result)
    }

    fn verify_query_snapshot(&self, request: &BlameRequest) -> Result<(), SegmentGraphError> {
        let QuerySnapshotExpectation::Core { receipt: expected } = &request.expected_snapshot;
        let actual = crate::protocol::CoreMaterializationReceiptIdentity::from_receipt(
            self.completed_receipt(),
        )
        .map_err(|error| SegmentGraphError::InvalidInput(error.message))?;
        if actual != *expected {
            return Err(SegmentGraphError::MaterializationRequired);
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn bound_blame_page(
        &self,
        request: &BlameRequest,
        page: &BlamePage,
        lineage: Option<&crate::query::commit_lineage::CommitLineageDraft>,
        graph_generation: u64,
    ) -> Result<BlameResult, SegmentGraphError> {
        if page.entries.len() != page.positions.len() {
            return Err(SegmentGraphError::InvalidInput(
                "blame page lost its continuation positions".to_owned(),
            ));
        }
        let original_len = page.entries.len();
        let mut retained = original_len;
        loop {
            let frame_clipped = retained < original_len;
            let has_more = page.has_more || frame_clipped;
            let next = if has_more {
                let position = page
                    .positions
                    .get(
                        retained
                            .checked_sub(1)
                            .ok_or(SegmentGraphError::QueryRecordTooLarge)?,
                    )
                    .ok_or(SegmentGraphError::QueryRecordTooLarge)?;
                Some(BlameContinuation {
                    cursor: self.encode_blame_cursor(&page.target, graph_generation, position)?,
                    reason: if frame_clipped {
                        ContinuationReason::MoreMatches
                    } else {
                        page.continuation_reason
                    },
                })
            } else {
                None
            };
            let result = render_page(page, retained, next, &request.expected_snapshot, lineage)?;
            let encoded = serde_json::to_vec(&result)
                .map_err(|_| SegmentGraphError::InvalidInput("blame encoding failed".to_owned()))?
                .len();
            if result.evidence.len() <= crate::protocol::MAX_BLAME_EVIDENCE
                && encoded <= MAX_BLAME_RESULT_BYTES
            {
                return Ok(result);
            }
            retained = retained
                .checked_sub(1)
                .ok_or(SegmentGraphError::QueryRecordTooLarge)?;
            if retained == 0 {
                return Err(SegmentGraphError::QueryRecordTooLarge);
            }
        }
    }
}

#[doc(hidden)]
pub fn render_page(
    page: &BlamePage,
    retained: usize,
    next: Option<BlameContinuation>,
    snapshot: &QuerySnapshotExpectation,
    lineage: Option<&crate::query::commit_lineage::CommitLineageDraft>,
) -> Result<BlameResult, SegmentGraphError> {
    let resources = page
        .resources
        .iter()
        .map(|resource| (resource.id.clone(), resource))
        .collect::<BTreeMap<_, _>>();
    let mut evidence = EvidencePage::default();
    let mut matches = Vec::new();
    let mut coverage = BlameCoverage {
        unit: coverage_unit(&page.target),
        evaluated: 0,
        proven: 0,
        possible: 0,
        conflicting: 0,
        none: 0,
    };
    for entry in page.entries.iter().take(retained) {
        let (attribution, evaluated) = entry_coverage(entry)?;
        add_coverage(&mut coverage, attribution, evaluated)?;
        let projected = match entry {
            BlameEntry::File(entry) => BlameMatch::File(FileBlameMatch {
                id: entry.id.clone(),
                lines: entry.lines.clone(),
                commit: public_resource(&entry.commit),
                line_evidence_numbers: evidence.numbers(&entry.line_citations)?,
                production: entry
                    .production
                    .iter()
                    .map(|attribution| public_attribution(attribution, &resources, &mut evidence))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            BlameEntry::Commit(entry) => BlameMatch::Commit(public_commit(entry, &mut evidence)?),
            BlameEntry::PullRequestActivity(entry) => {
                BlameMatch::PullRequest(PullRequestBlameMatch {
                    pull_request: public_resource(&entry.pull_request),
                    relationship: PullRequestBlameRelationship::Activity(PullRequestActivity {
                        fact_id: entry.fact.id.clone(),
                        action: entry.action,
                        session: public_resource(&entry.session),
                        direct_actor: entry.direct_actor.as_ref().map(public_resource),
                        owning_root: entry.owning_root.as_ref().map(public_resource),
                        fact_occurred_at_ms: entry.fact.occurred_at_ms,
                        confidence: public_confidence(entry.fact.confidence),
                        state: public_state(entry.fact.state),
                        evidence_numbers: evidence.numbers(&entry.fact.citations)?,
                    }),
                })
            }
            BlameEntry::PullRequestCommit(entry) => {
                BlameMatch::PullRequest(PullRequestBlameMatch {
                    pull_request: public_resource(&entry.pull_request),
                    relationship: PullRequestBlameRelationship::Commit(PullRequestCommit {
                        fact_id: entry.fact.id.clone(),
                        relationship: entry.relationship,
                        commit: public_resource(&entry.commit),
                        fact_occurred_at_ms: entry.fact.occurred_at_ms,
                        production: entry
                            .production
                            .iter()
                            .map(|attribution| {
                                public_attribution(attribution, &resources, &mut evidence)
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                        evidence_numbers: evidence.numbers(&entry.fact.citations)?,
                    }),
                })
            }
        };
        matches.push(projected);
        if evidence.values.len() > crate::protocol::MAX_BLAME_EVIDENCE {
            // This render attempt is discarded by `bound_blame_page`. Stop at
            // the first overflowing top-level match so the keyed lookup stays
            // bounded while the caller clips and retries with fewer matches.
            break;
        }
    }
    let lineage = lineage
        .map(|lineage| public_commit_lineage(lineage, &mut evidence))
        .transpose()?;
    Ok(BlameResult {
        snapshot: snapshot.clone(),
        target: page.target.clone(),
        git_snapshot: page.git_snapshot.clone(),
        outcome: BlameOutcome {
            attribution: aggregate_attribution(&coverage),
            coverage,
        },
        matches,
        evidence: evidence.values,
        next,
        lineage,
    })
}

#[doc(hidden)]
pub const fn coverage_unit(target: &ResolvedBlameTarget) -> BlameCoverageUnit {
    match target {
        ResolvedBlameTarget::File { .. } => BlameCoverageUnit::CommittedLine,
        ResolvedBlameTarget::Commit { .. } => BlameCoverageUnit::CommitFact,
        ResolvedBlameTarget::PullRequest { .. } => BlameCoverageUnit::PullRequestRelationship,
    }
}

fn entry_coverage(entry: &BlameEntry) -> Result<(AttributionOutcome, u32), SegmentGraphError> {
    match entry {
        BlameEntry::File(entry) => {
            let evaluated = entry
                .lines
                .end
                .checked_sub(entry.lines.start)
                .and_then(|span| span.checked_add(1))
                .ok_or(SegmentGraphError::QueryRecordTooLarge)?;
            Ok((attribution_outcome(&entry.production), evaluated))
        }
        BlameEntry::Commit(entry) => Ok((entry.attribution, 1)),
        BlameEntry::PullRequestActivity(entry) => Ok((fact_attribution(entry.fact.state), 1)),
        BlameEntry::PullRequestCommit(entry) => Ok((attribution_outcome(&entry.production), 1)),
    }
}

fn fact_attribution(state: InternalFactState) -> AttributionOutcome {
    match state {
        InternalFactState::Asserted => AttributionOutcome::Proven,
        InternalFactState::Ambiguous => AttributionOutcome::Possible,
        InternalFactState::Contradicted | InternalFactState::Superseded => AttributionOutcome::None,
    }
}

fn add_coverage(
    coverage: &mut BlameCoverage,
    attribution: AttributionOutcome,
    evaluated: u32,
) -> Result<(), SegmentGraphError> {
    coverage.evaluated = coverage
        .evaluated
        .checked_add(evaluated)
        .ok_or(SegmentGraphError::QueryRecordTooLarge)?;
    let count = match attribution {
        AttributionOutcome::Proven => &mut coverage.proven,
        AttributionOutcome::Possible => &mut coverage.possible,
        AttributionOutcome::Conflicting => &mut coverage.conflicting,
        AttributionOutcome::None => &mut coverage.none,
    };
    *count = count
        .checked_add(evaluated)
        .ok_or(SegmentGraphError::QueryRecordTooLarge)?;
    Ok(())
}

fn aggregate_attribution(coverage: &BlameCoverage) -> BlameAttribution {
    if coverage.conflicting > 0 {
        BlameAttribution::Conflicting
    } else if coverage.evaluated > 0 && coverage.proven == coverage.evaluated {
        BlameAttribution::Proven
    } else if coverage.proven > 0 || coverage.possible > 0 {
        BlameAttribution::Possible
    } else {
        BlameAttribution::None
    }
}

fn public_commit(
    entry: &CommitBlameEntry,
    evidence: &mut EvidencePage,
) -> Result<CommitBlameMatch, SegmentGraphError> {
    let (fact_type, predicate) = match entry.fact.fact_type.as_str() {
        "git.commit.produced" => (CommitFactType::Produced, CommitPredicate::ProducedBy),
        "git.commit.ambiguous" => (
            CommitFactType::Ambiguous,
            CommitPredicate::PossiblyProducedBy,
        ),
        "git.commit.amended" => (CommitFactType::Amended, CommitPredicate::AmendedBy),
        "git.commit.cherry_picked" => (
            CommitFactType::CherryPicked,
            CommitPredicate::CherryPickedFrom,
        ),
        "git.commit.reverted" => (CommitFactType::Reverted, CommitPredicate::Reverts),
        "git.commit.pushed" => (CommitFactType::Pushed, CommitPredicate::PushedBy),
        "git.commit.inspected" => (CommitFactType::Inspected, CommitPredicate::InspectedBy),
        "git.commit.referenced" => (CommitFactType::Referenced, CommitPredicate::ReferencedBy),
        _ => {
            return Err(SegmentGraphError::InvalidInput(
                "commit allowlist returned an unsupported fact".to_owned(),
            ));
        }
    };
    let confidence = public_confidence(entry.fact.confidence);
    let state = public_state(entry.fact.state);
    let (fact_type, predicate, confidence, state) = match (fact_type, state) {
        (CommitFactType::Produced, FactState::Asserted) => {
            (fact_type, predicate, confidence, state)
        }
        // A production observation whose repository authority was demoted is
        // evidence for a possible producer, not an asserted production fact.
        // Normalize both state and confidence to the public protocol's single
        // coherent representation of that uncertainty.
        (CommitFactType::Produced | CommitFactType::Ambiguous, FactState::Ambiguous) => (
            CommitFactType::Ambiguous,
            CommitPredicate::PossiblyProducedBy,
            FactConfidence::Ambiguous,
            FactState::Ambiguous,
        ),
        (CommitFactType::Produced | CommitFactType::Ambiguous, _) => {
            return Err(SegmentGraphError::InvalidInput(
                "commit allowlist returned inconsistent production semantics".to_owned(),
            ));
        }
        (_, _) => (fact_type, predicate, confidence, state),
    };
    Ok(CommitBlameMatch {
        fact_id: entry.fact.id.clone(),
        fact_type,
        predicate,
        subject: public_resource(&entry.subject),
        object: entry.object.as_ref().map(public_resource),
        parent_session: entry.parent_session.as_ref().map(public_resource),
        fact_occurred_at_ms: entry.fact.occurred_at_ms,
        confidence,
        state,
        direct_actor: entry.direct_actor.as_ref().map(public_resource),
        owning_root: entry.owning_root.as_ref().map(public_resource),
        evidence_numbers: evidence.numbers(&entry.fact.citations)?,
    })
}

fn public_attribution(
    attribution: &ProductionAttribution,
    resources: &BTreeMap<ResourceId, &Resource>,
    evidence: &mut EvidencePage,
) -> Result<AgentAttribution, SegmentGraphError> {
    let resource = |id: &ResourceId| {
        resources
            .get(id)
            .copied()
            .map(public_resource)
            .ok_or_else(|| {
                SegmentGraphError::InvalidInput(
                    "production attribution references a missing resource".to_owned(),
                )
            })
    };
    Ok(AgentAttribution {
        id: attribution.fact_id.clone(),
        relationship: attribution.relationship,
        producing_session: resource(&attribution.producing_session)?,
        parent_session: attribution
            .parent_session
            .as_ref()
            .map(resource)
            .transpose()?,
        direct_actor: attribution
            .direct_actor
            .as_ref()
            .map(resource)
            .transpose()?,
        owning_root: attribution.root_run.as_ref().map(resource).transpose()?,
        fact_occurred_at_ms: attribution.fact_occurred_at_ms,
        confidence: public_confidence(attribution.confidence),
        state: public_state(attribution.state),
        evidence_numbers: evidence.numbers(&attribution.citations)?,
    })
}

#[doc(hidden)]
pub fn public_resource(resource: &Resource) -> crate::protocol::ResourceRef {
    crate::protocol::ResourceRef {
        id: resource.id.0.clone(),
        kind: resource.kind,
        display: resource.display.clone(),
    }
}

const fn public_confidence(confidence: Confidence) -> FactConfidence {
    match confidence {
        Confidence::Verified => FactConfidence::Explicit,
        Confidence::High => FactConfidence::High,
        Confidence::Medium => FactConfidence::Medium,
        Confidence::Ambiguous => FactConfidence::Ambiguous,
    }
}

const fn public_state(state: InternalFactState) -> FactState {
    match state {
        InternalFactState::Asserted => FactState::Asserted,
        InternalFactState::Ambiguous => FactState::Ambiguous,
        InternalFactState::Contradicted => FactState::Contradicted,
        InternalFactState::Superseded => FactState::Superseded,
    }
}

const MAX_EVIDENCE_LOOKUP_ENTRIES: usize = crate::protocol::MAX_BLAME_EVIDENCE
    + (crate::protocol::MAX_BLAME_ATTRIBUTIONS_PER_MATCH + 1)
        * crate::protocol::MAX_CITATIONS_PER_FACT;

#[derive(Default)]
#[doc(hidden)]
pub struct EvidencePage {
    pub numbers_by_citation: HashMap<Citation, u32>,
    pub values: Vec<NumberedEvidence>,
}

impl EvidencePage {
    pub fn numbers(&mut self, citations: &[Citation]) -> Result<Vec<u32>, SegmentGraphError> {
        if citations.is_empty() || citations.len() > crate::protocol::MAX_CITATIONS_PER_FACT {
            return Err(SegmentGraphError::InvalidInput(
                "blame fact has an invalid citation count".to_owned(),
            ));
        }
        let mut numbers = Vec::new();
        for citation in citations {
            let number = if let Some(number) = self.numbers_by_citation.get(citation) {
                *number
            } else {
                if self.values.len() >= MAX_EVIDENCE_LOOKUP_ENTRIES {
                    return Err(SegmentGraphError::QueryRecordTooLarge);
                }
                let number = u32::try_from(self.values.len() + 1)
                    .map_err(|_| SegmentGraphError::QueryRecordTooLarge)?;
                self.values.push(NumberedEvidence {
                    number,
                    citation: public_citation(citation)?,
                });
                self.numbers_by_citation.insert(citation.clone(), number);
                number
            };
            numbers.push(number);
        }
        numbers.sort_unstable();
        numbers.dedup();
        Ok(numbers)
    }
}

fn public_citation(
    citation: &Citation,
) -> Result<crate::protocol::EvidenceCitation, SegmentGraphError> {
    if !citation.is_exact() {
        return Err(SegmentGraphError::InvalidInput(
            "non-Core citation cannot cross the protocol boundary".to_owned(),
        ));
    }
    Ok(citation.evidence().clone())
}
