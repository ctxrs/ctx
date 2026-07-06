#[allow(unused_imports)]
use super::*;

#[derive(Debug, Subcommand)]
pub(crate) enum CommandRoot {
    #[command(about = "Create local ctx storage and index discovered history")]
    Setup(SetupArgs),
    #[command(about = "Show local ctx index status")]
    Status(JsonArgs),
    #[command(about = "List configured and discovered agent history sources")]
    Sources(SourcesArgs),
    #[command(about = "Index provider history into local search")]
    Import(ImportArgs),
    #[command(about = "Show an indexed session transcript or event")]
    Show(ShowArgs),
    #[command(about = "Locate provider/source metadata for an indexed session or event")]
    Locate(LocateArgs),
    #[command(about = "Search indexed agent history")]
    Search(SearchArgs),
    #[command(about = "Run read-only SQL against the local ctx index")]
    Sql(SqlArgs),
    #[command(about = "Read embedded ctx documentation")]
    Docs(docs::DocsArgs),
    #[command(about = "Install or inspect the bundled ctx agent skill")]
    Skill(skill::SkillArgs),
    #[command(about = "Serve read-only ctx tools over MCP")]
    Mcp(mcp::McpArgs),
    #[command(about = "Check or apply signed ctx CLI upgrades")]
    Upgrade(upgrade::UpgradeArgs),
    #[command(about = "Check local ctx health")]
    Doctor(DoctorArgs),
}

pub(crate) fn run_setup(
    args: SetupArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    fs::create_dir_all(&data_root)?;
    let db_path = database_path(data_root.clone());
    let store = Store::open(&db_path)?;
    let config_path = data_root.join(CONFIG_FILE);
    config::write_default_config(&data_root)?;
    let sources = discovered_sources();
    let progress = ProgressReporter::new(args.progress, args.json, "setup", 0);
    progress.message("cataloging", "cataloging discovered Codex sessions");
    let (catalog, catalog_sources) = catalog_available_sources(&store, &sources)?;
    progress.done(
        "cataloging",
        format!("cataloged {} Codex sessions", catalog.cataloged_sessions),
        catalog.source_bytes,
    );
    let catalog_counts = store.catalog_session_counts()?;
    analytics::insert_count_bucket(
        analytics_properties,
        "providers_detected_bucket",
        sources.len() as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "cataloged_sessions_bucket",
        catalog.cataloged_sessions as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "pending_sessions_bucket",
        catalog_counts.pending as u64,
    );
    analytics::insert_bytes_bucket(
        analytics_properties,
        "catalog_source_bytes_bucket",
        catalog.source_bytes,
    );
    let import_report = if args.catalog_only {
        None
    } else {
        drop(store);
        let import_args = ImportArgs {
            provider: None,
            path: None,
            history_source: None,
            history_source_manifest: Vec::new(),
            reset_cursor: false,
            format: None,
            all: true,
            resume: false,
            json: args.json,
            progress: args.progress,
        };
        Some(run_import_internal(
            &import_args,
            data_root.clone(),
            analytics_properties,
            ImportRunOptions {
                progress: args.progress,
                json: args.json,
                print_human: !args.json,
                allow_empty_sources: true,
                include_history_source_plugins: false,
                operation: "setup",
            },
        )?)
    };
    let setup_store = Store::open(&db_path)?;
    let catalog_counts = setup_store.catalog_session_counts()?;
    let indexed_items = indexed_history_item_count(&setup_store)?;

    if args.json {
        print_json(json!({
            "schema_version": 1,
            "data_root": data_root,
            "database_path": db_path,
            "config_path": config_path,
            "mode": if args.catalog_only { "catalog_only" } else { "ready" },
            "indexed_items": indexed_items,
            "sources": sources_json(&sources),
            "catalog": {
                "sources": catalog.sources,
                "source_files": catalog.source_files,
                "source_bytes": catalog.source_bytes,
                "cataloged_sessions": catalog.cataloged_sessions,
                "cached_sessions": catalog.cached_sessions,
                "parsed_sessions": catalog.parsed_sessions,
                "indexed_sessions": catalog_counts.indexed,
                "pending_sessions": catalog_counts.pending,
                "skipped_sessions": catalog.skipped_sessions,
                "failed_sessions": catalog.failed_sessions,
                "failed_index_sessions": catalog_counts.failed,
                "stale_sessions": catalog_counts.stale,
            },
            "catalog_sources": catalog_sources,
            "import": setup_import_json(import_report.as_ref()),
            "network_required": false,
            "repo_writes": false,
        }))?;
    } else {
        progress.finish_line();
        print_setup_status_line(
            import_report.as_ref(),
            args.catalog_only,
            catalog_counts.pending,
            indexed_items,
        );
        println!("data_root: {}", data_root.display());
        println!("database_path: {}", db_path.display());
        println!("config_path: {}", config_path.display());
        println!("indexed_items: {indexed_items}");
        println!("cataloged_sessions: {}", catalog.cataloged_sessions);
        println!("cached_catalog_sessions: {}", catalog.cached_sessions);
        println!("parsed_catalog_sessions: {}", catalog.parsed_sessions);
        println!("indexed_catalog_sessions: {}", catalog_counts.indexed);
        println!("pending_catalog_sessions: {}", catalog_counts.pending);
        println!("failed_catalog_sessions: {}", catalog_counts.failed);
        println!("stale_catalog_sessions: {}", catalog_counts.stale);
        println!("catalog_source_files: {}", catalog.source_files);
        println!("catalog_source_bytes: {}", catalog.source_bytes);
        if let Some(report) = &import_report {
            println!("imported_sources: {}", report.totals.imported_sources);
            println!("failed_sources: {}", report.totals.failed_sources);
            println!("imported_sessions: {}", report.totals.imported_sessions);
            println!("imported_events: {}", report.totals.imported_events);
            println!("imported_edges: {}", report.totals.imported_edges);
        }
        println!("next_steps:");
        if args.catalog_only {
            println!("  ctx import --all");
            println!("  ctx sources");
        } else if setup_has_indexed_content(indexed_items) {
            println!("  ctx search \"what failed before\"");
            println!("  ctx sources");
            if setup_has_failed_sources(import_report.as_ref()) {
                println!("  ctx import --provider <provider>");
            }
        } else {
            println!("  ctx sources");
            println!("  ctx import --all");
        }
    }
    Ok(())
}

pub(crate) fn setup_has_indexed_content(indexed_items: usize) -> bool {
    indexed_items > 0
}
