use ctx_history_core::{compact_result_payload, ContentRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) const COMPLETE_CONTENT_LOCATOR_KEY: &str = "complete_content_locator_v1";
pub(super) const COMPLETE_CONTENT_BODY_DIGEST_KEY: &str = "complete_content_body_sha256";
pub(super) const RESULT_CONTENT_LOCATOR_KEY: &str = "result_content_locator_v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalResultOutcome {
    Success,
    Failure,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalResultEvidenceKind {
    CallId,
    GitCommitSummaryId,
    GitOid,
    GitAbbrevOid,
    ForgeUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalResultIdentifier {
    pub kind: CanonicalResultEvidenceKind,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalResultEvidence {
    pub outcome: CanonicalResultOutcome,
    pub identifiers: Vec<CanonicalResultIdentifier>,
    pub content_ref: Option<ContentRef>,
}

pub(super) fn take_result_evidence(payload: &mut Value) -> CanonicalResultEvidence {
    let Some(object) = payload.as_object_mut() else {
        return CanonicalResultEvidence::default();
    };
    let direct = take_result_fields(object);
    let nested = object
        .get_mut("body")
        .and_then(Value::as_object_mut)
        .and_then(take_result_fields);
    match (direct, nested) {
        (Some(direct), Some(nested)) if direct != nested => CanonicalResultEvidence::default(),
        (Some(result), _) | (_, Some(result)) => result,
        (None, None) => CanonicalResultEvidence::default(),
    }
}

fn take_result_fields(
    object: &mut serde_json::Map<String, Value>,
) -> Option<CanonicalResultEvidence> {
    let outcome_value = object.remove("result_outcome");
    let identifiers_value = object.remove("result_evidence");
    let content_ref_value = object.remove("result_content_ref");
    if outcome_value.is_none() && identifiers_value.is_none() && content_ref_value.is_none() {
        return None;
    }
    let compact = compact_result_payload(&serde_json::json!({
        "result_outcome": outcome_value,
        "result_evidence": identifiers_value,
        "result_content_ref": content_ref_value,
    }));
    let outcome = match compact.get("result_outcome").and_then(Value::as_str) {
        Some("success") => CanonicalResultOutcome::Success,
        Some("failure") => CanonicalResultOutcome::Failure,
        _ => CanonicalResultOutcome::Unknown,
    };
    let identifiers = compact
        .get("result_evidence")
        .and_then(Value::as_array)
        .cloned()
        .into_iter()
        .flatten()
        .filter_map(parse_identifier)
        .collect();
    let content_ref = compact
        .get("result_content_ref")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    Some(CanonicalResultEvidence {
        outcome,
        identifiers,
        content_ref,
    })
}

fn parse_identifier(value: Value) -> Option<CanonicalResultIdentifier> {
    let object = value.as_object()?;
    if object.len() != 2 {
        return None;
    }
    let kind = match object.get("kind")?.as_str()? {
        "call_id" => CanonicalResultEvidenceKind::CallId,
        "git_commit_summary_id" => CanonicalResultEvidenceKind::GitCommitSummaryId,
        "git_oid" => CanonicalResultEvidenceKind::GitOid,
        "git_abbrev_oid" => CanonicalResultEvidenceKind::GitAbbrevOid,
        "forge_url" => CanonicalResultEvidenceKind::ForgeUrl,
        _ => return None,
    };
    let value = object.get("value")?.as_str()?.to_owned();
    Some(CanonicalResultIdentifier { kind, value })
}

pub(super) fn strip_local_complete_content_metadata(metadata: &mut Value) {
    if let Some(object) = metadata.as_object_mut() {
        object.remove(COMPLETE_CONTENT_LOCATOR_KEY);
        object.remove(COMPLETE_CONTENT_BODY_DIGEST_KEY);
        object.remove(RESULT_CONTENT_LOCATOR_KEY);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalized_result_contract_is_typed_bounded_and_removed_from_payload() {
        let mut payload = json!({
            "result_outcome": "success",
            "result_evidence": [
                {"kind": "call_id", "value": "call-1"},
                {"kind": "git_oid", "value": "a".repeat(40)},
                {"kind": "future", "value": "ignored"},
                {"kind": "git_oid", "value": "NOT-HEX"}
            ],
            "result_content_ref": {
                "sha256": "b".repeat(64),
                "byte_len": 7
            }
        });
        let result = take_result_evidence(&mut payload);
        assert_eq!(result.outcome, CanonicalResultOutcome::Success);
        assert_eq!(result.identifiers.len(), 2);
        assert_eq!(
            result.content_ref.as_ref().map(ContentRef::byte_len),
            Some(7)
        );
        assert!(payload.get("result_outcome").is_none());
        assert!(payload.get("result_evidence").is_none());
        assert!(payload.get("result_content_ref").is_none());
    }

    #[test]
    fn provider_nested_result_contract_is_promoted_and_conflicts_fail_closed() {
        let result = json!({
            "result_outcome": "success",
            "result_evidence": [
                {"kind": "call_id", "value": "call-1"},
                {"kind": "git_commit_summary_id", "value": "fe9a28dd"}
            ]
        });
        let mut nested = json!({"body": result.clone()});
        let evidence = take_result_evidence(&mut nested);
        assert_eq!(evidence.outcome, CanonicalResultOutcome::Success);
        assert_eq!(evidence.identifiers.len(), 2);
        assert!(nested["body"].get("result_outcome").is_none());
        assert!(nested["body"].get("result_evidence").is_none());

        let mut conflicting = json!({
            "result_outcome": "failure",
            "result_evidence": [],
            "body": result
        });
        assert_eq!(
            take_result_evidence(&mut conflicting),
            CanonicalResultEvidence::default()
        );
        assert!(conflicting.get("result_outcome").is_none());
        assert!(conflicting["body"].get("result_outcome").is_none());
    }

    #[test]
    fn result_metadata_keeps_only_bounded_identity_and_timing() {
        let payload = json!({
            "provider": "codex",
            "body": {
                "tool": "exec_command",
                "command": "printf secret",
                "arguments_preview": "secret",
                "exit_code": 1,
                "duration_ms": 42,
                "output_bytes": 1024,
                "timed_out": false,
                "output_preview": "secret body"
            }
        });
        assert_eq!(
            compact_result_payload(&payload),
            json!({
                "tool": "exec_command",
                "exit_code": 1,
                "duration_ms": 42,
                "output_bytes": 1024,
                "timed_out": false
            })
        );
    }

    #[test]
    fn local_complete_content_locator_never_enters_pro_metadata() {
        let mut metadata = json!({
            "complete_content_locator_v1": {"family": "jsonl_range", "path": "/secret"},
            "complete_content_body_sha256": "a".repeat(64),
            "source_record_ordinal": 4
        });
        strip_local_complete_content_metadata(&mut metadata);
        assert_eq!(metadata, json!({"source_record_ordinal": 4}));
    }
}
