//! Prime Agent v3 sessions over the shared certified JSONL lifecycle.

use std::{fs, io, marker::PhantomData, path::Path, sync::Arc};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_native_session_id, AgentScope, CaptureProvider, ProviderNativeSessionRelationship,
    SourceAnchorScope, SourceKey, StableEntityId, TypedKey,
};
use ctx_history_jsonl::{
    observe_opened_file, probe_records_until, JsonlFamilyAdapter, JsonlFamilyAppendMode,
    JsonlFamilyError, JsonlFamilyMembershipObservation, JsonlFamilyProjector,
    JsonlFamilyRejectedLeaf, JsonlRecordRef,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::family::jsonl::{JsonlFamilyInventory, JsonlFamilyLeaf},
    CaptureError, JsonlProviderRuntime, Result,
};

mod inventory;
mod projection;
#[cfg(test)]
mod tests;

const SOURCE_FORMAT: &str = "prime_agent_session_jsonl";
const SCHEMA: &str = "prime-agent-session-v3";
const PARSER_REVISION: &str = "prime-agent-jsonl-v1";
const EVENT_REVISION: &str = "prime-agent-entry-v1";

pub(crate) fn adapter<R: JsonlProviderRuntime>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
    Arc::new(PrimeAdapter::<R> {
        scope: source_root_lineage
            .map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
        runtime: PhantomData,
    })
}

struct PrimeAdapter<R> {
    scope: SourceAnchorScope,
    runtime: PhantomData<fn() -> R>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    native_id: String,
    cwd: String,
    parent: Option<StableEntityId>,
    relationship: Option<ProviderNativeSessionRelationship>,
    agent_scope: Option<AgentScope>,
}

impl<R: JsonlProviderRuntime> JsonlFamilyAdapter for PrimeAdapter<R> {
    type Runtime = R;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::PrimeAgent
    }
    fn source_format(&self) -> &'static str {
        SOURCE_FORMAT
    }
    fn schema_variant(&self) -> &'static str {
        SCHEMA
    }
    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }
    fn event_identity_revision(&self) -> &'static str {
        EVENT_REVISION
    }
    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        let metadata = match fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.is_symlink() {
            return Err(invalid("Prime Agent history roots must not be symlinks"));
        }
        let directory = if metadata.is_file() {
            root.parent()
                .ok_or_else(|| invalid("Prime Agent session has no parent directory"))?
        } else {
            root
        };
        let authority = Arc::new(ProviderSourceRoot::open(directory)?);
        let mut leaves = Vec::new();
        let mut rejected = Vec::new();
        for path in inventory::paths(root)? {
            let path = std::path::absolute(path)?;
            let relative = path
                .strip_prefix(authority.named_path())
                .map_err(|_| invalid("Prime Agent transcript escaped its authority"))?
                .to_path_buf();
            let opened = match authority.open_file(&relative) {
                Ok(opened) => Arc::new(opened),
                Err(error)
                    if error.is_source_unavailable()
                        || error.is_ignorable_membership_entry()
                        || matches!(&error, CaptureError::Io(error) if error.kind() == io::ErrorKind::PermissionDenied) =>
                {
                    let detail = error.to_string();
                    rejected.push(
                        JsonlFamilyRejectedLeaf::bind_unobserved(
                            path.clone(),
                            relative,
                            TypedKey::utf8(detail.clone()).map_err(invalid)?,
                            0,
                        )
                        .with_logical_source_failure(rejected_source(&path, self.scope)?, detail),
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            let observation = observe_opened_file(&path, opened.as_ref())?;
            let parsed = probe_records_until(&path, &opened, 1, |record| {
                parse_header(record, &path, self.scope)
            });
            match parsed {
                Ok(Some((binding, _probe))) => {
                    let source = source_key(&binding.native_id, self.scope)?;
                    // Keep the header in the physical stream: its structured
                    // metadata represents even a new, otherwise empty session.
                    leaves.push(JsonlFamilyLeaf::observe(
                        source,
                        path,
                        Arc::clone(&authority),
                        relative,
                        TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(invalid)?,
                    )?);
                }
                Ok(None) => {
                    // The shared engine classifies a torn first record as pending.
                    rejected.push(JsonlFamilyRejectedLeaf::bind_observed(
                        path,
                        relative,
                        observation,
                        TypedKey::utf8("prime-agent-missing-header").map_err(invalid)?,
                        1,
                    ));
                }
                Err(CaptureError::InvalidPayload(detail)) => {
                    let source = rejected_source(&path, self.scope)?;
                    rejected.push(
                        JsonlFamilyRejectedLeaf::bind_observed(
                            path,
                            relative,
                            observation,
                            TypedKey::utf8(detail.clone()).map_err(invalid)?,
                            1,
                        )
                        .with_logical_source_failure(source, detail),
                    );
                }
                Err(error) => return Err(error),
            }
        }
        JsonlFamilyInventory::present_with_rejected(
            self.provider(),
            root,
            authority,
            leaves,
            rejected,
        )
    }

    fn observe_terminal_membership(
        &self,
        root: &Path,
        opening: &JsonlFamilyInventory,
    ) -> Result<JsonlFamilyMembershipObservation<CaptureError>> {
        if opening.root_missing() {
            return JsonlFamilyMembershipObservation::observe(root, opening);
        }
        JsonlFamilyMembershipObservation::observe_selected_paths(opening, inventory::paths(root)?)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = R>>> {
        let TypedKey::Bytes(bytes) = leaf.binding() else {
            return Err(invalid("Prime Agent binding must be bytes"));
        };
        let binding: Binding = serde_json::from_slice(bytes)?;
        Ok(Box::new(projection::PrimeProjector::<R>::new(
            leaf, binding,
        )?))
    }
}

fn rejected_source(path: &Path, scope: SourceAnchorScope) -> Result<SourceKey> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::PrimeAgent.as_str(),
        SOURCE_FORMAT,
        SCHEMA,
        1,
        "prime-agent.invalid-file",
        TypedKey::utf8(path.to_string_lossy()).map_err(invalid)?,
        scope,
    )
    .map_err(invalid)
}

fn parse_header(
    record: JsonlRecordRef<'_>,
    path: &Path,
    scope: SourceAnchorScope,
) -> Result<Option<Binding>> {
    let value: Value = serde_json::from_slice(record.bytes())
        .map_err(|_| invalid("malformed Prime Agent session header"))?;
    if value.get("type").and_then(Value::as_str) != Some("session") {
        return Err(invalid(
            "Prime Agent transcript must begin with a session header",
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(3) {
        return Err(invalid(
            "unsupported Prime Agent session version (expected 3)",
        ));
    }
    let native_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| safe_session_id(id))
        .ok_or_else(|| invalid("invalid Prime Agent session ID"))?;
    timestamp(&value).ok_or_else(|| invalid("invalid Prime Agent header timestamp"))?;
    let cwd = value
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .ok_or_else(|| invalid("missing Prime Agent session cwd"))?;
    let depth = value.get("rlmDepth").and_then(Value::as_u64);
    if value.get("rlmDepth").is_some_and(|depth| {
        !depth.is_null() && depth.as_u64().is_none_or(|depth| depth == u64::MAX)
    }) || value
        .get("parentSession")
        .is_some_and(|parent| !parent.is_null() && !parent.is_string())
    {
        return Err(invalid("invalid Prime Agent lineage metadata"));
    }
    let header_parent = value
        .get("parentSession")
        .and_then(Value::as_str)
        .and_then(parent_native_id);
    let layout_parent = rlm_parent(path);
    let delegated = layout_parent
        .as_deref()
        .filter(|_| depth.is_some_and(|depth| depth > 0));
    // A fork of a child can live in the same artifact directory. Its explicit
    // parent differs from that directory's owner and remains a fork.
    let parent = header_parent
        .as_deref()
        .or(delegated)
        .filter(|id| *id != native_id);
    let relationship = parent.map(|parent| {
        if delegated == Some(parent) {
            ProviderNativeSessionRelationship::Delegated
        } else {
            ProviderNativeSessionRelationship::Forked
        }
    });
    Ok(Some(Binding {
        native_id: native_id.to_owned(),
        cwd: cwd.to_owned(),
        parent: parent
            .map(|parent| session_identity(parent, scope))
            .transpose()?,
        relationship,
        agent_scope: depth.map(|depth| {
            if depth == 0 {
                AgentScope::Primary
            } else {
                AgentScope::Subagent
            }
        }),
    }))
}

fn safe_session_id(id: &str) -> bool {
    !id.trim().is_empty()
        && !matches!(id, "." | "..")
        && !id.contains(['/', '\\', ':', '\0'])
        && id.len() <= 65536
}

fn parent_native_id(value: &str) -> Option<String> {
    let id = if value.contains(['/', '\\']) {
        value.rsplit(['/', '\\']).next()?.strip_suffix(".jsonl")?
    } else {
        value.strip_suffix(".jsonl").unwrap_or(value)
    };
    safe_session_id(id).then(|| id.to_owned())
}

fn rlm_parent(path: &Path) -> Option<String> {
    let child = path.parent()?;
    if !child.file_name()?.to_str()?.starts_with("sub-") {
        return None;
    }
    let parent = child.parent()?;
    if parent.parent()?.file_name()? != "session-artifacts" {
        return None;
    }
    let id = parent.file_name()?.to_str()?;
    safe_session_id(id).then(|| id.to_owned())
}

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.get("timestamp")?.as_str()?)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn source_key(id: &str, scope: SourceAnchorScope) -> Result<SourceKey> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::PrimeAgent.as_str(),
        SOURCE_FORMAT,
        SCHEMA,
        1,
        "prime-agent.session",
        TypedKey::utf8(id).map_err(invalid)?,
        scope,
    )
    .map_err(invalid)
}

fn session_identity(id: &str, scope: SourceAnchorScope) -> Result<StableEntityId> {
    session_id(&source_key(id, scope)?, id)
}

fn session_id(source: &SourceKey, id: &str) -> Result<StableEntityId> {
    derive_native_session_id(
        source,
        "prime-agent-session",
        "prime-agent.session",
        TypedKey::utf8(id).map_err(invalid)?,
    )
    .map_err(invalid)
}

fn invalid(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
