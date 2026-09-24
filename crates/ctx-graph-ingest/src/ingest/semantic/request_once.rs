use super::*;

pub(super) fn request_once(
    s: &SemanticOptions,
    text: &str,
    image: Option<(&str, &str)>,
    budget: &mut RequestBudget,
    usage: &mut ProviderUsage,
) -> Result<String> {
    let instructions = if image.is_some() {
        INSTRUCTIONS.replace("Evidence must be a nonempty verbatim substring of the document.", "Evidence must describe a specific visible region of the image; do not claim text verification.")
    } else {
        INSTRUCTIONS.into()
    };
    let prompt = text.to_owned();
    if matches!(s.provider, Provider::Bedrock | Provider::ClaudeCli) {
        return cli_family(s, &instructions, &prompt, image, budget, usage);
    }
    if s.provider == Provider::Cli {
        let mut payload = json!({"model":s.model,"instructions":instructions,"input":text,"max_output_tokens":s.max_output_tokens,"image":image.map(|(mime,data)|json!({"mime_type":mime,"base64":data}))});
        apply_controls(&mut payload, s);
        let payload = serde_json::to_vec(&payload)?;
        let mut adapter = s.command.clone().context("CLI adapter missing")?;
        adapter.args = adapter
            .args
            .iter()
            .map(|a| a.replace("{model}", &s.model))
            .collect();
        reserve_calls(s, budget, 1)?;
        return String::from_utf8(
            super::super::convert::run_bytes_until(
                &adapter,
                None,
                Some(&payload),
                attempt_deadline(s, budget),
                s.max_response_bytes,
                &[],
            )
            .map_err(command_error)?,
        )
        .context("CLI provider output is not UTF-8");
    }
    let messages =
        json!([{"role":"system","content":instructions},{"role":"user","content":prompt}]);
    let mut body = match s.provider {
        Provider::OpenAi | Provider::Azure => {
            json!({"model":s.model,"messages":messages,"max_completion_tokens":s.max_output_tokens,
            "response_format":{"type":"json_object"},"stream":false})
        }
        Provider::Anthropic => {
            json!({"model":s.model,"system":instructions,"messages":[{"role":"user","content":prompt}],"max_tokens":s.max_output_tokens,"stream":false})
        }
        Provider::Gemini => {
            json!({"systemInstruction":{"parts":[{"text":instructions}]},"contents":[{"role":"user","parts":[{"text":prompt}]}],
            "generationConfig":{"responseMimeType":"application/json","maxOutputTokens":s.max_output_tokens}})
        }
        Provider::Ollama => {
            json!({"model":s.model,"messages":messages,"stream":false,"format":"json","options":{"num_predict":s.max_output_tokens}})
        }
        Provider::Cli | Provider::Bedrock | Provider::ClaudeCli => unreachable!(),
    };
    if let Some((mime, data)) = image {
        match s.provider {
            Provider::OpenAi | Provider::Azure => {
                body["messages"][1]["content"] = json!([
                {"type":"text","text":prompt},
                {"type":"image_url","image_url":{"url":format!("data:{mime};base64,{data}"),"detail":"low"}}])
            }
            Provider::Anthropic => {
                body["messages"][0]["content"] = json!([
                {"type":"text","text":prompt},
                {"type":"image","source":{"type":"base64","media_type":mime,"data":data}}])
            }
            Provider::Gemini => {
                body["contents"][0]["parts"] = json!([
                {"text":prompt},{"inlineData":{"mimeType":mime,"data":data}}])
            }
            Provider::Ollama => body["messages"][1]["images"] = json!([data]),
            Provider::Cli | Provider::Bedrock | Provider::ClaudeCli => unreachable!(),
        }
    }
    apply_controls(&mut body, s);
    let client = reqwest::blocking::Client::builder()
        .timeout(attempt_deadline(s, budget).saturating_duration_since(Instant::now()))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut endpoint = s.endpoint.clone();
    // Gemini's model is part of its generateContent route, never a body field.
    if s.provider == Provider::Gemini {
        ensure!(
            s.model
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')),
            "Gemini model contains invalid route characters"
        );
        endpoint = endpoint.replace("{model}", &s.model);
    }
    let mut request = client
        .post(super::super::safe_url(&endpoint, true)?)
        .json(&body);
    if s.provider == Provider::Anthropic {
        request = request.header("anthropic-version", "2023-06-01");
    }
    if let Some(name) = &s.key_env {
        let key = std::env::var(name)
            .map_err(|_| anyhow::anyhow!("semantic provider key environment variable is unset"))?;
        ensure!(
            !key.is_empty(),
            "semantic provider key environment variable is empty"
        );
        request = match s.provider {
            Provider::Anthropic => request.header("x-api-key", key),
            Provider::Gemini => request.header("x-goog-api-key", key),
            Provider::Azure => request.header("api-key", key),
            _ => request.bearer_auth(key),
        };
    }
    // Do not put URLs, response bodies, or credentials into diagnostics.
    reserve_calls(s, budget, 1)?;
    let response = request.send().map_err(|error| {
        if error.is_timeout() {
            anyhow::Error::new(Recovery::Timeout)
        } else {
            anyhow::anyhow!("semantic provider request failed")
        }
    })?;
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .take(s.max_response_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            if io_timeout(&error) {
                anyhow::Error::new(Recovery::Timeout)
            } else {
                anyhow::anyhow!("semantic provider response read failed")
            }
        })?;
    ensure!(
        bytes.len() <= s.max_response_bytes,
        "semantic provider response exceeds byte limit"
    );
    let parsed = serde_json::from_slice::<Value>(&bytes);
    if let Ok(value) = &parsed {
        read_usage(usage, value);
    }
    if status.as_u16() == 429 || status.is_server_error() {
        return Err(Recovery::Transient.into());
    }
    if !status.is_success() {
        if matches!(status.as_u16(), 400 | 413 | 422)
            && parsed.as_ref().is_ok_and(known_context_overflow)
        {
            return Err(Recovery::ContextOverflow.into());
        }
        anyhow::bail!("semantic provider returned HTTP {status}");
    }
    let response = parsed.context("invalid semantic provider JSON response")?;
    let content = match s.provider {
        Provider::OpenAi | Provider::Azure => {
            if response["choices"][0]["finish_reason"] == "length" {
                return Err(Truncated.into());
            }
            ensure!(
                response["choices"][0]["finish_reason"] == "stop",
                "semantic provider response incomplete/refused"
            );
            response["choices"][0]["message"]["content"]
                .as_str()
                .map(str::to_owned)
        }
        Provider::Anthropic => {
            if response["stop_reason"] == "max_tokens" {
                return Err(Truncated.into());
            }
            ensure!(
                response["stop_reason"] == "end_turn",
                "Anthropic response incomplete/refused"
            );
            response["content"].as_array().map(|parts| {
                parts
                    .iter()
                    .filter(|p| p["type"] == "text")
                    .filter_map(|p| p["text"].as_str())
                    .collect()
            })
        }
        Provider::Gemini => {
            if response["candidates"][0]["finishReason"] == "MAX_TOKENS" {
                return Err(Truncated.into());
            }
            ensure!(
                response["candidates"][0]["finishReason"] == "STOP",
                "Gemini response incomplete/refused"
            );
            response["candidates"][0]["content"]["parts"]
                .as_array()
                .map(|parts| {
                    parts
                        .iter()
                        .filter(|p| p["thought"] != true)
                        .filter_map(|p| p["text"].as_str())
                        .collect()
                })
        }
        Provider::Ollama => {
            if response["done"] == true && response["done_reason"] == "length" {
                return Err(Truncated.into());
            }
            ensure!(
                response["done"] == true && response["done_reason"] == "stop",
                "Ollama response incomplete/refused"
            );
            response["message"]["content"].as_str().map(str::to_owned)
        }
        Provider::Cli | Provider::Bedrock | Provider::ClaudeCli => unreachable!(),
    };
    content.ok_or_else(|| Recovery::Hollow.into())
}

pub(super) fn attempt_deadline(s: &SemanticOptions, budget: &RequestBudget) -> Instant {
    budget
        .deadline
        .min(Instant::now() + Duration::from_secs(s.timeout_secs))
}

pub(super) fn command_error(error: anyhow::Error) -> anyhow::Error {
    if error.is::<super::super::convert::CommandTimeout>() {
        Recovery::Timeout.into()
    } else {
        error
    }
}

pub(super) fn io_timeout(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::TimedOut {
        return true;
    }
    let mut source = error
        .get_ref()
        .map(|e| e as &(dyn std::error::Error + 'static));
    while let Some(error) = source {
        if error
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
        {
            return true;
        }
        source = error.source();
    }
    false
}

pub(super) fn known_context_overflow(value: &Value) -> bool {
    // Explicit protocol codes only. Never classify arbitrary provider prose,
    // authentication errors, malformed graphs or user content as size failures.
    [
        value.pointer("/error/code"),
        value.pointer("/error/type"),
        value.get("error_code"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .any(|code| {
        matches!(
            code,
            "context_length_exceeded" | "prompt_too_long" | "context_window_exceeded"
        )
    })
}

pub(super) fn reserve_calls(
    s: &SemanticOptions,
    budget: &mut RequestBudget,
    calls: usize,
) -> Result<()> {
    let output = u32::try_from(calls)?
        .checked_mul(s.max_output_tokens)
        .context("semantic output reservation overflow")?;
    ensure!(
        Instant::now() < budget.deadline,
        "semantic recovery deadline exhausted"
    );
    ensure!(
        calls > 0 && budget.calls >= calls && budget.output >= output,
        "semantic call/token budget exhausted"
    );
    // Check local dimensions first; shared calls and tokens commit under one
    // lock. Nothing below can fail, so rejection leaves both budgets untouched.
    if let Some(shared) = &s.runtime_budget {
        shared.reserve_calls(calls, output)?;
    }
    budget.calls -= calls;
    budget.output -= output;
    Ok(())
}

pub(super) fn read_usage(receipt: &mut ProviderUsage, value: &Value) {
    let usage = &value["usage"];
    receipt.reported_model = value["model"]
        .as_str()
        .or_else(|| value["modelVersion"].as_str())
        .filter(|v| v.len() <= 256)
        .map(str::to_owned);
    match receipt.provider {
        Provider::OpenAi | Provider::Azure => {
            receipt.input_tokens = usage["prompt_tokens"].as_u64();
            receipt.output_tokens = usage["completion_tokens"].as_u64();
            receipt.total_tokens = usage["total_tokens"].as_u64();
            receipt.cache_read_input_tokens =
                usage["prompt_tokens_details"]["cached_tokens"].as_u64();
            receipt.reasoning_tokens =
                usage["completion_tokens_details"]["reasoning_tokens"].as_u64();
        }
        Provider::Anthropic | Provider::ClaudeCli => {
            receipt.input_tokens = usage["input_tokens"].as_u64();
            receipt.output_tokens = usage["output_tokens"].as_u64();
            receipt.cache_read_input_tokens = usage["cache_read_input_tokens"].as_u64();
            receipt.cache_creation_input_tokens = usage["cache_creation_input_tokens"].as_u64();
            if receipt.provider == Provider::ClaudeCli {
                receipt.cost_usd = value["total_cost_usd"]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0);
                if let Some(models) = value["modelUsage"].as_object().filter(|v| !v.is_empty()) {
                    receipt.reported_model = if models.len() == 1 {
                        models.keys().next().filter(|v| v.len() <= 256).cloned()
                    } else {
                        None // Aggregate usage cannot be assigned to one of several models.
                    };
                }
            }
        }
        Provider::Gemini => {
            let usage = &value["usageMetadata"];
            receipt.input_tokens = usage["promptTokenCount"].as_u64();
            receipt.output_tokens = usage["candidatesTokenCount"].as_u64();
            receipt.total_tokens = usage["totalTokenCount"].as_u64();
            receipt.cache_read_input_tokens = usage["cachedContentTokenCount"].as_u64();
            receipt.reasoning_tokens = usage["thoughtsTokenCount"].as_u64();
        }
        Provider::Ollama => {
            receipt.input_tokens = value["prompt_eval_count"].as_u64();
            receipt.output_tokens = value["eval_count"].as_u64();
        }
        Provider::Bedrock => {
            receipt.input_tokens = usage["inputTokens"].as_u64();
            receipt.output_tokens = usage["outputTokens"].as_u64();
            receipt.total_tokens = usage["totalTokens"].as_u64();
            receipt.cache_read_input_tokens = usage["cacheReadInputTokens"].as_u64();
            receipt.cache_creation_input_tokens = usage["cacheWriteInputTokens"].as_u64();
        }
        Provider::Cli => {} // The generic adapter's contract is bare graph JSON.
    }
}

pub(super) fn deduplicate(facts: &mut FileFacts, start: usize) {
    let mut first: HashMap<(String, String), usize> = HashMap::new();
    let mut replacements = HashMap::new();
    for i in start..facts.nodes.len() {
        let n = &facts.nodes[i];
        if !matches!(n.kind.as_str(), "concept" | "entity") {
            continue;
        }
        let key = (n.kind.clone(), n.label.trim().to_owned());
        if let Some(&index) = first.get(&key) {
            replacements.insert(n.id.clone(), facts.nodes[index].id.clone());
            let evidence = n.metadata.clone();
            if !facts.nodes[index].metadata["corroborating_evidence"].is_array() {
                facts.nodes[index].metadata["corroborating_evidence"] = json!([]);
            }
            facts.nodes[index].metadata["corroborating_evidence"]
                .as_array_mut()
                .unwrap()
                .push(evidence);
        } else {
            first.insert(key, i);
        }
    }
    facts.nodes.retain(|n| !replacements.contains_key(&n.id));
    for edge in &mut facts.edges {
        if let Some(id) = replacements.get(&edge.source) {
            edge.source = id.clone();
        }
        if let Some(id) = replacements.get(&edge.target) {
            edge.target = id.clone();
        }
    }
    // Keep separate evidence-bearing relations; omit only redundant membership edges.
    let mut memberships = HashSet::new();
    facts.edges.retain(|e| {
        e.source != e.target
            && (e.relation != "mentions"
                || memberships.insert((e.source.clone(), e.target.clone())))
    });
}

pub(super) fn cli_family(
    s: &SemanticOptions,
    instructions: &str,
    prompt: &str,
    image: Option<(&str, &str)>,
    budget: &mut RequestBudget,
    usage: &mut ProviderUsage,
) -> Result<String> {
    let mut adapter = s.command.clone().unwrap_or_else(|| {
        if s.provider == Provider::Bedrock {
            CommandAdapter::bedrock()
        } else {
            CommandAdapter::claude_cli()
        }
    });
    adapter.args = adapter
        .args
        .iter()
        .map(|a| a.replace("{model}", &s.model))
        .collect();
    let turns = if s.provider == Provider::ClaudeCli && image.is_some() {
        3
    } else {
        1
    };
    // Keep the snapshot alive until the subprocess (and its process group) exits.
    let mut image_file = None;
    let payload = if s.provider == Provider::Bedrock {
        let mut content = vec![json!({"text":prompt})];
        if let Some((mime, data)) = image {
            content.push(json!({"image":{"format":mime.strip_prefix("image/").unwrap_or(mime),"source":{"bytes":data}}}));
        }
        let mut payload = json!({"modelId":s.model,"system":[{"text":instructions}],"messages":[{"role":"user","content":content}],"inferenceConfig":{"maxTokens":s.max_output_tokens}});
        apply_controls(&mut payload, s);
        serde_json::to_vec(&payload)?
    } else {
        let mut prompt = format!(
            "{instructions}\n\nNow extract the graph. Return only the specified JSON object.\n\n{prompt}"
        );
        if let Some((mime, data)) = image {
            // Native vision requires the restricted recipe. Arbitrary adapters
            // retain their explicit generic CLI route; do not silently widen tools.
            let native: Vec<_> = CommandAdapter::claude_cli()
                .args
                .iter()
                .map(|a| a.replace("{model}", &s.model))
                .collect();
            ensure!(
                !adapter.output_file && adapter.args.ends_with(&native),
                "Claude CLI vision requires the native restricted command recipe"
            );
            let max_turns = adapter
                .args
                .iter()
                .rposition(|a| a == "--max-turns")
                .context("Claude CLI turn limit missing")?;
            adapter.args[max_turns + 1] = turns.to_string();
            let suffix = match mime {
                "image/png" => ".png",
                "image/jpeg" => ".jpg",
                "image/gif" => ".gif",
                "image/webp" => ".webp",
                _ => anyhow::bail!("unsupported Claude CLI image type"),
            };
            let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
            ensure!(
                bytes.len() <= s.max_image_bytes,
                "image exceeds semantic image byte limit"
            );
            let directory = super::super::convert::private_tempdir()?;
            let mut file = tempfile::Builder::new()
                .prefix("graf-image-")
                .suffix(suffix)
                .tempfile_in(directory.path())?;
            file.write_all(&bytes)?;
            file.flush()?;
            let path = file.path().canonicalize()?;
            let path = path
                .to_str()
                .context("Claude CLI image path must be UTF-8")?
                .replace('\\', "/");
            // Read rules use // for absolute paths. Reject glob/rule delimiters
            // from an unusual temp directory instead of widening the allowlist.
            ensure!(
                !path.contains(['*', '?', '[', ']', '(', ')', ','])
                    && !path.chars().any(char::is_control),
                "temporary image path cannot be expressed as an exact Read rule"
            );
            let tools = adapter
                .args
                .iter()
                .rposition(|a| a == "--tools")
                .context("Claude CLI tools missing")?;
            adapter.args[tools + 1] = "Read".into();
            adapter.args.extend([
                "--add-dir".into(),
                directory
                    .path()
                    .canonicalize()?
                    .to_str()
                    .context("Claude CLI image directory must be UTF-8")?
                    .into(),
                "--allowedTools".into(),
                format!("Read(//{})", path.trim_start_matches('/')),
                "--permission-mode".into(),
                "dontAsk".into(),
            ]);
            prompt.push_str(&format!("\nUse Read to view the image at this exact JSON-quoted path: {}. Extract only visible evidence.", serde_json::to_string(&path)?));
            image_file = Some((file, directory));
        }
        prompt.into_bytes()
    };
    let env = if s.provider == Provider::ClaudeCli {
        vec![(
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
            s.max_output_tokens.to_string(),
        )]
    } else {
        vec![]
    };
    // Reserve the entire native turn allowance before any subprocess, including
    // capability discovery. No partial reservation or refund of unused turns.
    reserve_calls(s, budget, turns)?;
    if s.provider == Provider::ClaudeCli {
        let schema = if let Some(supported) = budget.claude_schema {
            supported
        } else {
            let supported = claude_schema_supported(
                &adapter,
                attempt_deadline(s, budget),
                s.max_response_bytes,
            );
            budget.claude_schema = Some(supported);
            supported
        };
        if schema {
            // Native Claude's structured-output validator expects JSON Schema Draft 7.
            let schema = schemars::generate::SchemaSettings::draft07()
                .into_generator()
                .into_root_schema_for::<Graph>();
            adapter
                .args
                .extend(["--json-schema".into(), serde_json::to_string(&schema)?]);
        }
    }
    let (bytes, success) = super::super::convert::run_provider_until(
        &adapter,
        None,
        Some(&payload),
        attempt_deadline(s, budget),
        s.max_response_bytes,
        &env,
    )
    .map_err(command_error)?;
    drop(image_file);
    ensure!(
        success || !bytes.iter().all(u8::is_ascii_whitespace),
        "CLI provider exited unsuccessfully without a JSON response (output omitted to protect credentials)"
    );
    let value: Value = serde_json::from_slice(&bytes).context("invalid CLI provider response")?;
    let value = if s.provider == Provider::ClaudeCli {
        if let Some(events) = value.as_array() {
            events
                .iter()
                .rev()
                .find(|e| e["type"] == "result")
                .context("Claude CLI has no result event")?
        } else {
            &value
        }
    } else {
        &value
    };
    read_usage(usage, value);
    if !success || value["is_error"] == true {
        if known_context_overflow(value) {
            return Err(Recovery::ContextOverflow.into());
        }
        anyhow::bail!("CLI provider response incomplete/failed");
    }
    if s.provider == Provider::Bedrock {
        if value["stopReason"] == "max_tokens" {
            return Err(Truncated.into());
        }
        ensure!(
            value["stopReason"] == "end_turn",
            "Bedrock response incomplete/refused"
        );
        return value["output"]["message"]["content"]
            .as_array()
            .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect())
            .ok_or_else(|| Recovery::Hollow.into());
    }
    if value["stop_reason"] == "max_tokens" {
        return Err(Truncated.into());
    }
    ensure!(
        value["is_error"] == false && value["subtype"] == "success",
        "Claude CLI response incomplete/failed"
    );
    ensure!(
        value["stop_reason"].is_null() || value["stop_reason"] == "end_turn",
        "Claude CLI response incomplete/refused"
    );
    if let Some(structured) = value.get("structured_output").filter(|v| !v.is_null()) {
        ensure!(
            structured.is_object(),
            "Claude CLI structured output must be an object"
        );
        return Ok(serde_json::to_string(structured)?);
    }
    value["result"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Recovery::Hollow.into())
}

pub(super) fn claude_schema_supported(
    adapter: &CommandAdapter,
    deadline: Instant,
    limit: usize,
) -> bool {
    // Probe the executable/wrapper prefix, not an inference invocation. Cache
    // per document so another configured executable cannot inherit the result.
    let Some(print) = adapter
        .args
        .iter()
        .position(|arg| matches!(arg.as_str(), "--print" | "-p"))
    else {
        return false;
    };
    let mut probe = adapter.clone();
    probe.args.truncate(print);
    probe.args.push("--help".into());
    probe.output_file = false;
    super::super::convert::run_bytes_until(&probe, None, None, deadline, limit, &[])
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .is_some_and(|help| help.split_whitespace().any(|word| word == "--json-schema"))
}

pub(super) fn cache_key_valid(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
