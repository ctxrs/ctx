use anyhow::{anyhow, Result};

use crate::SemanticModelContract;

pub(super) struct SemanticEmbeddingCanaryProbe {
    pub(super) id: &'static str,
    pub(super) text: &'static str,
}

pub(super) const QUERY_PROBES: &[SemanticEmbeddingCanaryProbe] = &[SemanticEmbeddingCanaryProbe {
    id: "query-daemon-recovery",
    text: "recover a background daemon after a local connection failure",
}];

pub(super) const DOCUMENT_PROBES: &[SemanticEmbeddingCanaryProbe] =
    &[SemanticEmbeddingCanaryProbe {
        id: "document-daemon-recovery",
        text: "Restart the background daemon when its local socket refuses connections.",
    }];

// Quantized from the pinned built-in CPU executor. Keeping the reference as
// i8 makes the public conformance point compact while retaining >0.998 cosine
// with the original normalized vector.
pub(super) const QUERY_DAEMON_RECOVERY_REFERENCE: &[i8; 384] = &[
    8, -7, 2, -3, 6, -6, 6, 4, 6, 1, 4, 6, 2, -4, -6, 6, 11, -6, -6, -6, 2, 1, -6, 9, 2, 5, -7, 10,
    3, -10, -10, 0, 2, -6, 4, 1, -2, -5, 12, -7, -9, 12, 2, 11, 1, 4, -7, 3, -2, -4, -8, 9, 1, 7,
    10, -6, -14, -6, -9, 9, 5, 0, 3, 3, 11, 10, 6, 7, -10, -3, -7, 7, 2, -6, 1, 1, 3, 0, 4, -1, -4,
    -4, 0, 3, -6, 9, 0, -11, 7, -6, 9, 4, -1, -10, -4, -7, -4, 3, 2, -9, 5, -7, 12, -9, -7, 5, 2,
    -8, 6, -6, -6, 8, 11, 7, -13, -5, -5, -6, 6, -8, 6, -2, -10, -8, -9, -8, 4, 6, 6, -1, -1, 1, 7,
    7, 5, 14, -3, 0, -8, -6, -4, 8, 0, 6, 11, 2, 14, -5, 8, -7, 6, -3, 16, 0, 3, -8, -13, 1, 4, 7,
    -6, -11, -9, 4, -8, -2, 8, 7, -2, -6, 0, 7, -5, 2, 1, 5, -9, 6, 7, 8, -6, 2, -7, -6, -9, -6,
    -7, 3, 8, -7, -3, 5, -1, -3, -5, 3, 2, 9, 7, 5, 1, -3, -1, 3, 10, 3, -4, 5, -3, 7, 5, -4, -5,
    9, -1, -1, 2, 7, -1, 7, 4, -3, 3, -9, -10, 6, 7, -10, -10, 7, -2, -5, -5, -11, -8, -7, -5, 7,
    8, -8, -2, -6, 5, -9, 10, -10, 1, 2, -6, 1, 4, -6, -5, 0, -4, 13, 9, 5, -3, 9, 5, -7, 8, 3, 3,
    -3, -6, -7, -8, -5, -4, 0, -1, -8, -7, -4, 6, 12, -4, -5, 15, 6, 11, 5, 11, -3, -8, 4, -4, -1,
    0, -4, 2, -2, 4, 7, 0, 9, -3, 1, -4, -9, 6, 9, -12, 0, 1, 6, 0, 6, 7, 12, -2, -9, 6, 8, -4, 3,
    -8, -4, -8, -6, 2, -7, 3, 7, -7, -4, 6, 2, 0, -3, -6, 4, -7, -2, -6, 6, -4, -6, 4, 2, -12, 3,
    -6, -5, 10, -5, -6, 2, 7, -14, 8, 8, -8, 5, -10, -12, -2, 6, -12, -7, 2, 0, 2, 4, -5, 7, 4,
    -10, 12, 7, 2, -3, -9, -6, -3, 2, -5, -4, 8, 1, -1, 6,
];

pub(super) const DOCUMENT_DAEMON_RECOVERY_REFERENCE: &[i8; 384] = &[
    5, -5, 2, -6, 10, -6, 0, -3, 11, 4, 6, 2, 4, -1, -7, 5, 8, -1, -9, -7, 3, 0, -5, 8, 5, 9, -5,
    5, 2, -13, -6, -5, 2, -5, 1, 4, -6, -9, 9, -7, -6, 12, 0, 16, 5, 6, -9, 2, -6, -2, -7, 9, -3,
    7, 5, -4, -10, -8, -10, 7, 9, -1, 4, 1, 11, 11, 5, 7, -7, -2, -6, 8, 0, -7, 0, 1, 2, -2, 4, 0,
    -2, -4, -3, 5, -9, 8, 3, -10, 8, -5, 8, 4, -4, -10, -5, -4, -9, 7, 2, -9, 5, -3, 9, -13, -9, 5,
    2, -9, 7, -7, -9, 4, 5, 4, -6, -8, -1, -1, 5, -6, 3, -3, -9, -12, -8, -5, 6, 6, -1, 3, 2, 3, 5,
    5, 6, 18, -4, 0, -7, -5, -5, 6, 0, 9, 8, 2, 12, -7, 9, -7, 9, -7, 16, 2, 6, -8, -11, -1, 8, 8,
    -6, -8, -11, 0, -7, -3, 6, 9, -8, -3, -4, 1, -2, 5, 5, 9, -7, 6, 10, 9, -1, -2, -7, -8, -6, -8,
    -4, 3, 7, -2, -7, 4, -1, -6, -5, 5, -1, 9, 6, 3, 3, -4, 2, 5, 3, 2, -4, 8, 0, 4, 5, -6, -10, 8,
    -4, 0, 4, 10, -3, 1, 4, 0, 7, -6, -13, 4, 9, -3, -7, 5, -3, 1, -10, -10, -5, -9, 3, 4, 5, -5,
    -4, -2, 3, -11, 9, -7, -3, 1, -6, 4, 7, -7, -4, -1, -1, 10, 10, 5, -4, 6, 1, -7, 9, 3, 6, -1,
    -9, -8, -12, -7, -4, 6, 2, -11, -4, -5, 3, 10, -8, -5, 8, 5, 10, 4, 13, -3, -6, 7, -7, -5, -3,
    -10, 3, -9, 4, 7, -2, 10, -5, 4, 2, -9, 6, 6, -11, 4, 2, 4, 0, 7, 7, 13, -6, -5, 4, 15, -1, 4,
    -5, -6, -9, -9, 1, -5, 0, 7, -8, -4, 6, 6, 3, -3, -1, 4, -9, -1, -5, 3, -4, -10, 2, 1, -9, 6,
    -3, -10, 9, -3, -3, -1, 8, -10, 4, 6, -2, 5, -10, -9, 0, 6, -12, 0, 0, 7, -1, 5, -2, 1, 2, -12,
    12, 7, 0, -5, -7, -4, -3, 8, -7, -11, 7, 5, 0, 7,
];

// Each entry is (probe ID, quantized normalized embedding, minimum cosine).
// These public probes catch accidental incompatibility and ordinary
// misconfiguration. An intentionally malicious endpoint can recognize and
// special-case them, so passing the canary is not an endpoint trust boundary.
pub(super) const FROZEN_CANARY_REFERENCES: &[(&str, &[i8], f64)] = &[
    (
        "query-daemon-recovery",
        QUERY_DAEMON_RECOVERY_REFERENCE,
        0.99,
    ),
    (
        "document-daemon-recovery",
        DOCUMENT_DAEMON_RECOVERY_REFERENCE,
        0.99,
    ),
];

pub(super) fn prepared_query_probes(contract: &SemanticModelContract) -> Vec<String> {
    QUERY_PROBES
        .iter()
        .map(|probe| contract.query_text(probe.text))
        .collect()
}

pub(super) fn prepared_document_probes(contract: &SemanticModelContract) -> Vec<String> {
    DOCUMENT_PROBES
        .iter()
        .map(|probe| contract.document_text(probe.text))
        .collect()
}

pub(super) fn validate_conformance_canary(
    query_embeddings: &[Vec<f32>],
    document_embeddings: &[Vec<f32>],
) -> Result<()> {
    if query_embeddings.len() != QUERY_PROBES.len()
        || document_embeddings.len() != DOCUMENT_PROBES.len()
    {
        return Err(canary_failed());
    }
    validate_frozen_references(query_embeddings, document_embeddings)
}

fn validate_frozen_references(
    query_embeddings: &[Vec<f32>],
    document_embeddings: &[Vec<f32>],
) -> Result<()> {
    for (probe_id, reference, minimum_cosine) in FROZEN_CANARY_REFERENCES {
        let actual = QUERY_PROBES
            .iter()
            .position(|probe| probe.id == *probe_id)
            .map(|index| &query_embeddings[index])
            .or_else(|| {
                DOCUMENT_PROBES
                    .iter()
                    .position(|probe| probe.id == *probe_id)
                    .map(|index| &document_embeddings[index])
            })
            .ok_or_else(|| anyhow!("semantic embedding conformance reference is invalid"))?;
        if !(0.0..=1.0).contains(minimum_cosine)
            || reference.len() != actual.len()
            || cosine_quantized(actual, reference)? < *minimum_cosine
        {
            return Err(canary_failed());
        }
    }
    Ok(())
}

fn cosine_quantized(actual: &[f32], reference: &[i8]) -> Result<f64> {
    if actual.len() != reference.len() || actual.is_empty() {
        return Err(canary_failed());
    }
    let dot = actual
        .iter()
        .zip(reference)
        .map(|(actual, reference)| f64::from(*actual) * f64::from(*reference))
        .sum::<f64>();
    let reference_norm = reference
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    let similarity = dot / reference_norm;
    if similarity.is_finite() {
        Ok(similarity)
    } else {
        Err(canary_failed())
    }
}

fn canary_failed() -> anyhow::Error {
    anyhow!("semantic embedding endpoint failed the conformance canary")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(reference: &[i8]) -> Vec<f32> {
        let vector = reference
            .iter()
            .map(|value| f32::from(*value))
            .collect::<Vec<_>>();
        let norm = vector.iter().map(|value| value.powi(2)).sum::<f32>().sqrt();
        vector.iter().map(|value| value / norm).collect()
    }

    #[test]
    fn correct_frozen_query_and_document_data_passes() {
        let query_embeddings = vec![normalized(QUERY_DAEMON_RECOVERY_REFERENCE)];
        let document_embeddings = vec![normalized(DOCUMENT_DAEMON_RECOVERY_REFERENCE)];

        validate_conformance_canary(&query_embeddings, &document_embeddings).unwrap();
    }

    #[test]
    fn swapped_or_reused_roles_fail_the_frozen_pair() {
        let frozen_query = normalized(QUERY_DAEMON_RECOVERY_REFERENCE);
        let frozen_document = normalized(DOCUMENT_DAEMON_RECOVERY_REFERENCE);
        assert!(validate_conformance_canary(
            std::slice::from_ref(&frozen_document),
            std::slice::from_ref(&frozen_query),
        )
        .is_err());
        assert!(validate_conformance_canary(
            std::slice::from_ref(&frozen_query),
            std::slice::from_ref(&frozen_query),
        )
        .is_err());
    }
}
