use super::*;

pub(super) const RESOURCES: &[(&str, &str)] = &[
    ("stats", "Snapshot counts and diagnostics"),
    ("graph", "Complete snapshot JSON, maximum 1 MiB"),
    (
        "report",
        "Offline structural Markdown report, maximum 1 MiB",
    ),
    ("god-nodes", "Ten highest-degree nodes"),
    (
        "computed-communities",
        "Explicit structural community analysis, capped at 5000 nodes",
    ),
    (
        "communities",
        "Preserved typed community IDs/composition paths and computed structural communities",
    ),
    (
        "surprises",
        "Up to five scored structural connections with signals and full recorded edge evidence",
    ),
    ("audit", "Confidence counts and percentages"),
    (
        "questions",
        "Up to seven evidence-backed questions with rationale and candidate counts",
    ),
];

fn community_listing(
    snapshot: &CachedSnapshot,
    computed: bool,
    facts: &mut GraphObservation,
) -> Result<Value> {
    let preserved: Vec<_> = snapshot
        .preserved
        .iter()
        .map(|c| json!({"id":c.id,"project":c.project,"names":c.names,"nodes":c.nodes.len()}))
        .collect();
    facts.result_count = Some(preserved.len() as u64);
    let mut output = json!({"schema_version":snapshot.snapshot.schema_version,
        "generation":snapshot.snapshot.generation,
        "community_source":"preserved", "communities":preserved,
        "preserved_communities":preserved, "computed_communities":null,
        "computed_status":"not_requested",
        "computed_resource":"computed-communities",
        "preserved_methodology":"Recorded memberships; integer/string IDs and composition paths remain distinct."});
    if computed || snapshot.preserved.is_empty() {
        let analysis = snapshot.analysis(facts)?;
        let groups: Vec<_> = analysis
            .communities
            .iter()
            .map(|c| json!({"id":c.id,"nodes":c.nodes.len(),"cohesion":c.cohesion}))
            .collect();
        facts.result_count = Some(groups.len() as u64);
        output["community_source"] = json!("computed");
        output["communities"] = json!(groups);
        output["computed_communities"] = json!(groups);
        output["computed_status"] = json!("computed");
        output["methodology"] = json!(analysis.methodology);
    }
    Ok(output)
}

fn resource_text(project: &Project, kind: &str, facts: &mut GraphObservation) -> Result<String> {
    if kind == "stats" {
        let stats = Store::open_read_only(&project.db)?.stats()?;
        facts.stats(&stats);
        return Ok(serde_json::to_string(&stats)?);
    }
    let d = project.snapshot(facts)?;
    // Snapshot-only paths never initialize structural analysis.
    let value = match kind {
        "graph" => serde_json::to_value(&d.snapshot)?,
        "communities" => community_listing(&d, false, facts)?,
        "computed-communities" => community_listing(&d, true, facts)?,
        "report" => {
            d.check_analysis_limits(facts)?;
            return d
                .report
                .get_or_init(|| {
                    export::render(&d.snapshot, ExportFormat::Markdown)
                        .map_err(|e| format!("{e:#}"))
                })
                .clone()
                .map_err(anyhow::Error::msg);
        }
        "god-nodes" => hubs(&d, 10, None, facts)?,
        "audit" => graph_stats(&d, facts)?,
        "surprises" => {
            let a = d.analysis(facts)?;
            facts.result_count = Some(a.surprises.len() as u64);
            facts.truncated = Some(a.surprise_candidates > a.surprises.len());
            json!({"schema_version":a.schema_version,"generation":a.generation,
                "surprises":a.surprises,"surprise_candidates":a.surprise_candidates,
                "truncated":a.surprise_candidates > a.surprises.len(),
                "methodology":a.methodology})
        }
        "questions" => {
            let a = d.analysis(facts)?;
            facts.result_count = Some(a.suggested_questions.len() as u64);
            facts.truncated = Some(a.suggested_question_candidates > a.suggested_questions.len());
            json!({"schema_version":a.schema_version,"generation":a.generation,
                "questions":a.suggested_questions,
                "suggested_question_candidates":a.suggested_question_candidates,
                "truncated":a.suggested_question_candidates > a.suggested_questions.len(),
                "methodology":a.methodology})
        }
        _ => bail!("unknown resource"),
    };
    Ok(serde_json::to_string(&value)?)
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Graf {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(Implementation::new("ctx-graph", env!("CARGO_PKG_VERSION")))
            .with_instructions("ctx graph reads only explicitly registered local databases. Optional tool project selects a registry name, never a path. Graph queries/resources do not check worktree freshness, refresh, launch processes, or make network/model calls. PR tools are available only with --github-repo and invoke bounded read-only GitHub commands only when explicitly called; repo must match the configured repository, and project selects a registered database. No PR tool invokes a model or inspects local worktrees. Run ctx graph index or ctx graph update explicitly outside MCP. Generation identifies each snapshot. SQL queries: depth 0..6, limit 1..500, at most 20 seeds/5000 examined edges. Snapshot cache: at most 100000 nodes/1000000 edges/1000000 unresolved references per registered project; payload bytes default to 64 MiB, configured by --snapshot-max-bytes. Preserved-community reads do not cluster; computed communities require explicit selection when recorded memberships exist. Structural analysis remains capped at 5000 nodes/20000 edges/20000 unresolved references and 8 MiB snapshot. Resource responses at most 1 MiB, structured tool payloads 512 KiB. Four concurrent read workers. Resources enumerate registered project names; graphify:// aliases select default. HTTP is stateless Streamable HTTP at the configured path; browser Origins are rejected. Input messages at most 1 MiB. Inspect truncation and unresolved references.")
    }

    async fn initialize(
        &self,
        request: rmcp::model::InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<rmcp::model::InitializeResult, ErrorData> {
        context.peer.set_peer_info(request.clone());
        let result = self.negotiate_initialize(&request);
        if let Some(observation) = RequestObservation::from_context(&context) {
            observation.update(|f| {
                f.operation = GraphOperation::Initialize;
                f.execution_succeeded = Some(result.is_ok());
                if result.is_err() {
                    f.fail(GraphFailureKind::Protocol);
                }
            });
        }
        result
    }

    async fn ping(
        &self,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<(), ErrorData> {
        if let Some(observation) = RequestObservation::from_context(&context) {
            observation.update(|f| {
                f.operation = GraphOperation::Ping;
                f.execution_succeeded = Some(true);
            });
        }
        Ok(())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<rmcp::model::ListToolsResult, ErrorData> {
        // Same negotiated cache hints as rmcp 3.4.0's tool_handler expansion.
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|v| v >= rmcp::model::ProtocolVersion::V_2026_07_28);
        let tools = self.tool_router.list_all();
        if let Some(observation) = RequestObservation::from_context(&context) {
            observation.update(|f| {
                f.operation = GraphOperation::ToolsList;
                f.execution_succeeded = Some(true);
                f.result_count = Some(tools.len() as u64);
            });
        }
        Ok(rmcp::model::ListToolsResult {
            result_type: Some(rmcp::model::ResultType::COMPLETE),
            tools,
            meta: None,
            next_cursor: None,
            ttl_ms: supports_cache_hints.then_some(0),
            cache_scope: supports_cache_hints.then_some(rmcp::model::CacheScope::Public),
        })
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListResourcesResult, ErrorData> {
        let observation = RequestObservation::from_context(&context);
        if let Some(observation) = &observation {
            observation.update(|f| f.operation = GraphOperation::ResourcesList);
        }
        if request.is_some_and(|r| r.cursor.is_some()) {
            if let Some(observation) = &observation {
                observation.update(|f| f.fail(GraphFailureKind::InvalidInput));
            }
            return Err(ErrorData::invalid_params(
                "resources are returned in one page; cursor is not supported",
                None,
            ));
        }
        let mut resources = Vec::new();
        for (kind, description) in RESOURCES {
            for prefix in ["graf://", "graphify://"] {
                resources.push(
                    Resource::new(format!("{prefix}{kind}"), format!("default/{kind}"))
                        .with_description(*description)
                        .with_mime_type(mime(kind)),
                );
            }
            for name in self.projects.keys() {
                resources.push(
                    Resource::new(
                        format!("graf://projects/{name}/{kind}"),
                        format!("{name}/{kind}"),
                    )
                    .with_description(*description)
                    .with_mime_type(mime(kind)),
                );
            }
        }
        if let Some(observation) = &observation {
            observation.update(|f| {
                f.execution_succeeded = Some(true);
                f.result_count = Some(resources.len() as u64);
            });
        }
        Ok(ListResourcesResult {
            resources,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<ReadResourceResponse, ErrorData> {
        let mut handler = self.clone();
        handler.observation = RequestObservation::from_context(&context);
        let result = handler.read_resource_observed(request).await;
        if let Some(observation) = &handler.observation {
            observation.update(|f| {
                f.execution_succeeded = Some(result.is_ok());
                if result.is_err() && f.failure.is_none() {
                    f.fail(GraphFailureKind::InvalidInput);
                }
            });
        }
        result
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<rmcp::model::CallToolResponse, ErrorData> {
        let mut handler = self.clone();
        handler.observation = RequestObservation::from_context(&context);
        if let Some(observation) = &handler.observation {
            observation.update(|f| {
                f.operation = observation::tool_operation(&request.name);
                f.phase = GraphPhase::Prepare;
            });
        }
        let call = rmcp::handler::server::tool::ToolCallContext::new(&handler, request, context);
        let result = handler.tool_router.call(call).await;
        if let Some(observation) = &handler.observation {
            observation.update(|f| {
                let failed = match &result {
                    Err(_) => true,
                    Ok(rmcp::model::CallToolResponse::Complete(result)) => {
                        result.is_error == Some(true)
                    }
                    _ => return,
                };
                f.execution_succeeded = Some(!failed);
                if failed && f.failure.is_none() {
                    f.fail(if result.is_err() {
                        GraphFailureKind::InvalidInput
                    } else {
                        GraphFailureKind::Unknown
                    });
                }
            });
        }
        result
    }
}

impl Graf {
    async fn read_resource_observed(
        &self,
        request: ReadResourceRequestParams,
    ) -> std::result::Result<ReadResourceResponse, ErrorData> {
        let uri = request.uri;
        let (project, kind) = if let Some(path) = uri.strip_prefix("graf://projects/") {
            path.split_once('/')
                .ok_or_else(|| ErrorData::invalid_params("invalid resource URI", None))?
        } else if let Some(kind) = uri
            .strip_prefix("graf://")
            .or_else(|| uri.strip_prefix("graphify://"))
        {
            ("default", kind)
        } else {
            return Err(ErrorData::resource_not_found("unknown resource URI", None));
        };
        if !RESOURCES.iter().any(|(key, _)| *key == kind) {
            return Err(ErrorData::resource_not_found("unknown resource URI", None));
        }
        if let Some(observation) = &self.observation {
            observation.update(|f| f.operation = observation::resource_operation(kind));
        }
        let content_type = mime(kind);
        let kind = kind.to_owned();
        let text = self
            .run(Some(project.to_owned()), move |p, facts| {
                let text = resource_text(p, &kind, facts)?;
                if text.len() > MAX_MESSAGE {
                    facts.phase = GraphPhase::Render;
                    facts.fail(GraphFailureKind::ResponseLimit);
                }
                ensure!(
                    text.len() <= MAX_MESSAGE,
                    "resource exceeds 1 MiB; use CLI export"
                );
                Ok(text)
            })
            .await
            .map_err(|e| ErrorData::invalid_params(format!("{e:#}"), None))?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri).with_mime_type(content_type),
        ])
        .into())
    }
}

fn mime(kind: &str) -> &'static str {
    if kind == "report" {
        "text/markdown"
    } else {
        "application/json"
    }
}
