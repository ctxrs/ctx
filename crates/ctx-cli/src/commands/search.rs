use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration as StdDuration, Instant, SystemTime},
};

use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use ctx_history_capture::{
    discover_provider_sources_for_provider, DiskIoPacer, ProviderImportSummary,
    ProviderSourceStatus,
};
use ctx_history_core::database_path;
use ctx_history_store::{ImportWorkClass, ProviderFilePublicationInventoryOwner, Store};

use crate::analytics::AnalyticsProperties;
use crate::commands::import::{
    error_summary, failed_inventory_pending_counts, import_error_scope,
    import_history_source_plugin, import_selected_source, import_totals_json,
    import_work_progress_done, import_work_progress_message, inventory_import_sources,
    one_line_error, publication_recovery_maintenance_warning,
    recover_provider_file_publication_retirement, rejected_source_summary,
    repair_import_maintenance, source_matches_publication_owner, ExecutableImportSlice,
    ImportExecutionPolicy, ImportFailureScope, ImportInventoryCursor, ImportInventoryCursorStep,
    ImportInventorySliceProgress, ImportPlan, ImportSourceFailure, ImportTotals, SourceStats,
};
use crate::commands::setup::{
    indexed_history_item_count, insert_db_size_bucket, insert_store_analytics_counts,
};
use crate::history_source_plugins::{
    discover_history_source_plugins, HistorySourcePluginRefresh, HistorySourcePluginSource,
};
use crate::output::{compact_json, print_json};
use crate::progress::{ProgressArg, ProgressReporter};
use crate::provider_args::ProviderArg;
use crate::provider_sources::{discovered_sources, home_dir, SourceInfo};
use crate::search_filters::{
    normalize_source_identity_filters, search_filters, search_no_results_target, SearchFilterInput,
    SourceIdentityFilterArgs, SourceIdentityFilters,
};
use crate::search_query_input::search_query_from_args;
use crate::search_render::{print_search_result_compact, print_search_result_verbose, SearchDto};
use crate::semantic::{search_packet_file_filter_with_backend, search_packet_query_with_backend};
use crate::store_util::open_existing_store_read_only;
use crate::transcript::shell_quote_arg;
use crate::{analytics, config, semantic, SearchArgs, SearchBackendArg, WAL_TRUNCATE_MIN_BYTES};

include!("search/query.rs");
include!("search/refresh_plan.rs");
include!("search/refresh_execute.rs");
include!("search/freshness_tests.rs");
