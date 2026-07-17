use std::{fmt, fs, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

const SEMANTIC_MODEL_RETRY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SemanticModelRetryPolicy {
    pub(crate) initial_backoff: Duration,
    pub(crate) max_backoff: Duration,
}

impl Default for SemanticModelRetryPolicy {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(30),
            max_backoff: Duration::from_secs(30 * 60),
        }
    }
}

impl SemanticModelRetryPolicy {
    pub(crate) fn validate(self) -> Result<Self, SemanticModelRetryPolicyError> {
        if self.initial_backoff.is_zero() {
            return Err(SemanticModelRetryPolicyError(
                "semantic model retry initial_backoff must be positive".to_owned(),
            ));
        }
        if self.max_backoff < self.initial_backoff {
            return Err(SemanticModelRetryPolicyError(
                "semantic model retry max_backoff must not be below initial_backoff".to_owned(),
            ));
        }
        Ok(self)
    }

    fn backoff_after_failure(self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(63);
        let multiplier = 1_u128 << exponent;
        let millis = self
            .initial_backoff
            .as_millis()
            .saturating_mul(multiplier)
            .min(self.max_backoff.as_millis())
            .min(u128::from(u64::MAX));
        Duration::from_millis(millis as u64)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SemanticModelRetryPolicyError(String);

impl fmt::Display for SemanticModelRetryPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SemanticModelRetryPolicyError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SemanticModelFailureClass {
    MemoryDeferred,
    Acquisition,
    Integrity,
    Inference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticModelFailure {
    pub(crate) class: SemanticModelFailureClass,
    pub(crate) message: String,
    pub(crate) retryable: bool,
}

impl SemanticModelFailure {
    pub(crate) fn retryable(class: SemanticModelFailureClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            retryable: true,
        }
    }

    pub(crate) fn terminal(class: SemanticModelFailureClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            retryable: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticModelRetryState {
    schema_version: u32,
    model_key: String,
    attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_eligible_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_failure: Option<SemanticModelFailure>,
}

impl SemanticModelRetryState {
    pub(crate) fn new(model_key: impl Into<String>) -> Self {
        Self {
            schema_version: SEMANTIC_MODEL_RETRY_SCHEMA_VERSION,
            model_key: model_key.into(),
            attempt: 0,
            next_eligible_at_ms: None,
            last_failure: None,
        }
    }

    pub(crate) fn reset_for_model(&mut self, model_key: &str) -> bool {
        if self.model_key == model_key {
            return false;
        }
        *self = Self::new(model_key);
        true
    }

    pub(crate) fn eligibility(
        &self,
        now_ms: i64,
        policy: SemanticModelRetryPolicy,
    ) -> Result<SemanticModelRetryEligibility, SemanticModelRetryPolicyError> {
        policy.validate()?;
        let Some(failure) = self.last_failure.as_ref() else {
            return Ok(SemanticModelRetryEligibility::Eligible {
                attempt: self.attempt.saturating_add(1),
            });
        };
        if !failure.retryable {
            return Ok(SemanticModelRetryEligibility::Terminal {
                attempt: self.attempt,
                failure: failure.clone(),
            });
        }
        if let Some(next_eligible_at_ms) = self
            .next_eligible_at_ms
            .filter(|next_eligible_at_ms| *next_eligible_at_ms > now_ms)
        {
            return Ok(SemanticModelRetryEligibility::Deferred {
                attempt: self.attempt,
                next_eligible_at_ms,
                failure: failure.clone(),
            });
        }
        Ok(SemanticModelRetryEligibility::Eligible {
            attempt: self.attempt.saturating_add(1),
        })
    }

    pub(crate) fn record_failure(
        &mut self,
        model_key: &str,
        now_ms: i64,
        failure: SemanticModelFailure,
        policy: SemanticModelRetryPolicy,
    ) -> Result<SemanticModelRetryEligibility, SemanticModelRetryPolicyError> {
        let policy = policy.validate()?;
        self.reset_for_model(model_key);
        self.attempt = self.attempt.saturating_add(1);
        self.next_eligible_at_ms = if failure.retryable {
            let delay_ms = policy
                .backoff_after_failure(self.attempt)
                .as_millis()
                .min(i64::MAX as u128) as i64;
            Some(now_ms.saturating_add(delay_ms))
        } else {
            None
        };
        self.last_failure = Some(failure);
        self.eligibility(now_ms, policy)
    }

    pub(crate) fn status(
        &self,
        now_ms: i64,
        policy: SemanticModelRetryPolicy,
    ) -> Result<SemanticModelRetryStatus, SemanticModelRetryPolicyError> {
        let eligibility = self.eligibility(now_ms, policy)?;
        let next_retry_at_ms = match &eligibility {
            SemanticModelRetryEligibility::Deferred {
                next_eligible_at_ms,
                ..
            } => Some(*next_eligible_at_ms),
            _ => None,
        };
        Ok(SemanticModelRetryStatus {
            failure_class: self.last_failure.as_ref().map(|failure| failure.class),
            attempt: self.attempt,
            next_retry_at_ms,
            retryable: self
                .last_failure
                .as_ref()
                .is_some_and(|failure| failure.retryable),
            terminal: matches!(eligibility, SemanticModelRetryEligibility::Terminal { .. }),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticModelRetryEligibility {
    Eligible {
        attempt: u32,
    },
    Deferred {
        attempt: u32,
        next_eligible_at_ms: i64,
        failure: SemanticModelFailure,
    },
    Terminal {
        attempt: u32,
        failure: SemanticModelFailure,
    },
}

impl SemanticModelRetryEligibility {
    pub(crate) fn into_failure(self) -> Option<SemanticModelRetryFailure> {
        match self {
            Self::Eligible { .. } => None,
            Self::Deferred {
                attempt,
                next_eligible_at_ms,
                failure,
            } => Some(SemanticModelRetryFailure::Deferred {
                attempt,
                next_eligible_at_ms,
                failure,
            }),
            Self::Terminal { attempt, failure } => {
                Some(SemanticModelRetryFailure::Terminal { attempt, failure })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticModelRetryFailure {
    Deferred {
        attempt: u32,
        next_eligible_at_ms: i64,
        failure: SemanticModelFailure,
    },
    Terminal {
        attempt: u32,
        failure: SemanticModelFailure,
    },
}

impl fmt::Display for SemanticModelRetryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deferred {
                attempt,
                next_eligible_at_ms,
                failure,
            } => write!(
                formatter,
                "semantic model retry attempt {attempt} is deferred until {next_eligible_at_ms}: {}",
                failure.message
            ),
            Self::Terminal { attempt, failure } => write!(
                formatter,
                "semantic model failure is terminal after attempt {attempt}: {}",
                failure.message
            ),
        }
    }
}

impl std::error::Error for SemanticModelRetryFailure {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticModelRetryStatus {
    #[serde(default)]
    pub(crate) failure_class: Option<SemanticModelFailureClass>,
    pub(crate) attempt: u32,
    #[serde(default)]
    pub(crate) next_retry_at_ms: Option<i64>,
    pub(crate) retryable: bool,
    pub(crate) terminal: bool,
}

impl SemanticModelRetryStatus {
    pub(crate) fn clear() -> Self {
        Self {
            failure_class: None,
            attempt: 0,
            next_retry_at_ms: None,
            retryable: false,
            terminal: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticModelRetryStore {
    path: PathBuf,
    policy: SemanticModelRetryPolicy,
}

impl SemanticModelRetryStore {
    pub(crate) fn new(path: impl Into<PathBuf>, policy: SemanticModelRetryPolicy) -> Self {
        Self {
            path: path.into(),
            policy,
        }
    }

    pub(crate) fn load_for_model(
        &self,
        model_key: &str,
    ) -> Result<SemanticModelRetryState, SemanticModelRetryStoreError> {
        self.policy
            .validate()
            .map_err(SemanticModelRetryStoreError::Policy)?;
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SemanticModelRetryState::new(model_key));
            }
            Err(error) => return Err(SemanticModelRetryStoreError::Io(error)),
        };
        let mut state: SemanticModelRetryState =
            serde_json::from_str(&text).map_err(SemanticModelRetryStoreError::Json)?;
        if state.schema_version != SEMANTIC_MODEL_RETRY_SCHEMA_VERSION {
            return Err(SemanticModelRetryStoreError::InvalidState(format!(
                "unsupported semantic model retry schema version {}",
                state.schema_version
            )));
        }
        let reset = state.reset_for_model(model_key);
        let normalized = state.normalize();
        if reset || normalized {
            self.save(&state)?;
        }
        Ok(state)
    }

    pub(crate) fn eligibility(
        &self,
        model_key: &str,
        now_ms: i64,
    ) -> Result<
        (SemanticModelRetryState, SemanticModelRetryEligibility),
        SemanticModelRetryStoreError,
    > {
        let state = self.load_for_model(model_key)?;
        let eligibility = state
            .eligibility(now_ms, self.policy)
            .map_err(SemanticModelRetryStoreError::Policy)?;
        Ok((state, eligibility))
    }

    pub(crate) fn record_failure(
        &self,
        model_key: &str,
        now_ms: i64,
        failure: SemanticModelFailure,
    ) -> Result<
        (SemanticModelRetryState, SemanticModelRetryEligibility),
        SemanticModelRetryStoreError,
    > {
        let mut state = self.load_for_model(model_key)?;
        let eligibility = state
            .record_failure(model_key, now_ms, failure, self.policy)
            .map_err(SemanticModelRetryStoreError::Policy)?;
        self.save(&state)?;
        Ok((state, eligibility))
    }

    pub(crate) fn record_success(
        &self,
        model_key: &str,
    ) -> Result<SemanticModelRetryState, SemanticModelRetryStoreError> {
        let state = SemanticModelRetryState::new(model_key);
        self.save(&state)?;
        Ok(state)
    }

    pub(crate) fn status(
        &self,
        model_key: &str,
        now_ms: i64,
    ) -> Result<SemanticModelRetryStatus, SemanticModelRetryStoreError> {
        self.load_for_model(model_key)?
            .status(now_ms, self.policy)
            .map_err(SemanticModelRetryStoreError::Policy)
    }

    pub(crate) fn save(
        &self,
        state: &SemanticModelRetryState,
    ) -> Result<(), SemanticModelRetryStoreError> {
        let value = serde_json::to_value(state).map_err(SemanticModelRetryStoreError::Json)?;
        super::write_private_json_file(&self.path, &value)
            .map_err(|error| SemanticModelRetryStoreError::Persistence(format!("{error:#}")))
    }
}

impl SemanticModelRetryState {
    fn normalize(&mut self) -> bool {
        let original = self.clone();
        match self.last_failure.as_ref() {
            None => {
                self.attempt = 0;
                self.next_eligible_at_ms = None;
            }
            Some(failure) => {
                self.attempt = self.attempt.max(1);
                if !failure.retryable {
                    self.next_eligible_at_ms = None;
                }
            }
        }
        self != &original
    }
}

#[derive(Debug)]
pub(crate) enum SemanticModelRetryStoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Policy(SemanticModelRetryPolicyError),
    InvalidState(String),
    Persistence(String),
}

impl fmt::Display for SemanticModelRetryStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "read semantic model retry state: {error}"),
            Self::Json(error) => write!(formatter, "parse semantic model retry state: {error}"),
            Self::Policy(error) => error.fmt(formatter),
            Self::InvalidState(message) | Self::Persistence(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for SemanticModelRetryStoreError {}
