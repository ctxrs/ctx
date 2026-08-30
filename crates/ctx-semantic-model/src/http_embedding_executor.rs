use std::{
    fmt,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use url::Url;
use uuid::Uuid;

use crate::{
    embedding_executor::{ensure_prepared_contract, HttpExecutorProtocol},
    http_embedding_canary::{
        prepared_document_probes, prepared_query_probes, validate_conformance_canary,
    },
    semantic_model_contract, ExternalSemanticSpace, PreparedSemanticDocuments,
    PreparedSemanticQuery, SemanticEmbeddingExecutor, SemanticModelContract,
};

mod endpoint;
mod request_body;
mod resolver;
mod response;
mod wire;

pub(crate) use endpoint::ValidatedHttpEndpoint;
#[cfg(test)]
use request_body::encoded_json_len;
use request_body::{encode_preflighted_request, RequestBodySizer};
use resolver::build_http_agent;
#[cfg(test)]
use resolver::{ResolverRuntime, RESOLVER_QUEUE_CAPACITY, RESOLVER_THREADS};
#[cfg(test)]
use response::validate_embedding;
use response::{map_embeddings_by_id, read_response_body, ResponseBodyError};
use wire::{
    parse_contract_response, BearerToken, EmbeddingInput, EmbeddingOutput, EmbeddingsRequest,
    EmbeddingsResponse, InputKind, LegacyContractResponse, LegacyEmbeddingsRequest,
    LegacyEmbeddingsResponse, PreparedEmbeddingsRequest,
};

pub const SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV: &str = "CTX_SEMANTIC_EMBEDDING_TOKEN";
pub const SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV: &str =
    "CTX_SEMANTIC_EMBEDDING_TOKEN_ENDPOINT";

const LEGACY_PROTOCOL_SCHEMA_VERSION: u32 = 1;
const LEGACY_CONTRACT_ROUTE: &str = "v1/contract";
const LEGACY_EMBEDDINGS_ROUTE: &str = "v1/embeddings";
const PROTOCOL_SCHEMA_VERSION: u32 = 2;
const CONTRACT_ROUTE: &str = "v2/contract";
const EMBEDDINGS_ROUTE: &str = "v2/embeddings";
const SCHEMA_HEADER: &str = "x-ctx-semantic-schema-version";
const LEGACY_MODEL_KEY_HEADER: &str = "x-ctx-semantic-model-key";
const LEGACY_CONTRACT_FINGERPRINT_HEADER: &str = "x-ctx-semantic-model-contract-fingerprint";
const LEGACY_MAX_INPUT_COUNT: usize = 512;
const MAX_TOKEN_BYTES: usize = 4 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONTRACT_BODY_BYTES: usize = 4 * 1024;
const UUID_WIRE_VALUE: &str = "00000000-0000-0000-0000-000000000000";
const DNS_RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const EXECUTION_BUDGET: Duration = Duration::from_secs(24);
const MAX_ATTEMPTS: usize = 2;

#[derive(Clone, Debug)]
struct SemanticEmbeddingPermanentFailure {
    message: String,
    http_status: Option<u16>,
}

impl fmt::Display for SemanticEmbeddingPermanentFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SemanticEmbeddingPermanentFailure {}

fn permanent_failure(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(SemanticEmbeddingPermanentFailure {
        message: message.into(),
        http_status: None,
    })
}

fn permanent_http_failure(status: u16) -> anyhow::Error {
    anyhow::Error::new(SemanticEmbeddingPermanentFailure {
        message: format!("semantic embedding endpoint returned HTTP status {status}"),
        http_status: Some(status),
    })
}

pub fn semantic_embedding_failure_is_permanent(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<SemanticEmbeddingPermanentFailure>()
        .is_some()
}

/// Credential material resolved by the final host. Debug output is always
/// redacted, and endpoint binding is revalidated by the HTTP executor.
#[derive(Clone, Default)]
pub struct SemanticEmbeddingExecutorAuth {
    bearer: Option<BearerAuthInput>,
}

#[derive(Clone)]
struct BearerAuthInput {
    token: String,
    endpoint_binding: String,
}

impl SemanticEmbeddingExecutorAuth {
    pub const fn none() -> Self {
        Self { bearer: None }
    }

    pub fn bearer(token: String, endpoint_binding: String) -> Self {
        Self {
            bearer: Some(BearerAuthInput {
                token,
                endpoint_binding,
            }),
        }
    }
}

impl fmt::Debug for SemanticEmbeddingExecutorAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticEmbeddingExecutorAuth")
            .field("configured", &self.bearer.is_some())
            .finish()
    }
}

/// Portable client for an explicitly selected HTTP embedding protocol.
pub struct HttpSemanticEmbeddingExecutor {
    endpoint: ValidatedHttpEndpoint,
    agent: ureq_semantic::Agent,
    bearer_token: Option<BearerToken>,
    protocol: HttpExecutorProtocol,
    contract: SemanticModelContract,
    lifecycle: Mutex<ExecutorLifecycle>,
    contract_verification_changed: Condvar,
}

#[derive(Clone, Debug)]
enum ExecutorLifecycle {
    Unverified,
    Verifying,
    Verified,
    PermanentlyFailed(SemanticEmbeddingPermanentFailure),
}

impl fmt::Debug for HttpSemanticEmbeddingExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpSemanticEmbeddingExecutor")
            .field("endpoint", &self.endpoint.as_str())
            .field("protocol", &self.protocol)
            .field("authentication_configured", &self.bearer_token.is_some())
            .field("contract_verified", &self.contract_verified())
            .finish()
    }
}

impl HttpSemanticEmbeddingExecutor {
    pub fn new(endpoint: impl AsRef<str>, space: ExternalSemanticSpace) -> Result<Self> {
        Self::new_with_auth(endpoint, space, SemanticEmbeddingExecutorAuth::none())
    }

    pub fn new_with_auth(
        endpoint: impl AsRef<str>,
        space: ExternalSemanticSpace,
        auth: SemanticEmbeddingExecutorAuth,
    ) -> Result<Self> {
        let endpoint = ValidatedHttpEndpoint::parse(endpoint.as_ref())?;
        let contract = SemanticModelContract::external_http(endpoint.as_str(), space.clone());
        Self::from_validated_selection(
            endpoint,
            HttpExecutorProtocol::ExternalSpaceV2(space),
            contract,
            auth,
        )
    }

    pub(crate) fn from_validated_selection(
        endpoint: ValidatedHttpEndpoint,
        protocol: HttpExecutorProtocol,
        contract: SemanticModelContract,
        auth: SemanticEmbeddingExecutorAuth,
    ) -> Result<Self> {
        Self::from_validated_selection_with_root_certs(
            endpoint,
            protocol,
            contract,
            auth,
            ureq_semantic::tls::RootCerts::PlatformVerifier,
        )
    }

    fn from_validated_selection_with_root_certs(
        endpoint: ValidatedHttpEndpoint,
        protocol: HttpExecutorProtocol,
        contract: SemanticModelContract,
        auth: SemanticEmbeddingExecutorAuth,
        root_certs: ureq_semantic::tls::RootCerts,
    ) -> Result<Self> {
        let bearer_token = BearerToken::from_auth(auth, &endpoint)?;
        let agent = build_http_agent(root_certs)?;
        Ok(Self {
            endpoint,
            agent,
            bearer_token,
            protocol,
            contract,
            lifecycle: Mutex::new(ExecutorLifecycle::Unverified),
            contract_verification_changed: Condvar::new(),
        })
    }

    #[cfg(test)]
    fn new_with_auth_and_root_certs(
        endpoint: impl AsRef<str>,
        space: ExternalSemanticSpace,
        auth: SemanticEmbeddingExecutorAuth,
        root_certs: ureq_semantic::tls::RootCerts,
    ) -> Result<Self> {
        let endpoint = ValidatedHttpEndpoint::parse(endpoint.as_ref())?;
        let contract = SemanticModelContract::external_http(endpoint.as_str(), space.clone());
        Self::from_validated_selection_with_root_certs(
            endpoint,
            HttpExecutorProtocol::ExternalSpaceV2(space),
            contract,
            auth,
            root_certs,
        )
    }

    pub(crate) fn discover_protocol_from_validated_endpoint(
        endpoint: ValidatedHttpEndpoint,
        auth: SemanticEmbeddingExecutorAuth,
    ) -> Result<HttpExecutorProtocol> {
        discover_protocol_with_root_certs(
            endpoint,
            auth,
            ureq_semantic::tls::RootCerts::PlatformVerifier,
        )
    }

    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str()
    }

    pub const fn authentication_configured(&self) -> bool {
        self.bearer_token.is_some()
    }

    pub fn external_space(&self) -> Option<&ExternalSemanticSpace> {
        match &self.protocol {
            HttpExecutorProtocol::ExternalSpaceV2(space) => Some(space),
            HttpExecutorProtocol::LegacyFixedV1 => None,
        }
    }

    pub fn contract_verified(&self) -> bool {
        self.lifecycle
            .lock()
            .map(|lifecycle| matches!(*lifecycle, ExecutorLifecycle::Verified))
            .unwrap_or(false)
    }

    /// Revalidates the configured semantic space without sending user content.
    ///
    /// External V2 verification performs only a content-free contract GET.
    /// Historical fixed-E5 V1 additionally submits frozen public canary text.
    /// Neither path sends user history or query content.
    pub fn verify_contract(&self) -> Result<()> {
        self.fail_if_permanently_failed()?;
        self.ensure_contract(execution_deadline())
    }

    fn ensure_contract(&self, deadline: Instant) -> Result<()> {
        loop {
            let mut lifecycle = self
                .lifecycle
                .lock()
                .map_err(|_| anyhow!("semantic embedding contract state is unavailable"))?;
            match &*lifecycle {
                ExecutorLifecycle::Verified => return Ok(()),
                ExecutorLifecycle::PermanentlyFailed(failure) => {
                    return Err(anyhow::Error::new(failure.clone()));
                }
                ExecutorLifecycle::Unverified => {
                    *lifecycle = ExecutorLifecycle::Verifying;
                    drop(lifecycle);
                    let result = self.fetch_and_verify_contract(deadline);
                    let mut lifecycle = self
                        .lifecycle
                        .lock()
                        .map_err(|_| anyhow!("semantic embedding contract state is unavailable"))?;
                    match result {
                        Ok(()) => {
                            *lifecycle = ExecutorLifecycle::Verified;
                            self.contract_verification_changed.notify_all();
                            return Ok(());
                        }
                        Err(error) => {
                            let error = if let Some(failure) = error
                                .downcast_ref::<SemanticEmbeddingPermanentFailure>()
                                .cloned()
                            {
                                *lifecycle = ExecutorLifecycle::PermanentlyFailed(failure.clone());
                                anyhow::Error::new(failure)
                            } else {
                                *lifecycle = ExecutorLifecycle::Unverified;
                                error
                            };
                            self.contract_verification_changed.notify_all();
                            return Err(error);
                        }
                    }
                }
                ExecutorLifecycle::Verifying => {
                    let remaining = remaining_budget(deadline)?;
                    let (next, wait) = self
                        .contract_verification_changed
                        .wait_timeout(lifecycle, remaining)
                        .map_err(|_| anyhow!("semantic embedding contract state is unavailable"))?;
                    if wait.timed_out() && matches!(*next, ExecutorLifecycle::Verifying) {
                        return Err(execution_budget_exhausted());
                    }
                }
            }
        }
    }

    fn fetch_and_verify_contract(&self, deadline: Instant) -> Result<()> {
        match &self.protocol {
            HttpExecutorProtocol::ExternalSpaceV2(_) => {
                let route = self.endpoint.route(CONTRACT_ROUTE);
                let response = self.exchange(&route, None, MAX_CONTRACT_BODY_BYTES, deadline)?;
                let asserted = parse_contract_response(&response)
                    .map_err(|error| permanent_failure(error.to_string()))?;
                self.validate_space(&asserted)
            }
            HttpExecutorProtocol::LegacyFixedV1 => {
                let route = self.endpoint.route(LEGACY_CONTRACT_ROUTE);
                let response = self.exchange(&route, None, MAX_CONTRACT_BODY_BYTES, deadline)?;
                let asserted: LegacyContractResponse =
                    serde_json::from_slice(&response).map_err(|_| {
                        permanent_failure("semantic embedding contract response is malformed")
                    })?;
                self.validate_legacy_identity(
                    asserted.schema_version,
                    &asserted.model_key,
                    &asserted.model_contract_fingerprint,
                )?;
                let query_embeddings = self.request_embeddings_without_verification(
                    InputKind::Query,
                    &prepared_query_probes(&self.contract),
                    deadline,
                )?;
                let document_embeddings = self.request_embeddings_without_verification(
                    InputKind::Documents,
                    &prepared_document_probes(&self.contract),
                    deadline,
                )?;
                validate_conformance_canary(&query_embeddings, &document_embeddings).map_err(|_| {
                    permanent_failure("semantic embedding endpoint failed the conformance canary")
                })
            }
        }
    }

    fn request_embeddings_without_verification(
        &self,
        input_kind: InputKind,
        inputs: &[String],
        deadline: Instant,
    ) -> Result<Vec<Vec<f32>>> {
        let request = self.prepare_embeddings_request(input_kind, inputs)?;
        self.exchange_embeddings(request, deadline)
    }

    fn embed(
        &self,
        input_kind: InputKind,
        inputs: &[String],
        deadline: Instant,
    ) -> Result<Vec<Vec<f32>>> {
        self.fail_if_permanently_failed()?;
        let request = self.prepare_embeddings_request(input_kind, inputs)?;
        self.embed_prepared(request, deadline)
    }

    fn embed_prepared(
        &self,
        request: PreparedEmbeddingsRequest,
        deadline: Instant,
    ) -> Result<Vec<Vec<f32>>> {
        self.ensure_contract(deadline)?;
        let result = self.exchange_embeddings(request, deadline);
        self.cache_permanent_result(result)
    }

    fn fail_if_permanently_failed(&self) -> Result<()> {
        let lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| anyhow!("semantic embedding contract state is unavailable"))?;
        match &*lifecycle {
            ExecutorLifecycle::PermanentlyFailed(failure) => {
                Err(anyhow::Error::new(failure.clone()))
            }
            _ => Ok(()),
        }
    }

    fn cache_permanent_result<T>(&self, result: Result<T>) -> Result<T> {
        result.map_err(|error| {
            let Some(failure) = error
                .downcast_ref::<SemanticEmbeddingPermanentFailure>()
                .cloned()
            else {
                return error;
            };
            let Ok(mut lifecycle) = self.lifecycle.lock() else {
                return error;
            };
            let failure = match &*lifecycle {
                ExecutorLifecycle::PermanentlyFailed(cached) => cached.clone(),
                _ => {
                    *lifecycle = ExecutorLifecycle::PermanentlyFailed(failure.clone());
                    self.contract_verification_changed.notify_all();
                    failure
                }
            };
            anyhow::Error::new(failure)
        })
    }

    fn prepare_embeddings_request(
        &self,
        input_kind: InputKind,
        inputs: &[String],
    ) -> Result<PreparedEmbeddingsRequest> {
        let body_len = self.plan_embeddings_request(input_kind, inputs)?;
        self.prepare_preflighted_embeddings_request(input_kind, inputs, body_len)
    }

    fn plan_embeddings_request(&self, input_kind: InputKind, inputs: &[String]) -> Result<usize> {
        if inputs.len() > self.max_inputs_per_request() {
            return Err(permanent_failure(
                "semantic embedding request exceeds the input or scalar count limit",
            ));
        }

        let mut sizer = RequestBodySizer::new(self, input_kind)?;
        for input in inputs {
            if !sizer.try_push(input)? {
                return Err(request_body_limit_failure());
            }
        }
        Ok(sizer.body_len())
    }

    fn plan_document_batch(&self, inputs: &[String]) -> Result<(usize, usize)> {
        let mut sizer = RequestBodySizer::new(self, InputKind::Documents)?;
        let mut input_count = 0;
        for input in inputs.iter().take(self.max_inputs_per_request()) {
            if !sizer.try_push(input)? {
                break;
            }
            input_count += 1;
        }
        if input_count == 0 {
            return Err(request_body_limit_failure());
        }
        Ok((input_count, sizer.body_len()))
    }

    fn prepare_preflighted_embeddings_request(
        &self,
        input_kind: InputKind,
        inputs: &[String],
        body_len: usize,
    ) -> Result<PreparedEmbeddingsRequest> {
        let request_id = Uuid::new_v4().to_string();
        let input_ids = inputs
            .iter()
            .map(|_| Uuid::new_v4().to_string())
            .collect::<Vec<_>>();
        let wire_inputs = input_ids
            .iter()
            .zip(inputs)
            .map(|(id, text)| EmbeddingInput { id, text })
            .collect::<Vec<_>>();
        let body = match &self.protocol {
            HttpExecutorProtocol::ExternalSpaceV2(space) => encode_preflighted_request(
                &EmbeddingsRequest {
                    schema_version: PROTOCOL_SCHEMA_VERSION,
                    space_id: space.space_id(),
                    dimensions: space.dimensions(),
                    request_id: &request_id,
                    input_kind,
                    inputs: &wire_inputs,
                },
                body_len,
            )?,
            HttpExecutorProtocol::LegacyFixedV1 => encode_preflighted_request(
                &LegacyEmbeddingsRequest {
                    schema_version: LEGACY_PROTOCOL_SCHEMA_VERSION,
                    model_key: self.contract.model_key(),
                    model_contract_fingerprint: self.contract.fingerprint(),
                    request_id: &request_id,
                    input_kind,
                    inputs: &wire_inputs,
                },
                body_len,
            )?,
        };
        Ok(PreparedEmbeddingsRequest {
            request_id,
            input_ids,
            body,
        })
    }

    fn exchange_embeddings(
        &self,
        request: PreparedEmbeddingsRequest,
        deadline: Instant,
    ) -> Result<Vec<Vec<f32>>> {
        let route = self.endpoint.route(match &self.protocol {
            HttpExecutorProtocol::LegacyFixedV1 => LEGACY_EMBEDDINGS_ROUTE,
            HttpExecutorProtocol::ExternalSpaceV2(_) => EMBEDDINGS_ROUTE,
        });
        let response = self.exchange(
            &route,
            Some(&request.body),
            MAX_RESPONSE_BODY_BYTES,
            deadline,
        )?;
        let (response_request_id, embeddings) = match &self.protocol {
            HttpExecutorProtocol::ExternalSpaceV2(_) => {
                let response: EmbeddingsResponse = serde_json::from_slice(&response)
                    .map_err(|_| permanent_failure("semantic embedding response is malformed"))?;
                self.validate_protocol_space(
                    response.schema_version,
                    &response.space_id,
                    response.dimensions,
                )?;
                (response.request_id, response.embeddings)
            }
            HttpExecutorProtocol::LegacyFixedV1 => {
                let response: LegacyEmbeddingsResponse = serde_json::from_slice(&response)
                    .map_err(|_| permanent_failure("semantic embedding response is malformed"))?;
                self.validate_legacy_identity(
                    response.schema_version,
                    &response.model_key,
                    &response.model_contract_fingerprint,
                )?;
                (response.request_id, response.embeddings)
            }
        };
        if response_request_id != request.request_id {
            return Err(permanent_failure(
                "semantic embedding response request ID does not match",
            ));
        }
        map_embeddings_by_id(embeddings, &request.input_ids, self.contract.dimensions())
            .map_err(|error| permanent_failure(error.to_string()))
    }

    fn validate_space(&self, space: &ExternalSemanticSpace) -> Result<()> {
        if self.external_space_or_none() != Some(space) {
            return Err(permanent_failure(
                "semantic embedding endpoint asserted a different semantic space",
            ));
        }
        Ok(())
    }

    fn validate_protocol_space(
        &self,
        schema_version: u32,
        space_id: &str,
        dimensions: usize,
    ) -> Result<()> {
        let Some(space) = self.external_space_or_none() else {
            return Err(permanent_failure(
                "semantic embedding endpoint asserted a different semantic space",
            ));
        };
        if schema_version != PROTOCOL_SCHEMA_VERSION
            || space_id != space.space_id()
            || dimensions != space.dimensions()
        {
            return Err(permanent_failure(
                "semantic embedding endpoint asserted a different semantic space",
            ));
        }
        Ok(())
    }

    fn validate_legacy_identity(
        &self,
        schema_version: u32,
        model_key: &str,
        fingerprint: &str,
    ) -> Result<()> {
        if schema_version != LEGACY_PROTOCOL_SCHEMA_VERSION
            || model_key != self.contract.model_key()
            || fingerprint != self.contract.fingerprint()
        {
            return Err(permanent_failure(
                "semantic embedding endpoint asserted a different model contract",
            ));
        }
        Ok(())
    }

    fn external_space_or_none(&self) -> Option<&ExternalSemanticSpace> {
        match &self.protocol {
            HttpExecutorProtocol::LegacyFixedV1 => None,
            HttpExecutorProtocol::ExternalSpaceV2(space) => Some(space),
        }
    }

    fn max_inputs_per_request(&self) -> usize {
        self.external_space_or_none().map_or(
            LEGACY_MAX_INPUT_COUNT,
            ExternalSemanticSpace::max_inputs_per_request,
        )
    }

    fn exchange(
        &self,
        route: &Url,
        body: Option<&[u8]>,
        max_response_body_bytes: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>> {
        exchange_http(
            &self.agent,
            self.bearer_token.as_ref(),
            match &self.protocol {
                HttpExecutorProtocol::LegacyFixedV1 => LEGACY_PROTOCOL_SCHEMA_VERSION,
                HttpExecutorProtocol::ExternalSpaceV2(_) => PROTOCOL_SCHEMA_VERSION,
            },
            route,
            body,
            max_response_body_bytes,
            deadline,
        )
    }
}

fn request_body_limit_failure() -> anyhow::Error {
    permanent_failure("semantic embedding request exceeds the body size limit")
}

fn discover_protocol_with_root_certs(
    endpoint: ValidatedHttpEndpoint,
    auth: SemanticEmbeddingExecutorAuth,
    root_certs: ureq_semantic::tls::RootCerts,
) -> Result<HttpExecutorProtocol> {
    let bearer_token = BearerToken::from_auth(auth, &endpoint)?;
    let agent = build_http_agent(root_certs)?;
    let deadline = execution_deadline();
    let response = exchange_http(
        &agent,
        bearer_token.as_ref(),
        PROTOCOL_SCHEMA_VERSION,
        &endpoint.route(CONTRACT_ROUTE),
        None,
        MAX_CONTRACT_BODY_BYTES,
        deadline,
    );
    match response {
        Ok(response) => {
            parse_contract_response(&response).map(HttpExecutorProtocol::ExternalSpaceV2)
        }
        Err(error)
            if error
                .downcast_ref::<SemanticEmbeddingPermanentFailure>()
                .is_some_and(|failure| failure.http_status == Some(404)) =>
        {
            let response = exchange_http(
                &agent,
                bearer_token.as_ref(),
                LEGACY_PROTOCOL_SCHEMA_VERSION,
                &endpoint.route(LEGACY_CONTRACT_ROUTE),
                None,
                MAX_CONTRACT_BODY_BYTES,
                deadline,
            )?;
            let response: LegacyContractResponse = serde_json::from_slice(&response)
                .map_err(|_| anyhow!("semantic embedding contract response is malformed"))?;
            let contract = semantic_model_contract();
            if response.schema_version != LEGACY_PROTOCOL_SCHEMA_VERSION
                || response.model_key != contract.model_key()
                || response.model_contract_fingerprint != contract.fingerprint()
            {
                return Err(permanent_failure(
                    "semantic embedding endpoint asserted a different model contract",
                ));
            }
            Ok(HttpExecutorProtocol::LegacyFixedV1)
        }
        Err(error) => Err(error),
    }
}

fn exchange_http(
    agent: &ureq_semantic::Agent,
    bearer_token: Option<&BearerToken>,
    schema_version: u32,
    route: &Url,
    body: Option<&[u8]>,
    max_response_body_bytes: usize,
    deadline: Instant,
) -> Result<Vec<u8>> {
    for attempt in 0..MAX_ATTEMPTS {
        let remaining = remaining_budget(deadline)?;
        // The resolver and connector each have their own ceiling in addition
        // to the request-global deadline. Avoid starting a network attempt when
        // either bounded phase lacks its full allowance.
        if remaining < DNS_RESOLVE_TIMEOUT || remaining < CONNECT_TIMEOUT {
            return Err(execution_budget_exhausted());
        }
        let result = match body {
            Some(body) => prepare_http_request(
                agent.post(route.as_str()),
                bearer_token,
                schema_version,
                remaining,
            )
            .header("content-type", "application/json")
            .send(body),
            None => prepare_http_request(
                agent.get(route.as_str()),
                bearer_token,
                schema_version,
                remaining,
            )
            .call(),
        };
        match result {
            Ok(response)
                if !response.status().is_success()
                    && retryable_status(response.status().as_u16())
                    && attempt + 1 < MAX_ATTEMPTS =>
            {
                continue;
            }
            Ok(response) if !response.status().is_success() => {
                let status = response.status().as_u16();
                if retryable_status(status) {
                    return Err(anyhow!(
                        "semantic embedding endpoint returned retryable HTTP status {status}"
                    ));
                }
                return Err(permanent_http_failure(status));
            }
            Ok(response) => match read_response_body(response, max_response_body_bytes) {
                Ok(response) => return Ok(response),
                Err(ResponseBodyError::TooLarge) => {
                    return Err(permanent_failure(
                        "semantic embedding response exceeds the body size limit",
                    ));
                }
                Err(ResponseBodyError::InvalidLength) => {
                    return Err(permanent_failure(
                        "semantic embedding response has an invalid body length",
                    ));
                }
                Err(ResponseBodyError::Transport) if attempt + 1 < MAX_ATTEMPTS => continue,
                Err(ResponseBodyError::Transport) => {
                    return Err(anyhow!(
                        "semantic embedding HTTP transport failed after bounded retry"
                    ));
                }
            },
            Err(error) if ureq_error_is_permanent(&error) => {
                return Err(permanent_failure(
                    "semantic embedding endpoint returned invalid HTTP protocol",
                ));
            }
            Err(_) if attempt + 1 < MAX_ATTEMPTS => continue,
            Err(_) => {
                return Err(anyhow!(
                    "semantic embedding HTTP transport failed after bounded retry"
                ));
            }
        }
    }
    unreachable!("HTTP exchange has at least one bounded attempt")
}

fn prepare_http_request<Any>(
    request: ureq_semantic::RequestBuilder<Any>,
    bearer_token: Option<&BearerToken>,
    schema_version: u32,
    timeout: Duration,
) -> ureq_semantic::RequestBuilder<Any> {
    let mut request = request
        .config()
        .timeout_global(Some(timeout))
        .build()
        .header("accept", "application/json")
        .header("accept-encoding", "identity")
        .header("cache-control", "no-store")
        .header(SCHEMA_HEADER, schema_version.to_string());
    if schema_version == LEGACY_PROTOCOL_SCHEMA_VERSION {
        let contract = semantic_model_contract();
        request = request
            .header(LEGACY_MODEL_KEY_HEADER, contract.model_key())
            .header(LEGACY_CONTRACT_FINGERPRINT_HEADER, contract.fingerprint());
    }
    if let Some(token) = bearer_token {
        request = request.header("authorization", format!("Bearer {}", token.expose()));
    }
    request
}

impl SemanticEmbeddingExecutor for HttpSemanticEmbeddingExecutor {
    fn contract(&self) -> &SemanticModelContract {
        &self.contract
    }

    fn embed_query(&self, query: PreparedSemanticQuery) -> Result<Vec<f32>> {
        self.fail_if_permanently_failed()?;
        ensure_prepared_contract(query.contract_fingerprint(), self.contract())?;
        let deadline = execution_deadline();
        let mut embeddings = self.embed(InputKind::Query, &[query.into_text()], deadline)?;
        Ok(embeddings.remove(0))
    }

    fn embed_documents(
        &self,
        documents: PreparedSemanticDocuments,
        pacing_deadline: Option<Instant>,
    ) -> Result<Vec<Vec<f32>>> {
        self.fail_if_permanently_failed()?;
        ensure_prepared_contract(documents.contract_fingerprint(), self.contract())?;
        let deadline = execution_deadline();
        // This deadline only paces local built-in batches. It is intentionally
        // neither serialized nor represented as remote cancellation.
        let _ = pacing_deadline;
        let documents = documents.into_texts();
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let mut embeddings = Vec::with_capacity(documents.len());
        let mut batch_start = 0;
        while batch_start < documents.len() {
            let (batch_len, body_len) = self.plan_document_batch(&documents[batch_start..])?;
            let batch_end = batch_start + batch_len;
            let request = self.prepare_preflighted_embeddings_request(
                InputKind::Documents,
                &documents[batch_start..batch_end],
                body_len,
            )?;
            embeddings.extend(self.embed_prepared(request, deadline)?);
            batch_start = batch_end;
        }
        Ok(embeddings)
    }
}

fn execution_deadline() -> Instant {
    Instant::now() + EXECUTION_BUDGET
}

fn remaining_budget(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(execution_budget_exhausted)
}

fn execution_budget_exhausted() -> anyhow::Error {
    anyhow!("semantic embedding execution exceeded its aggregate time budget")
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

fn ureq_error_is_permanent(error: &ureq_semantic::Error) -> bool {
    matches!(
        error,
        ureq_semantic::Error::Http(_)
            | ureq_semantic::Error::BadUri(_)
            | ureq_semantic::Error::Protocol(_)
            | ureq_semantic::Error::RedirectFailed
            | ureq_semantic::Error::BodyExceedsLimit(_)
            | ureq_semantic::Error::TooManyRedirects
            | ureq_semantic::Error::LargeResponseHeader(_, _)
    )
}

#[cfg(test)]
#[path = "http_embedding_executor_tests.rs"]
mod tests;
