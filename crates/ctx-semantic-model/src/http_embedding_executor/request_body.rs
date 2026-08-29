use std::io::{self, Write};

use anyhow::{anyhow, Result};
use serde::Serialize;

use super::{
    request_body_limit_failure, EmbeddingInput, EmbeddingsRequest, HttpSemanticEmbeddingExecutor,
    InputKind, MAX_REQUEST_BODY_BYTES, PROTOCOL_SCHEMA_VERSION, UUID_WIRE_VALUE,
};

pub(super) struct RequestBodySizer {
    body_len: usize,
    input_count: usize,
}

impl RequestBodySizer {
    pub(super) fn new(
        executor: &HttpSemanticEmbeddingExecutor,
        input_kind: InputKind,
    ) -> Result<Self> {
        let request = EmbeddingsRequest {
            schema_version: PROTOCOL_SCHEMA_VERSION,
            space_id: executor.space.space_id(),
            dimensions: executor.space.dimensions(),
            request_id: UUID_WIRE_VALUE,
            input_kind,
            inputs: &[],
        };
        let Some(body_len) = encoded_json_len(&request, MAX_REQUEST_BODY_BYTES)? else {
            return Err(request_body_limit_failure());
        };
        Ok(Self {
            body_len,
            input_count: 0,
        })
    }

    pub(super) fn try_push(&mut self, text: &str) -> Result<bool> {
        let separator_len = usize::from(self.input_count > 0);
        let Some(remaining) = MAX_REQUEST_BODY_BYTES
            .checked_sub(self.body_len)
            .and_then(|remaining| remaining.checked_sub(separator_len))
        else {
            return Ok(false);
        };
        let input = EmbeddingInput {
            id: UUID_WIRE_VALUE,
            text,
        };
        let Some(input_len) = encoded_json_len(&input, remaining)? else {
            return Ok(false);
        };
        self.body_len += separator_len + input_len;
        self.input_count += 1;
        Ok(true)
    }

    pub(super) const fn body_len(&self) -> usize {
        self.body_len
    }
}

pub(super) fn encoded_json_len(value: &impl Serialize, limit: usize) -> Result<Option<usize>> {
    let mut writer = CountingWriter {
        bytes: 0,
        limit,
        limit_exceeded: false,
    };
    match serde_json::to_writer(&mut writer, value) {
        Ok(()) => Ok(Some(writer.bytes)),
        Err(_) if writer.limit_exceeded => Ok(None),
        Err(_) => Err(anyhow!(
            "semantic embedding request could not be size-checked"
        )),
    }
}

struct CountingWriter {
    bytes: usize,
    limit: usize,
    limit_exceeded: bool,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.checked_add(bytes.len()) else {
            self.limit_exceeded = true;
            return Err(io::Error::other("semantic embedding request is too large"));
        };
        if next > self.limit {
            self.limit_exceeded = true;
            return Err(io::Error::other("semantic embedding request is too large"));
        }
        self.bytes = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn encode_preflighted_request(
    request: &EmbeddingsRequest<'_>,
    body_len: usize,
) -> Result<Vec<u8>> {
    let mut writer = BoundedBodyWriter {
        body: Vec::with_capacity(body_len),
        limit: body_len,
    };
    serde_json::to_writer(&mut writer, request)
        .map_err(|_| anyhow!("semantic embedding request could not be encoded"))?;
    if writer.body.len() != body_len {
        return Err(anyhow!(
            "semantic embedding request size changed after preflight"
        ));
    }
    Ok(writer.body)
}

struct BoundedBodyWriter {
    body: Vec<u8>,
    limit: usize,
}

impl Write for BoundedBodyWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.body.len().checked_add(bytes.len()) else {
            return Err(io::Error::other("semantic embedding request is too large"));
        };
        if next > self.limit {
            return Err(io::Error::other("semantic embedding request is too large"));
        }
        self.body.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
