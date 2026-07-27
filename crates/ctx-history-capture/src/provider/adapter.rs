use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::utc_now;
use uuid::Uuid;

use crate::{default_machine_id, ImportProfile};

#[derive(Debug, Clone)]
pub struct ProviderAdapterContext {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub source_root: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
}

impl ProviderAdapterContext {
    pub(crate) fn source_root_display(&self) -> Option<String> {
        self.source_root
            .as_ref()
            .or(self.source_path.as_ref())
            .map(|path| path.display().to_string())
    }
}

impl Default for ProviderAdapterContext {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            source_root: None,
            imported_at: utc_now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderImportOptions {
    pub history_record_id: Option<Uuid>,
    pub capture_work_limit: CaptureWorkLimit,
    pub inventory_observation_token: Option<String>,
    pub import_profile: ImportProfile,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaptureWorkLimit {
    #[default]
    Drain,
    OneSafeGroup,
}

impl Default for ProviderImportOptions {
    fn default() -> Self {
        Self {
            history_record_id: None,
            capture_work_limit: CaptureWorkLimit::Drain,
            inventory_observation_token: None,
            import_profile: ImportProfile::CoreOnly,
        }
    }
}
