mod hermes;
mod inventory;
mod registry;

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope, CoreRecord, EventIdentityInput, NativeItemKey,
    NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceInventoryObservation, SourceObservation, TypedKey,
};
use ctx_history_index::VerifiedIndex;
use tempfile::tempdir;

use super::*;
use crate::GEMINI_CLI_SOURCE_FORMAT;

fn fixture_source(
    provider: CaptureProvider,
    source_format: &'static str,
    lineage: u8,
) -> SourceKey {
    SourceKey::derive(
        provider.as_str(),
        source_format,
        "ordered-batch-test-v1",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap()
}

fn fixture_session_id(source: &SourceKey) -> ctx_history_core::StableEntityId {
    let session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "session",
        native_session_key: &session_key,
    })
    .unwrap()
}

fn fixture_executable_route(
    provider: CaptureProvider,
    selected_source_format: &'static str,
    driver: SourceBackedRouteDriver,
) -> SourceBackedRoute {
    SourceBackedRoute::automatic(
        fixture_provider_source(
            provider,
            selected_source_format,
            ProviderImportSupport::Native,
        ),
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )
    .unwrap()
}

fn fixture_route(
    provider: CaptureProvider,
    source_format: &'static str,
    lineage: u8,
) -> SourceBackedRoute {
    fixture_route_with_body(
        provider,
        source_format,
        lineage,
        format!("{} body", provider.as_str()),
    )
}

fn fixture_route_with_body(
    provider: CaptureProvider,
    source_format: &'static str,
    lineage: u8,
    body: String,
) -> SourceBackedRoute {
    fixture_route_with_body_and_rejections(provider, source_format, lineage, body, 0)
}

fn fixture_route_with_body_and_rejections(
    provider: CaptureProvider,
    source_format: &'static str,
    lineage: u8,
    body: String,
    rejected_records: u64,
) -> SourceBackedRoute {
    let source = SourceKey::derive(
        provider.as_str(),
        source_format,
        "coordinator-test-v1",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap();
    let session_id = fixture_session_id(&source);
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        1,
        "message",
        "coordinator-test-v1",
        body,
    )
    .unwrap();
    record.provider_session_id = Some("session".to_owned());
    record.native_event_id = Some(TypedKey::U64(1));
    record.occurred_at_unix_ms = Some(1);
    record.role = Some("user".to_owned());
    record.agent_scope = Some(AgentScope::Primary);
    let revision_digest = [lineage.saturating_add(10); 32];
    let observation =
        SourceObservation::new(source.clone(), "fixture-revision", vec![lineage]).unwrap();
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        "coordinator-test-v1",
        revision_digest,
        ScannedSourceCounts {
            complete_records: 1 + rejected_records,
            retained_records: 1,
            rejected_records,
            indexed_documents: 1,
            certified_bytes: 1,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap();
    let scan_certificate = certificate.clone();
    let revalidation_certificate = certificate;
    let owned_source = source;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            sink.report_completed_bytes(1)
                .map_err(route_coordinator_error)?;
            sink.replace_source(scan_certificate.clone(), [record.clone()])
                .map_err(route_coordinator_error)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(source) => source == &revalidation_certificate,
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
    );
    SourceBackedRoute::automatic(
        fixture_provider_source(provider, source_format, ProviderImportSupport::Native),
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )
    .unwrap()
}

fn fixture_provider_source(
    provider: CaptureProvider,
    source_format: &'static str,
    import_support: ProviderImportSupport,
) -> ProviderSource {
    ProviderSource {
        provider,
        path: PathBuf::from(format!("/fixture/{}", provider.as_str())),
        exists: true,
        source_format,
        source_kind: if import_support == ProviderImportSupport::Unsupported {
            ProviderSourceKind::DetectionOnly
        } else {
            ProviderSourceKind::NativeHistory
        },
        import_support,
        catalog_support: crate::ProviderCatalogSupport::None,
        status: if import_support == ProviderImportSupport::Unsupported {
            ProviderSourceStatus::Unsupported
        } else {
            ProviderSourceStatus::Available
        },
        unsupported_reason: None,
        route_provenance: Default::default(),
    }
}

fn fixture_provider_source_at(
    provider: CaptureProvider,
    source_format: &'static str,
    import_support: ProviderImportSupport,
    path: impl Into<PathBuf>,
) -> ProviderSource {
    let mut source = fixture_provider_source(provider, source_format, import_support);
    source.path = path.into();
    source
}
