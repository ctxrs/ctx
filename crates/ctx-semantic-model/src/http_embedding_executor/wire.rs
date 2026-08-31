use std::fmt;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::ExternalSemanticSpace;

use super::{
    SemanticEmbeddingExecutorAuth, ValidatedHttpEndpoint, MAX_TOKEN_BYTES, PROTOCOL_SCHEMA_VERSION,
};

#[derive(Clone)]
pub(super) struct BearerToken(String);

impl BearerToken {
    pub(super) fn from_auth(
        auth: SemanticEmbeddingExecutorAuth,
        endpoint: &ValidatedHttpEndpoint,
    ) -> Result<Option<Self>> {
        let Some(auth) = auth.bearer else {
            if endpoint.is_loopback() {
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

    pub(super) fn expose(&self) -> &str {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyContractResponse {
    pub(super) schema_version: u32,
    pub(super) model_key: String,
    pub(super) model_contract_fingerprint: String,
}

pub(super) fn parse_contract_response(response: &[u8]) -> Result<ExternalSemanticSpace> {
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
pub(super) enum InputKind {
    Query,
    Documents,
}

#[derive(Serialize)]
pub(super) struct EmbeddingsRequest<'a> {
    pub(super) schema_version: u32,
    pub(super) space_id: &'a str,
    pub(super) dimensions: usize,
    pub(super) request_id: &'a str,
    pub(super) input_kind: InputKind,
    pub(super) inputs: &'a [EmbeddingInput<'a>],
}

#[derive(Serialize)]
pub(super) struct LegacyEmbeddingsRequest<'a> {
    pub(super) schema_version: u32,
    pub(super) model_key: &'a str,
    pub(super) model_contract_fingerprint: &'a str,
    pub(super) request_id: &'a str,
    pub(super) input_kind: InputKind,
    pub(super) inputs: &'a [EmbeddingInput<'a>],
}

#[derive(Serialize)]
pub(super) struct EmbeddingInput<'a> {
    pub(super) id: &'a str,
    pub(super) text: &'a str,
}

pub(super) struct PreparedEmbeddingsRequest {
    pub(super) request_id: String,
    pub(super) input_ids: Vec<String>,
    pub(super) body: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmbeddingsResponse {
    pub(super) schema_version: u32,
    pub(super) space_id: String,
    pub(super) dimensions: usize,
    pub(super) request_id: String,
    pub(super) embeddings: Vec<EmbeddingOutput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyEmbeddingsResponse {
    pub(super) schema_version: u32,
    pub(super) model_key: String,
    pub(super) model_contract_fingerprint: String,
    pub(super) request_id: String,
    pub(super) embeddings: Vec<EmbeddingOutput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmbeddingOutput {
    pub(super) id: String,
    pub(super) embedding: Vec<f32>,
}
