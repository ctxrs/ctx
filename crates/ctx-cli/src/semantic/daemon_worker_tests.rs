use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use ctx_history_capture::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, SourceBackedProviderRegistry, SourceBackedRoute, SourceBackedRouteDriver,
    SourceBackedSelectorAuthority,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, HydratedProviderRecord,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator, StableEntityId, TypedKey,
};

use super::*;

const GEMINI_SOURCE_FORMAT: &str = "gemini_cli_chat_recording_jsonl";

#[derive(Clone)]
struct Fixture {
    source: SourceKey,
    session_id: StableEntityId,
}

impl Fixture {
    fn gemini() -> Self {
        let source = SourceKey::derive(
            CaptureProvider::Gemini.as_str(),
            GEMINI_SOURCE_FORMAT,
            "session",
            1,
            SourceAnchor::CatalogLineage([41; 32]),
        )
        .unwrap();
        let native_session_key =
            NativeSessionKey::native_id("session", TypedKey::utf8("gemini-session").unwrap())
                .unwrap();
        let session_id = derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &native_session_key,
        })
        .unwrap();
        Self { source, session_id }
    }

    fn event(&self, sequence: u64, role: EventRole) -> EventRecord {
        let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        let locator = SourceRecordLocator::new(
            self.source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: sequence.saturating_mul(100),
                byte_length: 80,
                physical_ordinal: sequence,
                native_session_key: Some(TypedKey::utf8("gemini-session").unwrap()),
                native_event_key: Some(TypedKey::U64(sequence)),
            },
            LocatorRevisionPolicy::ExactSourceRevision,
            Some([17; 32]),
            [sequence as u8; 32],
        )
        .unwrap();
        EventRecord {
            event_id,
            session_id: self.session_id,
            parent_session_id: None,
            root_session_id: self.session_id,
            locator,
            provider: CaptureProvider::Gemini.as_str().to_owned(),
            source_format: GEMINI_SOURCE_FORMAT.to_owned(),
            provider_session_id: Some("gemini-session".to_owned()),
            branch: None,
            source_path: Some("/provider/gemini/session.json".to_owned()),
            agent_type: AgentType::Primary.as_str().to_owned(),
            is_primary: true,
            event_sequence: sequence,
            occurred_at_unix_ms: Some(sequence as i64),
            event_type: EventType::Message.as_str().to_owned(),
            role: Some(role.as_str().to_owned()),
            workspace: Some("/workspace".to_owned()),
            cwd: Some("/workspace".to_owned()),
            touched_files: Vec::new(),
        }
    }
}

struct FixtureSessionReader {
    session_id: StableEntityId,
    events: Vec<EventRecord>,
}

impl SourceSemanticSessionReader for FixtureSessionReader {
    fn events_for_semantic_session(
        &self,
        anchor: &EventRecord,
    ) -> std::result::Result<Vec<EventRecord>, HydrationFailure> {
        assert_eq!(anchor.session_id, self.session_id);
        Ok(self.events.clone())
    }
}

fn fixture_registry(
    source: &SourceKey,
    responses: HashMap<StableEntityId, std::result::Result<Vec<u8>, HydrationFailure>>,
    calls: Arc<Mutex<Vec<StableEntityId>>>,
) -> SourceBackedResolverRegistry {
    let owned_source = source.clone();
    let driver = SourceBackedRouteDriver::new(
        |_| Ok(()),
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        |_| true,
        move |request| {
            calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(request.event_id());
            responses
                .get(&request.event_id())
                .cloned()
                .unwrap_or_else(|| {
                    Err(HydrationFailure {
                        kind: HydrationFailureKind::MissingRecord,
                        detail: "fixture source record is absent".to_owned(),
                    })
                })
                .map(|provider_bytes| HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes,
                })
        },
    );
    let route = SourceBackedRoute::automatic(
        ProviderSource {
            provider: CaptureProvider::Gemini,
            path: PathBuf::from("/provider/gemini"),
            exists: true,
            source_format: GEMINI_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )
    .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    registry.resolver_registry()
}

#[test]
fn non_codex_registry_hydration_builds_provider_native_lite_turn() {
    let fixture = Fixture::gemini();
    let user = fixture.event(1, EventRole::User);
    let early_assistant = fixture.event(2, EventRole::Assistant);
    let final_assistant = fixture.event(3, EventRole::Assistant);
    let next_user = fixture.event(4, EventRole::User);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sources = fixture_registry(
        &fixture.source,
        HashMap::from([
            (
                user.event_id,
                Ok(br#"{"type":"user","message":"exact Gemini question"}"#.to_vec()),
            ),
            (
                final_assistant.event_id,
                Ok(br#"{"type":"assistant","message":"exact Gemini answer"}"#.to_vec()),
            ),
        ]),
        Arc::clone(&calls),
    );
    let index = FixtureSessionReader {
        session_id: fixture.session_id,
        events: vec![
            user.clone(),
            early_assistant,
            final_assistant.clone(),
            next_user,
        ],
    };
    let request = EventHydrationRequest::new(user.event_id, user.locator.clone()).unwrap();
    let mut resolver = ProviderSourceSemanticResolver {
        index: &index,
        sources,
    };

    let document = resolver.resolve_document(&user, &request).unwrap();

    assert_eq!(document.event_id, user.event_id.as_uuid());
    assert_eq!(document.provider, Some(CaptureProvider::Gemini));
    assert_eq!(document.role, Some(EventRole::User));
    assert_eq!(document.rank_bucket, "lite_turn");
    assert_eq!(document.occurred_at_ms, 3);
    assert_eq!(
        document.text,
        concat!(
            "user:\n{\"type\":\"user\",\"message\":\"exact Gemini question\"}",
            "\n\n",
            "assistant:\n",
            "{\"type\":\"assistant\",\"message\":\"exact Gemini answer\"}"
        )
    );
    assert_eq!(
        *calls.lock().unwrap_or_else(|error| error.into_inner()),
        vec![user.event_id, final_assistant.event_id]
    );
}

#[test]
fn registry_hydration_failure_kind_and_detail_are_preserved() {
    let fixture = Fixture::gemini();
    let user = fixture.event(1, EventRole::User);
    let expected = HydrationFailure {
        kind: HydrationFailureKind::StaleSourceEvidence,
        detail: "fixture source revision changed".to_owned(),
    };
    let sources = fixture_registry(
        &fixture.source,
        HashMap::from([(user.event_id, Err(expected.clone()))]),
        Arc::new(Mutex::new(Vec::new())),
    );
    let index = FixtureSessionReader {
        session_id: fixture.session_id,
        events: vec![user.clone()],
    };
    let request = EventHydrationRequest::new(user.event_id, user.locator.clone()).unwrap();
    let mut resolver = ProviderSourceSemanticResolver {
        index: &index,
        sources,
    };

    let failure = resolver.resolve_document(&user, &request).unwrap_err();

    assert_eq!(failure, expected);
}

#[test]
fn mismatched_event_request_is_rejected_before_registry_dispatch() {
    let fixture = Fixture::gemini();
    let user = fixture.event(1, EventRole::User);
    let other = fixture.event(2, EventRole::User);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sources = fixture_registry(
        &fixture.source,
        HashMap::from([(other.event_id, Ok(b"other provider-native record".to_vec()))]),
        Arc::clone(&calls),
    );
    let index = FixtureSessionReader {
        session_id: fixture.session_id,
        events: vec![user.clone(), other.clone()],
    };
    let request = EventHydrationRequest::new(other.event_id, other.locator.clone()).unwrap();
    let mut resolver = ProviderSourceSemanticResolver {
        index: &index,
        sources,
    };

    let failure = resolver.resolve_document(&user, &request).unwrap_err();

    assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);
    assert!(calls
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_empty());
}
