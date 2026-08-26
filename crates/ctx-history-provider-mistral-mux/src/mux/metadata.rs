use std::{fmt, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_capture_model::{
    normalization::{provider_local_preview, provider_timestamp_seconds_to_datetime},
    push_provider_import_failure,
    time::parse_rfc3339_utc,
    ProviderImportSummary,
};
use ctx_history_provider_runtime::{CaptureError, Result};
use serde::{
    de::{IgnoredAny, MapAccess, Visitor},
    Deserialize, Deserializer as _, Serialize,
};
use serde_json::Value;

use super::source::MuxSessionSource;
use super::{MUX_MAX_FAILURE_BYTES, MUX_MAX_ID_BYTES, PROVIDER_MAX_PREVIEW_CHARS};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MuxBoundedSessionMetadata {
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) root_provider_session_id: Option<String>,
    pub(super) lineage_ambiguous: bool,
    pub(super) started_at: String,
    pub(super) cwd: Option<String>,
    pub(super) model: Option<String>,
    pub(super) metadata_revision: String,
    pub(super) metadata_failure: Option<String>,
}

pub(super) fn mux_bounded_session_metadata_from_bytes(
    source: &MuxSessionSource,
    metadata_revision: &str,
    imported_at: DateTime<Utc>,
    bytes: Option<&[u8]>,
) -> Result<MuxBoundedSessionMetadata> {
    let mut summary = ProviderImportSummary::default();
    let mut raw_lineage = match bytes {
        None => MuxRawLineageAuthority::default(),
        Some(bytes) => match mux_raw_lineage_authority(bytes) {
            Ok(authority) => authority,
            Err(_) => MuxRawLineageAuthority {
                audit_failed: true,
                ..MuxRawLineageAuthority::default()
            },
        },
    };
    let metadata = match bytes {
        None => Value::Null,
        Some(bytes) => match serde_json::from_slice::<Value>(bytes) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                raw_lineage.invalidate();
                push_provider_import_failure(
                    &mut summary,
                    0,
                    "Mux metadata.json must contain a JSON object".to_owned(),
                );
                Value::Null
            }
            Err(error) => {
                raw_lineage.invalidate();
                push_provider_import_failure(
                    &mut summary,
                    0,
                    format!("invalid Mux metadata.json: {error}"),
                );
                Value::Null
            }
        },
    };
    mux_bounded_session_metadata_from_value(
        source,
        metadata_revision,
        imported_at,
        metadata,
        summary,
        raw_lineage,
    )
}

fn mux_bounded_session_metadata_from_value(
    source: &MuxSessionSource,
    metadata_revision: &str,
    imported_at: DateTime<Utc>,
    metadata: Value,
    summary: ProviderImportSummary,
    raw_lineage: MuxRawLineageAuthority,
) -> Result<MuxBoundedSessionMetadata> {
    let provider_session_id = bounded_mux_id(
        mux_string_pointer(&metadata, &["/workspaceId", "/sessionId"])
            .unwrap_or_else(|| source.provider_session_id.clone()),
        &source.session_dir,
        "workspace id",
    )?;
    let parent_provider_session_id = raw_lineage
        .parent
        .value
        .clone()
        .or_else(|| source.parent_provider_session_id.clone())
        .map(|value| bounded_mux_id(value, &source.session_dir, "parent workspace id"))
        .transpose()?;
    let root_provider_session_id = raw_lineage
        .root
        .value
        .clone()
        .map(|value| bounded_mux_id(value, &source.session_dir, "root workspace id"))
        .transpose()?;
    let path_parent_conflicts = matches!(
        (
            raw_lineage.parent.value.as_deref(),
            source.parent_provider_session_id.as_deref(),
        ),
        (Some(metadata_parent), Some(path_parent)) if metadata_parent != path_parent
    );
    let self_parent = parent_provider_session_id
        .as_deref()
        .is_some_and(|parent| parent == provider_session_id);
    let foreign_parent_with_self_root = parent_provider_session_id
        .as_deref()
        .is_some_and(|parent| parent != provider_session_id)
        && root_provider_session_id
            .as_deref()
            .is_some_and(|root| root == provider_session_id);
    let lineage_ambiguous = raw_lineage.audit_failed
        || raw_lineage.parent.ambiguous
        || raw_lineage.root.ambiguous
        || path_parent_conflicts
        || self_parent
        || foreign_parent_with_self_root;
    let bounded_text = |value: Option<String>| {
        value.map(|text| provider_local_preview(&text, PROVIDER_MAX_PREVIEW_CHARS).0)
    };
    let started_at = ["/createdAt", "/createdAtMs"]
        .iter()
        .find_map(|pointer| metadata.pointer(pointer).and_then(mux_value_timestamp))
        .unwrap_or(imported_at);
    let cwd = bounded_text(mux_string_pointer(
        &metadata,
        &["/projectPath", "/workspacePath", "/cwd", "/repoPath"],
    ));
    let model = bounded_text(mux_string_pointer(&metadata, &["/model"]));
    let metadata_failure = summary
        .failures
        .first()
        .map(|failure| bounded_mux_failure(failure.error.clone()));
    Ok(MuxBoundedSessionMetadata {
        provider_session_id,
        parent_provider_session_id,
        root_provider_session_id,
        lineage_ambiguous,
        started_at: started_at.to_rfc3339(),
        cwd,
        model,
        metadata_revision: metadata_revision.to_owned(),
        metadata_failure,
    })
}

#[derive(Debug, Clone, Default)]
struct MuxRawLineageAuthority {
    audit_failed: bool,
    parent: MuxRawLineageClaim,
    root: MuxRawLineageClaim,
}

#[derive(Debug, Clone, Default)]
struct MuxRawLineageClaim {
    value: Option<String>,
    selected_priority: Option<u8>,
    ambiguous: bool,
}

impl MuxRawLineageAuthority {
    fn invalidate(&mut self) {
        *self = Self {
            audit_failed: true,
            ..Self::default()
        };
    }
}

impl MuxRawLineageClaim {
    fn observe(&mut self, value: Value, priority: u8) {
        let Some(claim) = value
            .as_str()
            .filter(|claim| !claim.trim().is_empty() && claim.len() <= MUX_MAX_ID_BYTES)
        else {
            self.ambiguous = true;
            return;
        };
        if self
            .value
            .as_deref()
            .is_some_and(|current| current != claim)
        {
            self.ambiguous = true;
        }
        if self
            .selected_priority
            .is_none_or(|selected| priority < selected)
        {
            self.value = Some(claim.to_owned());
            self.selected_priority = Some(priority);
        }
    }
}

fn mux_raw_lineage_authority(bytes: &[u8]) -> serde_json::Result<MuxRawLineageAuthority> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let authority = deserializer.deserialize_map(MuxRawLineageVisitor)?;
    deserializer.end()?;
    Ok(authority)
}

struct MuxRawLineageVisitor;

impl<'de> Visitor<'de> for MuxRawLineageVisitor {
    type Value = MuxRawLineageAuthority;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Mux metadata JSON object")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut authority = MuxRawLineageAuthority::default();
        while let Some(key) = map.next_key::<String>()? {
            match mux_lineage_kind(&key) {
                Some(MuxLineageKind::Parent(priority)) => {
                    authority
                        .parent
                        .observe(map.next_value::<Value>()?, priority);
                }
                Some(MuxLineageKind::Root(priority)) => {
                    authority.root.observe(map.next_value::<Value>()?, priority);
                }
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(authority)
    }
}

#[derive(Debug, Clone, Copy)]
enum MuxLineageKind {
    Parent(u8),
    Root(u8),
}

fn mux_lineage_kind(key: &str) -> Option<MuxLineageKind> {
    Some(match key {
        "parentWorkspaceId" => MuxLineageKind::Parent(0),
        "parentTaskId" => MuxLineageKind::Parent(1),
        "parentSessionId" => MuxLineageKind::Parent(2),
        "parent_session_id" => MuxLineageKind::Parent(3),
        "rootWorkspaceId" => MuxLineageKind::Root(0),
        "rootTaskId" => MuxLineageKind::Root(1),
        "rootSessionId" => MuxLineageKind::Root(2),
        _ => return None,
    })
}

pub(super) fn bounded_mux_id(value: String, path: &Path, label: &'static str) -> Result<String> {
    if value.len() > MUX_MAX_ID_BYTES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: match label {
                "workspace id" => "Mux workspace id exceeds the supported size",
                "parent workspace id" => "Mux parent workspace id exceeds the supported size",
                _ => "Mux root workspace id exceeds the supported size",
            },
        });
    }
    Ok(value)
}

pub(super) fn bounded_mux_failure(mut error: String) -> String {
    if error.len() <= MUX_MAX_FAILURE_BYTES {
        return error;
    }
    let mut boundary = MUX_MAX_FAILURE_BYTES;
    while !error.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    error.truncate(boundary);
    error
}

pub(super) fn mux_string_pointer(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|raw| !raw.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(super) fn mux_value_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(raw) => parse_rfc3339_utc(raw).or_else(|| {
            raw.parse::<f64>()
                .ok()
                .and_then(provider_timestamp_seconds_to_datetime)
        }),
        Value::Number(number) => number
            .as_f64()
            .and_then(provider_timestamp_seconds_to_datetime),
        _ => None,
    }
}

#[cfg(test)]
mod lineage_tests {
    use super::*;

    fn metadata(value: Value) -> MuxBoundedSessionMetadata {
        metadata_bytes(&serde_json::to_vec(&value).unwrap())
    }

    fn metadata_bytes(bytes: &[u8]) -> MuxBoundedSessionMetadata {
        let temp = tempfile::tempdir().unwrap();
        let source = MuxSessionSource {
            session_dir: temp.path().join("mux-child"),
            archive_path: None,
            chat_path: None,
            partial_path: None,
            metadata_path: None,
            provider_session_id: "mux-child".to_owned(),
            parent_provider_session_id: None,
        };
        mux_bounded_session_metadata_from_bytes(
            &source,
            "mux-lineage-test-v1",
            DateTime::<Utc>::UNIX_EPOCH,
            Some(bytes),
        )
        .unwrap()
    }

    #[test]
    fn malformed_alias_presence_is_ambiguous() {
        for value in [
            serde_json::json!({
                "workspaceId": "mux-child",
                "parentWorkspaceId": 7
            }),
            serde_json::json!({
                "workspaceId": "mux-child",
                "rootSessionId": null
            }),
        ] {
            assert!(metadata(value).lineage_ambiguous);
        }
    }

    #[test]
    fn equal_positive_duplicate_keys_and_aliases_remain_exact() {
        let metadata = metadata_bytes(
            br#"{
                "workspaceId": "mux-child",
                "parentSessionId": "mux-parent",
                "parentSessionId": "mux-parent",
                "parentWorkspaceId": "mux-parent",
                "parent_session_id": "mux-parent",
                "rootSessionId": "mux-root",
                "rootSessionId": "mux-root",
                "rootWorkspaceId": "mux-root",
                "rootTaskId": "mux-root"
            }"#,
        );

        assert!(!metadata.lineage_ambiguous);
        assert_eq!(
            metadata.parent_provider_session_id.as_deref(),
            Some("mux-parent")
        );
        assert_eq!(
            metadata.root_provider_session_id.as_deref(),
            Some("mux-root")
        );
    }

    #[test]
    fn direct_parent_does_not_synthesize_root() {
        let without_root = metadata(serde_json::json!({
            "workspaceId": "mux-child",
            "parentSessionId": "mux-parent"
        }));
        assert_eq!(
            without_root.parent_provider_session_id.as_deref(),
            Some("mux-parent")
        );
        assert_eq!(without_root.root_provider_session_id, None);

        let with_root = metadata(serde_json::json!({
            "workspaceId": "mux-child",
            "parentSessionId": "mux-parent",
            "rootSessionId": "mux-root"
        }));
        assert_eq!(
            with_root.root_provider_session_id.as_deref(),
            Some("mux-root")
        );
    }

    #[test]
    fn self_parent_and_foreign_parent_self_root_are_ambiguous() {
        for value in [
            serde_json::json!({
                "workspaceId": "mux-child",
                "parentWorkspaceId": "mux-child"
            }),
            serde_json::json!({
                "workspaceId": "mux-child",
                "parentWorkspaceId": "mux-parent",
                "rootWorkspaceId": "mux-child"
            }),
        ] {
            assert!(metadata(value).lineage_ambiguous);
        }
    }

    #[test]
    fn conflicting_null_or_malformed_lineage_occurrences_are_ambiguous() {
        for raw in [
            br#"{
                "workspaceId": "mux-child",
                "parentSessionId": "mux-parent",
                "parentSessionId": "conflicting-parent"
            }"#
            .as_slice(),
            br#"{
                "workspaceId": "mux-child",
                "rootSessionId": "mux-root",
                "rootSessionId": "conflicting-root"
            }"#
            .as_slice(),
            br#"{
                "workspaceId": "mux-child",
                "parentSessionId": "mux-parent",
                "parent_session_id": "conflicting-parent"
            }"#
            .as_slice(),
            br#"{
                "workspaceId": "mux-child",
                "parentSessionId": "mux-parent",
                "parentSessionId": null
            }"#
            .as_slice(),
            br#"{
                "workspaceId": "mux-child",
                "rootSessionId": "mux-root",
                "rootTaskId": null
            }"#
            .as_slice(),
            br#"{
                "workspaceId": "mux-child",
                "parentSessionId": "mux-parent",
                "parentSessionId": 7
            }"#
            .as_slice(),
            br#"{
                "workspaceId": "mux-child",
                "rootSessionId": {"unexpected": "object"}
            }"#
            .as_slice(),
        ] {
            assert!(metadata_bytes(raw).lineage_ambiguous);
        }
    }

    #[test]
    fn unrelated_duplicate_keys_do_not_ambiguate_lineage() {
        assert!(
            !metadata_bytes(
                br#"{
                "workspaceId": "mux-child",
                "title": "first",
                "title": "second"
            }"#
            )
            .lineage_ambiguous
        );
    }
}
