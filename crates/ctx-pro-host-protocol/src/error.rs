use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::MAX_BLAME_DIAGNOSTIC_CANDIDATES;

const MAX_BLAME_DIAGNOSTIC_CANDIDATE_BYTES: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    EntitlementExpired,
    KeyStoreUnavailable,
    KeyStoreLocked,
    NotMaterialized,
    ProtocolMismatch,
    MissingSource,
    MissingRepository,
    ResourceNotFound,
    StaleFact,
    LineOutOfRange,
    StaleSnapshot,
    Ambiguous,
    OperationUnavailable,
    Corrupt,
    InvalidRequest,
    Bounds,
    RebuildRequired,
    Sequence,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlameDiagnosticReason {
    TargetNotIndexed,
    RepositorySelectorNotIndexed,
    RepositoryNotBound,
    CheckoutUnavailable,
    GitUnavailable,
    RepositoryAmbiguous,
    TargetAmbiguous,
    CommitRewriteAmbiguous,
    FileBlameNotCovered,
    CommitBlameNotCovered,
    PullRequestBlameNotCovered,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BlameDiagnosticCandidate {
    Repository { selector: String },
    Commit { repository: String, oid: String },
}

impl BlameDiagnosticCandidate {
    fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Repository { selector } => validate_logical_repository(selector),
            Self::Commit { repository, oid } => {
                validate_logical_repository(repository)?;
                if !matches!(oid.len(), 40 | 64)
                    || !oid
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                {
                    return Err(invalid_blame_details(
                        "blame diagnostic commit candidate must use a full lowercase Git object ID",
                    ));
                }
                Ok(())
            }
        }
    }

    const fn is_repository(&self) -> bool {
        matches!(self, Self::Repository { .. })
    }

    const fn is_commit(&self) -> bool {
        matches!(self, Self::Commit { .. })
    }

    const fn target_discriminant(&self) -> Option<u8> {
        match self {
            Self::Commit { .. } => Some(0),
            Self::Repository { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlameDiagnosticDetails {
    pub reason: BlameDiagnosticReason,
    pub candidates: Vec<BlameDiagnosticCandidate>,
    pub candidates_truncated: bool,
}

impl BlameDiagnosticDetails {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.candidates.len() > MAX_BLAME_DIAGNOSTIC_CANDIDATES {
            return Err(invalid_blame_details(
                "blame diagnostic exceeds its candidate bound",
            ));
        }
        if self.candidates_truncated && self.candidates.len() != MAX_BLAME_DIAGNOSTIC_CANDIDATES {
            return Err(invalid_blame_details(
                "a truncated blame diagnostic must fill its candidate bound",
            ));
        }
        for candidate in &self.candidates {
            candidate.validate()?;
        }
        if self.candidates.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(invalid_blame_details(
                "blame diagnostic candidates must be strictly sorted and unique",
            ));
        }

        match self.reason {
            BlameDiagnosticReason::RepositoryAmbiguous => {
                validate_ambiguity_candidates(&self.candidates)?;
                if !self
                    .candidates
                    .iter()
                    .all(BlameDiagnosticCandidate::is_repository)
                {
                    return Err(invalid_blame_details(
                        "repository ambiguity must contain only repository candidates",
                    ));
                }
            }
            BlameDiagnosticReason::TargetAmbiguous => {
                validate_ambiguity_candidates(&self.candidates)?;
                if let Some(first) = self.candidates.first() {
                    let Some(kind) = first.target_discriminant() else {
                        return Err(invalid_blame_details(
                            "target ambiguity cannot contain repository candidates",
                        ));
                    };
                    if !self
                        .candidates
                        .iter()
                        .all(|candidate| candidate.target_discriminant() == Some(kind))
                    {
                        return Err(invalid_blame_details(
                            "target ambiguity candidates must have one target kind",
                        ));
                    }
                }
            }
            BlameDiagnosticReason::CommitRewriteAmbiguous => {
                validate_ambiguity_candidates(&self.candidates)?;
                if !self
                    .candidates
                    .iter()
                    .all(BlameDiagnosticCandidate::is_commit)
                {
                    return Err(invalid_blame_details(
                        "commit rewrite ambiguity must contain only commit candidates",
                    ));
                }
            }
            BlameDiagnosticReason::TargetNotIndexed
            | BlameDiagnosticReason::RepositorySelectorNotIndexed
            | BlameDiagnosticReason::RepositoryNotBound
            | BlameDiagnosticReason::CheckoutUnavailable
            | BlameDiagnosticReason::GitUnavailable
            | BlameDiagnosticReason::FileBlameNotCovered
            | BlameDiagnosticReason::CommitBlameNotCovered
            | BlameDiagnosticReason::PullRequestBlameNotCovered => {
                if !self.candidates.is_empty() || self.candidates_truncated {
                    return Err(invalid_blame_details(
                        "non-ambiguity blame diagnostics cannot contain candidates",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtocolError {
    pub class: ErrorClass,
    pub message: String,
    pub retryable: bool,
    pub details: Option<BlameDiagnosticDetails>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtocolErrorWire {
    class: ErrorClass,
    message: String,
    retryable: bool,
    details: Value,
}

impl<'de> Deserialize<'de> for ProtocolError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ProtocolErrorWire::deserialize(deserializer)?;
        let details = serde_json::from_value::<Option<BlameDiagnosticDetails>>(wire.details)
            .map_err(serde::de::Error::custom)?;
        let error = Self {
            class: wire.class,
            message: wire.message,
            retryable: wire.retryable,
            details,
        };
        error
            .validate()
            .map_err(|validation| serde::de::Error::custom(validation.message))?;
        Ok(error)
    }
}

impl ProtocolError {
    pub fn new(class: ErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            retryable: false,
            details: None,
        }
    }

    #[must_use]
    pub const fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    #[must_use]
    pub fn with_blame_details(mut self, details: BlameDiagnosticDetails) -> Self {
        self.details = Some(details);
        self
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        let Some(details) = &self.details else {
            return Ok(());
        };
        details.validate()?;
        let valid = matches!(
            (self.class, details.reason),
            (
                ErrorClass::ResourceNotFound,
                BlameDiagnosticReason::TargetNotIndexed
                    | BlameDiagnosticReason::RepositorySelectorNotIndexed
            ) | (
                ErrorClass::MissingRepository,
                BlameDiagnosticReason::RepositoryNotBound
                    | BlameDiagnosticReason::CheckoutUnavailable
            ) | (
                ErrorClass::MissingSource,
                BlameDiagnosticReason::GitUnavailable
            ) | (
                ErrorClass::Ambiguous,
                BlameDiagnosticReason::RepositoryAmbiguous
                    | BlameDiagnosticReason::TargetAmbiguous
                    | BlameDiagnosticReason::CommitRewriteAmbiguous
            ) | (
                ErrorClass::OperationUnavailable,
                BlameDiagnosticReason::FileBlameNotCovered
                    | BlameDiagnosticReason::CommitBlameNotCovered
                    | BlameDiagnosticReason::PullRequestBlameNotCovered
            )
        );
        if !valid {
            return Err(invalid_blame_details(
                "blame diagnostic reason does not match its error class",
            ));
        }
        Ok(())
    }

    /// Applies the stricter typed-detail contract required by blame errors.
    ///
    /// Generic protocol operations may continue to use a null `details` field.
    /// Blame needs typed reasons for resource, ambiguity, and operation classes
    /// so the client never guesses a public diagnosis from a broad error class.
    pub fn validate_blame_details(&self) -> Result<(), ProtocolError> {
        self.validate()?;
        if self.details.is_none()
            && matches!(
                self.class,
                ErrorClass::MissingSource
                    | ErrorClass::MissingRepository
                    | ErrorClass::ResourceNotFound
                    | ErrorClass::Ambiguous
                    | ErrorClass::OperationUnavailable
            )
        {
            return Err(invalid_blame_details(
                "blame error class requires typed diagnostic details",
            ));
        }
        Ok(())
    }
}

fn validate_ambiguity_candidates(
    candidates: &[BlameDiagnosticCandidate],
) -> Result<(), ProtocolError> {
    if candidates.len() == 1 {
        return Err(invalid_blame_details(
            "ambiguity must disclose either zero candidates or at least two",
        ));
    }
    Ok(())
}

fn validate_logical_repository(value: &str) -> Result<(), ProtocolError> {
    let Some((host, path)) = value
        .strip_prefix("forge:")
        .and_then(|identity| identity.split_once('/'))
    else {
        return Err(invalid_blame_details(
            "blame diagnostic repository candidate must use the public forge namespace",
        ));
    };
    if value.is_empty()
        || value.len() > MAX_BLAME_DIAGNOSTIC_CANDIDATE_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
        || !valid_public_forge_host(host)
        || !valid_forge_path(path)
    {
        return Err(invalid_blame_details(
            "blame diagnostic repository candidate is not a canonical safe public forge identity",
        ));
    }
    Ok(())
}

fn valid_public_forge_host(host: &str) -> bool {
    let reserved = matches!(
        host,
        "localhost" | "local" | "private" | "workspace" | "internal"
    ) || [
        ".localhost",
        ".local",
        ".private",
        ".workspace",
        ".internal",
    ]
    .iter()
    .any(|suffix| host.ends_with(suffix));
    !host.is_empty()
        && host.contains('.')
        && host.bytes().any(|byte| byte.is_ascii_alphabetic())
        && !host.bytes().any(|byte| byte.is_ascii_uppercase())
        && !reserved
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn valid_forge_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|component| {
            !component.is_empty()
                && !matches!(component, "." | "..")
                && component.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
                })
        })
}

fn invalid_blame_details(message: &'static str) -> ProtocolError {
    ProtocolError::new(ErrorClass::Corrupt, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository(selector: &str) -> BlameDiagnosticCandidate {
        BlameDiagnosticCandidate::Repository {
            selector: selector.to_owned(),
        }
    }

    fn commit(repository: &str, oid: char) -> BlameDiagnosticCandidate {
        BlameDiagnosticCandidate::Commit {
            repository: repository.to_owned(),
            oid: oid.to_string().repeat(40),
        }
    }

    fn details(
        reason: BlameDiagnosticReason,
        candidates: Vec<BlameDiagnosticCandidate>,
    ) -> BlameDiagnosticDetails {
        BlameDiagnosticDetails {
            reason,
            candidates,
            candidates_truncated: false,
        }
    }

    #[test]
    fn retryability_and_typed_details_round_trip_without_helper_message_parsing() {
        let error = ProtocolError::new(ErrorClass::Ambiguous, "private helper detail")
            .with_retryable(true)
            .with_blame_details(details(
                BlameDiagnosticReason::RepositoryAmbiguous,
                vec![
                    repository("forge:github.com/a/repo"),
                    repository("forge:github.com/b/repo"),
                ],
            ));
        error.validate().unwrap();
        let encoded = serde_json::to_value(&error).unwrap();
        assert_eq!(encoded["retryable"], true);
        assert_eq!(encoded["message"], "private helper detail");
        assert_eq!(
            serde_json::from_value::<ProtocolError>(encoded).unwrap(),
            error
        );
    }

    #[test]
    fn generic_details_are_nullable_but_the_field_and_shape_remain_strict() {
        let missing = serde_json::json!({
            "class": "internal",
            "message": "detail",
            "retryable": false
        });
        assert!(serde_json::from_value::<ProtocolError>(missing).is_err());

        let nullable = serde_json::json!({
            "class": "internal",
            "message": "detail",
            "retryable": false,
            "details": null
        });
        let decoded = serde_json::from_value::<ProtocolError>(nullable).unwrap();
        assert!(decoded.details.is_none());
        assert!(serde_json::to_value(decoded).unwrap()["details"].is_null());

        let mut unknown =
            serde_json::to_value(ProtocolError::new(ErrorClass::Internal, "detail")).unwrap();
        unknown["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ProtocolError>(unknown).is_err());
    }

    #[test]
    fn candidate_contract_is_bounded_sorted_typed_and_public_forge_only() {
        let mut candidates = (0..MAX_BLAME_DIAGNOSTIC_CANDIDATES)
            .map(|index| repository(&format!("forge:github.com/a/repo-{index}")))
            .collect::<Vec<_>>();
        let valid = BlameDiagnosticDetails {
            reason: BlameDiagnosticReason::RepositoryAmbiguous,
            candidates: candidates.clone(),
            candidates_truncated: true,
        };
        valid.validate().unwrap();

        candidates.push(repository("forge:github.com/a/repo-overflow"));
        assert_eq!(
            details(BlameDiagnosticReason::RepositoryAmbiguous, candidates)
                .validate()
                .unwrap_err()
                .class,
            ErrorClass::Corrupt
        );
        let duplicate = repository("forge:github.com/a/duplicate");
        assert!(details(
            BlameDiagnosticReason::RepositoryAmbiguous,
            vec![duplicate.clone(), duplicate],
        )
        .validate()
        .is_err());
        assert!(details(
            BlameDiagnosticReason::RepositoryAmbiguous,
            vec![
                repository("forge:github.com/z/repo"),
                repository("forge:github.com/a/repo"),
            ],
        )
        .validate()
        .is_err());

        for selector in [
            "private:repository",
            "local:repository",
            "workspace:repository",
            "internal:repository",
            "/private/repo",
            "C:/private/repo",
            "../../private/repo",
            "file:/private/repo",
            "https://user:token@example/repo",
            "forge:GitHub.com/ctxrs/ctx",
            "forge:github.com",
            "forge:github.com/",
            "forge:localhost/ctxrs/ctx",
            "forge:git.internal/ctxrs/ctx",
            "forge:127.0.0.1/ctxrs/ctx",
            "forge:github.com/ctxrs/ctx?token=secret",
            "forge:github.com/ctxrs/ctx#fragment",
            "forge:github.com/ctxrs/token=secret",
            " forge:github.com/ctxrs/ctx",
            "forge:github.com/ctxrs/ctx ",
            "forge:github.com/ctxrs/\nsecret",
        ] {
            assert!(
                repository(selector).validate().is_err(),
                "accepted {selector:?}"
            );
        }

        let prefix = "forge:github.com/";
        let at_bound = format!(
            "{prefix}{}",
            "a".repeat(MAX_BLAME_DIAGNOSTIC_CANDIDATE_BYTES - prefix.len())
        );
        assert_eq!(at_bound.len(), MAX_BLAME_DIAGNOSTIC_CANDIDATE_BYTES);
        repository(&at_bound).validate().unwrap();
        repository(&format!("{at_bound}a")).validate().unwrap_err();

        for oid in ["a".repeat(40), "b".repeat(64)] {
            BlameDiagnosticCandidate::Commit {
                repository: "forge:github.com/ctxrs/ctx".to_owned(),
                oid,
            }
            .validate()
            .unwrap();
        }
        for oid in ["a".repeat(39), "b".repeat(65), "A".repeat(40)] {
            BlameDiagnosticCandidate::Commit {
                repository: "forge:github.com/ctxrs/ctx".to_owned(),
                oid,
            }
            .validate()
            .unwrap_err();
        }
    }

    #[test]
    fn ambiguity_may_safely_omit_candidates_but_never_disclose_one() {
        for reason in [
            BlameDiagnosticReason::RepositoryAmbiguous,
            BlameDiagnosticReason::TargetAmbiguous,
            BlameDiagnosticReason::CommitRewriteAmbiguous,
        ] {
            details(reason, Vec::new()).validate().unwrap();
        }
        details(
            BlameDiagnosticReason::RepositoryAmbiguous,
            vec![repository("forge:github.com/a/repo")],
        )
        .validate()
        .unwrap_err();
    }

    #[test]
    fn reason_class_and_candidate_kind_matrix_is_closed() {
        let no_candidates = details(BlameDiagnosticReason::FileBlameNotCovered, Vec::new());
        ProtocolError::new(ErrorClass::OperationUnavailable, "detail")
            .with_blame_details(no_candidates.clone())
            .validate_blame_details()
            .unwrap();
        assert!(ProtocolError::new(ErrorClass::Ambiguous, "detail")
            .with_blame_details(no_candidates)
            .validate()
            .is_err());

        let candidates = vec![
            repository("forge:github.com/a/repo"),
            repository("forge:github.com/b/repo"),
        ];
        let ambiguous = details(BlameDiagnosticReason::RepositoryAmbiguous, candidates);
        ProtocolError::new(ErrorClass::Ambiguous, "detail")
            .with_blame_details(ambiguous.clone())
            .validate_blame_details()
            .unwrap();
        let commits = details(
            BlameDiagnosticReason::CommitRewriteAmbiguous,
            vec![
                commit("forge:github.com/a/repo", 'a'),
                commit("forge:github.com/a/repo", 'b'),
            ],
        );
        ProtocolError::new(ErrorClass::Ambiguous, "detail")
            .with_blame_details(commits)
            .validate_blame_details()
            .unwrap();
        assert!(ProtocolError::new(ErrorClass::ResourceNotFound, "detail")
            .with_blame_details(ambiguous)
            .validate()
            .is_err());
    }

    #[test]
    fn blame_boundary_requires_typed_details_without_changing_generic_errors() {
        for class in [
            ErrorClass::MissingSource,
            ErrorClass::MissingRepository,
            ErrorClass::ResourceNotFound,
            ErrorClass::Ambiguous,
            ErrorClass::OperationUnavailable,
        ] {
            let generic = ProtocolError::new(class, "private helper detail");
            generic.validate().unwrap();
            generic.validate_blame_details().unwrap_err();
        }

        ProtocolError::new(ErrorClass::Internal, "private helper detail")
            .validate_blame_details()
            .unwrap();
    }
}
