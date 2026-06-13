use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use ctx_core::models::{VcsKind, Workspace};
use ctx_settings_model::{ExecutionMode, ExecutionSettings, Settings};
use ctx_store::{Store, StoreManager};

use super::load_provider_model_catalog;

mod catalog_selection;
mod config_errors;
mod fixtures;

use fixtures::{
    save_sandbox_execution_mode, seed_ready_gemini_status, test_model_catalog_fixture,
    write_invalid_agent_server_config, write_invalid_harness_registry,
};
