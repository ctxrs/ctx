use super::*;

mod codex;
mod inventories;
mod native;
mod native_more;
mod selected;

pub use codex::*;
pub use inventories::*;
use native::*;
use native_more::*;
pub use selected::*;

/// Registers the landed Gemini adapter without moving any provider parsing
/// logic into the coordinator.
pub fn register_gemini_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    crate::provider::providers::gemini::nativepath::register_source_backed_route(
        registry, source, selection,
    )
}

/// Registers Cursor's sink-based adapter. Documents and its
/// `CertifiedSource` terminal are staged directly in the shared generation.
pub fn register_cursor_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let scan_root = root.clone();
    let revalidation_root = root.clone();
    let hydration_root = root;
    let driver = SourceBackedRouteDriver::new(
        move |sink| scan_cursor_route(&scan_root, sink),
        |source| {
            source.provider() == CaptureProvider::Cursor.as_str()
                && source.source_format() == CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
        },
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                revalidate_cursor_source(&revalidation_root, expected)
            }
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        move |request| hydrate_cursor_route(&hydration_root, request),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

/// Mechanical registration entry for landed routes that require no additional
/// selector token beyond their selected path. Providers with compound
/// selectors have dedicated constructors so those selectors cannot be
/// fabricated here.
pub fn register_landed_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    match source.provider {
        CaptureProvider::Codex if source.source_format == "codex_history_jsonl" => {
            register_codex_prompt_history_source_backed_route(registry, source, selection)
        }
        CaptureProvider::Codex if source.source_format == "codex_session_jsonl_tree" => {
            register_codex_session_tree_route(registry, source, selection)
        }
        CaptureProvider::Codex if source.source_format == "codex_session_jsonl" => {
            register_codex_explicit_session_route(registry, source, selection)
        }
        CaptureProvider::Codex => Err(invalid_route(
            source.provider,
            "unknown Codex source format",
        )),
        CaptureProvider::Zed => register_zed_route(registry, source, selection),
        CaptureProvider::Gemini => register_gemini_source_backed_route(registry, source, selection),
        CaptureProvider::Cursor => register_cursor_source_backed_route(registry, source, selection),
        CaptureProvider::Antigravity
        | CaptureProvider::Tabnine
        | CaptureProvider::Windsurf
        | CaptureProvider::CopilotCli
        | CaptureProvider::FactoryAiDroid
        | CaptureProvider::QwenCode
        | CaptureProvider::Qoder => {
            crate::provider::providers::native_jsonl::native_path::register_source_backed_route(
                registry, source, selection,
            )
        }
        CaptureProvider::CodeBuddy => {
            crate::provider::providers::codebuddy::native_path::register_source_backed_route(
                registry, source, selection,
            )
        }
        CaptureProvider::Claude => {
            crate::provider::providers::claude::nativepath::register_source_backed_route(
                registry, source, selection,
            )
        }
        CaptureProvider::KiroCli => {
            crate::provider::providers::kiro::native_path::register_source_backed_route(
                registry, source, selection,
            )
        }
        CaptureProvider::Auggie => {
            crate::provider::providers::auggie::native_path::register_source_backed_route(
                registry, source, selection,
            )
        }
        CaptureProvider::Pi => register_pi_route(registry, source, selection),
        CaptureProvider::Junie => register_junie_route(registry, source, selection),
        CaptureProvider::KimiCodeCli => register_kimi_route(registry, source, selection),
        CaptureProvider::Firebender => register_firebender_route(registry, source, selection),
        CaptureProvider::DeepAgents => register_deepagents_route(registry, source, selection),
        CaptureProvider::ForgeCode => {
            register_forgecode_selected_route(registry, source, selection)
        }
        CaptureProvider::MistralVibe => register_mistral_route(registry, source, selection),
        CaptureProvider::OpenCode | CaptureProvider::Kilo | CaptureProvider::MiMoCode => {
            register_opencode_family_route(registry, source, selection)
        }
        CaptureProvider::OpenHands => register_openhands_route(registry, source, selection),
        CaptureProvider::Cline | CaptureProvider::RooCode => {
            register_task_json_route(registry, source, selection)
        }
        CaptureProvider::Hermes => register_hermes_route(registry, source, selection),
        CaptureProvider::RovoDev => register_rovodev_route(registry, source, selection),
        CaptureProvider::Trae => register_trae_route(registry, source, selection),
        CaptureProvider::OpenClaw => register_openclaw_route(registry, source, selection),
        CaptureProvider::Continue => register_continue_route(registry, source, selection),
        CaptureProvider::Mux => register_mux_route(registry, source, selection),
        provider => Err(invalid_route(
            provider,
            "this provider requires its compound-selector registration constructor",
        )),
    }
}

pub(crate) fn provider_format_scope(
    provider: CaptureProvider,
    source_format: &'static str,
) -> impl Fn(&SourceKey) -> bool + Send + Sync + 'static {
    move |source| source.provider() == provider.as_str() && source.source_format() == source_format
}

pub(crate) fn executable_route(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    authority: SourceBackedSelectorAuthority,
    driver: SourceBackedRouteDriver,
) -> SourceBackedCoordinatorResult<SourceBackedRoute> {
    match selection {
        SourceBackedRouteSelection::Automatic => {
            SourceBackedRoute::automatic(source, authority, driver)
        }
        SourceBackedRouteSelection::ExplicitManual => SourceBackedRoute::explicit_manual(
            source,
            if authority == SourceBackedSelectorAuthority::DiscoveredWinner {
                SourceBackedSelectorAuthority::ExplicitPath
            } else {
                authority
            },
            driver,
        ),
    }
}

pub(in crate::provider::source_backed) fn validate_executable_route(
    source: &ProviderSource,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
) -> SourceBackedCoordinatorResult<&'static SourceBackedProviderRouteMetadata> {
    let known = landed_format_route(source.provider, source.source_format);
    let Some(known) = known else {
        return Err(invalid_route(
            source.provider,
            format!(
                "source format {:?} has no landed route",
                source.source_format
            ),
        ));
    };
    let selected_mode_supported = match selection {
        SourceBackedRouteSelection::Automatic => known.automatic,
        SourceBackedRouteSelection::ExplicitManual => known.explicit_manual,
    };
    if !selected_mode_supported
        || known.unsupported_reason.is_some()
        || !source.import_support.is_importable()
        || source.source_kind == ProviderSourceKind::DetectionOnly
        || source.status == ProviderSourceStatus::Unsupported
        || source.unsupported_reason.is_some()
    {
        return Err(invalid_route(
            source.provider,
            source
                .unsupported_reason
                .or(known.unsupported_reason)
                .unwrap_or("the selected automatic/manual mode is unsupported"),
        ));
    }
    if selection == SourceBackedRouteSelection::Automatic
        && source.import_support != ProviderImportSupport::Native
    {
        return Err(invalid_route(
            source.provider,
            "an explicit-only provider source cannot be registered automatically",
        ));
    }
    if selector_authority != known.selector_authority
        && !matches!(
            (selection, selector_authority),
            (
                SourceBackedRouteSelection::ExplicitManual,
                SourceBackedSelectorAuthority::ExplicitPath
            )
        )
    {
        return Err(invalid_route(
            source.provider,
            "the route omitted or changed its provider selector authority",
        ));
    }
    Ok(known)
}

pub(in crate::provider::source_backed) fn landed_format_route(
    provider: CaptureProvider,
    selected_source_format: &str,
) -> Option<&'static SourceBackedProviderRouteMetadata> {
    LANDED_SOURCE_BACKED_ROUTES
        .iter()
        .find(|route| route.provider == provider && route.source_format == selected_source_format)
}

pub(crate) fn invalid_route(
    provider: CaptureProvider,
    detail: impl Into<String>,
) -> SourceBackedCoordinatorError {
    SourceBackedCoordinatorError::InvalidRoute {
        provider,
        detail: detail.into(),
    }
}

struct CursorGenerationBridge<'sink, 'writer> {
    sink: &'sink mut SourceBackedGenerationSink<'writer>,
    active: Option<SourceKey>,
}

impl CursorSourceBackedSink for CursorGenerationBridge<'_, '_> {
    fn begin_cursor_source(&mut self, plan: &CursorSourceBackedSourcePlan) -> CaptureResult<()> {
        self.sink
            .begin_source(plan.source.clone())
            .map_err(capture_coordinator_error)?;
        self.active = Some(plan.source.clone());
        Ok(())
    }

    fn stage_cursor_source_page(&mut self, page: CursorSourceBackedPage) -> CaptureResult<()> {
        for record in page.records {
            if let Some(document) = record.lexical_document() {
                self.sink
                    .add_document(document)
                    .map_err(capture_coordinator_error)?;
            }
        }
        Ok(())
    }

    fn finish_cursor_source(&mut self, terminal: CursorSourceBackedTerminal) -> CaptureResult<()> {
        let active = self.active.take().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Cursor source-backed terminal arrived without an active source".to_owned(),
            )
        })?;
        if !active.exact_descriptor_eq(terminal.certified_source.observation().source()) {
            return Err(CaptureError::InvalidPayload(
                "Cursor source-backed terminal changed its active source".to_owned(),
            ));
        }
        self.sink
            .certify_source(terminal.certified_source)
            .map_err(capture_coordinator_error)
    }

    fn abort_cursor_source(&mut self) {
        self.active = None;
    }
}

fn scan_cursor_route(
    root: &Path,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<()> {
    let mut bridge = CursorGenerationBridge { sink, active: None };
    extract_cursor_source_backed_cold(root, &mut bridge).map_err(route_capture_error)?;
    if bridge.active.is_some() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "Cursor extraction ended with an uncertified active source",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct CursorEvidenceSink {
    certificates: Vec<CertifiedSource>,
}

impl CursorSourceBackedSink for CursorEvidenceSink {
    fn begin_cursor_source(&mut self, _plan: &CursorSourceBackedSourcePlan) -> CaptureResult<()> {
        Ok(())
    }

    fn stage_cursor_source_page(&mut self, _page: CursorSourceBackedPage) -> CaptureResult<()> {
        Ok(())
    }

    fn finish_cursor_source(&mut self, terminal: CursorSourceBackedTerminal) -> CaptureResult<()> {
        self.certificates.push(terminal.certified_source);
        Ok(())
    }

    fn abort_cursor_source(&mut self) {}
}

fn revalidate_cursor_source(root: &Path, expected: &CertifiedSource) -> bool {
    let mut sink = CursorEvidenceSink::default();
    if extract_cursor_source_backed_cold(root, &mut sink).is_err() {
        return false;
    }
    sink.certificates.into_iter().any(|certificate| {
        certificate
            .observation()
            .source()
            .exact_descriptor_eq(expected.observation().source())
            && certificate == *expected
    })
}

struct CursorHydrationSink<'request> {
    request: &'request EventHydrationRequest,
    record: Option<CursorSourceBackedRecord>,
}

impl CursorSourceBackedSink for CursorHydrationSink<'_> {
    fn begin_cursor_source(&mut self, _plan: &CursorSourceBackedSourcePlan) -> CaptureResult<()> {
        Ok(())
    }

    fn stage_cursor_source_page(&mut self, page: CursorSourceBackedPage) -> CaptureResult<()> {
        for record in page.records {
            if record.event_id == self.request.event_id()
                && record.locator == *self.request.locator()
                && self.record.replace(record).is_some()
            {
                return Err(CaptureError::InvalidPayload(
                    "Cursor exact locator resolved more than once".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn finish_cursor_source(&mut self, _terminal: CursorSourceBackedTerminal) -> CaptureResult<()> {
        Ok(())
    }

    fn abort_cursor_source(&mut self) {}
}

fn hydrate_cursor_route(
    root: &Path,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let mut sink = CursorHydrationSink {
        request,
        record: None,
    };
    extract_cursor_source_backed_cold(root, &mut sink)
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?;
    let record = sink.record.ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Cursor exact locator is absent from the selected transcript tree",
        )
    })?;
    let text = hydrate_cursor_source_backed_message(root, &record)
        .map_err(|error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error))?;
    Ok(HydratedProviderRecord {
        event_id: request.event_id(),
        provider_bytes: text.into_bytes(),
    })
}

pub(crate) fn route_capture_error(error: CaptureError) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Unavailable, error.to_string())
}

pub(crate) fn route_error(error: impl fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

pub(crate) fn route_coordinator_error(
    error: SourceBackedCoordinatorError,
) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

fn capture_coordinator_error(error: SourceBackedCoordinatorError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(in crate::provider::source_backed) fn codex_display_bytes(
    hydrated: CodexHydratedRecordV0,
) -> Result<Vec<u8>, HydrationFailure> {
    hydrated
        .decoded_display_text
        .map(String::into_bytes)
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Codex record has no exact decoded display text",
            )
        })
}

pub(in crate::provider::source_backed) fn firebender_display_bytes(
    messages_json: &[u8],
    message_index: u64,
) -> Result<Vec<u8>, HydrationFailure> {
    let messages = serde_json::from_slice::<Vec<serde_json::Value>>(messages_json)
        .map_err(|error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error))?;
    let index = usize::try_from(message_index).map_err(|_| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Firebender message index exceeds platform limits",
        )
    })?;
    let message = messages.get(index).ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Firebender message is absent from its verified source row",
        )
    })?;
    firebender_message_text(message)
        .map(String::into_bytes)
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Firebender message has no exact decoded display text",
            )
        })
}

pub(crate) fn hydration_failure(
    kind: HydrationFailureKind,
    detail: impl fmt::Display,
) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.to_string(),
    }
}
