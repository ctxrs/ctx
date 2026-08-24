use ctx_history_capture_model::{
    ProviderRootDefinition, ProviderRootSourceIdentity, SourceRouteIdentity,
    MAX_PROVIDER_ROOT_SELECTOR_BYTES,
};
use ctx_history_core::CaptureProvider;
use serde::{Deserialize, Serialize};

use super::{IndexError, Result};

/// Generation-authoritative expansion of one configured provider home.
///
/// Search resolves the human-facing id and group to exact physical route
/// identities from the same pinned generation. Group and path remain aliases;
/// the stable root id namespaces independently named homes so filesystem moves
/// do not rotate source, session, or event identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedProviderRoot {
    pub(super) definition: ProviderRootDefinition,
    source_identity: ProviderRootSourceIdentity,
    pub(super) routes: Vec<SourceRouteIdentity>,
}

impl AppliedProviderRoot {
    pub fn new(
        definition: ProviderRootDefinition,
        routes: Vec<SourceRouteIdentity>,
    ) -> Result<Self> {
        Self::with_source_identity(definition, ProviderRootSourceIdentity::NamedV1, routes)
    }

    pub fn with_source_identity(
        definition: ProviderRootDefinition,
        source_identity: ProviderRootSourceIdentity,
        mut routes: Vec<SourceRouteIdentity>,
    ) -> Result<Self> {
        routes.sort();
        let root = Self {
            definition,
            source_identity,
            routes,
        };
        root.validate_contract()?;
        Ok(root)
    }

    pub fn definition(&self) -> &ProviderRootDefinition {
        &self.definition
    }

    pub fn source_identity(&self) -> ProviderRootSourceIdentity {
        self.source_identity
    }

    pub fn routes(&self) -> &[SourceRouteIdentity] {
        &self.routes
    }

    pub(super) fn validate_contract(&self) -> Result<()> {
        validate_provider_root_definition(&self.definition)?;
        if self.routes.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(IndexError::InvalidProviderRoots(format!(
                "root {} routes are not strictly sorted and unique",
                self.definition.id
            )));
        }
        for route in &self.routes {
            route.validate().map_err(IndexError::from)?;
        }
        Ok(())
    }
}

fn validate_provider_root_definition(root: &ProviderRootDefinition) -> Result<()> {
    let valid_selector = |value: &str| {
        !value.is_empty()
            && value.len() <= MAX_PROVIDER_ROOT_SELECTOR_BYTES
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    };
    if !valid_selector(&root.id) {
        return Err(IndexError::InvalidProviderRoots(format!(
            "root id {:?} is invalid",
            root.id
        )));
    }
    if root
        .group
        .as_deref()
        .is_some_and(|group| !valid_selector(group))
    {
        return Err(IndexError::InvalidProviderRoots(format!(
            "root {} has invalid group",
            root.id
        )));
    }
    if !matches!(
        root.provider,
        CaptureProvider::Claude | CaptureProvider::Codex
    ) {
        return Err(IndexError::InvalidProviderRoots(format!(
            "root {} has unsupported provider {}",
            root.id,
            root.provider.as_str()
        )));
    }
    if !root.path.is_absolute()
        || root.path.to_str().is_none()
        || root.path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(IndexError::InvalidProviderRoots(format!(
            "root {} path is not normalized absolute UTF-8",
            root.id
        )));
    }
    Ok(())
}
