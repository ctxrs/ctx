use super::*;

const RESOURCES: &[(&str, &str)] = &[
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

fn community_listing(snapshot: &CachedSnapshot, computed: bool) -> Result<Value> {
    let preserved: Vec<_> = snapshot
        .preserved
        .iter()
        .map(|c| json!({"id":c.id,"project":c.project,"names":c.names,"nodes":c.nodes.len()}))
        .collect();
    let mut output = json!({"schema_version":snapshot.snapshot.schema_version,
        "generation":snapshot.snapshot.generation,
        "community_source":"preserved", "communities":preserved,
        "preserved_communities":preserved, "computed_communities":null,
        "computed_status":"not_requested",
        "computed_resource":"computed-communities",
        "preserved_methodology":"Recorded memberships; integer/string IDs and composition paths remain distinct."});
    if computed || snapshot.preserved.is_empty() {
        let analysis = snapshot.analysis()?;
        let groups: Vec<_> = analysis
            .communities
            .iter()
            .map(|c| json!({"id":c.id,"nodes":c.nodes.len(),"cohesion":c.cohesion}))
            .collect();
        output["community_source"] = json!("computed");
        output["communities"] = json!(groups);
        output["computed_communities"] = json!(groups);
        output["computed_status"] = json!("computed");
        output["methodology"] = json!(analysis.methodology);
    }
    Ok(output)
}

fn resource_text(project: &Project, kind: &str) -> Result<String> {
    if kind == "stats" {
        return Ok(serde_json::to_string(
            &Store::open_read_only(&project.db)?.stats()?,
        )?);
    }
    let d = project.snapshot()?;
    // Snapshot-only paths never initialize structural analysis.
    let value = match kind {
        "graph" => serde_json::to_value(&d.snapshot)?,
        "communities" => community_listing(&d, false)?,
        "computed-communities" => community_listing(&d, true)?,
        "report" => {
            d.check_analysis_limits()?;
            return d
                .report
                .get_or_init(|| {
                    export::render(&d.snapshot, ExportFormat::Markdown)
                        .map_err(|e| format!("{e:#}"))
                })
                .clone()
                .map_err(anyhow::Error::msg);
        }
        "god-nodes" => hubs(&d, 10, None)?,
        "audit" => graph_stats(&d)?,
        "surprises" => {
            let a = d.analysis()?;
            json!({"schema_version":a.schema_version,"generation":a.generation,
                "surprises":a.surprises,"surprise_candidates":a.surprise_candidates,
                "truncated":a.surprise_candidates > a.surprises.len(),
                "methodology":a.methodology})
        }
        "questions" => {
            let a = d.analysis()?;
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

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListResourcesResult, ErrorData> {
        if request.is_some_and(|r| r.cursor.is_some()) {
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
        Ok(ListResourcesResult {
            resources,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
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
        let content_type = mime(kind);
        let kind = kind.to_owned();
        let text = self
            .run(Some(project.to_owned()), move |p| {
                let text = resource_text(p, &kind)?;
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
