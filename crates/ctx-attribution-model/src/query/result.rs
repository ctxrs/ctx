use super::*;

impl BlameResult {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.snapshot.validate()?;
        self.outcome.validate()?;
        if self.matches.len() > MAX_BLAME_RESULTS as usize {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "blame result exceeds its match bound",
            ));
        }
        if self.evidence.len() > MAX_BLAME_EVIDENCE {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "blame result exceeds its evidence bound",
            ));
        }
        match (&self.target, &self.git_snapshot) {
            (ResolvedBlameTarget::File { .. }, Some(snapshot)) => {
                validate_bounded_text(&snapshot.head_oid, "Git HEAD object ID")?;
            }
            (ResolvedBlameTarget::File { .. }, None) => {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "file blame result is missing its Git snapshot",
                ));
            }
            (
                ResolvedBlameTarget::Commit { .. } | ResolvedBlameTarget::PullRequest { .. },
                Some(_),
            ) => {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "non-file blame result unexpectedly contains a Git snapshot",
                ));
            }
            (
                ResolvedBlameTarget::Commit { .. } | ResolvedBlameTarget::PullRequest { .. },
                None,
            ) => {}
        }
        let expected_unit = match &self.target {
            ResolvedBlameTarget::File { .. } => BlameCoverageUnit::CommittedLine,
            ResolvedBlameTarget::Commit { .. } => BlameCoverageUnit::CommitFact,
            ResolvedBlameTarget::PullRequest { .. } => BlameCoverageUnit::PullRequestRelationship,
        };
        if self.outcome.coverage.unit != expected_unit {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "blame coverage unit does not match the resolved target kind",
            ));
        }
        self.target.validate()?;
        if let Some(next) = &self.next {
            validate_cursor(Some(&next.cursor))?;
        }

        let mut available = BTreeSet::new();
        for (index, evidence) in self.evidence.iter().enumerate() {
            let expected = u32::try_from(index + 1).map_err(|_| {
                ProtocolError::new(ErrorClass::Bounds, "evidence number exceeds u32")
            })?;
            if evidence.number != expected || !evidence.citation.is_usable() {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "blame evidence must be usable and numbered contiguously from one",
                ));
            }
            available.insert(evidence.number);
        }

        let mut referenced = BTreeSet::new();
        for blame_match in &self.matches {
            blame_match.validate(&self.target, &available, &mut referenced)?;
        }
        match (&self.target, &self.lineage) {
            (ResolvedBlameTarget::Commit { commit, repository }, Some(lineage)) => {
                lineage.validate(commit, repository, &available, &mut referenced)?;
            }
            (
                ResolvedBlameTarget::File { .. } | ResolvedBlameTarget::PullRequest { .. },
                Some(_),
            ) => {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "commit lineage is only valid for commit blame results",
                ));
            }
            (_, None) => {}
        }
        if referenced != available {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "blame result contains unreferenced evidence",
            ));
        }
        if self.outcome != self.derived_page_outcome()? {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "blame outcome must exactly match the returned page evidence",
            ));
        }
        Ok(())
    }

    /// Derives semantics only from the bounded matches returned on this page.
    /// Continuations remain independent pages and are never scanned here.
    fn derived_page_outcome(&self) -> Result<BlameOutcome, ProtocolError> {
        let mut coverage = BlameCoverage::empty(match &self.target {
            ResolvedBlameTarget::File { .. } => BlameCoverageUnit::CommittedLine,
            ResolvedBlameTarget::Commit { .. } => BlameCoverageUnit::CommitFact,
            ResolvedBlameTarget::PullRequest { .. } => BlameCoverageUnit::PullRequestRelationship,
        });
        match &self.target {
            ResolvedBlameTarget::File { .. } => {
                let mut prior_end = None;
                for item in &self.matches {
                    let BlameMatch::File(item) = item else {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "file blame coverage contains a non-file match",
                        ));
                    };
                    if prior_end.is_some_and(|end| item.lines.start <= end) {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "file blame matches must have ordered, non-overlapping line ranges",
                        ));
                    }
                    let lines = item
                        .lines
                        .end
                        .checked_sub(item.lines.start)
                        .and_then(|span| span.checked_add(1))
                        .ok_or_else(|| {
                            ProtocolError::new(
                                ErrorClass::Bounds,
                                "file blame line count overflowed",
                            )
                        })?;
                    coverage.add(production_attribution(&item.production), lines)?;
                    prior_end = Some(item.lines.end);
                }
            }
            ResolvedBlameTarget::Commit { .. } => {
                let mut fact_ids = BTreeSet::new();
                let mut asserted_producers =
                    BTreeMap::<(ResourceKind, String), BTreeSet<(ResourceKind, String)>>::new();
                for item in &self.matches {
                    let BlameMatch::Commit(item) = item else {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "commit blame coverage contains a non-commit match",
                        ));
                    };
                    if !fact_ids.insert(item.fact_id.as_str()) {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "commit blame page contains duplicate fact units",
                        ));
                    }
                    if item.predicate == CommitPredicate::ProducedBy
                        && item.state == FactState::Asserted
                        && let Some(producer) = &item.object
                    {
                        asserted_producers
                            .entry((item.subject.kind, item.subject.id.clone()))
                            .or_default()
                            .insert((producer.kind, producer.id.clone()));
                    }
                }
                for item in &self.matches {
                    let BlameMatch::Commit(item) = item else {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "commit blame coverage contains a non-commit match",
                        ));
                    };
                    let conflicting_producers = asserted_producers
                        .get(&(item.subject.kind, item.subject.id.clone()))
                        .is_some_and(|producers| producers.len() >= 2);
                    coverage.add(commit_fact_attribution(item, conflicting_producers), 1)?;
                }
            }
            ResolvedBlameTarget::PullRequest { .. } => {
                let mut fact_ids = BTreeSet::new();
                for item in &self.matches {
                    let BlameMatch::PullRequest(item) = item else {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "pull request blame coverage contains a non-pull-request match",
                        ));
                    };
                    let (fact_id, attribution) = match &item.relationship {
                        PullRequestBlameRelationship::Activity(activity) => {
                            (activity.fact_id.as_str(), fact_attribution(activity.state))
                        }
                        PullRequestBlameRelationship::Commit(commit) => (
                            commit.fact_id.as_str(),
                            production_attribution(&commit.production),
                        ),
                    };
                    if !fact_ids.insert(fact_id) {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "pull request blame page contains duplicate fact units",
                        ));
                    }
                    coverage.add(attribution, 1)?;
                }
            }
        }
        Ok(BlameOutcome {
            attribution: coverage.aggregate_attribution(),
            coverage,
        })
    }

    pub fn validate_for_request(&self, request: &BlameRequest) -> Result<(), ProtocolError> {
        request.validate()?;
        self.validate()?;
        if self.snapshot != request.expected_snapshot {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "blame result snapshot does not match the requested Core snapshot",
            ));
        }
        let expected_core_generation_id = match &request.expected_snapshot {
            QuerySnapshotExpectation::Core { receipt } => &receipt.core_generation_id,
        };
        if self
            .evidence
            .iter()
            .any(|evidence| evidence.citation.core_generation_id != *expected_core_generation_id)
        {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "blame evidence generation does not match the requested Core snapshot",
            ));
        }
        if self.matches.len() > request.limit as usize {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "blame result exceeds the requested match limit",
            ));
        }

        match (&request.target, &self.target) {
            (
                BlameTarget::File {
                    path: requested_path,
                    repository: requested_repository,
                    lines: requested_lines,
                },
                ResolvedBlameTarget::File {
                    path: resolved_path,
                    repository: resolved_repository,
                    requested_lines: resolved_lines,
                },
            ) => {
                if requested_path != resolved_path
                    || !repository_selector_matches(
                        requested_repository.as_deref(),
                        resolved_repository,
                    )
                    || requested_lines != resolved_lines
                {
                    return Err(ProtocolError::new(
                        ErrorClass::Corrupt,
                        "resolved file target does not match the requested path, repository, and line range",
                    ));
                }
                if let Some(requested_lines) = requested_lines {
                    for blame_match in &self.matches {
                        let BlameMatch::File(file) = blame_match else {
                            return Err(ProtocolError::new(
                                ErrorClass::Corrupt,
                                "file blame request returned a non-file match",
                            ));
                        };
                        if !line_range_contains(requested_lines, &file.lines) {
                            return Err(ProtocolError::new(
                                ErrorClass::Corrupt,
                                "file blame match exceeds the requested line range",
                            ));
                        }
                    }
                }
            }
            (
                BlameTarget::Commit {
                    oid,
                    repository: requested_repository,
                },
                ResolvedBlameTarget::Commit {
                    commit: resolved_commit,
                    repository: resolved_repository,
                },
            ) => {
                if !commit_selector_matches(oid, &resolved_commit.display)
                    || !repository_selector_matches(
                        requested_repository.as_deref(),
                        resolved_repository,
                    )
                {
                    return Err(ProtocolError::new(
                        ErrorClass::Corrupt,
                        "resolved commit target does not match the requested object ID and repository",
                    ));
                }
                for blame_match in &self.matches {
                    let BlameMatch::Commit(commit) = blame_match else {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "commit blame request returned a non-commit match",
                        ));
                    };
                    if !same_resource_identity(&commit.subject, resolved_commit)
                        && !commit
                            .object
                            .as_ref()
                            .is_some_and(|object| same_resource_identity(object, resolved_commit))
                    {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "commit blame match does not involve the resolved commit",
                        ));
                    }
                }
            }
            (
                BlameTarget::PullRequest {
                    selector,
                    repository: requested_repository,
                },
                ResolvedBlameTarget::PullRequest {
                    selector: resolved_selector,
                    pull_request: resolved_pull_request,
                    repository: resolved_repository,
                },
            ) => {
                if selector != resolved_selector
                    || !repository_selector_matches(
                        requested_repository.as_deref(),
                        resolved_repository,
                    )
                {
                    return Err(ProtocolError::new(
                        ErrorClass::Corrupt,
                        "resolved pull request target does not match the requested selector and repository",
                    ));
                }
                for blame_match in &self.matches {
                    let BlameMatch::PullRequest(pull_request) = blame_match else {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "pull request blame request returned a non-pull-request match",
                        ));
                    };
                    if !same_resource_identity(&pull_request.pull_request, resolved_pull_request) {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "pull request blame match does not reference the resolved pull request",
                        ));
                    }
                }
            }
            _ => {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "resolved blame target kind does not match the request target",
                ));
            }
        }
        Ok(())
    }
}
