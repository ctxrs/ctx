#[path = "tool_catalog.rs"]
mod tool_catalog;

fn removed_lsp_tool_message(name: &str) -> Option<String> {
    if name.starts_with("lsp_")
        || matches!(
            name,
            "list_edit_plans" | "get_edit_plan" | "apply_edit_plan" | "discard_edit_plan"
        )
    {
        return Some(format!(
            "tool removed: {name} (this daemon no longer supports that legacy MCP tool; recover the last implementation from commit 795129c6a if needed)"
        ));
    }
    None
}

fn agent_scoped_tool_block_message(name: &str) -> Option<String> {
    if matches!(name, "list_workspaces" | "oracle") {
        return Some(format!(
            "tool removed: {name} (ctx-mcp is agent-only and only exposes session/worktree-local tools)"
        ));
    }
    None
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    if !cli.stdio {
        anyhow::bail!("only --stdio transport is implemented");
    }
    let server_version =
        build_identity::current_build_version().context("loading ctx-mcp build identity")?;

    let daemon_url = ctx_env("DAEMON_URL").unwrap_or_else(|_| "http://127.0.0.1:4399".to_string());
    let client = reqwest::Client::new();
    let mut cached_context: Option<ResolvedMcpContext> = None;

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut out = tokio::io::BufWriter::new(stdout);

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("invalid json: {e}");
                continue;
            }
        };

        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        // Notifications have no response.
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };

        let response = match method {
            "ping" => ok(id.clone(), json!({})),
            "initialize" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let protocol_version = params
                    .get("protocolVersion")
                    .cloned()
                    .unwrap_or_else(|| json!("2025-11-25"));
                ok(
                    id.clone(),
                    json!({
                        "protocolVersion": protocol_version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": {
                            "name": "ctx-mcp",
                            "title": "ctx MCP",
                            "version": server_version.clone(),
                            "description": "ctx daemon tools (bridge)"
                        }
                    }),
                )
            }
            "tools/list" => match cached_mcp_context(&client, &mut cached_context).await {
                Ok(context) => ok(
                    id.clone(),
                    tool_catalog::tools_list_response(
                        tool_catalog::ToolCatalogCapabilities::from_mcp_context(&context),
                    ),
                ),
                Err(e) => error(
                    id.clone(),
                    -32000,
                    "ctx-mcp context unavailable",
                    Some(json!({"error": e.to_string()})),
                ),
            },
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let raw_name = name.to_string();
                // Tool names must be [a-zA-Z0-9_-] to satisfy Codex MCP validation.
                // Accept ctx_* and ctx.* aliases but normalize to unprefixed underscore names.
                let name = if let Some(rest) = raw_name.strip_prefix("ctx.") {
                    rest.replace('.', "_")
                } else if let Some(rest) = raw_name.strip_prefix("ctx_") {
                    rest.to_string()
                } else {
                    raw_name
                };

                let context = match cached_mcp_context(&client, &mut cached_context).await {
                    Ok(context) => context,
                    Err(e) => {
                        write_response(
                            &mut out,
                            error(
                                id.clone(),
                                -32000,
                                "ctx-mcp context unavailable",
                                Some(json!({"error": e.to_string()})),
                            ),
                        )
                        .await?;
                        continue;
                    }
                };
                let tool_capabilities =
                    tool_catalog::ToolCatalogCapabilities::from_mcp_context(&context);
                if !dev_tools_enabled() && name.as_str() == "ping" {
                    ok(
                        id.clone(),
                        tool_err(anyhow::anyhow!(
                            "tool disabled: {name} (ping is dev-only; set CTX_MCP_DEV_MODE=1 to enable)"
                        )),
                    )
                } else if let Some(message) =
                    tool_capabilities.disabled_tool_message(name.as_str())
                {
                    ok(id.clone(), tool_err(anyhow::anyhow!(message)))
                } else if let Some(message) = agent_scoped_tool_block_message(name.as_str()) {
                    ok(id.clone(), tool_err(anyhow::anyhow!(message)))
                } else if let Some(message) = removed_lsp_tool_message(name.as_str()) {
                    ok(id.clone(), tool_err(anyhow::anyhow!(message)))
                } else {
                    let mut arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                    if let Some(tool_call_id) = tool_call_id_from_params(&params) {
                        if let Some(obj) = arguments.as_object_mut() {
                            obj.insert("tool_call_id".to_string(), Value::String(tool_call_id));
                        } else {
                            arguments = json!({ "tool_call_id": tool_call_id });
                        }
                    }
                    match name.as_str() {
                        "ping" => ok(
                            id.clone(),
                            json!({
                                "content": [{"type":"text","text": "{\"ok\":true}"}],
                                "isError": false
                            }),
                        ),
                        "merge_queue_submit" => {
                            match merge_queue_submit_call(
                                &client,
                                &daemon_url,
                                &context,
                                &arguments,
                            )
                            .await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "spawn_agent" => {
                            match spawn_agent_call(&client, &daemon_url, &context, &arguments).await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "send_input" => {
                            match send_input_call(&client, &daemon_url, &context, &arguments).await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "archive_agent" => {
                            match archive_agent_call(&client, &daemon_url, &context, &arguments)
                                .await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "wait_agent" => {
                            match wait_agent_call(&client, &daemon_url, &context, &arguments).await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "interrupt_agent" => {
                            match interrupt_agent_call(&client, &daemon_url, &context, &arguments)
                                .await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "list_agents" => {
                            match list_agents_call(&client, &daemon_url, &context).await {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "get_agent" => match get_agent_call(
                            &client,
                            &daemon_url,
                            &context,
                            &arguments,
                        )
                        .await
                        {
                            Ok(val) => ok(id.clone(), tool_ok(val)),
                            Err(e) => ok(id.clone(), tool_err(e)),
                        },
                        "artifacts_set" => {
                            let normalized = (|| -> std::result::Result<Vec<Value>, Value> {
                                    let items = arguments
                                        .get("artifacts")
                                        .and_then(|v| v.as_array())
                                        .ok_or_else(|| {
                                            error(
                                                id.clone(),
                                                -32602,
                                                "Invalid params",
                                                Some(json!({"missing":"artifacts"})),
                                            )
                                        })?;

                                    let mut normalized = Vec::with_capacity(items.len());
                                    for (idx, item) in items.iter().enumerate() {
                                        let obj = item.as_object().ok_or_else(|| {
                                        error(
                                            id.clone(),
                                            -32602,
                                            "Invalid params",
                                            Some(json!({"index": idx, "message": "artifact must be an object"})),
                                        )
                                    })?;
                                        let abs = obj
                                            .get("absoluteFilePath")
                                            .or_else(|| obj.get("absolute_file_path"))
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string());
                                        let absolute_file_path = abs
                                        .filter(|s| !s.trim().is_empty())
                                        .ok_or_else(|| {
                                        error(
                                            id.clone(),
                                            -32602,
                                            "Invalid params",
                                            Some(
                                                json!({"index": idx, "missing":"absoluteFilePath"}),
                                            ),
                                        )
                                    })?;
                                        let name = obj
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string());
                                        let mime_type = obj
                                            .get("mimeType")
                                            .or_else(|| obj.get("mime_type"))
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string());

                                        normalized.push(json!({
                                            "absolute_file_path": absolute_file_path,
                                            "name": name,
                                            "mime_type": mime_type,
                                        }));
                                    }

                                    Ok(normalized)
                                })();

                            match normalized {
                                Ok(normalized) => {
                                    match set_artifacts(
                                        &client,
                                        &daemon_url,
                                        &context.session_id,
                                        normalized,
                                    )
                                    .await
                                    {
                                        Ok(val) => ok(id.clone(), tool_ok(val)),
                                        Err(e) => ok(id.clone(), tool_err(e)),
                                    }
                                }
                                Err(err) => err,
                            }
                        }
                        // TODO: Re-enable web session MCP tool handlers.
                        /*
                        "session_create" => {
                            match web_sessions::session_create_call(&client, &daemon_url, &arguments)
                                .await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "session_list" => {
                            match web_sessions::session_list_call(&client, &daemon_url, &arguments)
                                .await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "session_info" => {
                            match web_sessions::session_info_call(&client, &daemon_url, &arguments)
                                .await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "session_run" => {
                            match web_sessions::session_run_call(
                                &client,
                                &daemon_url,
                                &arguments,
                                false,
                            )
                            .await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "session_eval" => {
                            match web_sessions::session_run_call(
                                &client,
                                &daemon_url,
                                &arguments,
                                true,
                            )
                            .await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        "session_close" => {
                            match web_sessions::session_close_call(&client, &daemon_url, &arguments)
                                .await
                            {
                                Ok(val) => ok(id.clone(), tool_ok(val)),
                                Err(e) => ok(id.clone(), tool_err(e)),
                            }
                        }
                        */
                        _ => error(
                            id.clone(),
                            -32601,
                            "Method not found",
                            Some(json!({"tool": name})),
                        ),
                    }
                }
            }
            _ => error(
                id.clone(),
                -32601,
                "Method not found",
                Some(json!({"method": method})),
            ),
        };

        write_response(&mut out, response).await?;
    }

    Ok(())
}
