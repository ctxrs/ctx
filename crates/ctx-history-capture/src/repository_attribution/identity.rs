use std::{collections::BTreeMap, fmt::Write};

use super::{git::CertifiedCandidate, outcome::UnscopedOutcomeObservation};
use ctx_history_core::{
    CoreRecordAnnotation, RepositoryAbstention, RepositoryAbstentionReason, RepositoryAlias,
    RepositoryAliasKind, RepositoryBinding, RepositoryEvidence, RepositoryEvidenceConfidence,
    RepositoryEvidenceKind, CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
};
use sha2::{Digest, Sha256};

const MAX_PROVIDER_NATIVE_IDENTITIES: usize = 16;

pub(super) enum ProviderIdentityResolution {
    Absent,
    Binding(Box<RepositoryBinding>),
    Abstained,
}

impl ProviderIdentityResolution {
    pub(super) fn was_attempted(&self) -> bool {
        !matches!(self, Self::Absent)
    }

    pub(super) fn binds_all_outcomes(&self, outcomes: &[UnscopedOutcomeObservation]) -> bool {
        let Self::Binding(binding) = self else {
            return false;
        };
        !outcomes.is_empty()
            && outcomes.iter().all(|outcome| {
                let UnscopedOutcomeObservation::Exact(outcome) = outcome else {
                    return false;
                };
                outcome.pull_request.as_ref().is_some_and(|pull_request| {
                    binding
                        .aliases
                        .iter()
                        .any(|alias| alias_identity_matches(alias, &pull_request.forge_repository))
                })
            })
    }
}

pub(super) fn resolve_provider_native_identity(
    aliases: Vec<RepositoryAlias>,
    annotation: &mut CoreRecordAnnotation,
) -> ProviderIdentityResolution {
    if aliases.is_empty() {
        return ProviderIdentityResolution::Absent;
    }
    if aliases.len() > MAX_PROVIDER_NATIVE_IDENTITIES {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeProject,
            RepositoryAbstentionReason::Ambiguous,
            "provider_native_identity_limit_exceeded",
        );
        return ProviderIdentityResolution::Abstained;
    }

    let mut by_logical_id = BTreeMap::<String, Vec<RepositoryAlias>>::new();
    for mut alias in aliases {
        if alias.kind != RepositoryAliasKind::Forge {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::ProviderNativeProject,
                RepositoryAbstentionReason::Unsafe,
                "provider_native_identity_is_not_forge_structured",
            );
            return ProviderIdentityResolution::Abstained;
        }
        alias.host.make_ascii_lowercase();
        let logical_id = logical_repository_id(&alias);
        let validation = logical_only_binding(logical_id.clone(), vec![alias.clone()]);
        if validation.validate_contract().is_err() {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::ProviderNativeProject,
                RepositoryAbstentionReason::Unsafe,
                "invalid_provider_native_repository_alias",
            );
            return ProviderIdentityResolution::Abstained;
        }
        let values = by_logical_id.entry(logical_id).or_default();
        if !values.contains(&alias) {
            values.push(alias);
        }
    }
    if by_logical_id.len() != 1 {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeProject,
            RepositoryAbstentionReason::ConflictingIdentity,
            "provider_native_repository_identities_conflict",
        );
        return ProviderIdentityResolution::Abstained;
    }
    let (logical_id, aliases) = by_logical_id.into_iter().next().expect("one identity");
    let binding = logical_only_binding(logical_id, aliases);
    if binding.validate_contract().is_err() {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeProject,
            RepositoryAbstentionReason::Unsafe,
            "invalid_provider_native_repository_binding",
        );
        ProviderIdentityResolution::Abstained
    } else {
        ProviderIdentityResolution::Binding(Box::new(binding))
    }
}

pub(super) fn reconcile_provider_identity(
    annotation: &mut CoreRecordAnnotation,
    certified: &mut [CertifiedCandidate],
    resolution: ProviderIdentityResolution,
) {
    let provider = match resolution {
        ProviderIdentityResolution::Absent => return,
        // Invalid or ambiguous provider metadata is an abstention for that
        // evidence lane only. Independently certified structured activity
        // remains valid and must not be erased.
        ProviderIdentityResolution::Abstained => return,
        ProviderIdentityResolution::Binding(provider) => provider,
    };
    let matching = certified
        .iter()
        .enumerate()
        .filter(|(_, certificate)| {
            provider
                .aliases
                .iter()
                .any(|native| binding_accepts_forge_repository(&certificate.binding, native))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if matching.len() > 1 {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeProject,
            RepositoryAbstentionReason::ConflictingIdentity,
            "provider_native_identity_matches_multiple_local_certificates",
        );
        annotation.repository_bindings.push(*provider);
        return;
    }
    if let Some(index) = matching.first().copied() {
        let certificate = &mut certified[index];
        for alias in &provider.aliases {
            if !certificate.binding.aliases.contains(alias) {
                certificate.binding.aliases.push(alias.clone());
            }
        }
        for evidence in &provider.evidence {
            if !certificate.binding.evidence.contains(evidence) {
                certificate.binding.evidence.push(evidence.clone());
            }
        }
        return;
    }
    if !certified.is_empty() {
        let identifies_secondary_remote = certified.iter().any(|certificate| {
            provider.aliases.iter().any(|native| {
                certificate
                    .binding
                    .aliases
                    .iter()
                    .any(|local| alias_identity_matches(local, native))
            })
        });
        if !identifies_secondary_remote {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::ProviderNativeProject,
                RepositoryAbstentionReason::ConflictingIdentity,
                "provider_native_identity_does_not_match_local_certificate",
            );
        }
        // Provider-native identity has precedence for its own exact outcome
        // segment. A provider identity naming a configured secondary remote is
        // a normal fork/upstream topology, not a repository conflict. Other
        // structured activity lanes remain independent and may certify
        // additional repositories in the same event.
        annotation.repository_bindings.push(*provider);
        return;
    }
    annotation.repository_bindings.push(*provider);
}

pub(super) fn alias_identity_matches(left: &RepositoryAlias, right: &RepositoryAlias) -> bool {
    left.host.eq_ignore_ascii_case(&right.host)
        && left.namespace == right.namespace
        && left.name == right.name
}

pub(super) fn binding_accepts_forge_repository(
    binding: &RepositoryBinding,
    forge_repository: &RepositoryAlias,
) -> bool {
    let logical_forge_matches = binding
        .logical_repository_id
        .strip_prefix("forge:")
        .map(|logical| forge_logical_identity_matches(logical, forge_repository));
    if logical_forge_matches == Some(false) {
        return false;
    }
    logical_forge_matches == Some(true)
        || (logical_forge_matches.is_none()
            && binding
                .aliases
                .iter()
                .any(|alias| alias_identity_matches(alias, forge_repository)))
}

fn logical_only_binding(
    logical_repository_id: String,
    aliases: Vec<RepositoryAlias>,
) -> RepositoryBinding {
    let mut digest = Sha256::new();
    digest.update(b"ctx.repository.provider-binding.v1");
    digest.update(logical_repository_id.as_bytes());
    let digest = digest.finalize();
    RepositoryBinding {
        binding_id: format!("binding:{}", hex_digest(&digest)),
        logical_repository_id,
        checkout_id: None,
        worktree_id: None,
        aliases,
        git_object_format: None,
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::ProviderNativeProject,
            confidence: RepositoryEvidenceConfidence::Explicit,
        }],
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn logical_repository_id(alias: &RepositoryAlias) -> String {
    let mut path = alias.namespace.join("/");
    path.push('/');
    path.push_str(&alias.name);
    format!("forge:{}/{path}", alias.host)
}

fn forge_logical_identity_matches(logical: &str, repository: &RepositoryAlias) -> bool {
    let Some((host, path)) = logical.split_once('/') else {
        return false;
    };
    let mut expected_path = repository.namespace.join("/");
    expected_path.push('/');
    expected_path.push_str(&repository.name);
    host.eq_ignore_ascii_case(&repository.host) && path == expected_path
}

pub(super) fn push_abstention(
    annotation: &mut CoreRecordAnnotation,
    evidence_kind: RepositoryEvidenceKind,
    reason: RepositoryAbstentionReason,
    detail: &str,
) {
    let abstention = RepositoryAbstention {
        evidence_kind,
        reason,
        detail: Some(detail.to_owned()),
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    };
    if !annotation.repository_abstentions.contains(&abstention) {
        annotation.repository_abstentions.push(abstention);
    }
}
