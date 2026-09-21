//! Exact target and repository resolution for bounded blame.

use std::collections::BTreeSet;

use super::service::{BlameGraph, BlameService, normalized_requested_repository};
use super::{
    AmbiguityCandidates, QueryError, Resource, ResourceId, ResourceKind, ResourceSelector,
};

const AMBIGUITY_LOOKAHEAD: usize = crate::protocol::MAX_BLAME_DIAGNOSTIC_CANDIDATES + 1;

pub(super) struct ResolvedCommitTarget {
    pub(super) commit: Resource,
    pub(super) repository: Resource,
}

impl<G: BlameGraph> BlameService<G> {
    pub(super) fn resolve_one(&self, selector: ResourceSelector) -> Result<Resource, QueryError> {
        let resources = self.graph.resolve(&selector, AMBIGUITY_LOOKAHEAD)?;
        match resources.as_slice() {
            [] => Err(QueryError::TargetNotFound(selector.kind)),
            [resource] => Ok(resource.clone()),
            _ => Err(self.resolution_ambiguity(selector.kind, &resources)?),
        }
    }

    fn resolution_ambiguity(
        &self,
        kind: ResourceKind,
        resources: &[Resource],
    ) -> Result<QueryError, QueryError> {
        let repository_ids = resources
            .iter()
            .map(|resource| {
                resource
                    .logical_repository
                    .clone()
                    .ok_or(QueryError::RepositoryNotBound)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let repositories = self.load_repositories(&repository_ids)?;
        if repositories.len() > 1 {
            let details = AmbiguityCandidates::repositories(
                repositories
                    .into_iter()
                    .map(|repository| repository.display),
            );
            return valid_ambiguity(details).map(QueryError::AmbiguousRepositoryCandidates);
        }
        if kind != ResourceKind::Commit {
            return Err(QueryError::Backend(
                "target resolution returned duplicate repository-local identities".to_owned(),
            ));
        }
        let repository = repositories.first().ok_or_else(|| {
            QueryError::Backend("target resolution lost its repository identity".to_owned())
        })?;
        let details = AmbiguityCandidates::commits(
            &repository.display,
            resources.iter().map(|resource| resource.display.clone()),
        );
        valid_ambiguity(details).map(QueryError::AmbiguousTarget)
    }

    pub(super) fn resolve_repository_selector(
        &self,
        requested: Option<&str>,
    ) -> Result<Option<Resource>, QueryError> {
        let Some(requested) = requested else {
            return Ok(None);
        };
        let repositories = self.graph.resolve(
            &ResourceSelector {
                kind: ResourceKind::Repository,
                value: requested.to_owned(),
                repository: None,
            },
            AMBIGUITY_LOOKAHEAD,
        )?;
        match repositories.as_slice() {
            [] => Err(QueryError::RepositorySelectorNotFound),
            [repository] if repository.kind == ResourceKind::Repository => {
                Ok(Some(repository.clone()))
            }
            [_] => Err(QueryError::RepositoryNotBound),
            _ if repositories
                .iter()
                .all(|repository| repository.kind == ResourceKind::Repository) =>
            {
                let details = AmbiguityCandidates::repositories(
                    repositories
                        .iter()
                        .map(|repository| repository.display.clone()),
                );
                Err(QueryError::AmbiguousRepositoryCandidates(valid_ambiguity(
                    details,
                )?))
            }
            _ => Err(QueryError::RepositoryNotBound),
        }
    }

    pub(super) fn resolve_commit_target(
        &self,
        oid: &str,
        requested_repository: Option<&str>,
    ) -> Result<ResolvedCommitTarget, QueryError> {
        let repository_selector = normalized_requested_repository(requested_repository)?;
        let requested_repository =
            self.resolve_repository_selector(repository_selector.as_deref())?;
        let selected = self.resolve_one(ResourceSelector {
            kind: ResourceKind::Commit,
            value: oid.to_ascii_lowercase(),
            repository: repository_selector.clone(),
        })?;
        let repository = self.repository_for(&selected, requested_repository.as_ref())?;
        Ok(ResolvedCommitTarget {
            commit: selected,
            repository,
        })
    }

    pub(super) fn repository_for(
        &self,
        resource: &Resource,
        requested: Option<&Resource>,
    ) -> Result<Resource, QueryError> {
        let repository_id = resource
            .logical_repository
            .as_ref()
            .ok_or(QueryError::RepositoryNotBound)?;
        let repositories = self
            .graph
            .resources(std::slice::from_ref(repository_id), 2)?;
        let repository = match repositories.as_slice() {
            [repository]
                if repository.id == *repository_id
                    && repository.kind == ResourceKind::Repository =>
            {
                repository.clone()
            }
            [] => return Err(QueryError::RepositoryNotBound),
            [_] => return Err(QueryError::RepositoryNotBound),
            _ => {
                return Err(QueryError::Backend(
                    "repository identity resolved to multiple resources".to_owned(),
                ));
            }
        };
        if let Some(requested) = requested
            && requested.id != repository.id
        {
            let details = AmbiguityCandidates::repositories([
                repository.display.clone(),
                requested.display.clone(),
            ]);
            return Err(QueryError::AmbiguousRepositoryCandidates(valid_ambiguity(
                details,
            )?));
        }
        Ok(repository)
    }

    fn load_repositories(
        &self,
        repository_ids: &BTreeSet<ResourceId>,
    ) -> Result<Vec<Resource>, QueryError> {
        let ids = repository_ids.iter().cloned().collect::<Vec<_>>();
        let repositories = self.graph.resources(&ids, ids.len())?;
        let loaded_ids = repositories
            .iter()
            .filter(|repository| repository.kind == ResourceKind::Repository)
            .map(|repository| repository.id.clone())
            .collect::<BTreeSet<_>>();
        if loaded_ids != *repository_ids || repositories.len() != repository_ids.len() {
            return Err(QueryError::Backend(
                "ambiguous target lost a certified repository identity".to_owned(),
            ));
        }
        Ok(repositories)
    }
}

fn valid_ambiguity(details: AmbiguityCandidates) -> Result<AmbiguityCandidates, QueryError> {
    if details.candidates.len() == 1 {
        return Err(QueryError::Backend(
            "ambiguous resolution exposed only one safe public candidate".to_owned(),
        ));
    }
    Ok(details)
}
