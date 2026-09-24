use super::*;

pub(super) fn unknown_usage(s: &SemanticOptions) -> ProviderUsage {
    ProviderUsage {
        provider: s.provider,
        requested_model: s.model.clone(),
        reported_model: None,
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        reasoning_tokens: None,
        cost_usd: None,
    }
}

pub(super) fn validate_controls(s: &SemanticOptions) -> Result<()> {
    let controls = json!({"thinking":s.thinking,"extra_body":s.extra_body});
    ensure!(
        serde_json::to_vec(&controls)?.len() <= 16 * 1024,
        "provider controls exceed byte limit"
    );
    if s.provider == Provider::ClaudeCli {
        ensure!(
            s.temperature.is_none() && s.thinking.is_none() && s.extra_body.is_empty(),
            "Claude CLI does not expose these request-body controls; configure a generic CLI adapter or HTTP provider"
        );
    }
    if let Some(t) = s.temperature {
        let maximum = if matches!(s.provider, Provider::Anthropic | Provider::Bedrock) {
            1.0
        } else {
            2.0
        };
        ensure!(
            t.is_finite() && (0.0..=maximum).contains(&t),
            "invalid provider temperature"
        );
    }
    if let Some(thinking) = &s.thinking {
        match s.provider {
            Provider::OpenAi | Provider::Azure => ensure!(
                thinking.as_str().is_some_and(|v| matches!(
                    v,
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                )),
                "thinking must be a supported reasoning effort string"
            ),
            Provider::Ollama => ensure!(
                thinking.is_boolean()
                    || thinking
                        .as_str()
                        .is_some_and(|v| matches!(v, "low" | "medium" | "high" | "max")),
                "invalid Ollama thinking control"
            ),
            Provider::Anthropic | Provider::Bedrock => {
                let object = thinking.as_object().context("thinking must be an object")?;
                ensure!(
                    object
                        .keys()
                        .all(|k| matches!(k.as_str(), "type" | "budget_tokens" | "display")),
                    "unsupported thinking field"
                );
                let kind = thinking["type"].as_str().unwrap_or("");
                ensure!(
                    matches!(kind, "enabled" | "disabled" | "adaptive"),
                    "invalid thinking type"
                );
                if kind == "enabled" {
                    ensure!(
                        thinking["budget_tokens"]
                            .as_u64()
                            .is_some_and(|v| v >= 1024 && v < u64::from(s.max_output_tokens)),
                        "thinking budget must be at least 1024 and below the output limit"
                    );
                } else {
                    ensure!(
                        !object.contains_key("budget_tokens"),
                        "thinking type does not accept a token budget"
                    );
                }
                ensure!(
                    !object.contains_key("display")
                        || thinking["display"]
                            .as_str()
                            .is_some_and(|v| matches!(v, "summarized" | "omitted")),
                    "invalid thinking display"
                );
                ensure!(
                    kind == "disabled" || s.temperature.is_none_or(|v| v == 1.0),
                    "thinking requires default temperature"
                );
            }
            Provider::Gemini => {
                let object = thinking
                    .as_object()
                    .context("Gemini thinking must be an object")?;
                ensure!(
                    !object.is_empty()
                        && object.keys().all(|k| matches!(
                            k.as_str(),
                            "thinkingBudget" | "thinkingLevel" | "includeThoughts"
                        )),
                    "unsupported Gemini thinking field"
                );
                ensure!(
                    !(object.contains_key("thinkingBudget")
                        && object.contains_key("thinkingLevel")),
                    "choose thinkingBudget or thinkingLevel"
                );
                if let Some(budget) = object.get("thinkingBudget") {
                    ensure!(
                        budget.as_i64().is_some_and(
                            |v| v == -1 || (0..=i64::from(s.max_output_tokens)).contains(&v)
                        ),
                        "invalid Gemini thinking budget"
                    );
                }
                if let Some(level) = object.get("thinkingLevel") {
                    ensure!(
                        level
                            .as_str()
                            .is_some_and(|v| matches!(v, "MINIMAL" | "LOW" | "MEDIUM" | "HIGH")),
                        "invalid Gemini thinking level"
                    );
                }
                ensure!(
                    object
                        .get("includeThoughts")
                        .is_none_or(|v| *v == json!(false)),
                    "thought summaries are not graph JSON"
                );
            }
            Provider::Cli => {}
            Provider::ClaudeCli => unreachable!(),
        }
    }
    validate_extra(
        &Value::Object(s.extra_body.clone().into_iter().collect()),
        0,
    )
}

pub(super) fn validate_extra(value: &Value, depth: usize) -> Result<()> {
    ensure!(depth <= 16, "provider controls nesting exceeds limit");
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
                ensure!(
                    key.len() <= 128
                        && !matches!(
                            normalized.as_str(),
                            "model"
                                | "modelid"
                                | "messages"
                                | "input"
                                | "prompt"
                                | "instructions"
                                | "system"
                                | "systeminstruction"
                                | "contents"
                                | "cachedcontent"
                                | "promptvariables"
                                | "image"
                                | "images"
                                | "audio"
                                | "files"
                                | "documents"
                                | "attachments"
                                | "tools"
                                | "toolconfig"
                                | "toolchoice"
                                | "functions"
                                | "functioncall"
                                | "paralleltoolcalls"
                                | "websearchoptions"
                                | "stream"
                                | "streamoptions"
                                | "responseformat"
                                | "responsemimetype"
                                | "responseschema"
                                | "responsejsonschema"
                                | "format"
                                | "outputconfig"
                                | "maxtokens"
                                | "maxoutputtokens"
                                | "maxcompletiontokens"
                                | "maxnewtokens"
                                | "maxgenlen"
                                | "maxgentokens"
                                | "maxlength"
                                | "numpredict"
                                | "n"
                                | "candidatecount"
                                | "bestof"
                                | "temperature"
                                | "thinking"
                                | "think"
                                | "thinkingconfig"
                                | "reasoningeffort"
                                | "budgettokens"
                                | "thinkingbudget"
                                | "apikey"
                                | "keyenv"
                                | "authorization"
                                | "headers"
                                | "endpoint"
                                | "baseurl"
                                | "password"
                                | "secret"
                                | "token"
                        ),
                    "extra_body contains a managed or credential field"
                );
                if matches!(
                    normalized.as_str(),
                    "generationconfig"
                        | "inferenceconfig"
                        | "options"
                        | "additionalmodelrequestfields"
                ) {
                    ensure!(
                        value.is_object(),
                        "provider configuration container must be an object"
                    );
                }
                validate_extra(value, depth + 1)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_extra(value, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn apply_controls(body: &mut Value, s: &SemanticOptions) {
    fn merge(target: &mut Value, value: &Value) {
        if let (Some(target), Some(value)) = (target.as_object_mut(), value.as_object()) {
            for (key, value) in value {
                if let Some(existing) = target.get_mut(key) {
                    merge(existing, value);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        } else {
            *target = value.clone();
        }
    }
    if let Some(temperature) = s.temperature {
        match s.provider {
            Provider::Gemini => body["generationConfig"]["temperature"] = json!(temperature),
            Provider::Ollama => body["options"]["temperature"] = json!(temperature),
            Provider::Bedrock => body["inferenceConfig"]["temperature"] = json!(temperature),
            _ => body["temperature"] = json!(temperature),
        }
    }
    if let Some(thinking) = &s.thinking {
        match s.provider {
            Provider::OpenAi | Provider::Azure => body["reasoning_effort"] = thinking.clone(),
            Provider::Gemini => body["generationConfig"]["thinkingConfig"] = thinking.clone(),
            Provider::Ollama => body["think"] = thinking.clone(),
            Provider::Bedrock => {
                body["additionalModelRequestFields"]["thinking"] = thinking.clone()
            }
            _ => body["thinking"] = thinking.clone(),
        }
    }
    merge(
        body,
        &Value::Object(s.extra_body.clone().into_iter().collect()),
    );
}

pub(super) fn validate_graph(graph: &Graph, text: &str, visual: bool) -> Result<()> {
    ensure!(
        graph.nodes.len() <= 128 && graph.edges.len() <= 256,
        "semantic graph exceeds entity/relation limit"
    );
    let mut ids = HashSet::new();
    for node in &graph.nodes {
        ensure!(
            !node.id.is_empty() && node.id.len() <= 128 && ids.insert(node.id.as_str()),
            "invalid or duplicate semantic node ID"
        );
        ensure!(
            !node.label.trim().is_empty()
                && node.label.len() <= 512
                && !node.kind.is_empty()
                && node.kind.len() <= 64,
            "invalid semantic node label/kind"
        );
        ensure!(
            !node.evidence.is_empty()
                && node.evidence.len() <= 4096
                && (visual || text.contains(&node.evidence)),
            "semantic node lacks literal source evidence"
        );
    }
    for edge in &graph.edges {
        ensure!(
            ids.contains(edge.source.as_str()) && ids.contains(edge.target.as_str()),
            "semantic relation has unknown endpoint"
        );
        ensure!(
            !edge.relation.is_empty()
                && edge.relation.len() <= 64
                && edge.confidence.is_finite()
                && (0.0..=1.0).contains(&edge.confidence),
            "invalid semantic relationship/confidence"
        );
        ensure!(
            !edge.evidence.is_empty()
                && edge.evidence.len() <= 4096
                && (visual || text.contains(&edge.evidence)),
            "semantic relation lacks literal source evidence"
        );
    }
    ensure!(
        graph.hyperedges.len() <= 32,
        "semantic hyperedge limit exceeded"
    );
    let mut group_ids = HashSet::new();
    for group in &graph.hyperedges {
        ensure!(
            !group.id.is_empty() && group.id.len() <= 128 && group_ids.insert(&group.id),
            "invalid semantic hyperedge ID"
        );
        ensure!(
            !group.label.trim().is_empty() && group.label.len() <= 512,
            "invalid semantic hyperedge label"
        );
        ensure!(
            (2..=64).contains(&group.members.len())
                && group.members.iter().all(|id| ids.contains(id.as_str())),
            "invalid semantic hyperedge members"
        );
        ensure!(
            (0.0..=1.0).contains(&group.confidence)
                && !group.evidence.is_empty()
                && group.evidence.len() <= 4096
                && (visual || text.contains(&group.evidence)),
            "invalid semantic hyperedge confidence/evidence"
        );
    }
    Ok(())
}

pub(super) fn enrich_source(
    facts: &mut FileFacts,
    text: &str,
    s: &SemanticOptions,
    image: Option<(&str, &str)>,
    force: bool,
) -> Result<()> {
    validate(s)?;
    if text.trim().is_empty() {
        return Ok(());
    }
    // A UTF-8 byte per token is deliberately conservative across tokenizers.
    // Reserve 1024 tokens for instruction and protocol overhead; never truncate.
    let capacity = s.max_input_tokens - 1024;
    let mut chunks = vec![];
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + capacity).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        ensure!(end > start, "semantic chunk budget too small");
        chunks.push((start, &text[start..end]));
        start = end;
    }
    ensure!(
        chunks.len() <= s.max_calls,
        "document exceeds semantic call budget; increase budget or split the document"
    );
    ensure!(
        chunks.len() as u64 * s.max_output_tokens as u64 <= s.max_total_output_tokens as u64,
        "document exceeds total semantic output token budget"
    );
    let mut settings = s.clone();
    settings.cache_dir = None;
    let settings = serde_json::to_vec(&settings)?;
    let settings_hash = blake3::hash(&settings).to_hex().to_string();
    // Collect and validate every response before touching even the in-memory facts.
    let mut complete = Vec::new();
    let mut budget = RequestBudget {
        calls: s.max_calls,
        output: s.max_total_output_tokens,
        deadline: Instant::now() + Duration::from_secs(s.timeout_secs * s.max_calls as u64),
        claude_schema: None,
    };
    let mut pending: std::collections::VecDeque<_> = chunks
        .into_iter()
        .map(|(offset, text)| (offset, text, 0u32))
        .collect();
    while let Some((offset, chunk, depth)) = pending.pop_front() {
        let cache_key = blake3::hash(
            &[
                b"graf-semantic-v1\0".as_slice(),
                INSTRUCTIONS.as_bytes(),
                settings.as_slice(),
                b"\0",
                chunk.as_bytes(),
                image.map(|(mime, _)| mime.as_bytes()).unwrap_or_default(),
                image.map(|(_, data)| data.as_bytes()).unwrap_or_default(),
            ]
            .concat(),
        )
        .to_hex()
        .to_string();
        let cached = s
            .cache_dir
            .as_ref()
            .map(|d| d.join(format!("{cache_key}.json")));
        let cached_value = if let Some(path) = cached.as_ref().filter(|p| !force && p.exists()) {
            let bytes = super::super::read_bounded(path, s.max_response_bytes as u64)
                .context("invalid semantic cache")?;
            Some(serde_json::from_slice::<Value>(&bytes).context("malformed semantic cache")?)
        } else {
            None
        };
        let mut split = None;
        let graph = if let Some(value) = cached_value {
            if value.get("split_at").is_some() {
                let marker: Split =
                    serde_json::from_value(value).context("invalid split cache marker")?;
                split = Some(marker.split_at);
                None
            } else {
                Some(serde_json::from_value::<Graph>(value).context("malformed semantic cache")?)
            }
        } else {
            match request(s, chunk, image, &mut budget) {
                Ok(response) => Some(serde_json::from_str::<Graph>(&response).context(
                    "malformed/incomplete semantic graph JSON; previous graph retained",
                )?),
                Err(error)
                    if (error.is::<Truncated>()
                        || matches!(
                            error.downcast_ref::<Recovery>(),
                            Some(Recovery::ContextOverflow | Recovery::Timeout)
                        ))
                        && image.is_none()
                        && depth < s.max_split_depth =>
                {
                    let target = chunk.len() / 2;
                    let midpoint = chunk
                        .char_indices()
                        .skip(1)
                        .filter(|(i, _)| *i >= chunk.len() / 3 && *i <= 2 * chunk.len() / 3)
                        .filter(|(_, c)| *c == '\n')
                        .min_by_key(|(i, _)| i.abs_diff(target))
                        .map(|(i, _)| i + 1)
                        .or_else(|| {
                            chunk
                                .char_indices()
                                .skip(1)
                                .min_by_key(|(i, _)| i.abs_diff(target))
                                .map(|(i, _)| i)
                        })
                        .context("truncated semantic chunk cannot be split further")?;
                    split = Some(midpoint);
                    None
                }
                Err(error) => return Err(error),
            }
        };
        if let Some(midpoint) = split {
            ensure!(
                image.is_none()
                    && depth < s.max_split_depth
                    && midpoint > 0
                    && midpoint < chunk.len()
                    && chunk.is_char_boundary(midpoint),
                "invalid or exhausted semantic split boundary"
            );
            save_cache(
                cached.as_deref(),
                &Split { split_at: midpoint },
                s.max_response_bytes,
            )?;
            pending.push_front((offset + midpoint, &chunk[midpoint..], depth + 1));
            pending.push_front((offset, &chunk[..midpoint], depth + 1));
            continue;
        }
        let graph = graph.context("missing complete semantic graph")?;
        validate_graph(&graph, chunk, image.is_some())?;
        save_cache(cached.as_deref(), &graph, s.max_response_bytes)?;
        complete.push((offset, cache_key, graph));
    }
    let provenance = if image.is_some() {
        "visual_inference"
    } else {
        "semantic"
    };
    let node_start = facts.nodes.len();
    let edge_start = facts.edges.len();
    for (batch, (offset, key, graph)) in complete.into_iter().enumerate() {
        let mut ids = HashMap::new();
        let root = facts.nodes[0].id.clone();
        for entity in graph.nodes {
            let id = format!(
                "semantic:{}:{batch}:{}",
                facts.path,
                blake3::hash(entity.id.as_bytes()).to_hex()
            );
            let evidence_offset = offset + text[offset..].find(&entity.evidence).unwrap_or(0);
            let line = text[..evidence_offset]
                .bytes()
                .filter(|b| *b == b'\n')
                .count() as u32
                + 1;
            ids.insert(entity.id, id.clone());
            facts.nodes.push(Node{id:id.clone(),label:entity.label,kind:entity.kind,file:facts.path.clone(),line:Some(line),end_line:None,
                qualified_name:None,binding_key:None,metadata:json!({"provenance":provenance,"inferred":true,"model":s.model,
                    "provider":s.provider,"settings_hash":settings_hash,"batch_hash":key,"evidence":entity.evidence})});
            super::super::edge(
                facts,
                &root,
                &id,
                "mentions",
                line,
                json!({"provenance":provenance,"inferred":true}),
            );
            facts.edges.last_mut().unwrap().confidence = "inferred".into();
        }
        for group in graph.hyperedges {
            let id = format!(
                "semantic-group:{}:{batch}:{}",
                facts.path,
                blake3::hash(group.id.as_bytes()).to_hex()
            );
            facts.nodes.push(Node{id:id.clone(),label:group.label,kind:"hyperedge".into(),file:facts.path.clone(),line:None,end_line:None,qualified_name:None,binding_key:None,
                metadata:json!({"provenance":provenance,"inferred":true,"model":s.model,"provider":s.provider,"confidence_score":group.confidence,"evidence":group.evidence,"batch_hash":key})});
            for member in group.members {
                super::super::edge(
                    facts,
                    &ids[&member],
                    &id,
                    "member_of",
                    1,
                    json!({"provenance":provenance,"inferred":true,"evidence":group.evidence,"confidence_score":group.confidence}),
                );
                let edge = facts.edges.last_mut().unwrap();
                edge.confidence = "inferred".into();
                edge.line = None;
            }
        }
        for relation in graph.edges {
            let evidence_offset = offset + text[offset..].find(&relation.evidence).unwrap_or(0);
            let line = text[..evidence_offset]
                .bytes()
                .filter(|b| *b == b'\n')
                .count() as u32
                + 1;
            super::super::edge(
                facts,
                &ids[&relation.source],
                &ids[&relation.target],
                &relation.relation,
                line,
                json!({"provenance":provenance,"inferred":true,"confidence_score":relation.confidence,"evidence":relation.evidence,
                    "provider":s.provider,"model":s.model,"settings_hash":settings_hash,"batch_hash":key}),
            );
            facts.edges.last_mut().unwrap().confidence = "inferred".into();
        }
    }
    if image.is_some() {
        for node in &mut facts.nodes[node_start..] {
            node.line = None;
            node.metadata["evidence_basis"] =
                json!("model description of pixels, not verified text");
        }
        for edge in &mut facts.edges[edge_start..] {
            edge.line = None;
        }
    }
    if s.deduplicate {
        deduplicate(facts, node_start);
    }
    Ok(())
}

pub(super) fn request(
    s: &SemanticOptions,
    text: &str,
    image: Option<(&str, &str)>,
    budget: &mut RequestBudget,
) -> Result<String> {
    for attempt in 0..=s.max_retries {
        let before = budget.calls;
        let mut usage = unknown_usage(s);
        let result = request_once(s, text, image, budget, &mut usage).and_then(|text| {
            if text.trim().is_empty() {
                Err(Recovery::Hollow.into())
            } else {
                Ok(text)
            }
        });
        if budget.calls < before
            && let Some(recorder) = &s.runtime_usage
        {
            recorder.record(usage)?;
        }
        match result {
            Err(error)
                if attempt < s.max_retries
                    && matches!(
                        error.downcast_ref::<Recovery>(),
                        Some(Recovery::Hollow | Recovery::Transient)
                    ) =>
            {
                ensure!(
                    budget.calls > 0 && budget.output >= s.max_output_tokens,
                    "semantic retry exceeds remaining call/token budget"
                );
                let delay = Duration::from_millis(s.retry_delay_ms);
                ensure!(
                    budget.deadline.saturating_duration_since(Instant::now()) > delay,
                    "semantic recovery deadline exhausted"
                );
                std::thread::sleep(delay);
            }
            result => return result,
        }
    }
    unreachable!()
}
