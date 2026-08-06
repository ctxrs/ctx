use ctx_history_core::GitObjectId;

use super::{
    repository_mutable_evidence_state, repository_object_domain_sha256, utf8_lines,
    CertifiedCandidate, EventProbeBudget, GitCertifier, ProbeFailure,
    MAX_VERIFIED_COMMIT_OPERATION_OBJECTS,
};

impl GitCertifier {
    /// Verifies one canonical bounded set of full operation objects in the
    /// certificate's immutable repository/object-format domain. The linked
    /// receipt, not object existence, remains the mapping authority.
    pub(in crate::repository_attribution) fn verify_commit_operation_objects(
        &self,
        certificate: &CertifiedCandidate,
        object_ids: &[GitObjectId],
        budget: &mut EventProbeBudget,
    ) -> Result<[u8; 32], ProbeFailure> {
        if object_ids.is_empty() || object_ids.len() > MAX_VERIFIED_COMMIT_OPERATION_OBJECTS {
            return Err(ProbeFailure::Failed(
                "commit_operation_object_bound_exceeded",
            ));
        }
        if object_ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(ProbeFailure::Failed(
                "commit_operation_objects_are_not_canonical",
            ));
        }
        for object_id in object_ids {
            object_id
                .validate_contract()
                .map_err(|_| ProbeFailure::Failed("invalid_commit_operation_object"))?;
            if object_id.format != certificate.object_format() {
                return Err(ProbeFailure::Failed(
                    "commit_operation_object_format_mismatch",
                ));
            }
        }

        certificate.ensure_current_geometry()?;
        let opening_mutable_state = repository_mutable_evidence_state(
            &certificate.git_dir,
            &certificate.common_dir,
            certificate.branch.as_deref(),
        )?;
        if opening_mutable_state != certificate.mutable_evidence_state {
            return Err(ProbeFailure::ConcurrentDrift);
        }

        let revisions = object_ids
            .iter()
            .map(|object_id| format!("{}^{{commit}}", object_id.hex))
            .collect::<Vec<_>>();
        let mut arguments = vec!["show", "-s", "--format=%H"];
        arguments.extend(revisions.iter().map(String::as_str));
        let output = self.run_git(&certificate.repository_root, &arguments, false, budget)?;
        let resolved = utf8_lines(&output)?;
        if resolved.len() != object_ids.len()
            || resolved
                .iter()
                .zip(object_ids)
                .any(|(actual, expected)| *actual != expected.hex.as_str())
        {
            return Err(ProbeFailure::Failed(
                "commit_operation_object_resolution_mismatch",
            ));
        }

        certificate.ensure_current_geometry()?;
        let closing_mutable_state = repository_mutable_evidence_state(
            &certificate.git_dir,
            &certificate.common_dir,
            certificate.branch.as_deref(),
        )?;
        if closing_mutable_state != opening_mutable_state {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        Ok(repository_object_domain_sha256(certificate))
    }
}
