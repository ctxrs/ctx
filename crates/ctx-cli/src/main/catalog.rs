#[allow(unused_imports)]
use super::*;

#[derive(Debug, Args)]
pub(crate) struct SetupArgs {
    #[arg(long, alias = "no-import")]
    pub(crate) catalog_only: bool,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long, value_enum, default_value_t = ProgressArg::Auto)]
    pub(crate) progress: ProgressArg,
}

#[derive(Debug, Default)]
pub(crate) struct CatalogTotals {
    pub(crate) sources: usize,
    pub(crate) source_files: usize,
    pub(crate) source_bytes: u64,
    pub(crate) cataloged_sessions: usize,
    pub(crate) cached_sessions: usize,
    pub(crate) parsed_sessions: usize,
    pub(crate) skipped_sessions: usize,
    pub(crate) failed_sessions: usize,
}

impl CatalogTotals {
    pub(crate) fn add(&mut self, summary: &CatalogSummary) {
        self.sources += 1;
        self.source_files += summary.source_files;
        self.source_bytes = self.source_bytes.saturating_add(summary.source_bytes);
        self.cataloged_sessions += summary.cataloged_sessions;
        self.cached_sessions += summary.cached_sessions;
        self.parsed_sessions += summary.parsed_sessions;
        self.skipped_sessions += summary.skipped_sessions;
        self.failed_sessions += summary.failed_sessions;
    }
}

pub(crate) fn command_analytics_properties(command: &CommandRoot) -> AnalyticsProperties {
    let mut properties = analytics::empty_properties();
    match command {
        CommandRoot::Setup(args) => {
            analytics::insert_bool(&mut properties, "catalog_only", args.catalog_only);
            analytics::insert_str(
                &mut properties,
                "progress_mode",
                progress_mode_name(args.progress),
            );
        }
        CommandRoot::Status(_)
        | CommandRoot::Sources(_)
        | CommandRoot::Sql(_)
        | CommandRoot::Doctor(_) => {}
        CommandRoot::Import(args) => {
            analytics::insert_bool(&mut properties, "resume", args.resume);
            analytics::insert_bool(&mut properties, "all_sources", args.all);
            analytics::insert_str(
                &mut properties,
                "source_mode",
                if args.format.is_some() {
                    "explicit_format"
                } else if args.history_source.is_some() {
                    "history_source_plugin"
                } else if args.path.is_some() {
                    "explicit_path"
                } else if args.all {
                    "all_discovered"
                } else if args.provider.is_some() {
                    "discovered_provider"
                } else {
                    "auto_discovered"
                },
            );
            if let Some(provider) = args.provider {
                analytics::insert_str(
                    &mut properties,
                    "provider_filter",
                    provider.capture_provider().as_str(),
                );
            }
            analytics::insert_bool(&mut properties, "reset_cursor", args.reset_cursor);
            analytics::insert_str(
                &mut properties,
                "progress_mode",
                progress_mode_name(args.progress),
            );
        }
        CommandRoot::Show(args) => match &args.target {
            ShowTarget::Session(args) => {
                analytics::insert_str(&mut properties, "target_kind", "session");
                analytics::insert_str(&mut properties, "transcript_mode", args.mode.as_str());
                analytics::insert_str(&mut properties, "output_format", args.format.as_str());
                analytics::insert_bool(&mut properties, "writes_out_file", args.out.is_some());
                analytics::insert_bool(
                    &mut properties,
                    "provider_lookup",
                    args.provider.is_some() || args.provider_session.is_some(),
                );
            }
            ShowTarget::Event(args) => {
                analytics::insert_str(&mut properties, "target_kind", "event");
                analytics::insert_str(&mut properties, "output_format", args.format.as_str());
                analytics::insert_count_bucket(
                    &mut properties,
                    "window_bucket",
                    args.window.unwrap_or(args.before.max(args.after)) as u64,
                );
            }
        },
        CommandRoot::Locate(args) => match &args.target {
            LocateTarget::Session(args) => {
                analytics::insert_str(&mut properties, "target_kind", "session");
                analytics::insert_str(&mut properties, "output_format", args.format.as_str());
                analytics::insert_bool(
                    &mut properties,
                    "provider_lookup",
                    args.provider.is_some() || args.provider_session.is_some(),
                );
            }
            LocateTarget::Event(args) => {
                analytics::insert_str(&mut properties, "target_kind", "event");
                analytics::insert_str(&mut properties, "output_format", args.format.as_str());
            }
        },
        CommandRoot::Search(args) => {
            analytics::insert_bool(&mut properties, "has_query", args.query.is_some());
            analytics::insert_bool(
                &mut properties,
                "has_provider_filter",
                args.provider.is_some(),
            );
            analytics::insert_bool(
                &mut properties,
                "has_workspace_filter",
                args.workspace.is_some(),
            );
            analytics::insert_bool(&mut properties, "has_since_filter", args.since.is_some());
            analytics::insert_bool(
                &mut properties,
                "has_event_type_filter",
                args.event_type.is_some(),
            );
            analytics::insert_bool(&mut properties, "has_file_filter", args.file.is_some());
            analytics::insert_bool(
                &mut properties,
                "has_session_filter",
                args.session.is_some(),
            );
            analytics::insert_bool(
                &mut properties,
                "event_results",
                args.events || args.session.is_some(),
            );
            analytics::insert_bool(&mut properties, "primary_only", args.primary_only);
            analytics::insert_bool(&mut properties, "include_subagents", args.include_subagents);
            analytics::insert_bool(
                &mut properties,
                "include_current_session",
                args.include_current_session,
            );
            analytics::insert_count_bucket(&mut properties, "limit_bucket", args.limit as u64);
            if let Some(provider) = args.provider {
                analytics::insert_str(
                    &mut properties,
                    "provider_filter",
                    provider.capture_provider().as_str(),
                );
            }
        }
        CommandRoot::Mcp(_) => {}
        CommandRoot::Docs(_) => {}
        CommandRoot::Skill(args) => {
            args.add_initial_analytics(&mut properties);
        }
        CommandRoot::Upgrade(args) => {
            analytics::insert_bool(&mut properties, "dry_run", args.dry_run);
            analytics::insert_bool(&mut properties, "background", args.background());
        }
    }
    properties
}

pub(crate) fn print_setup_status_line(
    report: Option<&ImportReport>,
    catalog_only: bool,
    pending_catalog_sessions: usize,
    indexed_items: usize,
) {
    if catalog_only {
        if pending_catalog_sessions > 0 {
            println!("ctx catalog is ready; import is still pending");
        } else {
            println!("ctx catalog is ready");
        }
        return;
    }
    let Some(report) = report else {
        println!("ctx is initialized; no local history was indexed");
        return;
    };
    if setup_has_indexed_content(indexed_items) && report.totals.failed_sources > 0 {
        println!("ctx indexed available local agent history; some sources were skipped");
    } else if setup_has_indexed_content(indexed_items) {
        println!("ctx local agent history search is ready");
    } else {
        println!("ctx is initialized; no local history was indexed");
    }
}

pub(crate) fn mark_catalog_sessions_indexed(
    store: &Store,
    sessions: &[CatalogSession],
    summary: &ProviderImportSummary,
) -> Result<()> {
    let indexed_at_ms = utc_now().timestamp_millis();
    let event_count = if sessions.len() == 1 {
        Some(
            summary
                .imported_events
                .saturating_add(summary.skipped_events) as u64,
        )
    } else {
        None
    };
    for session in sessions {
        mark_catalog_session_indexed(store, session, event_count, indexed_at_ms)?;
    }
    Ok(())
}

pub(crate) fn mark_catalog_session_indexed(
    store: &Store,
    session: &CatalogSession,
    event_count: Option<u64>,
    indexed_at_ms: i64,
) -> Result<()> {
    let file_sha256 =
        sha256_file_prefix_hex(Path::new(&session.source_path), session.file_size_bytes)
            .with_context(|| format!("hash checkpoint prefix for {}", session.source_path))?;
    store.mark_catalog_source_indexed(
        session.provider,
        CatalogSourceIndexUpdate {
            source_root: &session.source_root,
            source_path: &session.source_path,
            file_size_bytes: session.file_size_bytes,
            file_modified_at_ms: session.file_modified_at_ms,
            file_sha256: Some(&file_sha256),
            event_count,
            indexed_at_ms,
        },
    )?;
    Ok(())
}

pub(crate) fn catalog_import_checkpoint_matches(
    path: &Path,
    byte_count: u64,
    expected_sha256: Option<&str>,
) -> Result<bool> {
    let Some(expected_sha256) = expected_sha256 else {
        return Ok(true);
    };
    let actual_sha256 = sha256_file_prefix_hex(path, byte_count)?;
    Ok(actual_sha256 == expected_sha256)
}

pub(crate) fn mark_catalog_sessions_failed(
    store: &Store,
    sessions: &[CatalogSession],
    error: &str,
) -> Result<()> {
    let indexed_at_ms = utc_now().timestamp_millis();
    for session in sessions {
        store.mark_catalog_source_failed(
            session.provider,
            &session.source_root,
            &session.source_path,
            error,
            indexed_at_ms,
        )?;
    }
    Ok(())
}
