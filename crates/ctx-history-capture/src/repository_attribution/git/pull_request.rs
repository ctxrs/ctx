use ctx_history_core::GitObjectId;

use super::{
    parsing::{parse_exact_merge_metadata, utf8_lines},
    CertifiedCandidate, EventProbeBudget, GitCertifier, ProbeFailure,
    ResolvedPullRequestMergeMembership, MAX_PULL_REQUEST_CONTAINS_COMMITS,
};

impl GitCertifier {
    pub(in super::super) fn resolve_pull_request_merge_membership(
        &self,
        certificate: &CertifiedCandidate,
        merged_as: &GitObjectId,
        budget: &mut EventProbeBudget,
    ) -> Result<ResolvedPullRequestMergeMembership, ProbeFailure> {
        self.resolve_pull_request_merge_membership_with_between_probe(
            certificate,
            merged_as,
            budget,
            || {},
        )
    }

    #[cfg(test)]
    pub(in super::super) fn resolve_pull_request_merge_membership_for_test(
        &self,
        certificate: &CertifiedCandidate,
        merged_as: &GitObjectId,
        budget: &mut EventProbeBudget,
        between_probe: impl FnOnce(),
    ) -> Result<ResolvedPullRequestMergeMembership, ProbeFailure> {
        self.resolve_pull_request_merge_membership_with_between_probe(
            certificate,
            merged_as,
            budget,
            between_probe,
        )
    }

    fn resolve_pull_request_merge_membership_with_between_probe(
        &self,
        certificate: &CertifiedCandidate,
        merged_as: &GitObjectId,
        budget: &mut EventProbeBudget,
        between_probe: impl FnOnce(),
    ) -> Result<ResolvedPullRequestMergeMembership, ProbeFailure> {
        merged_as
            .validate_contract()
            .map_err(|_| ProbeFailure::Failed("invalid_pull_request_merge_oid"))?;
        if certificate.object_format() != merged_as.format {
            return Err(ProbeFailure::Failed(
                "pull_request_merge_object_format_mismatch",
            ));
        }
        certificate.ensure_current_geometry()?;
        let executable_state = self.executable_state()?;
        let opening_shallow = self.repository_is_shallow(certificate, budget)?;
        let revision = format!("{}^{{commit}}", merged_as.hex);
        let opening_metadata = self.run_git(
            &certificate.repository_root,
            &["show", "-s", "--format=%H%x00%P", &revision],
            false,
            budget,
        )?;
        let (object_id, _) =
            parse_exact_merge_metadata(&opening_metadata, certificate.object_format())?;
        if &object_id != merged_as {
            return Err(ProbeFailure::Failed("pull_request_merge_oid_mismatch"));
        }
        let range = format!("{}^1..{}^2", merged_as.hex, merged_as.hex);
        let opening_range = self.run_git(
            &certificate.repository_root,
            &["rev-list", "--topo-order", "--max-count=257", &range],
            false,
            budget,
        )?;
        let lines = utf8_lines(&opening_range)?;
        if lines.is_empty() || lines.len() > MAX_PULL_REQUEST_CONTAINS_COMMITS {
            return Err(ProbeFailure::Failed(
                "pull_request_merge_membership_limit_exceeded",
            ));
        }
        let mut contains_commits = lines
            .iter()
            .map(|line| {
                let object_id = GitObjectId {
                    format: merged_as.format,
                    hex: (*line).to_owned(),
                };
                object_id
                    .validate_contract()
                    .map_err(|_| ProbeFailure::Failed("invalid_pull_request_member_oid"))?;
                Ok(object_id)
            })
            .collect::<Result<Vec<_>, ProbeFailure>>()?;
        let original_len = contains_commits.len();
        contains_commits.sort();
        contains_commits.dedup();
        if contains_commits.len() != original_len
            || contains_commits
                .iter()
                .any(|object_id| object_id == merged_as)
        {
            return Err(ProbeFailure::Failed(
                "invalid_pull_request_merge_membership",
            ));
        }
        between_probe();
        certificate.ensure_current_geometry()?;
        let closing_metadata = self.run_git(
            &certificate.repository_root,
            &["show", "-s", "--format=%H%x00%P", &revision],
            false,
            budget,
        )?;
        let (closing_object_id, _) =
            parse_exact_merge_metadata(&closing_metadata, certificate.object_format())?;
        let closing_range = self.run_git(
            &certificate.repository_root,
            &["rev-list", "--topo-order", "--max-count=257", &range],
            false,
            budget,
        )?;
        let closing_shallow = self.repository_is_shallow(certificate, budget)?;
        certificate.ensure_current_geometry()?;
        if closing_object_id != *merged_as
            || opening_metadata != closing_metadata
            || opening_range != closing_range
            || opening_shallow != closing_shallow
            || executable_state != self.executable_state()?
        {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        if opening_shallow {
            return Err(ProbeFailure::Failed(
                "pull_request_merge_membership_shallow_repository",
            ));
        }
        Ok(ResolvedPullRequestMergeMembership { contains_commits })
    }

    fn repository_is_shallow(
        &self,
        certificate: &CertifiedCandidate,
        budget: &mut EventProbeBudget,
    ) -> Result<bool, ProbeFailure> {
        let output = self.run_git(
            &certificate.repository_root,
            &["rev-parse", "--is-shallow-repository"],
            false,
            budget,
        )?;
        match utf8_lines(&output)?.as_slice() {
            ["false"] => Ok(false),
            ["true"] => Ok(true),
            _ => Err(ProbeFailure::Failed(
                "unexpected_git_shallow_repository_status",
            )),
        }
    }
}
