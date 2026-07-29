use std::{path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use ctx_history_core::utc_now;
use uuid::Uuid;

use crate::{default_machine_id, ImportProfile};

macro_rules! import_options {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Debug, Clone)]
            pub struct $name {
                pub machine_id: String,
                pub source_path: Option<PathBuf>,
                pub imported_at: DateTime<Utc>,
                pub history_record_id: Option<Uuid>,
                pub capture_work_limit: crate::CaptureWorkLimit,
                pub inventory_observation_token: Option<String>,
                pub import_profile: ImportProfile,
            }

            impl Default for $name {
                fn default() -> Self {
                    Self {
                        machine_id: default_machine_id(),
                        source_path: None,
                        imported_at: utc_now(),
                        history_record_id: None,
                        capture_work_limit: crate::CaptureWorkLimit::Drain,
                        inventory_observation_token: None,
                        import_profile: ImportProfile::CoreOnly,
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
pub type MiMoCodeSqliteImportOptions = OpenCodeSqliteImportOptions;
pub type TabnineCliImportOptions = GeminiCliImportOptions;

#[derive(Clone)]
pub struct CodexSessionImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub max_session_files: Option<usize>,
    pub max_total_bytes: Option<u64>,
    pub progress: Option<CodexSessionImportProgressCallback>,
    pub capture_work_limit: crate::CaptureWorkLimit,
    pub inventory_observation_token: Option<String>,
    pub import_profile: ImportProfile,
}

impl Default for CodexSessionImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            max_session_files: None,
            max_total_bytes: None,
            progress: None,
            capture_work_limit: crate::CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            import_profile: ImportProfile::CoreOnly,
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
            .field("max_session_files", &self.max_session_files)
            .field("max_total_bytes", &self.max_total_bytes)
            .field("progress", &self.progress.as_ref().map(|_| "<callback>"))
            .field("capture_work_limit", &self.capture_work_limit)
            .field(
                "inventory_observation_token",
                &self.inventory_observation_token.as_ref().map(|_| "<token>"),
            )
            .field("import_profile", &self.import_profile)
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

#[derive(Debug, Clone)]
pub struct CodexSessionCatalogOptions {
    pub source_root: Option<PathBuf>,
    pub cataloged_at: DateTime<Utc>,
    pub max_session_files: Option<usize>,
    pub max_total_bytes: Option<u64>,
    pub parallelism: Option<usize>,
}

impl Default for CodexSessionCatalogOptions {
    fn default() -> Self {
        Self {
            source_root: None,
            cataloged_at: utc_now(),
            max_session_files: None,
            max_total_bytes: None,
            parallelism: None,
        }
    }
}
