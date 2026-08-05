use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{MAX_BLAME_DIAGNOSTIC_CANDIDATES, MAX_BLAME_TARGET_BYTES};

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
                let Some(kind) = self.candidates[0].target_discriminant() else {
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
            if matches!(
                self.class,
                ErrorClass::MissingRepository
                    | ErrorClass::ResourceNotFound
                    | ErrorClass::Ambiguous
                    | ErrorClass::OperationUnavailable
            ) {
                return Err(invalid_blame_details(
                    "this blame error class requires typed diagnostic details",
                ));
            }
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
}

fn validate_ambiguity_candidates(
    candidates: &[BlameDiagnosticCandidate],
) -> Result<(), ProtocolError> {
    if candidates.len() < 2 {
        return Err(invalid_blame_details(
            "ambiguity requires at least two disclosed candidates",
        ));
    }
    Ok(())
}

fn validate_logical_repository(value: &str) -> Result<(), ProtocolError> {
    let unsafe_absolute = value.starts_with('/')
        || value.starts_with('\\')
        || value.starts_with('~')
        || value.starts_with("file:")
        || value.contains("://")
        || value.contains('@')
        || value
            .split('/')
            .any(|component| matches!(component, "." | ".."))
        || value.as_bytes().get(1).is_some_and(|byte| *byte == b':');
    if value.trim().is_empty()
        || value.len() > MAX_BLAME_TARGET_BYTES
        || value.chars().any(char::is_control)
        || unsafe_absolute
    {
        return Err(invalid_blame_details(
            "blame diagnostic repository candidate is not a safe logical selector",
        ));
    }
    Ok(())
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
                    repository("workspace:repo"),
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
    fn typed_details_are_required_and_unknown_fields_remain_rejected() {
        let missing = serde_json::json!({
            "class": "internal",
            "message": "detail",
            "retryable": false
        });
        assert!(serde_json::from_value::<ProtocolError>(missing).is_err());

        let mut unknown =
            serde_json::to_value(ProtocolError::new(ErrorClass::Internal, "detail")).unwrap();
        unknown["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ProtocolError>(unknown).is_err());
    }

    #[test]
    fn candidate_contract_is_bounded_sorted_typed_and_path_safe() {
        let mut candidates = (0..MAX_BLAME_DIAGNOSTIC_CANDIDATES)
            .map(|index| repository(&format!("forge:github.com/a/repo-{index}")))
            .collect::<Vec<_>>();
        let valid = BlameDiagnosticDetails {
            reason: BlameDiagnosticReason::RepositoryAmbiguous,
            candidates: candidates.clone(),
            candidates_truncated: true,
        };
        valid.validate().unwrap();

        candidates.push(repository("workspace:overflow"));
        assert_eq!(
            details(BlameDiagnosticReason::RepositoryAmbiguous, candidates)
                .validate()
                .unwrap_err()
                .class,
            ErrorClass::Corrupt
        );
        let duplicate = repository("workspace:duplicate");
        assert!(details(
            BlameDiagnosticReason::RepositoryAmbiguous,
            vec![duplicate.clone(), duplicate],
        )
        .validate()
        .is_err());
        assert!(details(
            BlameDiagnosticReason::RepositoryAmbiguous,
            vec![repository("workspace:z"), repository("workspace:a")],
        )
        .validate()
        .is_err());
        for selector in [
            "/private/repo",
            "C:/private/repo",
            "../../private/repo",
            "file:/private/repo",
            "https://user:token@example/repo",
        ] {
            assert!(details(
                BlameDiagnosticReason::RepositoryAmbiguous,
                vec![repository(selector), repository("workspace:safe")],
            )
            .validate()
            .is_err());
        }
    }

    #[test]
    fn reason_class_and_candidate_kind_matrix_is_closed() {
        let no_candidates = details(BlameDiagnosticReason::FileBlameNotCovered, Vec::new());
        ProtocolError::new(ErrorClass::OperationUnavailable, "detail")
            .with_blame_details(no_candidates.clone())
            .validate()
            .unwrap();
        assert!(ProtocolError::new(ErrorClass::Ambiguous, "detail")
            .with_blame_details(no_candidates)
            .validate()
            .is_err());

        let candidates = vec![
            repository("forge:github.com/a/repo"),
            repository("workspace:repo"),
        ];
        let ambiguous = details(BlameDiagnosticReason::RepositoryAmbiguous, candidates);
        ProtocolError::new(ErrorClass::Ambiguous, "detail")
            .with_blame_details(ambiguous.clone())
            .validate()
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
            .validate()
            .unwrap();
        assert!(ProtocolError::new(ErrorClass::ResourceNotFound, "detail")
            .with_blame_details(ambiguous)
            .validate()
            .is_err());
    }
}
