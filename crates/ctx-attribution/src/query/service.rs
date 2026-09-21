//! Bounded blame query service over a work-owned graph interface.

use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
use super::AmbiguityCandidates;
use super::ordering::compare_production_attributions;
use super::service_resolution::ResolvedCommitTarget;
use super::{
    AttributionOutcome, BlameEntry, BlameFactPosition, BlamePage, BlamePosition, Citation,
    CommitBlameEntry, Fact, FactState, FileBlameEntry, FileBlamePosition, GitBlameWindow,
    LineRange, MAX_ATTRIBUTION_CANDIDATES, ProductionAttribution, PullRequestActivityEntry,
    PullRequestCommitEntry, QueryBounds, QueryError, QueryPage, Resource, ResourceId, ResourceKind,
    ResourceSelector, attribution_outcome,
};
use crate::protocol::{
    BlameTarget, ContinuationReason, GitSnapshot, PullRequestAction, PullRequestCommitRelationship,
    ResolvedBlameTarget, canonical_logical_repository_id,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlameFactFamily {
    Commit,
    PullRequest,
}

/// Shipping graph projection required to assemble blame responses.
pub trait BlameGraph {
    fn resolve(
        &self,
        selector: &ResourceSelector,
        limit: usize,
    ) -> Result<Vec<Resource>, QueryError>;

    fn resolve_commits(
        &self,
        commits: &[String],
        repository: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Resource)>, QueryError>;

    fn resources(&self, ids: &[ResourceId], limit: usize) -> Result<Vec<Resource>, QueryError>;

    fn blame_facts_page(
        &self,
        target: &ResourceId,
        family: BlameFactFamily,
        after: Option<&BlameFactPosition>,
        limit: usize,
    ) -> Result<QueryPage<(Fact, BlameFactPosition), BlameFactPosition>, QueryError>;

    fn git_blame_window(
        &self,
        file: &ResourceId,
        requested: Option<LineRange>,
        resume: Option<&FileBlamePosition>,
    ) -> Result<GitBlameWindow, QueryError>;

    fn production_attribution(
        &self,
        commit_ids: &[ResourceId],
        limit_per_commit: usize,
    ) -> Result<Vec<ProductionAttribution>, QueryError>;
}

pub struct BlameService<G> {
    pub(super) graph: G,
    bounds: QueryBounds,
}

impl<G: BlameGraph> BlameService<G> {
    pub fn new(graph: G, bounds: QueryBounds) -> Result<Self, QueryError> {
        Ok(Self {
            graph,
            bounds: bounds.validate()?,
        })
    }

    pub fn execute(
        &self,
        target: &BlameTarget,
        resume: Option<&BlamePosition>,
    ) -> Result<BlamePage, QueryError> {
        match target {
            BlameTarget::File {
                path,
                repository,
                lines,
            } => self.file(path, repository.as_deref(), lines.clone(), resume),
            BlameTarget::Commit { oid, repository } => {
                self.commit(oid, repository.as_deref(), resume)
            }
            BlameTarget::PullRequest {
                selector,
                repository,
            } => self.pull_request(selector, repository.as_deref(), resume),
        }
    }

    /// Resolves raw aliases to the exact canonical target used for cursor identity.
    pub fn resolve_target(&self, target: &BlameTarget) -> Result<ResolvedBlameTarget, QueryError> {
        match target {
            BlameTarget::File {
                path,
                repository,
                lines,
            } => {
                let repository = normalized_requested_repository(repository.as_deref())?;
                let requested_repository =
                    self.resolve_repository_selector(repository.as_deref())?;
                let file = self.resolve_one(ResourceSelector {
                    kind: ResourceKind::File,
                    value: path.clone(),
                    repository: repository.clone(),
                })?;
                let repository = self.repository_for(&file, requested_repository.as_ref())?;
                Ok(ResolvedBlameTarget::File {
                    path: file.display,
                    repository: public_resource(&repository),
                    requested_lines: lines.clone(),
                })
            }
            BlameTarget::Commit { oid, repository } => {
                let resolved = self.resolve_commit_target(oid, repository.as_deref())?;
                Ok(ResolvedBlameTarget::Commit {
                    commit: public_resource(&resolved.commit),
                    repository: public_resource(&resolved.repository),
                })
            }
            BlameTarget::PullRequest {
                selector,
                repository,
            } => {
                let canonical = canonical_pull_request_selector(selector, repository.as_deref())?;
                let requested_repository =
                    self.resolve_repository_selector(Some(&canonical.repository))?;
                let pull_request = self.resolve_one(ResourceSelector {
                    kind: ResourceKind::PullRequest,
                    value: canonical.graph_key,
                    repository: None,
                })?;
                let repository =
                    self.repository_for(&pull_request, requested_repository.as_ref())?;
                Ok(ResolvedBlameTarget::PullRequest {
                    selector: canonical.public_selector.clone(),
                    pull_request: public_pull_request(&pull_request, &canonical.public_selector),
                    repository: public_resource(&repository),
                })
            }
        }
    }

    fn file(
        &self,
        path: &str,
        repository: Option<&str>,
        requested_lines: Option<LineRange>,
        resume: Option<&BlamePosition>,
    ) -> Result<BlamePage, QueryError> {
        let resume = match resume {
            Some(BlamePosition::File(position)) => Some(position),
            None => None,
            _ => {
                return Err(QueryError::InvalidRequest(
                    "cursor target mismatch".to_owned(),
                ));
            }
        };
        let repository = normalized_requested_repository(repository)?;
        let requested_repository = self.resolve_repository_selector(repository.as_deref())?;
        let file = self.resolve_one(ResourceSelector {
            kind: ResourceKind::File,
            value: path.to_owned(),
            repository: repository.clone(),
        })?;
        let repository = self.repository_for(&file, requested_repository.as_ref())?;
        let repository_id = repository.id.0.clone();
        let window = self
            .graph
            .git_blame_window(&file.id, requested_lines.clone(), resume)?;
        let commit_names = window
            .observations
            .iter()
            .map(|observation| observation.commit_selector.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let resolved = self.graph.resolve_commits(
            &commit_names,
            Some(&repository.display),
            commit_names.len() * 2,
        )?;
        let mut commits_by_name = BTreeMap::<String, Vec<Resource>>::new();
        for (name, commit) in resolved {
            commits_by_name.entry(name).or_default().push(commit);
        }
        let mut commits = BTreeMap::new();
        let mut materialized_commit_ids = Vec::new();
        for name in commit_names {
            let candidates = commits_by_name.remove(&name).unwrap_or_default();
            let commit = match candidates.as_slice() {
                [] => observed_commit_resource(&name, &repository),
                [commit] => {
                    if commit.logical_repository.as_ref() != Some(&repository.id) {
                        return Err(QueryError::Backend(
                            "Core graph commit belongs to a different logical repository"
                                .to_owned(),
                        ));
                    }
                    materialized_commit_ids.push(commit.id.clone());
                    commit.clone()
                }
                _ => {
                    return Err(QueryError::Backend(
                        "one Git object resolved to multiple repository-local resources".to_owned(),
                    ));
                }
            };
            commits.insert(name, commit);
        }
        let mut production = BTreeMap::<ResourceId, Vec<ProductionAttribution>>::new();
        for attribution in self.graph.production_attribution(
            &materialized_commit_ids,
            self.bounds.max_attributions_per_match,
        )? {
            production
                .entry(attribution.commit.clone())
                .or_default()
                .push(attribution);
        }
        for attributions in production.values_mut() {
            normalize_attributions(attributions)?;
        }
        let supporting_resources = self.production_resources(
            production
                .values()
                .flat_map(|attributions| attributions.iter()),
        )?;

        let raw_count = window.observations.len();
        let mut entries = Vec::new();
        let mut positions = Vec::new();
        for observation in window
            .observations
            .iter()
            .take(self.bounds.max_matches)
            .cloned()
        {
            validate_citations(std::slice::from_ref(&observation.citation))?;
            let commit = commits
                .get(&observation.commit_selector)
                .cloned()
                .ok_or_else(|| {
                    QueryError::Backend("blame commit resolution was lost".to_owned())
                })?;
            let attributions = production.get(&commit.id).cloned().unwrap_or_default();
            validate_attributions(&attributions)?;
            let position = file_position_after(
                requested_lines.as_ref(),
                &window,
                observation.lines.end.saturating_add(1),
            );
            let id = crate::envelope::stable_id(
                "file-blame",
                &format!(
                    "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
                    crate::protocol::CORE_MATERIALIZATION_CONTRACT_VERSION,
                    super::BLAME_ORDERING_VERSION,
                    repository_id,
                    window.head_oid,
                    file.display,
                    observation.lines.start,
                    observation.lines.end,
                    commit.display,
                ),
            );
            entries.push(BlameEntry::File(FileBlameEntry {
                id,
                lines: observation.lines,
                commit,
                line_citations: vec![observation.citation],
                production: attributions,
            }));
            positions.push(BlamePosition::File(position));
        }
        let last_line = entries.last().and_then(|entry| match entry {
            BlameEntry::File(entry) => Some(entry.lines.end),
            _ => None,
        });
        let has_more = raw_count > entries.len()
            || (last_line == Some(window.window_end) && window.more_committed_lines);
        let continuation_reason = if raw_count > entries.len() {
            ContinuationReason::MoreMatches
        } else {
            ContinuationReason::MoreCommittedLines
        };
        Ok(BlamePage {
            target: ResolvedBlameTarget::File {
                path: file.display,
                repository: public_resource(&repository),
                requested_lines,
            },
            git_snapshot: Some(GitSnapshot {
                head_oid: window.head_oid,
                worktree_status: window.worktree_status,
            }),
            entries,
            positions,
            resources: supporting_resources,
            has_more,
            continuation_reason,
        })
    }

    fn commit(
        &self,
        oid: &str,
        repository: Option<&str>,
        resume: Option<&BlamePosition>,
    ) -> Result<BlamePage, QueryError> {
        let after = match resume {
            Some(BlamePosition::Commit(position)) => Some(position),
            None => None,
            _ => {
                return Err(QueryError::InvalidRequest(
                    "cursor target mismatch".to_owned(),
                ));
            }
        };
        let ResolvedCommitTarget { commit, repository } =
            self.resolve_commit_target(oid, repository)?;
        let mut attributions = self.graph.production_attribution(
            std::slice::from_ref(&commit.id),
            self.bounds.max_attributions_per_match,
        )?;
        normalize_attributions(&mut attributions)?;
        let page = self.graph.blame_facts_page(
            &commit.id,
            BlameFactFamily::Commit,
            after,
            self.bounds.max_matches,
        )?;
        let production_outcome = commit_page_production_outcome(&page.items)?;
        let mut entries = Vec::new();
        let mut positions = Vec::new();
        for (fact, position) in page.items {
            let production = attributions
                .iter()
                .find(|attribution| attribution.fact_id == fact.id);
            let mut resources = self.fact_resources(&fact)?;
            let additional_ids = production
                .into_iter()
                .flat_map(|attribution| {
                    attribution
                        .parent_session
                        .iter()
                        .chain(attribution.root_run.iter())
                })
                .filter(|id| !resources.contains_key(*id))
                .cloned()
                .collect::<Vec<_>>();
            for resource in self
                .graph
                .resources(&additional_ids, additional_ids.len())?
            {
                resources.insert(resource.id.clone(), resource);
            }
            let subject = resource(&resources, &fact.subject)?;
            let object = fact
                .object
                .as_ref()
                .map(|id| resource(&resources, id))
                .transpose()?;
            let direct_actor = fact
                .direct_actor
                .as_ref()
                .map(|id| resource(&resources, id))
                .transpose()?;
            let parent_session = production
                .and_then(|attribution| attribution.parent_session.as_ref())
                .map(|id| resource(&resources, id))
                .transpose()?;
            let owning_root = production
                .and_then(|attribution| attribution.root_run.as_ref())
                .or(fact.root_run.as_ref())
                .map(|id| resource(&resources, id))
                .transpose()?;
            entries.push(BlameEntry::Commit(CommitBlameEntry {
                attribution: commit_fact_attribution(&fact, production_outcome),
                fact,
                subject,
                object,
                parent_session,
                direct_actor,
                owning_root,
            }));
            positions.push(BlamePosition::Commit(position));
        }
        Ok(BlamePage {
            target: ResolvedBlameTarget::Commit {
                commit: public_resource(&commit),
                repository: public_resource(&repository),
            },
            git_snapshot: None,
            has_more: page.next_cursor.is_some(),
            continuation_reason: ContinuationReason::MoreMatches,
            entries,
            positions,
            resources: Vec::new(),
        })
    }

    fn pull_request(
        &self,
        selector: &str,
        repository: Option<&str>,
        resume: Option<&BlamePosition>,
    ) -> Result<BlamePage, QueryError> {
        let after = match resume {
            Some(BlamePosition::PullRequest(position)) => Some(position),
            None => None,
            _ => {
                return Err(QueryError::InvalidRequest(
                    "cursor target mismatch".to_owned(),
                ));
            }
        };
        let canonical = canonical_pull_request_selector(selector, repository)?;
        let requested_repository = self.resolve_repository_selector(Some(&canonical.repository))?;
        let pull_request = self.resolve_one(ResourceSelector {
            kind: ResourceKind::PullRequest,
            value: canonical.graph_key,
            repository: None,
        })?;
        let repository = self.repository_for(&pull_request, requested_repository.as_ref())?;
        let page = self.graph.blame_facts_page(
            &pull_request.id,
            BlameFactFamily::PullRequest,
            after,
            self.bounds.max_matches,
        )?;
        let commit_ids = page
            .items
            .iter()
            .filter(|(fact, _)| pull_request_action(&fact.fact_type).is_none())
            .filter_map(|(fact, _)| fact.object.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut production_by_commit = BTreeMap::<ResourceId, Vec<ProductionAttribution>>::new();
        for attribution in self
            .graph
            .production_attribution(&commit_ids, self.bounds.max_attributions_per_match)?
        {
            production_by_commit
                .entry(attribution.commit.clone())
                .or_default()
                .push(attribution);
        }
        for attributions in production_by_commit.values_mut() {
            normalize_attributions(attributions)?;
        }
        let supporting_resources = self.production_resources(
            production_by_commit
                .values()
                .flat_map(|attributions| attributions.iter()),
        )?;
        let mut entries = Vec::new();
        let mut positions = Vec::new();
        for (fact, position) in page.items {
            validate_fact(&fact)?;
            let resources = self.fact_resources(&fact)?;
            if let Some(action) = pull_request_action(&fact.fact_type) {
                let session_id = fact.object.as_ref().ok_or_else(|| {
                    QueryError::Backend("PR activity is missing its session".to_owned())
                })?;
                entries.push(BlameEntry::PullRequestActivity(PullRequestActivityEntry {
                    pull_request: pull_request.clone(),
                    session: resource(&resources, session_id)?,
                    direct_actor: fact
                        .direct_actor
                        .as_ref()
                        .map(|id| resource(&resources, id))
                        .transpose()?,
                    owning_root: fact
                        .root_run
                        .as_ref()
                        .map(|id| resource(&resources, id))
                        .transpose()?,
                    fact,
                    action,
                }));
            } else {
                let relationship = pull_request_commit_relationship(&fact.fact_type)?;
                let commit_id = fact.object.as_ref().ok_or_else(|| {
                    QueryError::Backend("PR membership is missing its commit".to_owned())
                })?;
                let commit = resource(&resources, commit_id)?;
                let production = production_by_commit
                    .get(commit_id)
                    .cloned()
                    .unwrap_or_default();
                validate_attributions(&production)?;
                entries.push(BlameEntry::PullRequestCommit(PullRequestCommitEntry {
                    pull_request: pull_request.clone(),
                    fact,
                    relationship,
                    commit,
                    production,
                }));
            }
            positions.push(BlamePosition::PullRequest(position));
        }
        let public_pull_request = public_pull_request(&pull_request, &canonical.public_selector);
        Ok(BlamePage {
            target: ResolvedBlameTarget::PullRequest {
                selector: canonical.public_selector,
                pull_request: public_pull_request,
                repository: public_resource(&repository),
            },
            git_snapshot: None,
            has_more: page.next_cursor.is_some(),
            continuation_reason: ContinuationReason::MoreMatches,
            entries,
            positions,
            resources: supporting_resources,
        })
    }

    fn fact_resources(&self, fact: &Fact) -> Result<BTreeMap<ResourceId, Resource>, QueryError> {
        let ids = std::iter::once(&fact.subject)
            .chain(fact.object.iter())
            .chain(fact.direct_actor.iter())
            .chain(fact.root_run.iter())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        Ok(self
            .graph
            .resources(&ids, ids.len())?
            .into_iter()
            .map(|resource| (resource.id.clone(), resource))
            .collect())
    }

    fn production_resources<'a>(
        &self,
        attributions: impl Iterator<Item = &'a ProductionAttribution>,
    ) -> Result<Vec<Resource>, QueryError> {
        let ids = attributions
            .flat_map(|attribution| {
                std::iter::once(&attribution.producing_session)
                    .chain(attribution.parent_session.iter())
                    .chain(attribution.root_run.iter())
                    .chain(attribution.direct_actor.iter())
            })
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.graph.resources(&ids, ids.len())
    }
}

struct CanonicalPullRequestSelector {
    graph_key: String,
    repository: String,
    public_selector: String,
}

fn canonical_pull_request_selector(
    selector: &str,
    requested_repository: Option<&str>,
) -> Result<CanonicalPullRequestSelector, QueryError> {
    if selector.bytes().all(|byte| byte.is_ascii_digit()) {
        let number = selector
            .parse::<u64>()
            .ok()
            .filter(|number| *number > 0)
            .ok_or_else(|| QueryError::InvalidRequest("invalid PR number".to_owned()))?;
        let repository = requested_repository
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                QueryError::InvalidRequest("PR number requires repository".to_owned())
            })?;
        let repository = normalized_forge_repository(repository)?;
        let graph_repository = repository.strip_prefix("forge:").unwrap_or(&repository);
        return Ok(CanonicalPullRequestSelector {
            graph_key: format!("{graph_repository}/pull_request/{number}"),
            repository,
            public_selector: number.to_string(),
        });
    }
    let rest = selector
        .strip_prefix("https://")
        .ok_or_else(|| QueryError::InvalidRequest("invalid canonical PR URL".to_owned()))?;
    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| QueryError::InvalidRequest("invalid canonical PR URL".to_owned()))?;
    let canonical_host = canonical_forge_host(host)
        .ok_or_else(|| QueryError::InvalidRequest("invalid canonical PR URL".to_owned()))?;
    let segments = path.split('/').collect::<Vec<_>>();
    let public_pull_request = segments.len() == 4
        && ((canonical_host == "github.com" && segments[2] == "pull")
            || (canonical_host == "codeberg.org" && segments[2] == "pulls"));
    let (repository_path, number) = if public_pull_request {
        (segments[..2].join("/"), segments[3])
    } else {
        let dash = segments
            .iter()
            .position(|segment| *segment == "-")
            .ok_or_else(|| QueryError::InvalidRequest("invalid canonical PR URL".to_owned()))?;
        if dash < 2
            || segments.get(dash + 1) != Some(&"merge_requests")
            || segments.get(dash + 2).is_none()
            || segments.len() != dash + 3
        {
            return Err(QueryError::InvalidRequest(
                "invalid canonical PR URL".to_owned(),
            ));
        }
        (segments[..dash].join("/"), segments[dash + 2])
    };
    let number = number
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| QueryError::InvalidRequest("invalid PR number".to_owned()))?;
    let repository = normalized_forge_repository(&format!("{canonical_host}/{repository_path}"))?;
    if let Some(requested) = requested_repository {
        let requested = normalized_forge_repository(requested)?;
        if requested != repository {
            return Err(QueryError::InvalidRequest(
                "PR URL and repository selector disagree".to_owned(),
            ));
        }
    }
    Ok(CanonicalPullRequestSelector {
        graph_key: format!(
            "{}/pull_request/{number}",
            repository.strip_prefix("forge:").unwrap_or(&repository)
        ),
        repository,
        public_selector: selector.to_owned(),
    })
}

fn normalized_forge_repository(value: &str) -> Result<String, QueryError> {
    let value = value
        .strip_prefix("forge:")
        .unwrap_or(value)
        .trim_end_matches('/');
    let value = value.strip_prefix("https://").unwrap_or(value);
    let (host, path) = value
        .split_once('/')
        .ok_or_else(|| QueryError::InvalidRequest("invalid forge repository".to_owned()))?;
    let host = canonical_forge_host(host)
        .ok_or_else(|| QueryError::InvalidRequest("invalid forge repository".to_owned()))?;
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
        || value.contains(['?', '#', '\0', '\n', '\r'])
    {
        return Err(QueryError::InvalidRequest(
            "invalid forge repository".to_owned(),
        ));
    }
    Ok(format!("forge:{host}/{path}"))
}

pub(super) fn normalized_requested_repository(
    value: Option<&str>,
) -> Result<Option<String>, QueryError> {
    value.map(normalized_repository_identity).transpose()
}

fn normalized_repository_identity(value: &str) -> Result<String, QueryError> {
    if value.trim().is_empty()
        || value.len() > crate::protocol::MAX_BLAME_TARGET_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(QueryError::InvalidRequest(
            "invalid repository identity".to_owned(),
        ));
    }
    Ok(canonical_logical_repository_id(value).into_owned())
}

fn canonical_forge_host(host: &str) -> Option<String> {
    if host.is_empty()
        || !host.contains('.')
        || host.starts_with('.')
        || host.ends_with('.')
        || !host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

fn public_resource(resource: &Resource) -> crate::protocol::ResourceRef {
    crate::protocol::ResourceRef {
        id: resource.id.0.clone(),
        kind: resource.kind,
        display: resource.display.clone(),
    }
}

fn observed_commit_resource(commit: &str, repository: &Resource) -> Resource {
    Resource {
        id: ResourceId(crate::envelope::stable_id(
            "git_observation_commit",
            &format!("{}\u{1f}{commit}", repository.id.0),
        )),
        kind: ResourceKind::Commit,
        display: commit.to_owned(),
        logical_repository: Some(repository.id.clone()),
    }
}

fn public_pull_request(resource: &Resource, selector: &str) -> crate::protocol::ResourceRef {
    crate::protocol::ResourceRef {
        id: resource.id.0.clone(),
        kind: resource.kind,
        display: selector.to_owned(),
    }
}

fn resource(
    resources: &BTreeMap<ResourceId, Resource>,
    id: &ResourceId,
) -> Result<Resource, QueryError> {
    resources
        .get(id)
        .cloned()
        .ok_or_else(|| QueryError::Backend("fact references a missing resource".to_owned()))
}

fn validate_fact(fact: &Fact) -> Result<(), QueryError> {
    if fact.citations.is_empty() {
        return Err(QueryError::UncitedFact(fact.id.clone()));
    }
    validate_citations(&fact.citations)
}

fn validate_attributions(attributions: &[ProductionAttribution]) -> Result<(), QueryError> {
    for attribution in attributions {
        if attribution.citations.is_empty() {
            return Err(QueryError::UncitedFact(attribution.fact_id.clone()));
        }
        validate_citations(&attribution.citations)?;
    }
    Ok(())
}

fn normalize_attributions(
    attributions: &mut Vec<ProductionAttribution>,
) -> Result<AttributionOutcome, QueryError> {
    validate_attributions(attributions)?;
    let outcome = attribution_outcome(attributions);
    sort_attributions(attributions);
    let mut retained_sessions = BTreeSet::new();
    attributions
        .retain(|attribution| retained_sessions.insert(attribution.producing_session.clone()));
    attributions.truncate(MAX_ATTRIBUTION_CANDIDATES);
    debug_assert_eq!(attribution_outcome(attributions), outcome);
    Ok(outcome)
}

fn commit_fact_attribution(
    fact: &Fact,
    production_outcome: AttributionOutcome,
) -> AttributionOutcome {
    match (fact.fact_type.as_str(), fact.state) {
        ("git.commit.produced", FactState::Asserted)
            if production_outcome == AttributionOutcome::Conflicting =>
        {
            AttributionOutcome::Conflicting
        }
        ("git.commit.produced", FactState::Asserted) => AttributionOutcome::Proven,
        // Projection deliberately demotes otherwise exact production evidence
        // when its repository authority boundary is incomplete. Preserve that
        // evidence as a possible attribution instead of turning it into either
        // proven production or an unexplained `none` result.
        ("git.commit.produced", FactState::Ambiguous) => AttributionOutcome::Possible,
        ("git.commit.ambiguous", FactState::Ambiguous) => AttributionOutcome::Possible,
        _ => AttributionOutcome::None,
    }
}

fn commit_page_production_outcome(
    items: &[(Fact, BlameFactPosition)],
) -> Result<AttributionOutcome, QueryError> {
    let mut asserted_producers = BTreeSet::new();
    for (fact, _) in items {
        validate_fact(fact)?;
        if fact.fact_type == "git.commit.produced" && fact.state == FactState::Asserted {
            asserted_producers.extend(fact.object.iter().cloned());
        }
    }
    Ok(match asserted_producers.len() {
        2.. => AttributionOutcome::Conflicting,
        1 => AttributionOutcome::Proven,
        _ => AttributionOutcome::None,
    })
}

fn sort_attributions(attributions: &mut [ProductionAttribution]) {
    attributions.sort_by(compare_production_attributions);
}

fn validate_citations(citations: &[Citation]) -> Result<(), QueryError> {
    if citations.iter().any(|citation| !citation.is_exact()) {
        return Err(QueryError::InvalidCitation);
    }
    Ok(())
}

fn pull_request_action(fact_type: &str) -> Option<PullRequestAction> {
    match fact_type {
        "forge.pull_request.referenced" => Some(PullRequestAction::Referenced),
        "forge.create" => Some(PullRequestAction::Created),
        "forge.review" => Some(PullRequestAction::Reviewed),
        "forge.comment" => Some(PullRequestAction::Commented),
        "forge.merge" => Some(PullRequestAction::Merged),
        "forge.edit" => Some(PullRequestAction::Edited),
        "forge.close" => Some(PullRequestAction::Closed),
        "forge.reopen" => Some(PullRequestAction::Reopened),
        _ => None,
    }
}

fn pull_request_commit_relationship(
    fact_type: &str,
) -> Result<PullRequestCommitRelationship, QueryError> {
    match fact_type {
        "forge.pull_request.contains_commit" => Ok(PullRequestCommitRelationship::ContainsCommit),
        "forge.pull_request.merged_as" => Ok(PullRequestCommitRelationship::MergedAs),
        _ => Err(QueryError::Backend(
            "unsupported PR blame fact escaped its allowlist".to_owned(),
        )),
    }
}

fn file_position_after(
    requested: Option<&LineRange>,
    window: &GitBlameWindow,
    next_line: u32,
) -> FileBlamePosition {
    let requested_start = requested.map_or(1, |lines| lines.start);
    let requested_end = requested.map(|lines| lines.end);
    if next_line > window.window_end && window.more_committed_lines {
        let window_start = next_line;
        let natural_end = window_start.saturating_add(499);
        let window_end = requested_end.map_or(natural_end, |end| end.min(natural_end));
        FileBlamePosition {
            head_oid: window.head_oid.clone(),
            requested_start,
            requested_end,
            window_start,
            window_end,
            next_line,
        }
    } else {
        FileBlamePosition {
            head_oid: window.head_oid.clone(),
            requested_start,
            requested_end,
            window_start: window.window_start,
            window_end: window.window_end,
            next_line,
        }
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
