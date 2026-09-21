use serde::{Deserialize, Serialize};

use ctx_attribution_model::ResourceKind;
use ctx_history_core::{SourceKey, StableEntityId};

use super::{
    MAX_CORE_IDENTITY_BYTES, MAX_FACT_FAMILY_BYTES, MAX_IDENTIFIER_BYTES, ServingModelError,
    validate_repository_id, validate_text, validate_token,
};

const STABLE_ENTITY_PREFIX: &str = "_ctx.core_stable.v1:";
const SOURCE_RESOURCE_PREFIX: &str = "_ctx.core_source_resource.v1:";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServingResource {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
}

impl ServingResource {
    pub fn from_core(
        kind: ResourceKind,
        display: impl Into<String>,
        repository_id: Option<String>,
        worktree_id: Option<String>,
        stable_entity: Option<StableEntityId>,
        source: &SourceKey,
    ) -> Result<Self, ServingModelError> {
        let display = display.into();
        let id = if let Some(stable_entity) = stable_entity.as_ref() {
            encode_stable_entity(stable_entity)?
        } else if repository_id.is_none() {
            encode_source_resource(&CoreSourceResource {
                source: source.clone(),
                display,
            })?
        } else {
            display
        };
        let resource = Self {
            kind: kind.wire_name().to_owned(),
            id,
            repository_id,
            worktree_id,
        };
        resource.validate_projected()?;
        Ok(resource)
    }

    pub fn typed_kind(&self) -> Result<ResourceKind, ServingModelError> {
        ResourceKind::from_wire_name(&self.kind).ok_or(ServingModelError::InvalidResourceIdentity)
    }

    pub fn display(&self) -> Result<String, ServingModelError> {
        if let Some(identity) = decode_stable_entity(&self.id) {
            return identity.map(|identity| identity.to_string());
        }
        if let Some(resource) = decode_source_resource(&self.id) {
            return resource.map(|resource| resource.display);
        }
        Ok(self.id.clone())
    }

    /// Returns the exact resource ID produced by `graph/store/resources.rs`.
    pub fn graph_id(&self) -> Result<String, ServingModelError> {
        let kind = self.typed_kind()?;
        if let Some(identity) = decode_stable_entity(&self.id) {
            let identity = identity?;
            let namespace_id = crate::stable_id(
                "core_stable_namespace",
                &hex::encode(identity.source_digest()),
            );
            return Ok(crate::stable_id(
                "core_stable_resource",
                &format!(
                    "{namespace_id}\u{1f}{}\u{1f}{}",
                    kind.wire_name(),
                    hex::encode(identity.digest())
                ),
            ));
        }
        if let Some(resource) = decode_source_resource(&self.id) {
            let resource = resource?;
            let namespace_id = crate::stable_id(
                "core_source_namespace",
                &hex::encode(resource.source.exact_descriptor_digest()),
            );
            return Ok(crate::stable_id(
                "resource",
                &format!(
                    "{namespace_id}\u{1f}{}\u{1f}{}",
                    kind.wire_name(),
                    resource.display
                ),
            ));
        }
        let repository_id = self
            .repository_id
            .as_deref()
            .ok_or(ServingModelError::InvalidResourceIdentity)?;
        let namespace_identity = self.worktree_id.as_ref().map_or_else(
            || repository_id.to_owned(),
            |worktree_id| format!("{repository_id}\u{1f}{worktree_id}"),
        );
        let namespace_id = crate::stable_id("repository_namespace", &namespace_identity);
        Ok(crate::stable_id(
            "resource",
            &format!(
                "{namespace_id}\u{1f}{}\u{1f}{}",
                kind.wire_name(),
                self.display()?
            ),
        ))
    }

    pub fn logical_repository_graph_id(&self) -> Result<Option<String>, ServingModelError> {
        let kind = self.typed_kind()?;
        if kind == ResourceKind::Repository {
            return Ok(None);
        }
        if matches!(kind, ResourceKind::Session | ResourceKind::Run)
            && decode_stable_entity(&self.id).transpose()?.is_some()
        {
            return Ok(None);
        }
        self.repository_id
            .as_deref()
            .map(logical_repository_graph_id)
            .transpose()
    }

    pub fn validate(&self, repository_id: &str) -> Result<(), ServingModelError> {
        let kind = self.typed_kind()?;
        validate_token("resource kind", &self.kind, MAX_FACT_FAMILY_BYTES)?;
        // Public Core repository resources can carry the complete opaque
        // logical identity. Flat stores it in the record and indexes only its
        // bounded derived graph ID.
        if kind == ResourceKind::Repository && self.id == repository_id {
            validate_repository_id(&self.id)?;
        } else {
            validate_text("resource id", &self.id, MAX_CORE_IDENTITY_BYTES)?;
        }
        if let Some(resource_repository_id) = &self.repository_id {
            validate_repository_id(resource_repository_id)?;
            if resource_repository_id != repository_id {
                return Err(ServingModelError::RepositoryScopeMismatch);
            }
        }
        if let Some(worktree_id) = &self.worktree_id {
            validate_text("worktree id", worktree_id, MAX_IDENTIFIER_BYTES)?;
        }
        Ok(())
    }

    pub fn validate_projected(&self) -> Result<(), ServingModelError> {
        let kind = self.typed_kind()?;
        if kind == ResourceKind::Repository
            && self.repository_id.as_deref() == Some(self.id.as_str())
        {
            validate_repository_id(&self.id)?;
        } else {
            validate_text("resource id", &self.id, MAX_CORE_IDENTITY_BYTES)?;
        }
        if let Some(repository_id) = &self.repository_id {
            validate_repository_id(repository_id)?;
        }
        if let Some(worktree_id) = &self.worktree_id {
            validate_text("worktree id", worktree_id, MAX_IDENTIFIER_BYTES)?;
        }
        let _ = self.graph_id()?;
        let _ = self.display()?;
        let _ = self.logical_repository_graph_id()?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreSourceResource {
    source: SourceKey,
    display: String,
}

pub fn encode_stable_entity(identity: &StableEntityId) -> Result<String, ServingModelError> {
    let encoded =
        serde_json::to_string(identity).map_err(|_| ServingModelError::InvalidResourceIdentity)?;
    let value = format!("{STABLE_ENTITY_PREFIX}{encoded}");
    validate_text("stable resource identity", &value, MAX_CORE_IDENTITY_BYTES)?;
    Ok(value)
}

fn decode_stable_entity(value: &str) -> Option<Result<StableEntityId, ServingModelError>> {
    value.strip_prefix(STABLE_ENTITY_PREFIX).map(|encoded| {
        serde_json::from_str::<StableEntityId>(encoded)
            .map_err(|_| ServingModelError::InvalidResourceIdentity)
            .and_then(|identity| {
                identity
                    .validate_contract()
                    .map_err(|_| ServingModelError::InvalidResourceIdentity)?;
                Ok(identity)
            })
    })
}

pub fn decode_stable_entity_required(value: &str) -> Result<StableEntityId, ServingModelError> {
    decode_stable_entity(value)
        .ok_or(ServingModelError::InvalidCoreCitation)?
        .map_err(|_| ServingModelError::InvalidCoreCitation)
}

fn encode_source_resource(value: &CoreSourceResource) -> Result<String, ServingModelError> {
    value
        .source
        .validate_contract()
        .map_err(|_| ServingModelError::InvalidResourceIdentity)?;
    validate_text(
        "source resource display",
        &value.display,
        MAX_IDENTIFIER_BYTES,
    )?;
    let encoded =
        serde_json::to_string(value).map_err(|_| ServingModelError::InvalidResourceIdentity)?;
    let encoded = format!("{SOURCE_RESOURCE_PREFIX}{encoded}");
    validate_text(
        "source resource identity",
        &encoded,
        MAX_CORE_IDENTITY_BYTES,
    )?;
    Ok(encoded)
}

fn decode_source_resource(value: &str) -> Option<Result<CoreSourceResource, ServingModelError>> {
    value.strip_prefix(SOURCE_RESOURCE_PREFIX).map(|encoded| {
        serde_json::from_str::<CoreSourceResource>(encoded)
            .map_err(|_| ServingModelError::InvalidResourceIdentity)
            .and_then(|resource| {
                resource
                    .source
                    .validate_contract()
                    .map_err(|_| ServingModelError::InvalidResourceIdentity)?;
                validate_text(
                    "source resource display",
                    &resource.display,
                    MAX_IDENTIFIER_BYTES,
                )?;
                Ok(resource)
            })
    })
}

pub fn logical_repository_graph_id(value: &str) -> Result<String, ServingModelError> {
    validate_repository_id(value)?;
    let namespace_id = crate::stable_id("repository_namespace", value);
    Ok(crate::stable_id(
        "resource",
        &format!(
            "{namespace_id}\u{1f}{}\u{1f}{value}",
            ResourceKind::Repository.wire_name()
        ),
    ))
}
