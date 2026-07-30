mod codex;
mod codex_active_contracts;
mod inventory;
mod registry;

use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::{
    derive_event_id, derive_session_id, BatchHydrationRequest, BatchHydrationResult,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ScannedSourceCounts, SessionHydrationRequest, SessionIdentityInput,
    SourceAnchor, SourceInventoryObservation, SourceObservation, SourceRecordLocator, TypedKey,
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

fn codex_rollout_bytes(native_session_id: &str, messages: &[&str]) -> Vec<u8> {
    let mut records = vec![serde_json::json!({
        "timestamp": "2026-07-29T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": native_session_id,
            "timestamp": "2026-07-29T12:00:00Z",
            "cwd": "/tmp/explicit-codex-source",
            "originator": "codex_cli_rs",
            "cli_version": "0.1.0",
            "source": "cli",
            "model_provider": "openai"
        }
    })];
    records.extend(messages.iter().enumerate().map(|(index, message)| {
        serde_json::json!({
            "timestamp": format!("2026-07-29T12:00:{:02}Z", index.saturating_add(1)),
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": if index == 0 { "user" } else { "assistant" },
                "content": [{
                    "type": "input_text",
                    "text": message
                }]
            }
        })
    }));
    let mut bytes = records
        .into_iter()
        .flat_map(|record| {
            let mut line = serde_json::to_vec(&record).unwrap();
            line.push(b'\n');
            line
        })
        .collect::<Vec<_>>();
    bytes.shrink_to_fit();
    bytes
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

fn fixture_event_request(source: &SourceKey, native_event_id: &str) -> EventHydrationRequest {
    let session_id = fixture_session_id(source);
    let item_key =
        NativeItemKey::native_id("message", TypedKey::utf8(native_event_id).unwrap()).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderNative {
            namespace: "ordered-batch-test".to_owned(),
            coordinate: TypedKey::utf8(native_event_id).unwrap(),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        [41; 32],
    )
    .unwrap();
    EventHydrationRequest::new(event_id, locator).unwrap()
}

fn fixture_hydrated_record(request: &EventHydrationRequest) -> HydratedProviderRecord {
    HydratedProviderRecord {
        event_id: request.event_id(),
        provider_bytes: request.event_id().as_uuid().as_bytes().to_vec(),
    }
}

fn fixture_batch_result(request: &BatchHydrationRequest) -> BatchHydrationResult {
    BatchHydrationResult::new(
        request
            .events()
            .iter()
            .map(fixture_hydrated_record)
            .collect(),
    )
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

fn fixture_batch_resolver(
    source: &SourceKey,
    hydrate_batch: impl Fn(&BatchHydrationRequest) -> Result<BatchHydrationResult, HydrationFailure>
        + Send
        + Sync
        + 'static,
) -> SourceBackedResolverRegistry {
    let owned_source = source.clone();
    let driver = SourceBackedRouteDriver::new(
        |_sink| Ok(()),
        move |candidate| owned_source.exact_descriptor_eq(candidate),
        |_target| false,
        |request| Ok(fixture_hydrated_record(request)),
    )
    .with_batch_hydration(hydrate_batch);
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(fixture_executable_route(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        driver,
    ));
    registry.resolver_registry()
}

fn fixture_route(
    provider: CaptureProvider,
    source_format: &'static str,
    lineage: u8,
    coordinate: NativeRecordCoordinate,
    provider_bytes: Vec<u8>,
) -> (SourceBackedRoute, EventHydrationRequest) {
    fixture_route_with_selected_format(
        provider,
        source_format,
        source_format,
        lineage,
        coordinate,
        provider_bytes,
    )
}

fn fixture_route_with_selected_format(
    provider: CaptureProvider,
    selected_source_format: &'static str,
    certified_source_format: &'static str,
    lineage: u8,
    coordinate: NativeRecordCoordinate,
    provider_bytes: Vec<u8>,
) -> (SourceBackedRoute, EventHydrationRequest) {
    let source = SourceKey::derive(
        provider.as_str(),
        certified_source_format,
        "coordinator-test-v1",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap();
    let session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "session",
        native_session_key: &session_key,
    })
    .unwrap();
    let item_key = NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let revision_digest = [lineage.saturating_add(10); 32];
    let record_digest = [lineage.saturating_add(20); 32];
    let locator = SourceRecordLocator::new(
        source.clone(),
        coordinate,
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
        record_digest,
    )
    .unwrap();
    let request = EventHydrationRequest::new(event_id, locator.clone()).unwrap();
    let document = LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some("session".to_owned()),
        branch: None,
        source_path: Some(format!("/fixture/{}", provider.as_str())),
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: 1,
        occurred_at_unix_ms: Some(1),
        event_type: "message".to_owned(),
        role: Some("user".to_owned()),
        body: format!("{} preview", provider.as_str()),
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    };
    let observation =
        SourceObservation::new(source.clone(), "fixture-revision", vec![lineage]).unwrap();
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        "coordinator-test-v1",
        revision_digest,
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 1,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap();
    let scan_certificate = certificate.clone();
    let scan_document = document.clone();
    let owned_source = source.clone();
    let revalidation_certificate = certificate;
    let hydrated_bytes = provider_bytes;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            sink.replace_source(scan_certificate.clone(), [scan_document.clone()])
                .map_err(route_coordinator_error)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(source) => source == &revalidation_certificate,
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        move |request| {
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated_bytes.clone(),
            })
        },
    );
    let provider_source = fixture_provider_source(
        provider,
        selected_source_format,
        ProviderImportSupport::Native,
    );
    (
        SourceBackedRoute::automatic(
            provider_source,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )
        .unwrap(),
        request,
    )
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
