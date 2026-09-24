use super::*;

pub(crate) fn native_root(db: &Path) -> Result<PathBuf> {
    let stats = Store::open_read_only(db)?.stats()?;
    ensure!(
        stats.kind == "native",
        "this command requires a native index"
    );
    Ok(PathBuf::from(
        stats.root.context("native index has no source root")?,
    ))
}

pub(crate) fn retryable_update(error: &anyhow::Error) -> bool {
    error.is::<ctx_graph_core::store::StaleStore>() || error.chain().any(|cause| {
        matches!(cause.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(code, _)) if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
    })
}

pub fn run_parsed(cli: GraphArgs) -> Result<()> {
    if let Command::HookGuard(args) = &cli.command {
        if let Some(value) = crate::hook_context(
            args.platform,
            args.project.as_deref(),
            cli.db.as_deref(),
            io::stdin().lock(),
        ) {
            // Hook output is independent of normal CLI formatting and notices.
            // A closed host pipe must not turn optional context into a denial.
            let _ = writeln!(io::stdout().lock(), "{value}");
        }
        return Ok(());
    }
    match &cli.command {
        Command::Clone {
            url,
            output,
            branch,
            index,
            refresh,
        } => {
            let parsed =
                reqwest::Url::parse(url).context("expected an HTTPS GitHub repository URL")?;
            ensure!(
                parsed.scheme() == "https"
                    && parsed.host_str() == Some("github.com")
                    && parsed.username().is_empty()
                    && parsed.password().is_none()
                    && parsed.query().is_none()
                    && parsed.fragment().is_none()
                    && parsed.port().is_none(),
                "expected an HTTPS GitHub repository URL without credentials or query parameters"
            );
            let parts: Vec<_> = parsed.path().trim_matches('/').split('/').collect();
            ensure!(
                parts.len() == 2
                    && parts.iter().all(|p| !matches!(*p, "" | "." | "..")
                        && p.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))),
                "expected github.com/OWNER/REPOSITORY"
            );
            let name = parts[1].strip_suffix(".git").unwrap_or(parts[1]);
            ensure!(
                !name.is_empty() && name != "." && name != "..",
                "invalid repository name"
            );
            let output = match output {
                Some(output) => output.clone(),
                None => PathBuf::from(
                    std::env::var_os("HOME")
                        .or_else(|| std::env::var_os("USERPROFILE"))
                        .context("home directory unavailable; pass --output")?,
                )
                .join(".graf/repos")
                .join(parts[0])
                .join(name),
            };
            ensure!(*index || cli.db.is_none(), "--db requires clone --index");
            let reused = output.try_exists()?;
            if reused {
                ensure!(
                    output.is_dir() && output.join(".git").try_exists()?,
                    "clone destination is not a Git checkout"
                );
                let git = |args: &[&str]| -> Result<String> {
                    let result = std::process::Command::new("git")
                        .arg("-C")
                        .arg(&output)
                        .args(args)
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .output()
                        .context("cannot launch Git")?;
                    ensure!(
                        result.status.success(),
                        "Git cache operation failed; inspect the checkout and repository access"
                    );
                    Ok(String::from_utf8(result.stdout)
                        .context("Git returned non-UTF-8 metadata")?
                        .trim_end_matches(['\r', '\n'])
                        .to_owned())
                };
                let origin = git(&["config", "--get", "remote.origin.url"])?;
                ensure!(
                    origin
                        .trim_end_matches('/')
                        .trim_end_matches(".git")
                        .eq_ignore_ascii_case(
                            parsed
                                .as_str()
                                .trim_end_matches('/')
                                .trim_end_matches(".git")
                        ),
                    "existing checkout has a different origin; choose another --output"
                );
                let current = git(&["symbolic-ref", "--short", "HEAD"])?;
                if let Some(branch) = branch {
                    ensure!(
                        branch == &current,
                        "existing checkout uses a different branch; choose another --output"
                    );
                }
                if *refresh {
                    git(&["pull", "--ff-only", "--", "origin", &current])?;
                }
            } else {
                if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)?;
                }
                let mut command = std::process::Command::new("git");
                command
                    .args(["clone", "--depth", "1"])
                    .env("GIT_TERMINAL_PROMPT", "0");
                if let Some(branch) = branch {
                    command.arg("--branch").arg(branch);
                }
                let result = command
                    .arg("--")
                    .arg(parsed.as_str())
                    .arg(&output)
                    .output()
                    .context("cannot launch Git")?;
                ensure!(
                    result.status.success(),
                    "Git clone failed; check repository access, branch and destination"
                );
            }
            let report = if *index {
                let db = cli
                    .db
                    .clone()
                    .unwrap_or_else(|| output.join(".graf/index.db"));
                Some(ctx_graph_core::index::run(&output, &db)?)
            } else {
                None
            };
            return print_value(
                &serde_json::json!({"status":if reused { if *refresh { "refreshed" } else { "reused" } } else { "cloned" },"path":output,"index":report}),
                cli.json,
            );
        }
        Command::Extended(args) => return commands::run(args, cli.db.as_deref(), cli.json),
        Command::Connect(args) => return connect::run(args, cli.db.as_deref(), cli.json),
        Command::Provider(args) => return print_value(&extraction::provider(args)?, cli.json),
        Command::Cache(args) => return print_value(&extraction::cache(args)?, cli.json),
        Command::Install(args) => {
            ensure!(cli.db.is_none(), "install selects a project; omit --db");
            return print_value(&agent_setup::install(args)?, cli.json);
        }
        Command::Uninstall(args) => {
            ensure!(cli.db.is_none(), "uninstall selects a project; omit --db");
            return print_value(&agent_setup::uninstall(args)?, cli.json);
        }
        Command::Hook(args) => {
            ensure!(cli.db.is_none(), "hook selects a project; omit --db");
            return print_value(&agent_setup::hook(args)?, cli.json);
        }
        _ => (),
    }

    if let Command::Switch(args) = cli.command {
        ensure!(
            cli.db.is_none(),
            "switch uses the project's .graf/index.db; omit --db"
        );
        let report = switch::run(args)?;
        if cli.json {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            println!(
                "{}: {} nodes, {} edges.\nMCP config: {}\nDatabase: {}",
                report.status,
                report.nodes,
                report.edges,
                human(&report.config.display().to_string()),
                human(&report.database.display().to_string())
            );
            if report.status != "undone" {
                println!(
                    "Verified ctx graph MCP. Restart your client to load graph tools.\nImported graphs are snapshots; Graphify generation remains available.\nUndo: ctx graph switch --undo (use the same --project and --config, if supplied)"
                );
            } else {
                println!(
                    "Restored the MCP configuration; the imported database was retained. Restart your client."
                );
            }
        }
        return Ok(());
    }
    let db = database(&cli)?;
    let show_learning = match &cli.command {
        Command::Show(args) => args
            .memory_dir
            .as_ref()
            .map(|dir| (dir.clone(), args.symbol.navigation.budget)),
        _ => None,
    };
    let command = match cli.command {
        Command::Switch(_)
        | Command::Install(_)
        | Command::Uninstall(_)
        | Command::Hook(_)
        | Command::HookGuard(_)
        | Command::Extended(_)
        | Command::Connect(_)
        | Command::Provider(_)
        | Command::Cache(_)
        | Command::Clone { .. } => unreachable!(),
        Command::Index { path, extraction } => {
            let options = extraction.configure(index::stored_options(&db)?, &path, &db)?;
            return print_output(
                Output::Index(index::run_with_options(&path, &db, &options)?),
                cli.json,
            );
        }
        Command::Add {
            source,
            name,
            contributor,
            captured_at_unix_secs,
            project,
            extraction,
        } => {
            let root = project.canonicalize().context("cannot resolve project")?;
            ensure!(root.is_dir(), "project must be a directory");
            let options = extraction.configure(index::stored_options(&db)?, &root, &db)?;
            if db.try_exists()? {
                let stats = Store::open_read_only(&db)?.stats()?;
                ensure!(
                    stats.kind == "native" && stats.root.as_deref() == root.to_str(),
                    "add requires this project's native graph"
                );
            }
            let capture = ctx_graph_core::ingest::CaptureMetadata {
                contributor,
                captured_at_unix_secs,
            };
            let (record, report) = ctx_graph_core::sources::add_and_index(
                &root,
                &db,
                &source,
                name.as_deref(),
                &options,
                &capture,
            )?;
            return print_value(
                &serde_json::json!({"source":record.source,"path":record.facts.path,"index":report}),
                cli.json,
            );
        }
        Command::CheckUpdate => {
            return print_value(&index::check_update(&native_root(&db)?, &db)?, cli.json);
        }
        Command::Compact => {
            let report = Store::open(&db)?.compact()?;
            if cli.json {
                return print_value(&report, true);
            }
            println!(
                "Compacted database: {} -> {} bytes of database pages.",
                report.pages_before * report.page_size,
                report.pages_after * report.page_size
            );
            if report.checkpoint_busy {
                println!("Another connection is delaying disk-space reclamation.");
            }
            return Ok(());
        }
        Command::Watch {
            interval_ms,
            iterations,
        } => {
            let root = native_root(&db)?;
            let mut polls = 0;
            loop {
                let result = (|| -> Result<()> {
                    if !index::check_update(&root, &db)?.fresh {
                        print_output(Output::Index(index::run(&root, &db)?), cli.json)?;
                    }
                    Ok(())
                })();
                let pending = match result {
                    Err(error) if retryable_update(&error) => {
                        eprintln!(
                            "index is busy or changed concurrently; retrying at the next watch poll"
                        );
                        Some(error)
                    }
                    Err(error) => return Err(error),
                    Ok(()) => None,
                };
                polls += 1;
                if iterations.is_some_and(|limit| polls >= limit) {
                    return pending.map_or(Ok(()), Err);
                }
                std::thread::sleep(std::time::Duration::from_millis(interval_ms));
            }
        }
        Command::Update {
            timing,
            force,
            refresh_cache,
            allow_semantic_shrink,
        } => {
            let stats = Store::open_read_only(&db)?.stats()?;
            ensure!(
                stats.kind == "native",
                "update requires a native index, not {}",
                stats.kind
            );
            let root = stats
                .root
                .context("native index has no recorded source root")?;
            let mut options = index::stored_options(&db)?;
            options.force = force || refresh_cache;
            options.timing = timing;
            options.ingest.force_cache_refresh = refresh_cache;
            options.allow_semantic_shrink = allow_semantic_shrink;
            return print_output(
                Output::Index(index::run_with_options(Path::new(&root), &db, &options)?),
                cli.json,
            );
        }
        Command::Import { format } => {
            let (graph, refresh) = match format {
                ImportFormat::Graphify {
                    file,
                    format,
                    refresh,
                } => (
                    match format {
                        SnapshotFormat::NodeLink => import::read_graphify(&file)?,
                        SnapshotFormat::Export => import::read_graphify_export(&file)?,
                    },
                    refresh,
                ),
                ImportFormat::Graf { file, refresh } => {
                    (ctx_graph_core::snapshot::read(&file)?, refresh)
                }
            };
            if let Some(parent) = db.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            let mut store = Store::create(&db)?;
            let stats = if refresh {
                store.refresh_import(graph)?
            } else {
                store.import_graph(graph)?
            };
            return print_output(Output::Stats(stats), cli.json);
        }
        Command::Serve(args) => {
            return tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(mcp::serve(db, args));
        }
        Command::Query(a) => ReadCommand::Query(a),
        Command::Show(a) => ReadCommand::Show(a.symbol),
        Command::Callers(a) => ReadCommand::Callers(a),
        Command::Callees(a) => ReadCommand::Callees(a),
        Command::Impact(a) => ReadCommand::Impact(a),
        Command::Path(a) => ReadCommand::Path(a),
        Command::Stats => ReadCommand::Stats,
    };
    let (kind, question) = match &command {
        ReadCommand::Query(a) => ("query", a.text.clone()),
        ReadCommand::Show(a) => ("show", a.symbol.clone()),
        ReadCommand::Callers(a) => ("callers", a.symbol.clone()),
        ReadCommand::Callees(a) => ("callees", a.symbol.clone()),
        ReadCommand::Impact(a) => ("impact", a.symbol.clone()),
        ReadCommand::Path(a) => ("path", format!("{} -> {}", a.source, a.target)),
        ReadCommand::Stats => ("stats", String::new()),
    };
    let start = std::time::Instant::now();
    let output = read(&db, command)?;
    if let Some(path) = cli.query_log.as_deref() {
        let response = serde_json::to_value(&output)?;
        let graph = response.get("result").unwrap_or(&response);
        let graph = graph.get("graph").unwrap_or(graph);
        let mut record = serde_json::json!({
            "timestamp_unix_secs":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(),
            "kind":kind,"question":question,"corpus":db,
            "duration_ms":start.elapsed().as_millis(),"generation":graph.get("generation"),
            "nodes":graph.get("nodes").and_then(|v|v.as_array()).map(Vec::len),
            "response_bytes":serde_json::to_vec(&response)?.len(),
        });
        if cli.log_responses {
            record["response"] = response;
        }
        if append_query_log(path, &record).is_err() {
            eprintln!("ctx graph: query log could not be written; query result is still available");
        }
    }
    if let Some((memory_dir, budget)) = show_learning {
        let graph = match &output {
            Output::Graph(graph) => graph,
            Output::Search(result) => &result.graph,
            _ => unreachable!("show returns a graph or search result"),
        };
        let snapshot = mcp::snapshot_for_learning(&db).ok();
        let annotation = learning_annotations(&memory_dir, graph, snapshot.as_ref(), budget);
        return print_show_output(output, annotation, cli.json);
    }
    print_output(output, cli.json)
}
