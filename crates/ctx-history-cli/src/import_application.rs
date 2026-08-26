use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ctx_history_capture::{DiscoveryReport, ProviderSource};
use ctx_history_core::CaptureProvider;
use ctx_history_ingest_application::{
    CaptureAdmissionPort, HistorySourcePluginSource, IngestProgressPort, IngestPublication,
    IngestRefreshPort, IngestReport, IngestRequest, RefreshSelection, SourceDiscoveryPort,
    SourceStats,
};
use ctx_history_refresh::ExplicitSourceCatalogUpsert;
use ctx_terminal::Ui;

use crate::{
    provider_selection_guidance, CliSourceDiscoveryPort, HistoryCliConfig,
    HistoryConfigSnapshotPort, HistoryProvider, ImportRequest, ProgressMode, ProgressReporter,
};

/// Final-host authority retained outside the history application. Implementors
/// perform concrete data-root, catalog, and daemon work; this adapter only
/// sequences the validated import lifecycle.
pub trait ImportApplicationPort {
    fn protect_data_root(&mut self, data_root: &Path) -> Result<()>;

    fn explicit_source(
        &self,
        data_root: &Path,
        path: &Path,
        provider: Option<CaptureProvider>,
        custom_jsonl: bool,
    ) -> Result<ProviderSource>;

    fn prepare_plugin(
        &mut self,
        source: &HistorySourcePluginSource,
        reset_cursor: bool,
    ) -> Result<ProviderSource>;

    fn admit_exact(
        &mut self,
        data_root: &Path,
        source: &ProviderSource,
        relocate_from: Option<&Path>,
    ) -> Result<ExplicitSourceCatalogUpsert>;

    fn source_failure_identity(&self, source: &ProviderSource) -> Result<String>;

    fn refresh(
        &mut self,
        data_root: &Path,
        selection: RefreshSelection,
        no_daemon: bool,
        progress: &mut ProgressReporter<'_>,
    ) -> Result<IngestPublication>;
}

/// Runs import application orchestration after the final parser has produced a
/// neutral request. The final host resolves home once and retains concrete I/O,
/// configuration persistence, daemon wake, telemetry, and output delivery.
pub fn run_import_application<P: ImportApplicationPort, C: HistoryConfigSnapshotPort>(
    request: ImportRequest,
    data_root: &Path,
    home: Option<PathBuf>,
    config: &C,
    port: &mut P,
    ui: &mut Ui,
) -> Result<IngestReport> {
    let progress_mode = request.progress;
    let json_output = request.format == crate::OutputFormat::Json;
    let request = ingest_request(request);
    let mut host = HistoryImportHost {
        home,
        data_root: data_root.to_path_buf(),
        port,
        config: config.snapshot(),
        progress_mode,
        json_output,
        ui: Some(ui),
        progress: None,
    };
    ctx_history_ingest_application::run_ingest(&request, data_root, &mut host)
}

fn ingest_request(request: ImportRequest) -> IngestRequest {
    IngestRequest {
        path: request.path,
        provider: request.provider.map(HistoryProvider::capture_provider),
        custom_jsonl: request.input_format.is_some(),
        history_source: request.history_source,
        history_source_manifests: request.history_source_manifests,
        all: request.all,
        resume: request.resume,
        relocate_from: request.relocate_from,
        reset_cursor: request.reset_cursor,
        no_daemon: request.no_daemon,
    }
}

struct HistoryImportHost<'a, P> {
    home: Option<PathBuf>,
    data_root: PathBuf,
    port: &'a mut P,
    config: HistoryCliConfig,
    progress_mode: ProgressMode,
    json_output: bool,
    ui: Option<&'a mut Ui>,
    progress: Option<ProgressReporter<'a>>,
}

impl<P: ImportApplicationPort> SourceDiscoveryPort for HistoryImportHost<'_, P> {
    fn discover_all(&self) -> Result<DiscoveryReport> {
        self.discovery().discover_all()
    }

    fn discover_provider(&self, provider: CaptureProvider) -> Result<DiscoveryReport> {
        self.discovery().discover_provider(provider)
    }

    fn provider_selection_guidance(
        &self,
        provider: CaptureProvider,
    ) -> ctx_history_ingest_application::ProviderSelectionGuidance {
        provider_selection_guidance(provider)
    }
}

impl<P: ImportApplicationPort> CaptureAdmissionPort for HistoryImportHost<'_, P> {
    fn protect_data_root(&mut self, data_root: &Path) -> Result<()> {
        self.port.protect_data_root(data_root)
    }

    fn explicit_source(
        &self,
        data_root: &Path,
        path: &Path,
        provider: Option<CaptureProvider>,
        custom_jsonl: bool,
    ) -> Result<ProviderSource> {
        self.port
            .explicit_source(data_root, path, provider, custom_jsonl)
    }

    fn prepare_plugin(
        &mut self,
        source: &HistorySourcePluginSource,
        reset_cursor: bool,
    ) -> Result<ProviderSource> {
        self.port.prepare_plugin(source, reset_cursor)
    }

    fn admit_exact(
        &mut self,
        data_root: &Path,
        source: &ProviderSource,
        relocate_from: Option<&Path>,
    ) -> Result<ExplicitSourceCatalogUpsert> {
        self.port.admit_exact(data_root, source, relocate_from)
    }

    fn source_failure_identity(&self, source: &ProviderSource) -> Result<String> {
        self.port.source_failure_identity(source)
    }
}

impl<P: ImportApplicationPort> IngestProgressPort for HistoryImportHost<'_, P> {
    fn begin(&mut self, total_bytes: u64) -> Result<()> {
        let ui = self
            .ui
            .take()
            .context("ingest progress was initialized more than once")?;
        self.progress = Some(ProgressReporter::new(
            ui,
            self.progress_mode,
            self.json_output,
            "import",
            total_bytes,
        ));
        Ok(())
    }

    fn catalog_exact(&mut self, source: &ProviderSource, stats: SourceStats) -> Result<()> {
        self.progress_mut()?.message(
            "cataloging",
            format!(
                "Cataloging {} source {} ({}).",
                source.provider.as_str(),
                source.path.display(),
                crate::format_bytes(stats.bytes)
            ),
        )?;
        Ok(())
    }

    fn catalog_plugin(&mut self, source: &HistorySourcePluginSource) -> Result<()> {
        self.progress_mut()?.message(
            "cataloging",
            format!(
                "Cataloging provider-owned history source plugin path for {}.",
                source.label()
            ),
        )?;
        Ok(())
    }
}

impl<P: ImportApplicationPort> IngestRefreshPort for HistoryImportHost<'_, P> {
    fn refresh(
        &mut self,
        data_root: &Path,
        selection: RefreshSelection,
        no_daemon: bool,
    ) -> Result<IngestPublication> {
        let progress = self
            .progress
            .as_mut()
            .context("ingest refresh requested before progress initialization")?;
        self.port.refresh(data_root, selection, no_daemon, progress)
    }
}

impl<'a, P> HistoryImportHost<'a, P> {
    fn discovery(&self) -> CliSourceDiscoveryPort {
        CliSourceDiscoveryPort::new(self.home.clone(), self.data_root.clone())
            .with_automatic_provider_discovery(self.config.automatic_provider_discovery)
            .with_provider_roots(self.config.provider_roots.clone())
    }

    fn progress_mut(&mut self) -> Result<&mut ProgressReporter<'a>> {
        self.progress
            .as_mut()
            .context("ingest refresh requested before progress initialization")
    }
}
