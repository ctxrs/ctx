use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use ring::constant_time;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use ctx_protocol::{
    SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED, SEARCH_MAX_CLAUSE_BYTES,
    SEARCH_MAX_TOTAL_CLAUSE_BYTES,
};

use super::readiness::{
    SemanticEffectiveBackend, SemanticReadinessBlocker, SemanticReadinessBlockerCode,
    SemanticReadinessDiagnostics, SemanticReadinessState, SemanticRetrievalDiagnostics,
    SemanticRetrievalRequestMode,
};

pub(crate) const SEMANTIC_QUERY_RPC_SCHEMA_VERSION: u32 = 1;
pub(crate) const SEMANTIC_QUERY_MAX_CLAUSES: usize = 1;
pub(crate) const SEMANTIC_QUERY_MAX_TEXT_BYTES_PER_CLAUSE: usize = SEARCH_MAX_CLAUSE_BYTES;
pub(crate) const SEMANTIC_QUERY_MAX_TOTAL_TEXT_BYTES: usize = SEARCH_MAX_TOTAL_CLAUSE_BYTES;
pub(crate) const SEMANTIC_QUERY_MAX_CANDIDATE_EVENT_IDS: usize =
    SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED;
pub(crate) const SEMANTIC_QUERY_MAX_HITS_PER_CLAUSE: usize =
    SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED;
pub(crate) const SEMANTIC_QUERY_MAX_TOTAL_HITS: usize =
    SEARCH_MAX_CANDIDATES_PER_POSITIVE_SEED;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SemanticQueryServiceOperation {
    RetrieveSemanticClauses,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticQueryServiceRequest {
    pub(crate) schema_version: u32,
    pub(crate) op: SemanticQueryServiceOperation,
    pub(crate) token: String,
    pub(crate) model_key: String,
    pub(crate) request_mode: SemanticRetrievalRequestMode,
    pub(crate) clauses: Vec<SemanticQueryClauseRequest>,
}

impl fmt::Debug for SemanticQueryServiceRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticQueryServiceRequest")
            .field("schema_version", &self.schema_version)
            .field("op", &self.op)
            .field("token", &"<redacted>")
            .field("model_key", &self.model_key)
            .field("request_mode", &self.request_mode)
            .field("clauses", &self.clauses)
            .finish()
    }
}

impl SemanticQueryServiceRequest {
    pub(crate) fn new(
        model_key: impl Into<String>,
        request_mode: SemanticRetrievalRequestMode,
        clauses: Vec<SemanticQueryClauseRequest>,
    ) -> Self {
        Self {
            schema_version: SEMANTIC_QUERY_RPC_SCHEMA_VERSION,
            op: SemanticQueryServiceOperation::RetrieveSemanticClauses,
            // daemon_query_request injects the endpoint token immediately
            // before serialization to the local transport.
            token: String::new(),
            model_key: model_key.into(),
            request_mode,
            clauses,
        }
    }

    pub(crate) fn authenticate_and_validate(
        self,
        expected_token: &str,
        expected_model_key: &str,
    ) -> Result<AuthenticatedSemanticQueryRequest, SemanticQueryContractError> {
        if expected_token.is_empty()
            || constant_time::verify_slices_are_equal(
                self.token.as_bytes(),
                expected_token.as_bytes(),
            )
            .is_err()
        {
            return Err(SemanticQueryContractError::new(
                SemanticQueryFailureCode::AuthenticationFailed,
                "semantic query authentication failed",
            ));
        }
        self.validate_for_model(expected_model_key)?;
        Ok(AuthenticatedSemanticQueryRequest {
            model_key: self.model_key,
            request_mode: self.request_mode,
            clauses: self.clauses,
        })
    }

    pub(crate) fn validate_for_model(
        &self,
        expected_model_key: &str,
    ) -> Result<(), SemanticQueryContractError> {
        if self.schema_version != SEMANTIC_QUERY_RPC_SCHEMA_VERSION {
            return Err(SemanticQueryContractError::new(
                SemanticQueryFailureCode::UnsupportedSchema,
                format!(
                    "unsupported semantic query schema version {}",
                    self.schema_version
                ),
            ));
        }
        if self.model_key != expected_model_key {
            return Err(SemanticQueryContractError::new(
                SemanticQueryFailureCode::ModelMismatch,
                "semantic query model key mismatch",
            ));
        }
        validate_semantic_query_clauses(&self.clauses)?;
        if self.request_mode == SemanticRetrievalRequestMode::AutomaticRerank
            && self.clauses.iter().any(|clause| {
                clause
                    .candidate_event_ids
                    .as_ref()
                    .map_or(true, Vec::is_empty)
            })
        {
            return Err(SemanticQueryContractError::invalid_request(
                "automatic semantic reranking requires a non-empty bounded lexical candidate set",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticQueryClauseRequest {
    pub(crate) clause_id: u32,
    pub(crate) text: String,
    pub(crate) hit_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) candidate_event_ids: Option<Vec<Uuid>>,
}

impl SemanticQueryClauseRequest {
    pub(crate) fn new(clause_id: u32, text: impl Into<String>, hit_limit: usize) -> Self {
        Self {
            clause_id,
            text: text.into(),
            hit_limit,
            candidate_event_ids: None,
        }
    }

    pub(crate) fn with_candidate_event_ids(mut self, event_ids: Vec<Uuid>) -> Self {
        self.candidate_event_ids = Some(event_ids);
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedSemanticQueryRequest {
    model_key: String,
    request_mode: SemanticRetrievalRequestMode,
    clauses: Vec<SemanticQueryClauseRequest>,
}

impl AuthenticatedSemanticQueryRequest {
    pub(crate) fn model_key(&self) -> &str {
        &self.model_key
    }

    pub(crate) fn clauses(&self) -> &[SemanticQueryClauseRequest] {
        &self.clauses
    }

    pub(crate) fn request_mode(&self) -> SemanticRetrievalRequestMode {
        self.request_mode
    }

    pub(crate) fn into_clauses(self) -> Vec<SemanticQueryClauseRequest> {
        self.clauses
    }
}

fn validate_semantic_query_clauses(
    clauses: &[SemanticQueryClauseRequest],
) -> Result<(), SemanticQueryContractError> {
    if clauses.is_empty() || clauses.len() > SEMANTIC_QUERY_MAX_CLAUSES {
        return Err(SemanticQueryContractError::invalid_request(format!(
            "semantic query must contain between 1 and {SEMANTIC_QUERY_MAX_CLAUSES} clauses"
        )));
    }

    let mut clause_ids = HashSet::with_capacity(clauses.len());
    let mut total_text_bytes = 0_usize;
    let mut total_candidate_ids = 0_usize;
    let mut total_hits = 0_usize;
    for clause in clauses {
        if !clause_ids.insert(clause.clause_id) {
            return Err(SemanticQueryContractError::invalid_request(format!(
                "duplicate semantic clause id {}",
                clause.clause_id
            )));
        }
        let text = clause.text.trim();
        if text.is_empty() {
            return Err(SemanticQueryContractError::invalid_request(format!(
                "semantic clause {} has empty text",
                clause.clause_id
            )));
        }
        if text.len() > SEMANTIC_QUERY_MAX_TEXT_BYTES_PER_CLAUSE {
            return Err(SemanticQueryContractError::invalid_request(format!(
                "semantic clause {} exceeds the {} byte text limit",
                clause.clause_id, SEMANTIC_QUERY_MAX_TEXT_BYTES_PER_CLAUSE
            )));
        }
        total_text_bytes = total_text_bytes.saturating_add(text.len());
        if clause.hit_limit == 0 || clause.hit_limit > SEMANTIC_QUERY_MAX_HITS_PER_CLAUSE {
            return Err(SemanticQueryContractError::invalid_request(format!(
                "semantic clause {} hit limit must be between 1 and {}",
                clause.clause_id, SEMANTIC_QUERY_MAX_HITS_PER_CLAUSE
            )));
        }
        total_hits = total_hits.saturating_add(clause.hit_limit);
        total_candidate_ids = total_candidate_ids.saturating_add(
            clause
                .candidate_event_ids
                .as_ref()
                .map(Vec::len)
                .unwrap_or(0),
        );
    }

    if total_text_bytes > SEMANTIC_QUERY_MAX_TOTAL_TEXT_BYTES {
        return Err(SemanticQueryContractError::invalid_request(format!(
            "semantic query exceeds the {SEMANTIC_QUERY_MAX_TOTAL_TEXT_BYTES} byte total text limit"
        )));
    }
    if total_candidate_ids > SEMANTIC_QUERY_MAX_CANDIDATE_EVENT_IDS {
        return Err(SemanticQueryContractError::invalid_request(format!(
            "semantic query exceeds the {SEMANTIC_QUERY_MAX_CANDIDATE_EVENT_IDS} candidate event limit"
        )));
    }
    if total_hits > SEMANTIC_QUERY_MAX_TOTAL_HITS {
        return Err(SemanticQueryContractError::invalid_request(format!(
            "semantic query exceeds the {SEMANTIC_QUERY_MAX_TOTAL_HITS} total hit limit"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticQueryServiceResponse {
    pub(crate) schema_version: u32,
    pub(crate) ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) readiness: Option<SemanticReadinessDiagnostics>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) clauses: Vec<SemanticQueryClauseResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<SemanticQueryFailure>,
}

impl SemanticQueryServiceResponse {
    pub(crate) fn success(
        request: &AuthenticatedSemanticQueryRequest,
        readiness: SemanticReadinessDiagnostics,
        clauses: Vec<SemanticQueryClauseResponse>,
    ) -> Result<Self, SemanticQueryContractError> {
        if !readiness.retrieval_available {
            return Err(SemanticQueryContractError::invalid_response(
                "semantic query success requires retrieval-ready state",
            ));
        }
        validate_semantic_query_response(request, &clauses)?;
        Ok(Self {
            schema_version: SEMANTIC_QUERY_RPC_SCHEMA_VERSION,
            ok: true,
            model_key: Some(request.model_key().to_owned()),
            readiness: Some(readiness),
            clauses,
            error: None,
        })
    }

    pub(crate) fn failure(error: SemanticQueryFailure) -> Self {
        Self {
            schema_version: SEMANTIC_QUERY_RPC_SCHEMA_VERSION,
            ok: false,
            model_key: None,
            readiness: None,
            clauses: Vec::new(),
            error: Some(error),
        }
    }

    pub(crate) fn explicit_semantic_unavailable(readiness: &SemanticReadinessDiagnostics) -> Self {
        let mut response = Self::failure(SemanticQueryFailure::from_readiness(readiness));
        response.readiness = Some(readiness.clone());
        response
    }

    pub(crate) fn validate_for_request(
        &self,
        request: &SemanticQueryServiceRequest,
        expected_model_key: &str,
    ) -> Result<(), SemanticQueryContractError> {
        request.validate_for_model(expected_model_key)?;
        if self.schema_version != SEMANTIC_QUERY_RPC_SCHEMA_VERSION {
            return Err(SemanticQueryContractError::invalid_response(format!(
                "unsupported semantic query response schema version {}",
                self.schema_version
            )));
        }
        if self.ok {
            if self.error.is_some() {
                return Err(SemanticQueryContractError::invalid_response(
                    "semantic query success must not contain an error",
                ));
            }
            if self.model_key.as_deref() != Some(expected_model_key) {
                return Err(SemanticQueryContractError::invalid_response(
                    "semantic query response model key mismatch",
                ));
            }
            let readiness = self.readiness.as_ref().ok_or_else(|| {
                SemanticQueryContractError::invalid_response(
                    "semantic query success is missing readiness diagnostics",
                )
            })?;
            if !readiness.retrieval_available {
                return Err(SemanticQueryContractError::invalid_response(
                    "semantic query success requires retrieval-ready state",
                ));
            }
            let authenticated = AuthenticatedSemanticQueryRequest {
                model_key: request.model_key.clone(),
                request_mode: request.request_mode,
                clauses: request.clauses.clone(),
            };
            validate_semantic_query_response(&authenticated, &self.clauses)?;
        } else {
            if self.error.is_none() {
                return Err(SemanticQueryContractError::invalid_response(
                    "semantic query failure is missing an error",
                ));
            }
            if !self.clauses.is_empty() {
                return Err(SemanticQueryContractError::invalid_response(
                    "semantic query failure must not contain clause results",
                ));
            }
        }
        Ok(())
    }
}

fn validate_semantic_query_response(
    request: &AuthenticatedSemanticQueryRequest,
    clauses: &[SemanticQueryClauseResponse],
) -> Result<(), SemanticQueryContractError> {
    if clauses.len() != request.clauses().len() {
        return Err(SemanticQueryContractError::invalid_response(
            "semantic query response clause count does not match the request",
        ));
    }
    let limits = request
        .clauses()
        .iter()
        .map(|clause| (clause.clause_id, clause.hit_limit))
        .collect::<HashMap<_, _>>();
    let mut response_ids = HashSet::with_capacity(clauses.len());
    let mut total_hits = 0_usize;
    for clause in clauses {
        let Some(hit_limit) = limits.get(&clause.clause_id) else {
            return Err(SemanticQueryContractError::invalid_response(format!(
                "semantic query response contains unknown clause id {}",
                clause.clause_id
            )));
        };
        if !response_ids.insert(clause.clause_id) {
            return Err(SemanticQueryContractError::invalid_response(format!(
                "semantic query response repeats clause id {}",
                clause.clause_id
            )));
        }
        if clause.hits.len() > *hit_limit {
            return Err(SemanticQueryContractError::invalid_response(format!(
                "semantic query response clause {} exceeds its requested hit limit",
                clause.clause_id
            )));
        }
        if !clause.diagnostics.attempted
            || clause.diagnostics.request_mode != request.request_mode()
            || clause.diagnostics.effective_backend
                != match request.request_mode() {
                    SemanticRetrievalRequestMode::AutomaticRerank => {
                        SemanticEffectiveBackend::Hybrid
                    }
                    SemanticRetrievalRequestMode::ExplicitSemantic => {
                        SemanticEffectiveBackend::Semantic
                    }
                }
        {
            return Err(SemanticQueryContractError::invalid_response(format!(
                "semantic query response clause {} has inconsistent retrieval diagnostics",
                clause.clause_id
            )));
        }
        total_hits = total_hits.saturating_add(clause.hits.len());
        for hit in &clause.hits {
            if !hit.similarity.is_finite() || hit.start_char > hit.end_char {
                return Err(SemanticQueryContractError::invalid_response(format!(
                    "semantic query response clause {} contains an invalid vector hit",
                    clause.clause_id
                )));
            }
        }
    }
    if total_hits > SEMANTIC_QUERY_MAX_TOTAL_HITS {
        return Err(SemanticQueryContractError::invalid_response(
            "semantic query response exceeds the total hit limit",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticQueryClauseResponse {
    pub(crate) clause_id: u32,
    pub(crate) hits: Vec<SemanticQueryVectorHit>,
    pub(crate) diagnostics: SemanticRetrievalDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticQueryVectorHit {
    pub(crate) event_id: Uuid,
    pub(crate) similarity: f32,
    pub(crate) source_text_hash: String,
    pub(crate) start_char: usize,
    pub(crate) end_char: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SemanticQueryFailureCode {
    AuthenticationFailed,
    UnsupportedSchema,
    InvalidRequest,
    ModelMismatch,
    NotReady,
    RetryDeferred,
    Busy,
    RetrievalFailed,
    CandidateLimitExceeded,
    VectorByteLimitExceeded,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticQueryFailure {
    pub(crate) code: SemanticQueryFailureCode,
    pub(crate) message: String,
    pub(crate) retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next_eligible_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) readiness: Option<SemanticReadinessState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) blocker: Option<SemanticReadinessBlockerCode>,
}

impl SemanticQueryFailure {
    pub(crate) fn new(
        code: SemanticQueryFailureCode,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            next_eligible_at_ms: None,
            readiness: None,
            blocker: None,
        }
    }

    pub(crate) fn from_readiness(readiness: &SemanticReadinessDiagnostics) -> Self {
        let blocker = readiness.primary_blocker();
        let code = match blocker {
            Some(SemanticReadinessBlocker::ModelRetryDeferred { .. }) => {
                SemanticQueryFailureCode::RetryDeferred
            }
            _ => SemanticQueryFailureCode::NotReady,
        };
        let retryable = !matches!(
            blocker,
            Some(
                SemanticReadinessBlocker::SemanticDisabled
                    | SemanticReadinessBlocker::UnsupportedPlatform
                    | SemanticReadinessBlocker::ModelFailureTerminal { .. }
            )
        );
        Self {
            code,
            message: "explicit semantic retrieval is not ready".to_owned(),
            retryable,
            next_eligible_at_ms: readiness.model_retry.next_retry_at_ms,
            readiness: Some(readiness.state),
            blocker: blocker.map(SemanticReadinessBlocker::code),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SemanticQueryContractError {
    code: SemanticQueryFailureCode,
    message: String,
}

impl SemanticQueryContractError {
    fn new(code: SemanticQueryFailureCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(SemanticQueryFailureCode::InvalidRequest, message)
    }

    fn invalid_response(message: impl Into<String>) -> Self {
        Self::new(SemanticQueryFailureCode::Internal, message)
    }

    pub(crate) fn into_failure(self) -> SemanticQueryFailure {
        SemanticQueryFailure::new(self.code, self.message, false)
    }
}

impl fmt::Display for SemanticQueryContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SemanticQueryContractError {}
