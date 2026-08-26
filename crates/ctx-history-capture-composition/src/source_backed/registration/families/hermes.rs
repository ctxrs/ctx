use std::path::Path;

use ctx_history_core::{SourceAnchor, SourceAnchorScope, TypedKey};
use ctx_history_provider_hermes::registration::{
    hermes_automatic_registration_scoped, hermes_explicit_registration,
    hermes_explicit_registration_scoped, hermes_released_registration_scoped,
};

use super::*;
use crate::provider::source_backed::family::document::{
    install_hermes_registration, CaptureDocumentLifecycle, CaptureDocumentSpool,
};

pub(super) fn register_hermes_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let source_scope =
        source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage);
    if selection == SourceBackedRouteSelection::Automatic {
        let registration = hermes_automatic_registration_scoped::<
            CaptureDocumentLifecycle,
            CaptureDocumentSpool,
        >(source, selection, data_root, source_scope)
        .map_err(|error| invalid_route(provider, error.to_string()))?;
        return install_hermes_registration(registry, registration);
    }
    let registration = {
        let anchor = SourceAnchor::provider_native(
            "ctx-configured-root-hermes.v1",
            TypedKey::bytes(source_root_lineage.unwrap_or_default().to_vec())
                .map_err(|error| invalid_route(provider, error.to_string()))?,
        )
        .map_err(|error| invalid_route(provider, error.to_string()))?;
        hermes_explicit_registration_scoped::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
            source,
            data_root,
            anchor,
            source_scope,
        )
    }
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    install_hermes_registration(registry, registration)
}

pub(in crate::source_backed) fn register_hermes_released_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    data_root: &Path,
    identity_path: &Path,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let registration =
        hermes_released_registration_scoped::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
            source,
            data_root,
            identity_path,
            SourceAnchorScope::Unqualified,
        )
        .map_err(|error| invalid_route(provider, error.to_string()))?;
    install_hermes_registration(registry, registration)
}

pub fn register_hermes_explicit_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    data_root: &Path,
    anchor: SourceAnchor,
) -> SourceBackedCoordinatorResult<()> {
    let provider = source.provider;
    let registration =
        hermes_explicit_registration::<CaptureDocumentLifecycle, CaptureDocumentSpool>(
            source, data_root, anchor,
        )
        .map_err(|error| invalid_route(provider, error.to_string()))?;
    install_hermes_registration(registry, registration)
}
