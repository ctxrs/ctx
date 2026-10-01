use super::*;
use crate::observation::{GraphLifecycle, GraphPhase};
use ctx_history_platform::resource_format::format_bytes;

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
    let mut facts = GraphObservation::new(
        observation::operation(&cli.command),
        GraphInvocation::Library,
    );
    run_parsed_observed(cli, &mut facts, None)
}

/// Execute with caller-owned facts, retaining them on Err. Does not finalize host output.
pub fn run_parsed_observed(
    cli: GraphArgs,
    facts: &mut GraphObservation,
    observer: Option<GraphObserver>,
) -> Result<()> {
    let started = std::time::Instant::now();
    facts.operation = observation::operation(&cli.command);
    let result = run_inner(cli, facts, observer);
    facts.duration = Some(started.elapsed());
    if let Err(error) = &result {
        if error.is::<output::OutputFailure>() {
            facts.phase = GraphPhase::OutputWrite;
            facts.output_failure = Some(observation::failure_kind(error));
        } else {
            observation::failed(facts, error);
        }
    } else {
        facts.execution_succeeded = Some(true);
    }
    if facts.operation == GraphOperation::Watch && result.is_err() && facts.lifecycle.is_none() {
        facts.lifecycle = Some(GraphLifecycle::StartFailed);
    }
    result
}

fn run_inner(
    cli: GraphArgs,
    facts: &mut GraphObservation,
    observer: Option<GraphObserver>,
) -> Result<()> {
    if let Command::HookGuard(args) = &cli.command {
        if let Some(value) = crate::hook_context(
            args.platform,
            args.project.as_deref(),
            cli.db.as_deref(),
            io::stdin().lock(),
        ) {
            // Hook output is independent of normal CLI formatting and notices.
            // A closed host pipe must not turn optional context into a denial.
            if let Err(error) = writeln!(io::stdout().lock(), "{value}") {
                facts.output_served = Some(false);
                facts.output_failure = Some(observation::failure_kind(&error.into()));
            }
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
                Some(ctx_graph_core::index::run_observed(&output, &db, facts)?)
            } else {
                None
            };
            facts.execution_succeeded = Some(true);
            return print_value(
                &serde_json::json!({"status":if reused { if *refresh { "refreshed" } else { "reused" } } else { "cloned" },"path":output,"index":report}),
                cli.json,
            );
        }
        Command::Extended(args) => {
            return commands::run_observed(args, cli.db.as_deref(), cli.json, facts);
        }
        Command::Connect(args) => return connect::run(args, cli.db.as_deref(), cli.json),
        Command::Provider(args) => {
            let value = extraction::provider(args)?;
            facts.execution_succeeded = Some(true);
            return print_value(&value, cli.json);
        }
        Command::Cache(args) => {
            return print_value(&extraction::cache_observed(args, facts)?, cli.json);
        }
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
        facts.nodes = Some(report.nodes as u64);
        facts.edges = Some(report.edges as u64);
        facts.execution_succeeded = Some(true);
        if cli.json {
            crate::output::stdout(format_args!("{}", serde_json::to_string(&report)?))?;
        } else {
            crate::output::stdout(format_args!(
                "{}: {} nodes, {} edges.\nMCP config: {}\nDatabase: {}",
                report.status,
                report.nodes,
                report.edges,
                human(&report.config.display().to_string()),
                human(&report.database.display().to_string())
            ))?;
            if report.status != "undone" {
                crate::output::stdout(format_args!(
                    "Verified ctx graph MCP. Restart your client to load graph tools.\nImported graphs are snapshots; Graphify generation remains available.\nUndo: ctx graph switch --undo (use the same --project and --config, if supplied)"
                ))?;
            } else {
                crate::output::stdout(format_args!(
                    "Restored the MCP configuration; the imported database was retained. Restart your client."
                ))?;
            }
        }
        return Ok(());
    }
    facts.phase = GraphPhase::Discover;
    let db = database(&cli)?;
    facts.phase = GraphPhase::Prepare;
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
                Output::Index(index::run_with_options_observed(
                    &path, &db, &options, facts,
                )?),
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
            let (record, report) = ctx_graph_core::sources::add_and_index_observed(
                &root,
                &db,
                &source,
                name.as_deref(),
                &options,
                &capture,
                facts,
            )?;
            return print_value(
                &serde_json::json!({"source":record.source,"path":record.facts.path,"index":report}),
                cli.json,
            );
        }
        Command::CheckUpdate => {
            facts.phase = GraphPhase::Detect;
            let report = index::check_update(&native_root(&db)?, &db)?;
            facts.fresh = Some(report.fresh);
            facts.execution_succeeded = Some(true);
            return print_value(&report, cli.json);
        }
        Command::Compact => {
            let report = Store::open(&db)?.compact()?;
            facts.execution_succeeded = Some(true);
            if cli.json {
                return print_value(&report, true);
            }
            crate::output::stdout(format_args!(
                "Compacted database: {} -> {} of database pages.",
                format_bytes(report.pages_before * report.page_size),
                format_bytes(report.pages_after * report.page_size)
            ))?;
            if report.checkpoint_busy {
                crate::output::stdout(format_args!(
                    "Another connection is delaying disk-space reclamation."
                ))?;
            }
            return Ok(());
        }
        Command::Watch {
            interval_ms,
            iterations,
        } => {
            let startup = std::time::Instant::now();
            let root = native_root(&db)?;
            facts.lifecycle = Some(GraphLifecycle::Ready);
            facts.duration = Some(startup.elapsed());
            facts.polls = Some(0);
            facts.retries = Some(0);
            observation::emit(&observer, *facts);
            loop {
                let started = std::time::Instant::now();
                let mut tick = GraphObservation::new(GraphOperation::Update, facts.invocation);
                tick.phase = GraphPhase::Detect;
                let mut attempted = false;
                let result = (|| -> Result<()> {
                    let fresh = index::check_update(&root, &db)?.fresh;
                    tick.fresh = Some(fresh);
                    tick.detect_duration = Some(started.elapsed());
                    if !fresh {
                        attempted = true;
                        let report = index::run_observed(&root, &db, &mut tick)?;
                        print_output(Output::Index(report), cli.json)?;
                        output::output_result(io::stdout().lock().flush().map_err(Into::into))?;
                        tick.output_boundary = observation::GraphOutputBoundary::CliFlush;
                        tick.output_served = Some(true);
                    }
                    Ok(())
                })();
                if let Err(error) = &result {
                    if error.is::<output::OutputFailure>() {
                        tick.output_served = Some(false);
                        tick.output_failure = Some(observation::failure_kind(error));
                        tick.output_boundary = observation::GraphOutputBoundary::CliFlush;
                    } else {
                        observation::failed(&mut tick, error);
                    }
                } else {
                    tick.execution_succeeded = Some(true);
                }
                tick.duration = Some(started.elapsed());
                if attempted || result.is_err() {
                    observation::emit(&observer, tick);
                }
                facts.polls = facts.polls.map(|n| n.saturating_add(1));
                let pending = match result {
                    Err(error) if retryable_update(&error) => {
                        facts.retries = facts.retries.map(|n| n.saturating_add(1));
                        crate::output::stderr(format_args!(
                            "index is busy or changed concurrently; retrying at the next watch poll"
                        ))?;
                        Some(error)
                    }
                    Err(error) => {
                        facts.lifecycle = Some(GraphLifecycle::Stopped);
                        return Err(error);
                    }
                    Ok(()) => None,
                };
                if iterations
                    .is_some_and(|limit| facts.polls.is_some_and(|polls| polls >= u64::from(limit)))
                {
                    facts.lifecycle = Some(GraphLifecycle::Stopped);
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
                Output::Index(index::run_with_options_observed(
                    Path::new(&root),
                    &db,
                    &options,
                    facts,
                )?),
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
            facts.stats(&stats);
            facts.index = Some(observation::GraphIndexDisposition::Committed);
            facts.execution_succeeded = Some(true);
            return print_output(Output::Stats(stats), cli.json);
        }
        Command::Serve(args) => {
            return tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(mcp::serve_observed(db, args, facts, observer));
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
    let output = read_observed(&db, command, facts)?;
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
            crate::output::stderr(format_args!(
                "ctx graph: query log could not be written; query result is still available"
            ))?;
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
