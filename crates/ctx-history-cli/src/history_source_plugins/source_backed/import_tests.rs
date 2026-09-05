use std::{fs, io};

use ctx_history_ingest_application::{IngestPublication, RefreshSelection};
use ctx_history_refresh::ExplicitSourceCatalogUpsert;
use ctx_terminal::{RenderContext, StreamKind, TestContext, Ui};

use super::*;
use crate::{
    run_import_application, HistoryCliConfig, HistoryConfigSnapshotPort, ImportApplicationPort,
    ImportRequest, OutputFormat, ProgressMode, ProgressReporter,
};

#[derive(Default)]
struct ImportHost {
    preparations: usize,
    admissions: usize,
}

impl ImportApplicationPort for ImportHost {
    fn protect_data_root(&mut self, data_root: &Path) -> Result<()> {
        fs::create_dir_all(data_root)?;
        Ok(())
    }

    fn explicit_source(
        &self,
        _: &Path,
        _: &Path,
        _: Option<CaptureProvider>,
        _: bool,
    ) -> Result<ProviderSource> {
        panic!("plugin import must use plugin preparation")
    }

    fn prepare_plugin(
        &mut self,
        source: &HistorySourcePluginSource,
        reset_cursor: bool,
    ) -> Result<ProviderSource> {
        self.preparations += 1;
        Ok(
            prepare_source_backed_history_source(source.clone(), reset_cursor)?
                .provider_source()
                .clone(),
        )
    }

    fn admit_exact(
        &mut self,
        _: &Path,
        _: &ProviderSource,
        _: Option<&Path>,
    ) -> Result<ExplicitSourceCatalogUpsert> {
        self.admissions += 1;
        bail!("test stopped at source admission")
    }

    fn source_failure_identity(&self, _: &ProviderSource) -> Result<String> {
        panic!("plugin preflight must not resolve a refresh failure")
    }

    fn refresh(
        &mut self,
        _: &Path,
        _: RefreshSelection,
        _: bool,
        _: &mut ProgressReporter<'_>,
    ) -> Result<IngestPublication> {
        panic!("plugin preflight must not reach refresh")
    }
}

struct Config;

impl HistoryConfigSnapshotPort for Config {
    fn snapshot(&self) -> HistoryCliConfig {
        HistoryCliConfig {
            daemon_enabled: false,
            semantic_search_enabled: false,
            semantic_executor: Default::default(),
            local_usage_enabled: false,
            automatic_provider_discovery: false,
            provider_roots: Vec::new(),
        }
    }
}

#[test]
fn plugin_header_rejection_precedes_source_admission_and_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("history.jsonl");
    let manifest_path = temp.path().join("ctx-history-plugin.json");
    fs::write(
        &manifest_path,
        serde_json::json!({
            "schema_version":1,"name":"example","history_sources":[{
                "id":"default","source_format":"example-v1","path":source_path
            }]
        })
        .to_string(),
    )
    .unwrap();
    let request = ImportRequest {
        provider: None,
        path: None,
        relocate_from: None,
        history_source: None,
        history_source_manifests: vec![manifest_path],
        reset_cursor: false,
        input_format: None,
        all: false,
        resume: false,
        no_daemon: true,
        format: OutputFormat::Json,
        progress: ProgressMode::None,
    };
    let mut host = ImportHost::default();
    let mut ui = Ui::with_writers(
        io::sink(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
        io::sink(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
    );
    fs::write(&source_path, vec![b' '; 2 * MAX_HEADER_BYTES]).unwrap();
    let error = run_import_application(
        request.clone(),
        &temp.path().join("ctx-data"),
        Some(temp.path().into()),
        &Config,
        &mut host,
        &mut ui,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("header exceeds the bounded validation window"),
        "{error:#}"
    );
    assert_eq!((host.preparations, host.admissions), (1, 0));

    fs::write(&source_path, concat!(
        "{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\"}\n",
        "{\"record_type\":\"source\",\"provider_key\":\"example\",\"source_id\":\"default\",\"source_format\":\"example-v1\"}\n",
    )).unwrap();
    let error = run_import_application(
        request,
        &temp.path().join("ctx-data"),
        Some(temp.path().into()),
        &Config,
        &mut host,
        &mut ui,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("test stopped at source admission"),
        "{error:#}"
    );
    assert_eq!((host.preparations, host.admissions), (2, 1));
}
