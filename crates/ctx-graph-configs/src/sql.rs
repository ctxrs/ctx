use super::*;

pub(super) fn sql(f: &mut Facts, source: &str) -> Result<()> {
    let root = f.root("sql", json!({}));
    let tokens = sql_tokens(source);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_sequel::LANGUAGE.into())?;
    let mut start = 0;
    let mut routine_owner: Option<String> = None;
    while start < tokens.len() {
        let end = tokens[start..]
            .iter()
            .position(|t| t.text == ";")
            .map(|n| start + n)
            .unwrap_or(tokens.len());
        let ts = &tokens[start..end];
        start = end + 1;
        if ts.is_empty() {
            continue;
        }
        let offset = ts[0].start;
        let source_end = ts.last().unwrap().end;
        let fragment = &source[offset..source_end];
        let ln = source[..offset].bytes().filter(|b| *b == b'\n').count() as u32 + 1;
        let mut owner = routine_owner.clone().unwrap_or_else(|| root.clone());
        let mut header = 0;
        while ts.get(header).is_some_and(|t| t.is("BEGIN") || t.is("GO")) {
            header += 1;
        }
        let create = ts.get(header).is_some_and(|t| t.is("CREATE"));
        let alter = ts.get(header).is_some_and(|t| t.is("ALTER"));
        if create || alter {
            let mut i = header + 1;
            while ts.get(i).is_some_and(|t| {
                [
                    "OR",
                    "REPLACE",
                    "ALTER",
                    "TEMP",
                    "TEMPORARY",
                    "UNLOGGED",
                    "UNIQUE",
                    "MATERIALIZED",
                ]
                .iter()
                .any(|w| t.is(w))
            }) {
                i += 1;
            }
            let kind = ts
                .get(i)
                .and_then(|t| {
                    [
                        "TABLE",
                        "VIEW",
                        "FUNCTION",
                        "PROCEDURE",
                        "PROC",
                        "INDEX",
                        "TRIGGER",
                    ]
                    .iter()
                    .find(|w| t.is(w))
                })
                .copied();
            if let Some(kind) = kind {
                i += 1;
                while ts.get(i).is_some_and(|t| {
                    ["IF", "NOT", "EXISTS", "CONCURRENTLY", "ONLY"]
                        .iter()
                        .any(|w| t.is(w))
                }) {
                    i += 1;
                }
                if !ts.get(i).is_some_and(|t| t.is("ON"))
                    && let Some((label, key, _)) = sql_name(ts, i)
                {
                    let routine = matches!(kind, "FUNCTION" | "PROCEDURE" | "PROC");
                    let binding = if matches!(kind, "TABLE" | "VIEW") {
                        sql_relation_key(&key)
                    } else {
                        format!("sql:{}:{key}", kind.to_ascii_lowercase())
                    };
                    let existing = if alter {
                        f.0.nodes
                            .iter()
                            .find(|n| n.binding_key.as_ref() == Some(&binding))
                            .map(|n| n.id.clone())
                    } else {
                        None
                    };
                    owner = existing.unwrap_or_else(|| {
                        let id = f.node(
                            &label,
                            if alter {
                                "table_reference"
                            } else if kind == "PROC" {
                                "procedure"
                            } else {
                                match kind {
                                    "TABLE" => "table",
                                    "VIEW" => "view",
                                    "FUNCTION" => "function",
                                    "PROCEDURE" => "procedure",
                                    "INDEX" => "index",
                                    _ => "trigger",
                                }
                            },
                            if alter { None } else { Some(binding.clone()) },
                            ln,
                            json!({"language":"sql"}),
                        );
                        f.edge(&root, &id, "contains", ln);
                        if alter {
                            f.reference(&id, &label, "alters", vec![binding], ln);
                        }
                        id
                    });
                    routine_owner = if routine { Some(owner.clone()) } else { None };
                    if matches!(kind, "INDEX" | "TRIGGER")
                        && let Some(on) = ts.iter().position(|t| t.is("ON"))
                        && let Some((label, key, _)) = sql_name(ts, on + 1)
                    {
                        f.reference(
                            &owner,
                            &label,
                            if kind == "INDEX" {
                                "indexes"
                            } else {
                                "references"
                            },
                            vec![sql_relation_key(&key)],
                            ln,
                        );
                    }
                }
            }
        }
        let tree = parser
            .parse(fragment, None)
            .ok_or_else(|| anyhow::anyhow!("SQL parser failed"))?;
        sql_reads(
            f,
            fragment,
            tree.root_node(),
            &owner,
            ln - 1,
            &HashSet::new(),
            0,
        );
        // REFERENCES has the same lexical form in table and ALTER constraints,
        // including dialects that recover as grammar errors. Quoted strings do not match.
        for (i, t) in ts.iter().enumerate() {
            if t.is("REFERENCES")
                && let Some((label, key, _)) = sql_name(ts, i + 1)
            {
                f.reference(
                    &owner,
                    &label,
                    "references",
                    vec![sql_relation_key(&key)],
                    ln,
                );
            }
            if t.kind == b'd' && routine_owner.is_some() {
                let tag_end = t.text[1..].find('$').map(|p| p + 2).unwrap_or(2);
                if t.text.len() >= tag_end * 2 {
                    let body = &t.text[tag_end..t.text.len() - tag_end];
                    if let Some(tree) = parser.parse(body, None) {
                        sql_reads(
                            f,
                            body,
                            tree.root_node(),
                            &owner,
                            source[..t.start + tag_end]
                                .bytes()
                                .filter(|b| *b == b'\n')
                                .count() as u32,
                            &HashSet::new(),
                            0,
                        );
                    }
                }
            }
        }
        if !create && ts.first().is_some_and(|t| t.is("END")) {
            routine_owner = None;
        }
    }
    Ok(())
}

pub(super) fn sql_reads(
    f: &mut Facts,
    source: &str,
    node: Syntax<'_>,
    owner: &str,
    line_offset: u32,
    inherited: &HashSet<String>,
    depth: usize,
) {
    if depth > 128 {
        return;
    }
    let cs = children(node);
    let mut ctes = inherited.clone();
    for c in &cs {
        if c.kind() == "cte"
            && let Some(name) = children(*c).into_iter().find(|n| n.kind() == "identifier")
            && let Some((_, key, _)) = sql_name(&sql_tokens(&source[name.byte_range()]), 0)
        {
            ctes.insert(key);
        }
    }
    if matches!(node.kind(), "relation" | "from" | "join" | "cross_join") {
        for c in &cs {
            if c.kind() == "object_reference"
                && let Some((label, key, _)) = sql_name(&sql_tokens(&source[c.byte_range()]), 0)
                && !ctes.contains(&key)
            {
                f.reference(
                    owner,
                    &label,
                    "reads_from",
                    vec![sql_relation_key(&key)],
                    line(*c) + line_offset,
                );
            }
        }
    }
    for c in cs {
        if !matches!(c.kind(), "comment" | "literal" | "function_body") {
            sql_reads(f, source, c, owner, line_offset, &ctes, depth + 1);
        }
    }
}

pub(super) fn mcp_config(path: &str) -> bool {
    matches!(
        basename(path),
        ".mcp.json" | "claude_desktop_config.json" | "mcp.json" | "mcp_servers.json"
    )
}

pub(super) fn mcp(f: &mut Facts, source: &str) -> Result<()> {
    let data: Value = serde_json::from_str(source)?;
    let servers = data["mcpServers"]
        .as_object()
        .or_else(|| data["mcp"]["servers"].as_object())
        .ok_or_else(|| anyhow::anyhow!("missing server map"))?;
    let root = f.root("mcp-config", json!({}));
    let mut shared = HashMap::new();
    for (name, spec) in servers.iter().filter(|(_, v)| v.is_object()).take(200) {
        let name: String = name.chars().filter(|c| !c.is_control()).take(200).collect();
        if name.is_empty() {
            continue;
        }
        let owner = f.node(
            &name,
            "mcp_server",
            Some(format!("mcp:server:{}:{name}", f.0.path)),
            1,
            json!({}),
        );
        f.edge(&root, &owner, "contains", 1);
        let mut concepts = vec![];
        if let Some(cmd) = spec["command"].as_str() {
            let cmd = cmd.rsplit(['/', '\\']).next().unwrap_or("");
            if safe_name(cmd) {
                concepts.push(("mcp_command", cmd.to_owned(), "references"));
            }
        }
        let mut skip_value = false;
        for arg in strings(&spec["args"]) {
            if skip_value {
                skip_value = false;
                continue;
            }
            if arg.starts_with('-') {
                skip_value =
                    !matches!(arg.as_str(), "-y" | "--yes" | "--no-cache") && !arg.contains('=');
                continue;
            }
            let bare = if let Some(stripped) = arg.strip_prefix('@') {
                stripped
                    .split_once('@')
                    .map(|(a, _)| format!("@{a}"))
                    .unwrap_or_else(|| arg.clone())
            } else {
                arg.split('@').next().unwrap_or("").into()
            };
            let package = if let Some(stripped) = bare.strip_prefix('@') {
                stripped
                    .split_once('/')
                    .is_some_and(|(scope, name)| safe_name(scope) && safe_name(name))
            } else {
                safe_name(&bare)
                    && (bare.starts_with("mcp-")
                        || bare.ends_with("-mcp")
                        || bare.contains("-mcp-"))
            };
            if package {
                concepts.push(("mcp_package", bare, "references"));
                break;
            }
        }
        if let Some(env) = spec["env"].as_object() {
            for name in env.keys() {
                if !name.is_empty()
                    && name.len() <= 200
                    && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                {
                    concepts.push(("env_var", name.clone(), "requires_env"));
                }
            }
        }
        for (kind, name, relation) in concepts {
            let key = format!("mcp:{kind}:{name}");
            let id = shared
                .entry(key.clone())
                .or_insert_with(|| f.node(&name, kind, Some(key), 1, json!({})))
                .clone();
            f.edge(&owner, &id, relation, 1);
        }
    }
    if servers.len() > 200 {
        diagnostic(&mut f.0, None, "MCP server limit reached");
    }
    Ok(())
}

pub(super) fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
