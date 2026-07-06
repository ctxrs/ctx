#[allow(unused_imports)]
use super::*;

pub(crate) const MAX_SEARCH_LIMIT: usize = 200;

#[derive(Debug, Parser)]
#[command(name = "ctx", version, about = "Search local agent history")]
pub(crate) struct Cli {
    #[arg(long, env = "CTX_DATA_ROOT", global = true)]
    pub(crate) data_root: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: CommandRoot,
}

#[derive(Debug, Args)]
pub(crate) struct SearchArgs {
    #[arg(help = "Natural-language query to search local agent history")]
    pub(crate) query: Option<String>,
    #[arg(
        long,
        help = "Add another search query or keyword; repeat to broaden with OR-style merged results"
    )]
    pub(crate) term: Vec<String>,
    #[arg(
        long,
        default_value_t = 20,
        value_parser = parse_search_limit,
        help = "Maximum results to return, from 1 to 200"
    )]
    pub(crate) limit: usize,
    #[arg(
        long,
        value_parser = parse_provider_arg,
        hide_possible_values = true,
        help = "Search only one provider, for example codex, claude, cursor, pi, copilot-cli, or opencode"
    )]
    pub(crate) provider: Option<ProviderArg>,
    #[arg(
        long = "history-source",
        help = "Filter custom history imports by plugin/source or provider_key/source_id"
    )]
    pub(crate) history_source: Option<String>,
    #[arg(
        long = "provider-key",
        help = "Filter custom history imports by provider_key"
    )]
    pub(crate) provider_key: Option<String>,
    #[arg(
        long = "source-id",
        help = "Filter custom history imports by source_id"
    )]
    pub(crate) source_id: Option<String>,
    #[arg(
        long = "source-format",
        help = "Filter custom history imports by source_format"
    )]
    pub(crate) source_format: Option<String>,
    #[arg(
        long,
        help = "Filter by stored workspace, cwd, source path, or repo-name text"
    )]
    pub(crate) workspace: Option<String>,
    #[arg(
        long,
        help = "Filter to recent history, as RFC3339 or a day window like 30d"
    )]
    pub(crate) since: Option<String>,
    #[arg(
        long,
        hide = true,
        help = "Deprecated alias for the default primary-agent search scope"
    )]
    pub(crate) primary_only: bool,
    #[arg(
        long,
        help = "Include subagent sessions in addition to primary-agent sessions"
    )]
    pub(crate) include_subagents: bool,
    #[arg(
        long,
        help = "Filter by event type: message, tool_call, tool_output, command_started, command_output, command_finished, file_touched, vcs_change, artifact, summary, or notice"
    )]
    pub(crate) event_type: Option<String>,
    #[arg(
        long,
        help = "Filter by indexed touched-file path metadata, not the current filesystem"
    )]
    pub(crate) file: Option<PathBuf>,
    #[arg(
        long,
        help = "Search event hits within one ctx session id or unambiguous id prefix"
    )]
    pub(crate) session: Option<String>,
    #[arg(
        long,
        help = "Return dense event-level results instead of diverse session results"
    )]
    pub(crate) events: bool,
    #[arg(
        long,
        value_enum,
        default_value_t = RefreshArg::Auto,
        help = "Pre-search refresh behavior: auto, off, or strict",
        long_help = "Pre-search refresh behavior. auto best-effort refreshes discovered native provider sources and enabled auto history-source plugins, then serves the existing index if refresh fails; off searches the existing index only; strict fails if the refresh cannot run or import successfully."
    )]
    pub(crate) refresh: RefreshArg,
    #[arg(
        long,
        help = "Include the active Codex session tree when CODEX_THREAD_ID is set"
    )]
    pub(crate) include_current_session: bool,
    #[arg(long, help = "Print machine-readable JSON")]
    pub(crate) json: bool,
    #[arg(
        long,
        help = "Print expanded text details such as full ids, provider ids, citations, and next commands"
    )]
    pub(crate) verbose: bool,
}

pub(crate) struct SearchIntentInput<'a> {
    pub(crate) query: Option<&'a str>,
    pub(crate) terms: &'a [String],
    pub(crate) file: Option<&'a Path>,
}

pub(crate) fn search_has_intent(input: SearchIntentInput<'_>) -> bool {
    input.query.is_some_and(has_search_token)
        || input.terms.iter().any(|term| has_search_token(term))
        || input
            .file
            .and_then(|path| path.to_str())
            .is_some_and(|file| !file.trim().is_empty())
}

pub(crate) fn has_search_token(value: &str) -> bool {
    value.split_whitespace().any(|term| {
        term.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
            .chars()
            .any(char::is_alphanumeric)
    })
}

pub(crate) fn missing_search_intent_error() -> anyhow::Error {
    anyhow!(
        "search needs a query, --term, or --file\n\nTry:\n  ctx search \"failed migration\"\n  ctx search --term \"failed migration\" --term rollback\n  ctx search --file crates/foo/src/lib.rs"
    )
}

pub(crate) fn search_no_results_target(query: &str, terms: &[String]) -> String {
    if !query.trim().is_empty() {
        return shell_quote_arg(query);
    }
    let rendered_terms = terms
        .iter()
        .filter(|term| !term.trim().is_empty())
        .map(|term| format!("--term {}", shell_quote_arg(term)))
        .collect::<Vec<_>>();
    if rendered_terms.is_empty() {
        "search".to_owned()
    } else {
        rendered_terms.join(" ")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SearchRefreshReport {
    pub(crate) mode: RefreshArg,
    pub(crate) status: &'static str,
    pub(crate) source_count: usize,
    pub(crate) totals: ImportTotals,
    pub(crate) error: Option<String>,
}

impl SearchRefreshReport {
    pub(crate) fn skipped(mode: RefreshArg, status: &'static str) -> Self {
        Self {
            mode,
            status,
            source_count: 0,
            totals: ImportTotals::default(),
            error: None,
        }
    }

    pub(crate) fn completed(mode: RefreshArg, source_count: usize, totals: ImportTotals) -> Self {
        Self {
            mode,
            status: "completed",
            source_count,
            totals,
            error: None,
        }
    }

    pub(crate) fn failed(mode: RefreshArg, source_count: usize, error: String) -> Self {
        Self {
            mode,
            status: "failed",
            source_count,
            totals: ImportTotals::default(),
            error: Some(error),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "mode": self.mode.as_str(),
            "status": self.status,
            "source_count": self.source_count,
            "totals": import_totals_json(&self.totals),
            "error": self.error,
        }))
    }
}

pub(crate) struct SearchDto;

pub(crate) fn event_preview(event: &Event) -> String {
    let preview = ctx_history_search::event_preview_text(event);
    if preview.trim().is_empty() {
        format!("{} event", event.event_type.as_str())
    } else {
        ctx_history_search::display_snippet(&preview, 120)
    }
}

pub(crate) fn resolve_session_by_id_text(store: &Store, value: &str) -> Result<Session> {
    if let Ok(id) = Uuid::parse_str(value.trim()) {
        return store.get_session(id).with_context(|| {
            format!("session {id} was not found; rerun the search that found it with `--verbose` to get ctx_session_id")
        });
    }
    let prefix = normalize_uuid_prefix(value, "session")?;
    match store.sessions_by_id_prefix(&prefix)?.as_slice() {
        [session] => Ok(session.clone()),
        [] => Err(anyhow!(
            "session id prefix {prefix:?} was not found; rerun the search that found it with `--verbose` to get ctx_session_id"
        )),
        matches => Err(anyhow!(
            "session id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_session_id",
            matches[0].id,
            matches[1].id
        )),
    }
}

pub(crate) fn resolve_event(store: &Store, value: &str) -> Result<Event> {
    if let Ok(id) = Uuid::parse_str(value.trim()) {
        return store.get_event(id).with_context(|| {
            format!(
                "event {id} was not found; rerun the event search with `--events --verbose` to get ctx_event_id"
            )
        });
    }
    let prefix = normalize_uuid_prefix(value, "event")?;
    match store.events_by_id_prefix(&prefix)?.as_slice() {
        [event] => Ok(event.clone()),
        [] => Err(anyhow!(
            "event id prefix {prefix:?} was not found; rerun the event search with `--events --verbose` to get ctx_event_id"
        )),
        matches => Err(anyhow!(
            "event id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_event_id",
            matches[0].id,
            matches[1].id
        )),
    }
}

impl SearchDto {
    pub(crate) fn packet(
        store: &Store,
        packet: &ctx_history_search::SearchPacket,
        refresh: &SearchRefreshReport,
        suggested_next_query: Option<&str>,
    ) -> Value {
        compact_json(json!({
            "schema_version": packet.schema_version,
            "query": packet.query,
            "filters": packet.filters,
            "freshness": refresh.to_json(),
            "generated_at": packet.generated_at,
            "results": packet
                .results
                .iter()
                .map(|result| {
                    compact_json(json!({
                        "item_id": result.record_id,
                        "item_type": search_result_item_type(store, result),
                        "ctx_event_id": result.event_id,
                        "ctx_session_id": result.session_id,
                        "session_id": result.session_id,
                        "event_id": result.event_id,
                        "event_seq": result.event_seq,
                        "title": result.title,
                        "snippet": result.snippet,
                        "rank": result.rank,
                        "result_scope": result.result_scope,
                        "session_importance": (result.result_scope == ctx_history_search::SearchResultScope::Session)
                            .then_some(result.session_importance),
                        "more_matches_in_session": (result.result_scope == ctx_history_search::SearchResultScope::Session)
                            .then_some(result.more_matches_in_session),
                        "provider": result.provider,
                        "provider_session_id": result.provider_session_id,
                        "history_source": result.history_source,
                        "history_source_plugin": result.history_source_plugin,
                        "provider_key": result.provider_key,
                        "source_id": result.source_id,
                        "source_format": result.source_format,
                        "timestamp": result.timestamp,
                        "cwd": result.cwd,
                        "source_path": result.raw_source_path,
                        "source_exists": result.raw_source_exists,
                        "cursor": result.cursor,
                        "suggested_next_commands": search_next_commands(result, suggested_next_query),
                        "why_matched": result.why_matched,
                        "citations": public_citations(&result.citations),
                        "links": result.links,
                        "visibility": result.visibility,
                    }))
                })
                .collect::<Vec<_>>(),
            "pagination": packet.pagination,
            "truncation": packet.truncation,
        }))
    }
}

pub(crate) fn search_result_item_type(
    store: &Store,
    result: &ctx_history_search::SearchPacketResult,
) -> String {
    if result.result_scope == ctx_history_search::SearchResultScope::Session {
        return "session_result".to_owned();
    }
    if result.event_id == Some(result.record_id) {
        return "event".to_owned();
    }
    if result.session_id == Some(result.record_id) {
        return "session".to_owned();
    }
    item_type_for_id(store, result.record_id)
}

pub(crate) fn search_next_commands(
    result: &ctx_history_search::SearchPacketResult,
    query: Option<&str>,
) -> Vec<String> {
    let mut commands = Vec::new();
    if result.result_scope == ctx_history_search::SearchResultScope::Session {
        if let Some(id) = result.session_id {
            commands.push(format!("ctx show session {id}"));
            if let Some(event_id) = result.event_id {
                commands.push(format!("ctx show event {event_id} --window 10"));
            }
            if let Some(query) = query.filter(|query| !query.trim().is_empty()) {
                commands.push(format!(
                    "ctx search {} --session {id}",
                    shell_quote_arg(query)
                ));
            }
            commands.push(format!("ctx locate session {id}"));
            if let Some(event_id) = result.event_id {
                commands.push(format!("ctx locate event {event_id}"));
            }
        }
        return commands;
    }
    if let Some(id) = result.event_id {
        commands.push(format!("ctx show event {id} --window 10"));
        commands.push(format!("ctx locate event {id}"));
    }
    if result.result_scope != ctx_history_search::SearchResultScope::Session {
        if let Some(id) = result.session_id {
            if let Some(query) = query.filter(|query| !query.trim().is_empty()) {
                commands.push(format!(
                    "ctx search {} --session {id}",
                    shell_quote_arg(query)
                ));
            }
            commands.push(format!("ctx show session {id}"));
            commands.push(format!("ctx locate session {id}"));
        }
    }
    commands
}

pub(crate) fn parse_search_limit(value: &str) -> std::result::Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|err| format!("invalid search limit: {err}"))?;
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(format!(
            "search limit must be between 1 and {MAX_SEARCH_LIMIT}"
        ));
    }
    Ok(limit)
}

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    if !search_has_intent(SearchIntentInput {
        query: args.query.as_deref(),
        terms: &args.term,
        file: args.file.as_deref(),
    }) {
        return Err(missing_search_intent_error());
    }

    let db_path = database_path(data_root.clone());
    let had_existing_store = db_path.exists();
    let refresh_started = Instant::now();
    let refresh = refresh_before_search(&args, &data_root)?;
    analytics::insert_duration(
        analytics_properties,
        "refresh_duration",
        refresh_started.elapsed(),
    );
    analytics::insert_str(
        analytics_properties,
        "search_refresh_mode",
        refresh.mode.as_str(),
    );
    analytics::insert_str(
        analytics_properties,
        "search_refresh_status",
        refresh.status,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "search_refresh_source_count_bucket",
        refresh.source_count as u64,
    );
    insert_db_size_bucket(analytics_properties, &db_path);
    if refresh.status == "failed" && args.refresh == RefreshArg::Auto && !had_existing_store {
        return Err(anyhow!(
            "search refresh failed and no existing ctx index is available; run `ctx import` first or retry with `--refresh strict`: {}",
            refresh.error.as_deref().unwrap_or("unknown refresh error")
        ));
    }
    let store = if args.refresh == RefreshArg::Off
        || refresh.status == "failed"
        || refresh.status == "completed"
        || had_existing_store
    {
        open_existing_store_read_only(&db_path, "ctx search")?
    } else {
        Store::open(&db_path)?
    };
    insert_store_analytics_counts(analytics_properties, &store)?;
    let source_identity = SourceIdentityFilterArgs::from(&args);
    let query = args.query.unwrap_or_default();
    let query_term_count = query
        .split_whitespace()
        .filter(|term| !term.trim().is_empty())
        .count()
        .saturating_add(
            args.term
                .iter()
                .filter(|term| !term.trim().is_empty())
                .count(),
        );
    analytics::insert_text_length_bucket(
        analytics_properties,
        "query_length_bucket",
        query.chars().count(),
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "query_term_count_bucket",
        query_term_count as u64,
    );
    let event_results = args.events || args.session.is_some();
    let options = ctx_history_search::PacketOptions {
        limit: args.limit,
        filters: search_filters(
            SearchFilterInput {
                session: args.session,
                provider: args.provider,
                source_identity,
                workspace: args.workspace.clone(),
                since: args.since.clone(),
                primary_only: args.primary_only,
                include_subagents: args.include_subagents,
                event_type: args.event_type.clone(),
                file: args.file.clone(),
                include_current_session: args.include_current_session,
            },
            Some(&store),
        )?,
        result_mode: if event_results {
            ctx_history_search::SearchResultMode::Events
        } else {
            ctx_history_search::SearchResultMode::Sessions
        },
        ..ctx_history_search::PacketOptions::default()
    };
    let uses_composed_terms = args.term.iter().any(|term| !term.trim().is_empty());
    let query_started = Instant::now();
    let packet = if uses_composed_terms {
        ctx_history_search::search_packet_terms(&store, &query, &args.term, &options)?
    } else {
        ctx_history_search::search_packet(&store, &query, &options)?
    };
    analytics::insert_duration(
        analytics_properties,
        "query_duration",
        query_started.elapsed(),
    );
    let result_count = packet.results.len();
    let citation_count = packet
        .results
        .iter()
        .map(|result| result.citations.len())
        .sum::<usize>();
    analytics::insert_count_bucket(
        analytics_properties,
        "result_count_bucket",
        result_count as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "citation_count_bucket",
        citation_count as u64,
    );
    analytics::insert_bool(analytics_properties, "zero_result", result_count == 0);
    let render_started = Instant::now();
    if args.json {
        let suggested_next_query = (!uses_composed_terms).then_some(query.as_str());
        print_share_safe_value(SearchDto::packet(
            &store,
            &packet,
            &refresh,
            suggested_next_query,
        ))?;
    } else {
        if refresh.status == "failed" && args.refresh == RefreshArg::Auto {
            if let Some(error) = &refresh.error {
                eprintln!(
                    "warning: search refresh failed; serving existing index; use --refresh strict to fail instead: {error}"
                );
            }
        }
        if packet.results.is_empty() {
            if let Some(file) = args
                .file
                .as_deref()
                .filter(|_| query.trim().is_empty() && !uses_composed_terms)
            {
                println!("no indexed events touched {}", file.display());
                let indexed_items = indexed_history_item_count(&store)?;
                if indexed_items == 0 {
                    println!("next: ctx import --all");
                } else {
                    println!(
                        "next: ctx search {}",
                        shell_quote_arg(&file.display().to_string())
                    );
                }
            } else {
                println!(
                    "no results for {}",
                    search_no_results_target(&query, &args.term)
                );
                let indexed_items = indexed_history_item_count(&store)?;
                if indexed_items == 0 {
                    println!("next: ctx import --all");
                } else {
                    println!("next: try broader terms with ctx search --term \"<term>\"");
                }
            }
        }
        let suggested_next_query = (!uses_composed_terms).then_some(query.as_str());
        for (index, result) in packet.results.iter().enumerate() {
            if args.verbose {
                print_search_result_verbose(result, suggested_next_query);
            } else {
                print_search_result_compact(index + 1, result);
            }
        }
    }
    analytics::insert_duration(
        analytics_properties,
        "render_duration",
        render_started.elapsed(),
    );
    Ok(())
}
