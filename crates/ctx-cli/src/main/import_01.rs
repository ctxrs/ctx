#[allow(unused_imports)]
use super::*;

pub(crate) const LARGE_IMPORT_SOURCE_FILES_WARNING: usize = 10_000;

pub(crate) const LARGE_IMPORT_SOURCE_BYTES_WARNING: u64 = 1024 * 1024 * 1024;

impl CommandRoot {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Setup(_) => "setup",
            Self::Status(_) => "status",
            Self::Sources(_) => "sources",
            Self::Import(_) => "import",
            Self::Show(_) => "show",
            Self::Locate(_) => "locate",
            Self::Search(_) => "search",
            Self::Sql(_) => "sql",
            Self::Docs(_) => "docs",
            Self::Skill(_) => "skill",
            Self::Mcp(_) => "mcp",
            Self::Upgrade(_) => "upgrade",
            Self::Doctor(_) => "doctor",
        }
    }

    pub(crate) fn sends_analytics(&self) -> bool {
        match self {
            Self::Sql(_) | Self::Mcp(_) => false,
            Self::Upgrade(args) if args.background() => false,
            _ => true,
        }
    }

    pub(crate) fn json_output(&self) -> bool {
        match self {
            Self::Setup(args) => args.json,
            Self::Status(args) => args.json,
            Self::Sources(args) => args.json,
            Self::Import(args) => args.json,
            Self::Show(args) => args.json_output(),
            Self::Locate(args) => args.json_output(),
            Self::Search(args) => args.json,
            Self::Sql(args) => args.json_output(),
            Self::Docs(args) => args.json_output(),
            Self::Skill(args) => args.json_output(),
            Self::Mcp(_) => false,
            Self::Upgrade(args) => args.json_output(),
            Self::Doctor(args) => args.json,
        }
    }

    pub(crate) fn allows_background_upgrade(&self) -> bool {
        !matches!(
            self,
            Self::Docs(_) | Self::Mcp(_) | Self::Sql(_) | Self::Upgrade(_)
        )
    }
}

impl ImportFormatArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CtxHistoryJsonlV1 => "ctx-history-jsonl-v1",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ImportTotals {
    pub(crate) source_files: usize,
    pub(crate) source_bytes: u64,
    pub(crate) imported_sources: usize,
    pub(crate) failed_sources: usize,
    pub(crate) imported_sessions: usize,
    pub(crate) imported_events: usize,
    pub(crate) imported_edges: usize,
    pub(crate) skipped_sessions: usize,
    pub(crate) skipped_events: usize,
    pub(crate) skipped_edges: usize,
    pub(crate) skipped: usize,
    pub(crate) failed: usize,
}

#[derive(Debug)]
pub(crate) struct ImportReport {
    pub(crate) resume: bool,
    pub(crate) totals: ImportTotals,
    pub(crate) sources: Vec<Value>,
}

impl ImportReport {
    pub(crate) fn empty(resume: bool) -> Self {
        Self {
            resume,
            totals: ImportTotals::default(),
            sources: Vec::new(),
        }
    }

    pub(crate) fn resume_mode(&self) -> &'static str {
        resume_mode_name(self.resume)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ImportRunOptions {
    pub(crate) progress: ProgressArg,
    pub(crate) json: bool,
    pub(crate) print_human: bool,
    pub(crate) allow_empty_sources: bool,
    pub(crate) include_history_source_plugins: bool,
    pub(crate) operation: &'static str,
}

impl ImportTotals {
    pub(crate) fn add(&mut self, summary: &ProviderImportSummary, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.imported_sources += 1;
        self.imported_sessions += summary.imported_sessions;
        self.imported_events += summary.imported_events;
        self.imported_edges += summary.imported_edges;
        self.skipped_sessions += summary.skipped_sessions;
        self.skipped_events += summary.skipped_events;
        self.skipped_edges += summary.skipped_edges;
        self.skipped += summary.skipped;
        self.failed += summary.failed;
    }

    pub(crate) fn add_source_failure(&mut self, stats: &SourceStats) {
        self.source_files += stats.files;
        self.source_bytes = self.source_bytes.saturating_add(stats.bytes);
        self.failed_sources += 1;
    }
}

pub(crate) fn setup_import_json(report: Option<&ImportReport>) -> Value {
    match report {
        Some(report) => json!({
            "ran": true,
            "resume": report.resume,
            "resume_mode": report.resume_mode(),
            "totals": import_totals_json(&report.totals),
            "sources": report.sources.clone(),
        }),
        None => json!({
            "ran": false,
            "reason": "catalog_only",
        }),
    }
}

pub(crate) fn setup_has_failed_sources(report: Option<&ImportReport>) -> bool {
    report.is_some_and(|report| report.totals.failed_sources > 0)
}

pub(crate) fn run_import(
    args: ImportArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let json = args.json;
    let progress = args.progress;
    let report = run_import_internal(
        &args,
        data_root,
        analytics_properties,
        ImportRunOptions {
            progress,
            json,
            print_human: !json,
            allow_empty_sources: false,
            include_history_source_plugins: true,
            operation: "import",
        },
    )?;
    print_import_report(&report, json)
}
