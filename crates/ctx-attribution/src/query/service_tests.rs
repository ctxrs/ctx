//! Direct query-service contract tests.

use super::*;
use crate::query::{Confidence, FactState, QueryOperation};

const KNOWN_COMMIT: &str = "1111111111111111111111111111111111111111";
const UNKNOWN_COMMIT: &str = "2222222222222222222222222222222222222222";
const HEAD_COMMIT: &str = "3333333333333333333333333333333333333333";

struct MixedCommitGraph {
    possible: bool,
}

impl MixedCommitGraph {
    const PROVEN: Self = Self { possible: false };
    const POSSIBLE: Self = Self { possible: true };
}

impl BlameGraph for MixedCommitGraph {
    fn resolve(
        &self,
        selector: &ResourceSelector,
        _limit: usize,
    ) -> Result<Vec<Resource>, QueryError> {
        if selector.kind == ResourceKind::File && selector.value == "src/lib.rs" {
            Ok(vec![file_resource()])
        } else if selector.kind == ResourceKind::Commit && selector.value == KNOWN_COMMIT {
            Ok(vec![known_commit_resource()])
        } else {
            Err(QueryError::Backend(
                "unexpected mixed-commit resolve".to_owned(),
            ))
        }
    }

    fn resolve_commits(
        &self,
        commits: &[String],
        repository: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<(String, Resource)>, QueryError> {
        if commits != [KNOWN_COMMIT.to_owned(), UNKNOWN_COMMIT.to_owned()]
            || repository != Some("logical-repository")
        {
            return Err(QueryError::Backend(
                "unexpected mixed-commit resolution request".to_owned(),
            ));
        }
        Ok(vec![(KNOWN_COMMIT.to_owned(), known_commit_resource())])
    }

    fn resources(&self, ids: &[ResourceId], _limit: usize) -> Result<Vec<Resource>, QueryError> {
        ids.iter()
            .map(|id| match id.0.as_str() {
                "repository-resource" => Ok(repository_resource()),
                "known-commit-resource" => Ok(known_commit_resource()),
                "agent-session" => Ok(resource("agent-session", ResourceKind::Session)),
                "manager-session" => Ok(resource("manager-session", ResourceKind::Session)),
                "root-run" => Ok(resource("root-run", ResourceKind::Run)),
                "direct-agent" => Ok(resource("direct-agent", ResourceKind::Agent)),
                _ => Err(QueryError::Backend(
                    "unexpected mixed-commit resource request".to_owned(),
                )),
            })
            .collect()
    }

    fn blame_facts_page(
        &self,
        target: &ResourceId,
        family: BlameFactFamily,
        _after: Option<&BlameFactPosition>,
        _limit: usize,
    ) -> Result<QueryPage<(Fact, BlameFactPosition), BlameFactPosition>, QueryError> {
        if target != &ResourceId("known-commit-resource".to_owned())
            || family != BlameFactFamily::Commit
        {
            return Err(QueryError::Backend(
                "unexpected blame facts query".to_owned(),
            ));
        }
        let fact = Fact {
            id: "known-production-fact".to_owned(),
            fact_type: "git.commit.produced".to_owned(),
            subject: target.clone(),
            predicate: "produced_by".to_owned(),
            object: Some(ResourceId("agent-session".to_owned())),
            value: None,
            occurred_at_ms: None,
            confidence: if self.possible {
                Confidence::Ambiguous
            } else {
                Confidence::Verified
            },
            state: if self.possible {
                FactState::Ambiguous
            } else {
                FactState::Asserted
            },
            detector_version: "test@1".to_owned(),
            root_run: Some(ResourceId("root-run".to_owned())),
            direct_actor: Some(ResourceId("direct-agent".to_owned())),
            citations: vec![citation()],
        };
        Ok(QueryPage {
            items: vec![(
                fact,
                BlameFactPosition {
                    class: 0,
                    rank: 0,
                    occurred_at_ms: None,
                    resource_id: target.0.clone(),
                    fact_id: "known-production-fact".to_owned(),
                },
            )],
            next_cursor: None,
        })
    }

    fn git_blame_window(
        &self,
        file: &ResourceId,
        requested: Option<LineRange>,
        resume: Option<&FileBlamePosition>,
    ) -> Result<GitBlameWindow, QueryError> {
        if file.0 != "file-resource" || requested.is_some() || resume.is_some() {
            return Err(QueryError::Backend(
                "unexpected mixed-commit Git blame request".to_owned(),
            ));
        }
        Ok(GitBlameWindow {
            head_oid: HEAD_COMMIT.to_owned(),
            worktree_status: crate::protocol::WorktreeStatus::Clean,
            window_start: 1,
            window_end: 2,
            observations: vec![
                git_observation(1, KNOWN_COMMIT),
                git_observation(2, UNKNOWN_COMMIT),
            ],
            more_committed_lines: false,
        })
    }

    fn production_attribution(
        &self,
        commit_ids: &[ResourceId],
        _limit_per_commit: usize,
    ) -> Result<Vec<ProductionAttribution>, QueryError> {
        if commit_ids != [ResourceId("known-commit-resource".to_owned())] {
            return Err(QueryError::Backend(
                "production requested for a non-Core commit".to_owned(),
            ));
        }
        Ok(vec![ProductionAttribution {
            fact_id: "known-production-fact".to_owned(),
            commit: ResourceId("known-commit-resource".to_owned()),
            producing_session: ResourceId("agent-session".to_owned()),
            parent_session: Some(ResourceId("manager-session".to_owned())),
            root_run: Some(ResourceId("root-run".to_owned())),
            direct_actor: Some(ResourceId("direct-agent".to_owned())),
            fact_occurred_at_ms: Some(1_700_000_000_000),
            relationship: if self.possible {
                crate::protocol::ProductionRelationship::PossiblyProducedBy
            } else {
                crate::protocol::ProductionRelationship::ProducedBy
            },
            confidence: if self.possible {
                Confidence::Ambiguous
            } else {
                Confidence::Verified
            },
            state: if self.possible {
                FactState::Ambiguous
            } else {
                FactState::Asserted
            },
            citations: vec![citation()],
        }])
    }
}

struct PaginatedCommitGraph;

impl BlameGraph for PaginatedCommitGraph {
    fn resolve(
        &self,
        selector: &ResourceSelector,
        _limit: usize,
    ) -> Result<Vec<Resource>, QueryError> {
        if selector.kind == ResourceKind::Commit && selector.value == KNOWN_COMMIT {
            Ok(vec![known_commit_resource()])
        } else {
            Err(QueryError::Backend(
                "unexpected paginated-commit resolve".to_owned(),
            ))
        }
    }

    fn resolve_commits(
        &self,
        _commits: &[String],
        _repository: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<(String, Resource)>, QueryError> {
        Err(QueryError::Backend(
            "unexpected paginated-commit lookup".to_owned(),
        ))
    }

    fn resources(&self, ids: &[ResourceId], _limit: usize) -> Result<Vec<Resource>, QueryError> {
        ids.iter()
            .map(|id| match id.0.as_str() {
                "repository-resource" => Ok(repository_resource()),
                "known-commit-resource" => Ok(known_commit_resource()),
                "producer-a" | "producer-b" => Ok(resource(&id.0, ResourceKind::Session)),
                _ => Err(QueryError::Backend(
                    "unexpected paginated-commit resource request".to_owned(),
                )),
            })
            .collect()
    }

    fn blame_facts_page(
        &self,
        target: &ResourceId,
        family: BlameFactFamily,
        after: Option<&BlameFactPosition>,
        limit: usize,
    ) -> Result<QueryPage<(Fact, BlameFactPosition), BlameFactPosition>, QueryError> {
        if target != &ResourceId("known-commit-resource".to_owned())
            || family != BlameFactFamily::Commit
        {
            return Err(QueryError::Backend(
                "unexpected paginated-commit fact lookup".to_owned(),
            ));
        }
        let facts = [
            paginated_producer_fact("producer-fact-a", "producer-a", 0),
            paginated_producer_fact("producer-fact-b", "producer-b", 1),
        ];
        let start: usize = match after.map(|position| position.fact_id.as_str()) {
            None => 0,
            Some("producer-fact-a") => 1,
            Some("producer-fact-b") => 2,
            Some(_) => {
                return Err(QueryError::InvalidRequest(
                    "unknown paginated-commit cursor".to_owned(),
                ));
            }
        };
        let end = start.saturating_add(limit).min(facts.len());
        let items = facts[start..end].to_vec();
        let next_cursor = (end < facts.len())
            .then(|| items.last().map(|(_, position)| position.clone()))
            .flatten();
        Ok(QueryPage { items, next_cursor })
    }

    fn git_blame_window(
        &self,
        _file: &ResourceId,
        _requested: Option<LineRange>,
        _resume: Option<&FileBlamePosition>,
    ) -> Result<GitBlameWindow, QueryError> {
        Err(QueryError::Backend(
            "unexpected paginated-commit Git lookup".to_owned(),
        ))
    }

    fn production_attribution(
        &self,
        commit_ids: &[ResourceId],
        limit_per_commit: usize,
    ) -> Result<Vec<ProductionAttribution>, QueryError> {
        if commit_ids != [ResourceId("known-commit-resource".to_owned())] {
            return Err(QueryError::Backend(
                "unexpected paginated-commit production lookup".to_owned(),
            ));
        }
        Ok([
            paginated_production("producer-fact-a", "producer-a"),
            paginated_production("producer-fact-b", "producer-b"),
        ]
        .into_iter()
        .take(limit_per_commit)
        .collect())
    }
}

struct ExactRepositoryGraph<'a> {
    repository: &'a str,
}

impl BlameGraph for ExactRepositoryGraph<'_> {
    fn resolve(
        &self,
        selector: &ResourceSelector,
        _limit: usize,
    ) -> Result<Vec<Resource>, QueryError> {
        if selector.kind == ResourceKind::File
            && selector.value == "opaque.rs"
            && selector.repository.as_deref() == Some(self.repository)
        {
            return Ok(vec![Resource {
                id: ResourceId("exact-file".to_owned()),
                kind: ResourceKind::File,
                display: "opaque.rs".to_owned(),
                logical_repository: Some(ResourceId("exact-repository".to_owned())),
            }]);
        }
        if selector.kind == ResourceKind::File
            && selector.value == "unbound.rs"
            && selector.repository.is_none()
        {
            return Ok(vec![Resource {
                id: ResourceId("unbound-file".to_owned()),
                kind: ResourceKind::File,
                display: "unbound.rs".to_owned(),
                logical_repository: None,
            }]);
        }
        if selector.kind == ResourceKind::File
            && selector.value == "operation-unavailable.rs"
            && selector.repository.is_none()
        {
            return Ok(vec![Resource {
                id: ResourceId("operation-unavailable-file".to_owned()),
                kind: ResourceKind::File,
                display: "operation-unavailable.rs".to_owned(),
                logical_repository: Some(ResourceId("exact-repository".to_owned())),
            }]);
        }
        if selector.kind == ResourceKind::File
            && selector.value == "ambiguous.rs"
            && selector.repository.is_none()
        {
            return Ok(vec![
                Resource {
                    id: ResourceId("ambiguous-file-a".to_owned()),
                    kind: ResourceKind::File,
                    display: "ambiguous.rs".to_owned(),
                    logical_repository: Some(ResourceId("exact-repository".to_owned())),
                },
                Resource {
                    id: ResourceId("ambiguous-file-b".to_owned()),
                    kind: ResourceKind::File,
                    display: "ambiguous.rs".to_owned(),
                    logical_repository: Some(ResourceId("other-repository".to_owned())),
                },
            ]);
        }
        if selector.kind == ResourceKind::Commit
            && selector.value == "abc"
            && selector.repository.is_none()
        {
            return Ok(vec![
                Resource {
                    id: ResourceId("commit-a".to_owned()),
                    kind: ResourceKind::Commit,
                    display: "a".repeat(40),
                    logical_repository: Some(ResourceId("exact-repository".to_owned())),
                },
                Resource {
                    id: ResourceId("commit-b".to_owned()),
                    kind: ResourceKind::Commit,
                    display: "b".repeat(40),
                    logical_repository: Some(ResourceId("exact-repository".to_owned())),
                },
            ]);
        }
        if selector.kind == ResourceKind::Repository
            && selector.value == self.repository
            && selector.repository.is_none()
        {
            return Ok(vec![exact_repository_resource(self.repository)]);
        }
        Ok(Vec::new())
    }

    fn resolve_commits(
        &self,
        _commits: &[String],
        _repository: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<(String, Resource)>, QueryError> {
        Err(QueryError::Backend("unexpected commit lookup".to_owned()))
    }

    fn resources(&self, ids: &[ResourceId], _limit: usize) -> Result<Vec<Resource>, QueryError> {
        ids.iter()
            .map(|id| match id.0.as_str() {
                "exact-repository" => Ok(exact_repository_resource(self.repository)),
                "other-repository" => Ok(Resource {
                    id: id.clone(),
                    kind: ResourceKind::Repository,
                    display: "forge:github.com/ctxrs/other".to_owned(),
                    logical_repository: None,
                }),
                _ => Err(QueryError::Backend(
                    "unexpected exact repository resource lookup".to_owned(),
                )),
            })
            .collect()
    }

    fn blame_facts_page(
        &self,
        _target: &ResourceId,
        _family: BlameFactFamily,
        _after: Option<&BlameFactPosition>,
        _limit: usize,
    ) -> Result<QueryPage<(Fact, BlameFactPosition), BlameFactPosition>, QueryError> {
        Err(QueryError::Backend("unexpected fact lookup".to_owned()))
    }

    fn git_blame_window(
        &self,
        file: &ResourceId,
        _requested: Option<LineRange>,
        _resume: Option<&FileBlamePosition>,
    ) -> Result<GitBlameWindow, QueryError> {
        if file == &ResourceId("operation-unavailable-file".to_owned()) {
            return Err(QueryError::OperationUnavailable(QueryOperation::FileBlame));
        }
        Err(QueryError::Backend("unexpected Git lookup".to_owned()))
    }

    fn production_attribution(
        &self,
        _commit_ids: &[ResourceId],
        _limit_per_commit: usize,
    ) -> Result<Vec<ProductionAttribution>, QueryError> {
        Err(QueryError::Backend(
            "unexpected production lookup".to_owned(),
        ))
    }
}

fn exact_repository_resource(repository: &str) -> Resource {
    Resource {
        id: ResourceId("exact-repository".to_owned()),
        kind: ResourceKind::Repository,
        display: repository.to_owned(),
        logical_repository: None,
    }
}

#[test]
fn whole_file_blame_keeps_known_agent_production_and_unknown_human_lines() {
    let service = BlameService::new(MixedCommitGraph::PROVEN, QueryBounds::default())
        .unwrap_or_else(|_| unreachable!());
    let page = service
        .execute(
            &BlameTarget::File {
                path: "src/lib.rs".to_owned(),
                repository: None,
                lines: None,
            },
            None,
        )
        .unwrap_or_else(|_| unreachable!());

    assert_eq!(page.entries.len(), 2);
    let BlameEntry::File(known) = &page.entries[0] else {
        unreachable!();
    };
    assert_eq!(known.lines, LineRange { start: 1, end: 1 });
    assert_eq!(known.commit, known_commit_resource());
    assert_eq!(known.production.len(), 1);
    assert_eq!(known.production[0].producing_session.0, "agent-session");
    assert_eq!(
        known.production[0]
            .parent_session
            .as_ref()
            .map(|id| id.0.as_str()),
        Some("manager-session")
    );

    let BlameEntry::File(unknown) = &page.entries[1] else {
        unreachable!();
    };
    assert_eq!(unknown.lines, LineRange { start: 2, end: 2 });
    assert_eq!(unknown.commit.kind, ResourceKind::Commit);
    assert_eq!(unknown.commit.display, UNKNOWN_COMMIT);
    assert_eq!(
        unknown.commit.logical_repository,
        Some(ResourceId("repository-resource".to_owned()))
    );
    assert!(unknown.production.is_empty());
    assert!(public_resource(&unknown.commit).validate().is_ok());
    assert_eq!(page.resources.len(), 4);
}

#[test]
fn file_blame_preserves_possible_producer_evidence() {
    let service = BlameService::new(MixedCommitGraph::POSSIBLE, QueryBounds::default())
        .unwrap_or_else(|_| unreachable!());
    let page = service
        .execute(
            &BlameTarget::File {
                path: "src/lib.rs".to_owned(),
                repository: None,
                lines: None,
            },
            None,
        )
        .unwrap_or_else(|_| unreachable!());
    let BlameEntry::File(known) = &page.entries[0] else {
        unreachable!();
    };
    let [possible] = known.production.as_slice() else {
        unreachable!();
    };
    assert_eq!(
        possible.relationship,
        crate::protocol::ProductionRelationship::PossiblyProducedBy
    );
    assert_eq!(possible.confidence, Confidence::Ambiguous);
    assert_eq!(possible.state, FactState::Ambiguous);
}

#[test]
fn direct_commit_blame_keeps_parent_and_canonical_root_lineage() {
    let service = BlameService::new(MixedCommitGraph::PROVEN, QueryBounds::default())
        .unwrap_or_else(|_| unreachable!());
    let page = service
        .execute(
            &BlameTarget::Commit {
                oid: KNOWN_COMMIT.to_owned(),
                repository: None,
            },
            None,
        )
        .unwrap_or_else(|error| panic!("direct commit blame: {error:?}"));

    let BlameEntry::Commit(entry) = &page.entries[0] else {
        unreachable!();
    };
    assert_eq!(
        entry
            .parent_session
            .as_ref()
            .map(|value| value.id.0.as_str()),
        Some("manager-session")
    );
    assert_eq!(
        entry.owning_root.as_ref().map(|value| value.id.0.as_str()),
        Some("root-run")
    );
    assert_eq!(entry.attribution, AttributionOutcome::Proven);
}

#[test]
fn commit_conflicts_are_scoped_to_exact_retained_pages() {
    let target = BlameTarget::Commit {
        oid: KNOWN_COMMIT.to_owned(),
        repository: None,
    };
    let one_at_a_time = BlameService::new(
        PaginatedCommitGraph,
        QueryBounds {
            max_matches: 1,
            ..QueryBounds::default()
        },
    )
    .unwrap_or_else(|_| unreachable!());
    let first = one_at_a_time
        .execute(&target, None)
        .unwrap_or_else(|error| panic!("first commit page: {error:?}"));
    assert!(first.has_more);
    assert_eq!(first.continuation_reason, ContinuationReason::MoreMatches);
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.positions.len(), 1);
    let BlameEntry::Commit(first_entry) = &first.entries[0] else {
        unreachable!();
    };
    assert_eq!(first_entry.fact.id, "producer-fact-a");
    assert_eq!(first_entry.attribution, AttributionOutcome::Proven);

    let second = one_at_a_time
        .execute(&target, first.positions.first())
        .unwrap_or_else(|error| panic!("second commit page: {error:?}"));
    assert!(!second.has_more);
    assert_eq!(second.entries.len(), 1);
    let BlameEntry::Commit(second_entry) = &second.entries[0] else {
        unreachable!();
    };
    assert_eq!(second_entry.fact.id, "producer-fact-b");
    assert_eq!(second_entry.attribution, AttributionOutcome::Proven);

    let together = BlameService::new(
        PaginatedCommitGraph,
        QueryBounds {
            max_matches: 2,
            ..QueryBounds::default()
        },
    )
    .unwrap_or_else(|_| unreachable!())
    .execute(&target, None)
    .unwrap_or_else(|error| panic!("combined commit page: {error:?}"));
    assert!(!together.has_more);
    assert_eq!(together.entries.len(), 2);
    assert!(together.entries.iter().all(|entry| matches!(
        entry,
        BlameEntry::Commit(entry) if entry.attribution == AttributionOutcome::Conflicting
    )));
}

#[test]
fn resolved_pr_selector_is_cli_parseable_and_keeps_graph_key_opaque() {
    let canonical = canonical_pull_request_selector(
        "https://gitlab.corp.example/platform/ctx/-/merge_requests/42",
        None,
    )
    .unwrap_or_else(|_| unreachable!());
    assert_eq!(
        canonical.graph_key,
        "gitlab.corp.example/platform/ctx/pull_request/42"
    );
    assert_eq!(
        canonical.public_selector,
        "https://gitlab.corp.example/platform/ctx/-/merge_requests/42"
    );
}

#[test]
fn numbered_pr_requires_separate_repository() {
    assert!(canonical_pull_request_selector("42", None).is_err());
    let canonical = canonical_pull_request_selector("42", Some("github.com/ctxrs/ctx"))
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(canonical.public_selector, "42");
    assert_eq!(canonical.repository, "forge:github.com/ctxrs/ctx");
}

#[test]
fn repository_aliases_normalize_without_becoming_ambiguous() {
    assert_eq!(
        normalized_forge_repository("forge:github.com/ctxrs/ctx"),
        normalized_forge_repository("https://github.com/ctxrs/ctx/")
    );
    assert!(
        canonical_pull_request_selector(
            "https://github.com/ctxrs/ctx/pull/42",
            Some("forge:gitlab.com/ctxrs/ctx")
        )
        .is_err()
    );
}

#[test]
fn opaque_repository_identities_reach_exact_graph_lookup() {
    let cases = [
        (
            "https://GitHub.COM/ctxrs/../private?view=all#exact/",
            "https://GitHub.COM/ctxrs/../private?view=all#exact/",
        ),
        (
            "forge:GitHub.COM/ctxrs/../private?view=all#exact/",
            "forge:github.com/ctxrs/../private?view=all#exact/",
        ),
        ("forge:LOCALHOST/repo", "forge:localhost/repo"),
        (
            "forge:UNRECOGNIZED-HOST/repo",
            "forge:unrecognized-host/repo",
        ),
        ("forge:GitHub.COM/", "forge:github.com/"),
    ];
    for (requested, stored) in cases {
        let service = BlameService::new(
            ExactRepositoryGraph { repository: stored },
            QueryBounds::default(),
        )
        .unwrap_or_else(|_| unreachable!());
        let target = service
            .resolve_target(&BlameTarget::File {
                path: "opaque.rs".to_owned(),
                repository: Some(requested.to_owned()),
                lines: None,
            })
            .unwrap_or_else(|error| panic!("opaque repository lookup failed: {error:?}"));
        let ResolvedBlameTarget::File { repository, .. } = target else {
            unreachable!();
        };
        assert_eq!(repository.display, stored);
    }
}

#[test]
fn resolution_and_availability_failures_keep_distinct_non_sensitive_causes() {
    let repository = "forge:github.com/ctxrs/ctx";
    let service = BlameService::new(ExactRepositoryGraph { repository }, QueryBounds::default())
        .unwrap_or_else(|_| unreachable!());

    let target_absent = service
        .resolve_target(&BlameTarget::File {
            path: "private/missing.rs".to_owned(),
            repository: Some(repository.to_owned()),
            lines: None,
        })
        .unwrap_err();
    assert_eq!(
        target_absent,
        QueryError::TargetNotFound(ResourceKind::File)
    );

    let repository_absent = service
        .resolve_target(&BlameTarget::File {
            path: "opaque.rs".to_owned(),
            repository: Some("forge:github.com/ctxrs/missing".to_owned()),
            lines: None,
        })
        .unwrap_err();
    assert_eq!(repository_absent, QueryError::RepositorySelectorNotFound);

    let not_bound = service
        .resolve_target(&BlameTarget::File {
            path: "unbound.rs".to_owned(),
            repository: None,
            lines: None,
        })
        .unwrap_err();
    assert_eq!(not_bound, QueryError::RepositoryNotBound);

    let repository_ambiguous = service
        .resolve_target(&BlameTarget::File {
            path: "ambiguous.rs".to_owned(),
            repository: None,
            lines: None,
        })
        .unwrap_err();
    let QueryError::AmbiguousRepositoryCandidates(repository_candidates) = repository_ambiguous
    else {
        panic!("expected repository ambiguity");
    };
    assert_eq!(
        repository_candidates.candidates,
        vec![
            crate::protocol::BlameDiagnosticCandidate::Repository {
                selector: repository.to_owned(),
            },
            crate::protocol::BlameDiagnosticCandidate::Repository {
                selector: "forge:github.com/ctxrs/other".to_owned(),
            },
        ]
    );
    assert!(!repository_candidates.candidates_truncated);

    let commit_ambiguous = service
        .resolve_target(&BlameTarget::Commit {
            oid: "abc".to_owned(),
            repository: None,
        })
        .unwrap_err();
    let QueryError::AmbiguousTarget(commit_candidates) = commit_ambiguous else {
        panic!("expected commit target ambiguity");
    };
    assert_eq!(
        commit_candidates.candidates,
        vec![
            crate::protocol::BlameDiagnosticCandidate::Commit {
                repository: repository.to_owned(),
                oid: "a".repeat(40),
            },
            crate::protocol::BlameDiagnosticCandidate::Commit {
                repository: repository.to_owned(),
                oid: "b".repeat(40),
            },
        ]
    );
    assert!(!commit_candidates.candidates_truncated);

    let operation_unavailable = service
        .execute(
            &BlameTarget::File {
                path: "operation-unavailable.rs".to_owned(),
                repository: None,
                lines: None,
            },
            None,
        )
        .unwrap_err();
    assert_eq!(
        operation_unavailable,
        QueryError::OperationUnavailable(QueryOperation::FileBlame)
    );
}

#[test]
fn repository_ambiguity_candidates_are_public_sorted_bounded_and_never_singleton() {
    let details = AmbiguityCandidates::repositories(
        ["g", "c", "a", "f", "b", "e", "d", "a"]
            .into_iter()
            .map(|suffix| format!("forge:github.com/example/repo-{suffix}")),
    );
    assert_eq!(details.candidates.len(), 5);
    assert!(details.candidates_truncated);
    assert_eq!(
        details.candidates,
        ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(
                |suffix| crate::protocol::BlameDiagnosticCandidate::Repository {
                    selector: format!("forge:github.com/example/repo-{suffix}"),
                }
            )
            .collect::<Vec<_>>()
    );

    for details in [
        AmbiguityCandidates::repositories([
            "forge:github.com/example/only-public".to_owned(),
            "workspace:private-repository".to_owned(),
        ]),
        AmbiguityCandidates::repositories([
            "forge:github.com/example/duplicate".to_owned(),
            "forge:github.com/example/duplicate".to_owned(),
        ]),
    ] {
        assert!(details.candidates.is_empty());
        assert!(!details.candidates_truncated);
    }
}

#[test]
fn ambiguity_candidates_reject_private_repositories_and_malformed_commit_oids() {
    let repositories = AmbiguityCandidates::repositories(
        [
            "forge:github.com/example/b",
            "/home/private/repository",
            "forge:git.internal/example/repository",
            "forge:GitHub.com/example/repository",
            "forge:github.com/example/repository?token=secret",
            "forge:github.com/example/a",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    assert_eq!(
        repositories.candidates,
        ["a", "b"]
            .into_iter()
            .map(
                |suffix| crate::protocol::BlameDiagnosticCandidate::Repository {
                    selector: format!("forge:github.com/example/{suffix}"),
                }
            )
            .collect::<Vec<_>>()
    );

    let repository = "forge:github.com/example/repository";
    let commits = AmbiguityCandidates::commits(
        repository,
        [
            "a".repeat(40),
            "b".repeat(64),
            "c".repeat(39),
            "d".repeat(65),
            "g".repeat(40),
        ],
    );
    assert_eq!(
        commits.candidates,
        vec![
            crate::protocol::BlameDiagnosticCandidate::Commit {
                repository: repository.to_owned(),
                oid: "a".repeat(40),
            },
            crate::protocol::BlameDiagnosticCandidate::Commit {
                repository: repository.to_owned(),
                oid: "b".repeat(64),
            },
        ]
    );
    assert!(
        AmbiguityCandidates::commits("forge:git.internal/example/repository", ["a".repeat(40)])
            .candidates
            .is_empty()
    );
}

#[test]
fn mixed_case_pr_hosts_canonicalize_and_malformed_urls_stay_invalid() {
    let selector = "https://GitHub.COM/ctxrs/ctx/pull/42";
    let canonical = canonical_pull_request_selector(selector, None)
        .unwrap_or_else(|error| panic!("mixed-case PR host failed: {error:?}"));
    assert_eq!(canonical.graph_key, "github.com/ctxrs/ctx/pull_request/42");
    assert_eq!(canonical.repository, "forge:github.com/ctxrs/ctx");
    assert_eq!(canonical.public_selector, selector);
    let malformed = "https://github.com/ctxrs/ctx/pull/not-a-number";
    assert!(matches!(
        canonical_pull_request_selector(malformed, None),
        Err(QueryError::InvalidRequest(_))
    ));
    assert!(
        BlameTarget::PullRequest {
            selector: malformed.to_owned(),
            repository: None,
        }
        .validate()
        .is_err()
    );
}

#[test]
fn repository_scope_distinguishes_omitted_empty_and_exact_identity() {
    assert!(matches!(normalized_requested_repository(None), Ok(None)));
    for repository in ["", "   ", "\t"] {
        assert!(matches!(
            normalized_requested_repository(Some(repository)),
            Err(QueryError::InvalidRequest(_))
        ));
    }
    assert!(matches!(
        normalized_requested_repository(Some("workspace:CaseSensitiveRepo")),
        Ok(Some(repository)) if repository == "workspace:CaseSensitiveRepo"
    ));
}

#[test]
fn whitespace_repository_never_becomes_a_query_filter() {
    let service = BlameService::new(MixedCommitGraph::PROVEN, QueryBounds::default())
        .unwrap_or_else(|_| unreachable!());
    let error = service
        .resolve_target(&BlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: Some("   ".to_owned()),
            lines: None,
        })
        .unwrap_err();
    assert!(matches!(error, QueryError::InvalidRequest(_)));
}

#[test]
fn asserted_producer_conflicts_return_five_deterministic_cited_candidates() {
    let mut attributions = (b'a'..=b'g')
        .rev()
        .map(|suffix| {
            let suffix = char::from(suffix);
            production_attribution(&format!("producer-{suffix}"), &format!("fact-{suffix}"))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        normalize_attributions(&mut attributions),
        Ok(AttributionOutcome::Conflicting)
    );
    assert_eq!(attributions.len(), MAX_ATTRIBUTION_CANDIDATES);
    assert_eq!(
        attributions
            .iter()
            .map(|candidate| candidate.producing_session.0.as_str())
            .collect::<Vec<_>>(),
        [
            "producer-a",
            "producer-b",
            "producer-c",
            "producer-d",
            "producer-e",
        ]
    );
    assert!(attributions.iter().all(|candidate| {
        !candidate.citations.is_empty() && candidate.citations.iter().all(Citation::is_exact)
    }));
}

#[test]
fn possible_and_absent_evidence_remain_distinct_success_outcomes() {
    let mut possible = vec![possible_attribution("candidate-b", "possible-b")];
    assert_eq!(
        normalize_attributions(&mut possible),
        Ok(AttributionOutcome::Possible)
    );
    assert_eq!(
        normalize_attributions(&mut Vec::new()),
        Ok(AttributionOutcome::None)
    );

    possible.push(production_attribution("producer-a", "asserted-a"));
    assert_eq!(
        normalize_attributions(&mut possible),
        Ok(AttributionOutcome::Proven)
    );
    assert_eq!(possible[0].producing_session.0, "producer-a");
}

fn repository_resource() -> Resource {
    Resource {
        id: ResourceId("repository-resource".to_owned()),
        kind: ResourceKind::Repository,
        display: "logical-repository".to_owned(),
        logical_repository: None,
    }
}

fn file_resource() -> Resource {
    Resource {
        id: ResourceId("file-resource".to_owned()),
        kind: ResourceKind::File,
        display: "src/lib.rs".to_owned(),
        logical_repository: Some(ResourceId("repository-resource".to_owned())),
    }
}

fn known_commit_resource() -> Resource {
    Resource {
        id: ResourceId("known-commit-resource".to_owned()),
        kind: ResourceKind::Commit,
        display: KNOWN_COMMIT.to_owned(),
        logical_repository: Some(ResourceId("repository-resource".to_owned())),
    }
}

fn resource(id: &str, kind: ResourceKind) -> Resource {
    Resource {
        id: ResourceId(id.to_owned()),
        kind,
        display: id.to_owned(),
        logical_repository: None,
    }
}

fn production_attribution(session: &str, fact_id: &str) -> ProductionAttribution {
    ProductionAttribution {
        fact_id: fact_id.to_owned(),
        commit: ResourceId(KNOWN_COMMIT.to_owned()),
        producing_session: ResourceId(session.to_owned()),
        parent_session: None,
        root_run: None,
        direct_actor: None,
        fact_occurred_at_ms: None,
        relationship: crate::protocol::ProductionRelationship::ProducedBy,
        confidence: Confidence::Verified,
        state: FactState::Asserted,
        citations: vec![citation()],
    }
}

fn possible_attribution(session: &str, fact_id: &str) -> ProductionAttribution {
    ProductionAttribution {
        fact_id: fact_id.to_owned(),
        commit: ResourceId(KNOWN_COMMIT.to_owned()),
        producing_session: ResourceId(session.to_owned()),
        parent_session: None,
        root_run: None,
        direct_actor: None,
        fact_occurred_at_ms: None,
        relationship: crate::protocol::ProductionRelationship::PossiblyProducedBy,
        confidence: Confidence::Ambiguous,
        state: FactState::Ambiguous,
        citations: vec![citation()],
    }
}

fn paginated_producer_fact(fact_id: &str, producer: &str, rank: u8) -> (Fact, BlameFactPosition) {
    (
        Fact {
            id: fact_id.to_owned(),
            fact_type: "git.commit.produced".to_owned(),
            subject: ResourceId("known-commit-resource".to_owned()),
            predicate: "produced_by".to_owned(),
            object: Some(ResourceId(producer.to_owned())),
            value: None,
            occurred_at_ms: None,
            confidence: Confidence::Verified,
            state: FactState::Asserted,
            detector_version: "test@1".to_owned(),
            root_run: None,
            direct_actor: None,
            citations: vec![citation()],
        },
        BlameFactPosition {
            class: 0,
            rank,
            occurred_at_ms: None,
            resource_id: "known-commit-resource".to_owned(),
            fact_id: fact_id.to_owned(),
        },
    )
}

fn paginated_production(fact_id: &str, producer: &str) -> ProductionAttribution {
    ProductionAttribution {
        fact_id: fact_id.to_owned(),
        commit: ResourceId("known-commit-resource".to_owned()),
        producing_session: ResourceId(producer.to_owned()),
        parent_session: None,
        root_run: None,
        direct_actor: None,
        fact_occurred_at_ms: None,
        relationship: crate::protocol::ProductionRelationship::ProducedBy,
        confidence: Confidence::Verified,
        state: FactState::Asserted,
        citations: vec![citation()],
    }
}

fn git_observation(line: u32, commit: &str) -> crate::query::GitLineObservation {
    crate::query::GitLineObservation {
        file: ResourceId("file-resource".to_owned()),
        lines: LineRange {
            start: line,
            end: line,
        },
        commit_selector: commit.to_owned(),
        citation: citation(),
    }
}

fn citation() -> Citation {
    use crate::protocol::{CoreRecord, EvidenceCitation};

    let record: CoreRecord = crate::test_support::core_record();
    Citation::new(EvidenceCitation {
        core_generation_id: "a".repeat(64),
        source: record.source,
        session_id: record.session_id,
        event_id: record.event_id,
        event_sequence: record.event_sequence,
        byte_range: None,
        evidence_sha256: None,
    })
    .unwrap_or_else(|_| unreachable!())
}
