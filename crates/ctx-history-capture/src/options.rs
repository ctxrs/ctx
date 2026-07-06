use std::{path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use ctx_history_core::{utc_now, CaptureProvider, Fidelity};
use uuid::Uuid;

use crate::default_machine_id;

#[derive(Debug, Clone)]
pub struct FixtureOptions {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub dedupe_key: Option<String>,
    pub machine_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub occurred_at: DateTime<Utc>,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            title: "Fixture capture".to_owned(),
            body: "fixture body".to_owned(),
            tags: vec!["fixture".to_owned()],
            dedupe_key: None,
            machine_id: None,
            cwd: None,
            occurred_at: utc_now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderFixtureImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub expected_provider: Option<CaptureProvider>,
    pub allow_partial_failures: bool,
    pub source_format: String,
    pub fidelity: Fidelity,
}

impl Default for ProviderFixtureImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            expected_provider: None,
            allow_partial_failures: false,
            source_format: "normalized_provider_fixture_jsonl".to_owned(),
            fidelity: Fidelity::Imported,
        }
    }
}

macro_rules! import_options {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Debug, Clone)]
            pub struct $name {
                pub machine_id: String,
                pub source_path: Option<PathBuf>,
                pub imported_at: DateTime<Utc>,
                pub history_record_id: Option<Uuid>,
                pub allow_partial_failures: bool,
            }

            impl Default for $name {
                fn default() -> Self {
                    Self {
                        machine_id: default_machine_id(),
                        source_path: None,
                        imported_at: utc_now(),
                        history_record_id: None,
                        allow_partial_failures: false,
                    }
                }
            }
        )+
    };
}

import_options!(
    CustomHistoryJsonlV1ImportOptions,
    CodexHistoryImportOptions,
    PiSessionImportOptions,
    ClaudeProjectsImportOptions,
    ClineTaskJsonImportOptions,
    RooTaskJsonImportOptions,
    CodeBuddyImportOptions,
    AuggieImportOptions,
    JunieImportOptions,
    FirebenderSqliteImportOptions,
    OpenCodeSqliteImportOptions,
    ForgeCodeSqliteImportOptions,
    DeepAgentsSqliteImportOptions,
    CrushSqliteImportOptions,
    GooseSessionsSqliteImportOptions,
    OpenClawImportOptions,
    HermesSqliteImportOptions,
    NanoClawImportOptions,
    AstrBotSqliteImportOptions,
    ShelleySqliteImportOptions,
    ContinueCliImportOptions,
    OpenHandsImportOptions,
    WarpSqliteImportOptions,
    LingmaSqliteImportOptions,
    TraeImportOptions,
    AntigravityCliImportOptions,
    GeminiCliImportOptions,
    FactoryAiDroidImportOptions,
    CopilotCliImportOptions,
    CursorNativeImportOptions,
    WindsurfCascadeHookImportOptions,
    QoderImportOptions,
    ZedThreadsSqliteImportOptions,
    QwenCodeImportOptions,
    KimiCodeCliImportOptions,
    RovoDevImportOptions,
    MistralVibeImportOptions,
    MuxImportOptions,
);

pub type KiloSqliteImportOptions = OpenCodeSqliteImportOptions;
pub type KiroSqliteImportOptions = OpenCodeSqliteImportOptions;
pub type TabnineCliImportOptions = GeminiCliImportOptions;

#[derive(Clone)]
pub struct CodexSessionImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
    pub max_session_files: Option<usize>,
    pub max_total_bytes: Option<u64>,
    pub tool_output_mode: CodexToolOutputMode,
    pub event_mode: CodexEventImportMode,
    pub include_notices: bool,
    pub fast_event_inserts: bool,
    pub progress: Option<CodexSessionImportProgressCallback>,
}

impl Default for CodexSessionImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            allow_partial_failures: false,
            max_session_files: None,
            max_total_bytes: None,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
            fast_event_inserts: true,
            progress: None,
        }
    }
}

impl std::fmt::Debug for CodexSessionImportOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexSessionImportOptions")
            .field("machine_id", &self.machine_id)
            .field("source_path", &self.source_path)
            .field("imported_at", &self.imported_at)
            .field("history_record_id", &self.history_record_id)
            .field("allow_partial_failures", &self.allow_partial_failures)
            .field("max_session_files", &self.max_session_files)
            .field("max_total_bytes", &self.max_total_bytes)
            .field("tool_output_mode", &self.tool_output_mode)
            .field("event_mode", &self.event_mode)
            .field("include_notices", &self.include_notices)
            .field("fast_event_inserts", &self.fast_event_inserts)
            .field("progress", &self.progress.as_ref().map(|_| "<callback>"))
            .finish()
    }
}

pub type CodexSessionImportProgressCallback =
    Arc<dyn Fn(CodexSessionImportProgress) + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub struct CodexSessionImportProgress {
    pub source_path: Option<PathBuf>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub completed_files: usize,
    pub completed_bytes: u64,
    pub imported_sessions: usize,
    pub imported_events: usize,
    pub imported_edges: usize,
    pub skipped: usize,
    pub failed: usize,
    pub done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexEventImportMode {
    Search,
    Rich,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexToolOutputMode {
    Full,
    Metadata,
    Failures,
    Skip,
}

#[derive(Debug, Clone)]
pub struct CodexSessionCatalogOptions {
    pub source_root: Option<PathBuf>,
    pub cataloged_at: DateTime<Utc>,
    pub allow_partial_failures: bool,
    pub max_session_files: Option<usize>,
    pub max_total_bytes: Option<u64>,
    pub parallelism: Option<usize>,
}

impl Default for CodexSessionCatalogOptions {
    fn default() -> Self {
        Self {
            source_root: None,
            cataloged_at: utc_now(),
            allow_partial_failures: true,
            max_session_files: None,
            max_total_bytes: None,
            parallelism: None,
        }
    }
}
