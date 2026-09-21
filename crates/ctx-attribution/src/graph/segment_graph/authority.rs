//! Local Git authority resolved from checked work-graph records.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::git_executable::GitExecutable;
use crate::protocol::ResourceKind;
use crate::query::{Citation, QueryError, ResourceId};

use super::SegmentGraph;
use super::merge::{Lookup, graph_id};
use crate::graph::git::{
    CertifiedLiveRoot, ExactGitBlameFile, GitBlameAuthority, RepositoryWorktreeIdentity,
};
use crate::graph::segment::{
    AttributeValue, EvidenceRelationship, FILE_TOUCHED, REPOSITORY_LIVE_ACCESS, ServingRecord,
};

impl GitBlameAuthority for SegmentGraph {
    fn authorized_git_executable(&self) -> Option<&GitExecutable> {
        self.git_executable.as_ref()
    }

    fn exact_file_authority(&self, file_id: &ResourceId) -> Result<ExactGitBlameFile, QueryError> {
        let records = self.merged_records(
            FILE_TOUCHED,
            Lookup::Exact {
                repository: None,
                term: &file_id.0,
            },
        )?;
        let records = records
            .into_iter()
            .filter(|record| graph_id(&record.subject).as_deref() == Ok(file_id.0.as_str()))
            .filter(|record| record.subject.typed_kind().ok() == Some(ResourceKind::File))
            .filter(|record| record.grants_file_identity_authority())
            .collect::<Vec<_>>();
        if records.is_empty() {
            return Err(QueryError::RepositoryUnavailable);
        }

        let mut identity = None::<(String, String, Option<String>)>;
        let mut citations = BTreeMap::<String, Citation>::new();
        for record in &records {
            let current = (
                record
                    .subject
                    .display()
                    .map_err(|_| QueryError::RepositoryUnavailable)?,
                record.repository_id.clone(),
                record.subject.worktree_id.clone(),
            );
            if identity
                .as_ref()
                .is_some_and(|identity| identity != &current)
            {
                return Err(QueryError::RepositoryUnavailable);
            }
            identity = Some(current);
            for stored in record
                .citations
                .iter()
                .filter(|citation| citation.relationship == EvidenceRelationship::Supports)
            {
                let citation = self
                    .active_citations(std::slice::from_ref(stored))?
                    .pop()
                    .ok_or(QueryError::RepositoryUnavailable)?;
                if citations
                    .get(&stored.citation_id)
                    .is_some_and(|prior| prior != &citation)
                {
                    return Err(Self::query_error());
                }
                citations
                    .entry(stored.citation_id.clone())
                    .or_insert(citation);
            }
        }
        let core_citation = citations
            .into_values()
            .next()
            .ok_or(QueryError::RepositoryUnavailable)?;
        let (relative_path, repository_id, worktree_id) =
            identity.ok_or(QueryError::RepositoryUnavailable)?;
        Ok(ExactGitBlameFile {
            relative_path,
            repository: RepositoryWorktreeIdentity {
                repository_id,
                worktree_id,
            },
            core_citation,
        })
    }

    fn certified_live_root_candidates(
        &self,
        repository: &RepositoryWorktreeIdentity,
    ) -> Result<Vec<CertifiedLiveRoot>, QueryError> {
        let records = self.merged_records(
            REPOSITORY_LIVE_ACCESS,
            Lookup::Exact {
                repository: Some(&repository.repository_id),
                term: &repository.repository_id,
            },
        )?;
        let mut roots = Vec::new();
        for record in records {
            if record.repository_id != repository.repository_id
                || record.subject.repository_id.as_deref()
                    != Some(repository.repository_id.as_str())
                || repository.worktree_id.as_ref().is_some_and(|worktree_id| {
                    record.subject.worktree_id.as_ref() != Some(worktree_id)
                })
            {
                continue;
            }
            if let Some(root) = self.root_from_record(&record)? {
                roots.push(root);
            }
        }
        latest_roots(roots)
    }

    fn revalidate_certified_live_root(
        &self,
        authorization: &CertifiedLiveRoot,
    ) -> Result<(), QueryError> {
        let records = self.merged_records(
            REPOSITORY_LIVE_ACCESS,
            Lookup::Exact {
                repository: None,
                term: &authorization.worktree_resource_id,
            },
        )?;
        let mut roots = Vec::new();
        for record in records {
            if graph_id(&record.subject).as_deref()
                != Ok(authorization.worktree_resource_id.as_str())
                || record.object.as_ref().map(graph_id).transpose()?.as_deref()
                    != Some(authorization.repository_resource_id.as_str())
            {
                continue;
            }
            if let Some(root) = self.root_from_record(&record)? {
                roots.push(root);
            }
        }
        let roots = latest_roots(roots)?;
        if roots.as_slice() == [authorization.clone()] {
            Ok(())
        } else {
            Err(QueryError::RepositoryUnavailable)
        }
    }
}

impl SegmentGraph {
    fn root_from_record(
        &self,
        record: &ServingRecord,
    ) -> Result<Option<CertifiedLiveRoot>, QueryError> {
        if !record.grants_live_repository_access() {
            return Ok(None);
        }
        let Some(repository) = record.object.as_ref() else {
            return Ok(None);
        };
        if record.subject.typed_kind().ok() != Some(ResourceKind::Worktree)
            || repository.typed_kind().ok() != Some(ResourceKind::Repository)
        {
            return Ok(None);
        }
        // A structural Flat record is insufficient for live authority: require
        // at least one exact active-generation supporting Core citation.
        let supporting = record
            .citations
            .iter()
            .filter(|citation| citation.relationship == EvidenceRelationship::Supports)
            .cloned()
            .collect::<Vec<_>>();
        if supporting.is_empty() {
            return Ok(None);
        }
        let _ = self.active_citations(&supporting)?;
        let Some(AttributeValue::String(local_root)) = record.attributes.get("local_root") else {
            return Err(Self::query_error());
        };
        let Some(AttributeValue::String(fingerprint)) =
            record.attributes.get("locator_fingerprint")
        else {
            return Err(Self::query_error());
        };
        let Some(AttributeValue::Integer(observed_at_unix_ms)) =
            record.attributes.get("observed_at_unix_ms")
        else {
            return Err(Self::query_error());
        };
        Ok(Some(CertifiedLiveRoot {
            worktree_resource_id: graph_id(&record.subject)?,
            repository_resource_id: graph_id(repository)?,
            path: PathBuf::from(local_root),
            security_geometry_fingerprint: fingerprint.clone(),
            observed_at_unix_ms: *observed_at_unix_ms,
        }))
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RootKey {
    worktree_resource_id: String,
    repository_resource_id: String,
    path: PathBuf,
    fingerprint: String,
    observed_at_unix_ms: i64,
}

fn latest_roots(roots: Vec<CertifiedLiveRoot>) -> Result<Vec<CertifiedLiveRoot>, QueryError> {
    let mut latest_by_worktree = BTreeMap::<(String, String), i64>::new();
    for root in &roots {
        latest_by_worktree
            .entry((
                root.worktree_resource_id.clone(),
                root.repository_resource_id.clone(),
            ))
            .and_modify(|latest| *latest = (*latest).max(root.observed_at_unix_ms))
            .or_insert(root.observed_at_unix_ms);
    }
    let mut unique = BTreeMap::<RootKey, CertifiedLiveRoot>::new();
    for root in roots {
        let worktree = (
            root.worktree_resource_id.clone(),
            root.repository_resource_id.clone(),
        );
        if latest_by_worktree.get(&worktree) != Some(&root.observed_at_unix_ms) {
            continue;
        }
        let key = RootKey {
            worktree_resource_id: root.worktree_resource_id.clone(),
            repository_resource_id: root.repository_resource_id.clone(),
            path: root.path.clone(),
            fingerprint: root.security_geometry_fingerprint.clone(),
            observed_at_unix_ms: root.observed_at_unix_ms,
        };
        unique.entry(key).or_insert(root);
    }
    Ok(unique.into_values().collect())
}
