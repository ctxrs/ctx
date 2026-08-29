use std::{
    fmt,
    net::IpAddr,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::{
    embedding_executor::ensure_prepared_contract, ExternalSemanticSpace, PreparedSemanticDocuments,
    PreparedSemanticQuery, SemanticEmbeddingExecutor, SemanticModelContract,
};

mod request_body;
mod resolver;
mod response;

#[cfg(test)]
use request_body::encoded_json_len;
use request_body::{encode_preflighted_request, RequestBodySizer};
use resolver::build_http_agent;
#[cfg(test)]
use resolver::{ResolverRuntime, RESOLVER_QUEUE_CAPACITY, RESOLVER_THREADS};
#[cfg(test)]
use response::validate_embedding;
use response::{map_embeddings_by_id, read_response_body, ResponseBodyError};

pub const SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV: &str = "CTX_SEMANTIC_EMBEDDING_TOKEN";
pub const SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV: &str =
    "CTX_SEMANTIC_EMBEDDING_TOKEN_ENDPOINT";

const PROTOCOL_SCHEMA_VERSION: u32 = 1;
const CONTRACT_ROUTE: &str = "v1/contract";
const EMBEDDINGS_ROUTE: &str = "v1/embeddings";
const SCHEMA_HEADER: &str = "x-ctx-semantic-schema-version";
const MAX_ENDPOINT_BYTES: usize = 2 * 1024;
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
struct SemanticEmbeddingPermanentFailure(String);

impl fmt::Display for SemanticEmbeddingPermanentFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SemanticEmbeddingPermanentFailure {}

fn permanent_failure(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(SemanticEmbeddingPermanentFailure(message.into()))
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedHttpEndpoint {
    base: Url,
    exact_loopback_ip: bool,
}

impl ValidatedHttpEndpoint {
    pub(crate) fn parse(endpoint: &str) -> Result<Self> {
        if endpoint.is_empty() || endpoint.len() > MAX_ENDPOINT_BYTES || endpoint.trim() != endpoint
        {
            return Err(anyhow!("semantic embedding endpoint is invalid"));
        }
        let raw_host = raw_url_host(endpoint)?;
        let mut base =
            Url::parse(endpoint).map_err(|_| anyhow!("semantic embedding endpoint is invalid"))?;
        if base.cannot_be_a_base() || base.host().is_none() {
            return Err(anyhow!("semantic embedding endpoint must contain a host"));
        }
        if authority_contains_credentials(endpoint)
            || !base.username().is_empty()
            || base.password().is_some()
        {
            return Err(anyhow!(
                "semantic embedding endpoint must not contain credentials"
            ));
        }
        if base.query().is_some() {
            return Err(anyhow!(
                "semantic embedding endpoint must not contain a query"
            ));
        }
        if base.fragment().is_some() {
            return Err(anyhow!(
                "semantic embedding endpoint must not contain a fragment"
            ));
        }

        let exact_loopback_ip = raw_host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
        match base.scheme() {
            "http" if !exact_loopback_ip => {
                return Err(anyhow!(
                    "plain HTTP semantic embedding requires an exact loopback IP host"
                ));
            }
            "http" | "https" => {}
            _ => {
                return Err(anyhow!(
                    "semantic embedding endpoint must use HTTPS or loopback HTTP"
                ));
            }
        }

        if !base.path().ends_with('/') {
            let normalized = format!("{}/", base.as_str());
            base = Url::parse(&normalized)
                .map_err(|_| anyhow!("semantic embedding endpoint is invalid"))?;
        }
        Ok(Self {
            base,
            exact_loopback_ip,
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        self.base.as_str()
    }

    pub(crate) const fn is_loopback(&self) -> bool {
        self.exact_loopback_ip
    }

    fn route(&self, route: &str) -> Url {
        self.base
            .join(route)
            .expect("validated semantic embedding base URL accepts relative routes")
    }
}

/// Portable client for one explicitly accepted external semantic space.
pub struct HttpSemanticEmbeddingExecutor {
    endpoint: ValidatedHttpEndpoint,
    agent: ureq_semantic::Agent,
    bearer_token: Option<BearerToken>,
    space: ExternalSemanticSpace,
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
            .field("space", &self.space)
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
        Self::from_validated_selection(endpoint, space, contract, auth)
    }

    pub(crate) fn from_validated_selection(
        endpoint: ValidatedHttpEndpoint,
        space: ExternalSemanticSpace,
        contract: SemanticModelContract,
        auth: SemanticEmbeddingExecutorAuth,
    ) -> Result<Self> {
        Self::from_validated_selection_with_root_certs(
            endpoint,
            space,
            contract,
            auth,
            ureq_semantic::tls::RootCerts::PlatformVerifier,
        )
    }

    fn from_validated_selection_with_root_certs(
        endpoint: ValidatedHttpEndpoint,
        space: ExternalSemanticSpace,
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
            space,
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
        Self::from_validated_selection_with_root_certs(endpoint, space, contract, auth, root_certs)
    }

    pub(crate) fn discover_space_from_validated_endpoint(
        endpoint: ValidatedHttpEndpoint,
        auth: SemanticEmbeddingExecutorAuth,
    ) -> Result<ExternalSemanticSpace> {
        discover_space_with_root_certs(
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

    pub fn external_space(&self) -> &ExternalSemanticSpace {
        &self.space
    }

    pub fn contract_verified(&self) -> bool {
        self.lifecycle
            .lock()
            .map(|lifecycle| matches!(*lifecycle, ExecutorLifecycle::Verified))
            .unwrap_or(false)
    }

    /// Revalidates the configured semantic space without sending user content.
    ///
    /// This performs and caches only `GET v1/contract`. It is suitable for a
    /// fail-closed activation check before opening a writable vector store.
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
        let route = self.endpoint.route(CONTRACT_ROUTE);
        let response = self.exchange(&route, None, MAX_CONTRACT_BODY_BYTES, deadline)?;
        let asserted = parse_contract_response(&response)
            .map_err(|error| permanent_failure(error.to_string()))?;
        self.validate_space(&asserted)
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
        let request = EmbeddingsRequest {
            schema_version: PROTOCOL_SCHEMA_VERSION,
            space_id: self.space.space_id(),
            dimensions: self.space.dimensions(),
            request_id: &request_id,
            input_kind,
            inputs: &wire_inputs,
        };
        let body = encode_preflighted_request(&request, body_len)?;
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
        let route = self.endpoint.route(EMBEDDINGS_ROUTE);
        let response = self.exchange(
            &route,
            Some(&request.body),
            MAX_RESPONSE_BODY_BYTES,
            deadline,
        )?;
        let response: EmbeddingsResponse = serde_json::from_slice(&response)
            .map_err(|_| permanent_failure("semantic embedding response is malformed"))?;
        self.validate_protocol_space(
            response.schema_version,
            &response.space_id,
            response.dimensions,
        )?;
        if response.request_id != request.request_id {
            return Err(permanent_failure(
                "semantic embedding response request ID does not match",
            ));
        }
        map_embeddings_by_id(
            response.embeddings,
            &request.input_ids,
            self.contract.dimensions(),
        )
        .map_err(|error| permanent_failure(error.to_string()))
    }

    fn validate_space(&self, space: &ExternalSemanticSpace) -> Result<()> {
        if space != &self.space {
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
        if schema_version != PROTOCOL_SCHEMA_VERSION
            || space_id != self.space.space_id()
            || dimensions != self.space.dimensions()
        {
            return Err(permanent_failure(
                "semantic embedding endpoint asserted a different semantic space",
            ));
        }
        Ok(())
    }

    fn max_inputs_per_request(&self) -> usize {
        self.space.max_inputs_per_request()
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

fn discover_space_with_root_certs(
    endpoint: ValidatedHttpEndpoint,
    auth: SemanticEmbeddingExecutorAuth,
    root_certs: ureq_semantic::tls::RootCerts,
) -> Result<ExternalSemanticSpace> {
    let bearer_token = BearerToken::from_auth(auth, &endpoint)?;
    let agent = build_http_agent(root_certs)?;
    let response = exchange_http(
        &agent,
        bearer_token.as_ref(),
        &endpoint.route(CONTRACT_ROUTE),
        None,
        MAX_CONTRACT_BODY_BYTES,
        execution_deadline(),
    )?;
    parse_contract_response(&response)
}

fn exchange_http(
    agent: &ureq_semantic::Agent,
    bearer_token: Option<&BearerToken>,
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
            Some(body) => prepare_http_request(agent.post(route.as_str()), bearer_token, remaining)
                .header("content-type", "application/json")
                .send(body),
            None => prepare_http_request(agent.get(route.as_str()), bearer_token, remaining).call(),
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
                return Err(permanent_failure(format!(
                    "semantic embedding endpoint returned HTTP status {status}"
                )));
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
    timeout: Duration,
) -> ureq_semantic::RequestBuilder<Any> {
    let mut request = request
        .config()
        .timeout_global(Some(timeout))
        .build()
        .header("accept", "application/json")
        .header("accept-encoding", "identity")
        .header("cache-control", "no-store")
        .header(SCHEMA_HEADER, "1");
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

#[derive(Clone)]
struct BearerToken(String);

impl BearerToken {
    fn from_auth(
        auth: SemanticEmbeddingExecutorAuth,
        endpoint: &ValidatedHttpEndpoint,
    ) -> Result<Option<Self>> {
        let Some(auth) = auth.bearer else {
            if endpoint.exact_loopback_ip {
                return Ok(None);
            }
            return Err(anyhow!(
                "remote semantic embedding requires an authentication token"
            ));
        };
        let token = Self::parse(auth.token)?;
        let binding = ValidatedHttpEndpoint::parse(&auth.endpoint_binding).map_err(|_| {
            anyhow!("semantic embedding authentication endpoint binding is invalid")
        })?;
        if binding != *endpoint {
            return Err(anyhow!(
                "semantic embedding authentication endpoint binding does not match"
            ));
        }
        Ok(Some(token))
    }

    fn parse(token: String) -> Result<Self> {
        if token.is_empty()
            || token.len() > MAX_TOKEN_BYTES
            || token.chars().any(|character| !character.is_ascii_graphic())
        {
            return Err(anyhow!(
                "semantic embedding authentication token is invalid"
            ));
        }
        Ok(Self(token))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractResponse {
    schema_version: u32,
    space_id: String,
    dimensions: usize,
}

fn parse_contract_response(response: &[u8]) -> Result<ExternalSemanticSpace> {
    let response: ContractResponse = serde_json::from_slice(response)
        .map_err(|_| anyhow!("semantic embedding contract response is malformed"))?;
    if response.schema_version != PROTOCOL_SCHEMA_VERSION {
        return Err(anyhow!(
            "semantic embedding endpoint uses an unsupported contract schema"
        ));
    }
    ExternalSemanticSpace::new(response.space_id, response.dimensions)
        .map_err(|_| anyhow!("semantic embedding contract response declares an invalid space"))
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum InputKind {
    Query,
    Documents,
}

#[derive(Serialize)]
struct EmbeddingsRequest<'a> {
    schema_version: u32,
    space_id: &'a str,
    dimensions: usize,
    request_id: &'a str,
    input_kind: InputKind,
    inputs: &'a [EmbeddingInput<'a>],
}

#[derive(Serialize)]
struct EmbeddingInput<'a> {
    id: &'a str,
    text: &'a str,
}

struct PreparedEmbeddingsRequest {
    request_id: String,
    input_ids: Vec<String>,
    body: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingsResponse {
    schema_version: u32,
    space_id: String,
    dimensions: usize,
    request_id: String,
    embeddings: Vec<EmbeddingOutput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingOutput {
    id: String,
    embedding: Vec<f32>,
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

fn authority_contains_credentials(endpoint: &str) -> bool {
    endpoint
        .split_once("://")
        .and_then(|(_, remainder)| remainder.split(['/', '?', '#']).next())
        .is_some_and(|authority| authority.contains('@'))
}

fn raw_url_host(endpoint: &str) -> Result<&str> {
    let authority = endpoint
        .split_once("://")
        .and_then(|(_, remainder)| remainder.split(['/', '?', '#']).next())
        .ok_or_else(|| anyhow!("semantic embedding endpoint is invalid"))?;
    if authority.contains('@') {
        return Err(anyhow!(
            "semantic embedding endpoint must not contain credentials"
        ));
    }
    if let Some(ipv6) = authority.strip_prefix('[') {
        return ipv6
            .split_once(']')
            .map(|(host, _)| host)
            .filter(|host| !host.is_empty())
            .ok_or_else(|| anyhow!("semantic embedding endpoint is invalid"));
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(authority, |(host, _)| host);
    if host.is_empty() {
        return Err(anyhow!("semantic embedding endpoint is invalid"));
    }
    Ok(host)
}

#[cfg(test)]
#[path = "http_embedding_executor_tests.rs"]
mod tests;
