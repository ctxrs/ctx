use std::{collections::HashMap, io::Read};

use anyhow::{anyhow, Result};

use super::EmbeddingOutput;

const NORMALIZED_NORM_SQUARED_TOLERANCE: f64 = 1.0e-3;

pub(super) enum ResponseBodyError {
    TooLarge,
    InvalidLength,
    Transport,
}

pub(super) fn read_response_body(
    mut response: ureq_semantic::http::Response<ureq_semantic::Body>,
    max_body_bytes: usize,
) -> std::result::Result<Vec<u8>, ResponseBodyError> {
    let declared_length = response
        .headers()
        .get("content-length")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| ResponseBodyError::InvalidLength)?
                .parse::<usize>()
                .map_err(|_| ResponseBodyError::InvalidLength)
        })
        .transpose()?;
    if declared_length.is_some_and(|length| length > max_body_bytes) {
        return Err(ResponseBodyError::TooLarge);
    }
    let mut body = Vec::with_capacity(declared_length.unwrap_or(0));
    response
        .body_mut()
        .as_reader()
        .take((max_body_bytes + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|_| ResponseBodyError::Transport)?;
    if body.len() > max_body_bytes {
        return Err(ResponseBodyError::TooLarge);
    }
    if declared_length.is_some_and(|length| length != body.len()) {
        return Err(ResponseBodyError::Transport);
    }
    Ok(body)
}

pub(super) fn map_embeddings_by_id(
    embeddings: Vec<EmbeddingOutput>,
    input_ids: &[String],
    dimensions: usize,
) -> Result<Vec<Vec<f32>>> {
    let expected = input_ids
        .iter()
        .enumerate()
        .map(|(index, input_id)| (input_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    if expected.len() != input_ids.len() {
        return Err(anyhow!(
            "semantic embedding request contains a duplicate input ID"
        ));
    }
    let mut ordered = (0..input_ids.len()).map(|_| None).collect::<Vec<_>>();
    for output in embeddings {
        let Some(index) = expected.get(output.id.as_str()).copied() else {
            return Err(anyhow!(
                "semantic embedding response returned an unknown input ID"
            ));
        };
        if ordered[index].is_some() {
            return Err(anyhow!(
                "semantic embedding response returned a duplicate input ID"
            ));
        }
        validate_embedding(&output.embedding, dimensions)?;
        ordered[index] = Some(output.embedding);
    }
    ordered
        .into_iter()
        .map(|embedding| {
            embedding.ok_or_else(|| anyhow!("semantic embedding response is missing an input ID"))
        })
        .collect()
}

pub(super) fn validate_embedding(embedding: &[f32], dimensions: usize) -> Result<()> {
    if embedding.len() != dimensions {
        return Err(anyhow!(
            "semantic embedding response returned the wrong dimensions"
        ));
    }
    let mut norm_squared = 0.0_f64;
    for value in embedding {
        if !value.is_finite() {
            return Err(anyhow!(
                "semantic embedding response contains a non-finite value"
            ));
        }
        norm_squared += f64::from(*value) * f64::from(*value);
    }
    if norm_squared == 0.0 {
        return Err(anyhow!(
            "semantic embedding response contains a zero-norm vector"
        ));
    }
    if !norm_squared.is_finite() || (norm_squared - 1.0).abs() > NORMALIZED_NORM_SQUARED_TOLERANCE {
        return Err(anyhow!(
            "semantic embedding response contains a vector that is not L2-normalized"
        ));
    }
    Ok(())
}
