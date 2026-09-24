use super::*;

pub(super) fn sln(f: &mut Facts, source: &str) {
    let root = f.root("solution", json!({}));
    let mut guids = HashMap::new();
    let mut current = None;
    let mut deps = false;
    let mut nested = false;
    let mut links = vec![];
    for (i, line) in source.lines().enumerate() {
        let s = line.trim();
        let ln = i as u32 + 1;
        if s.starts_with("Project(") {
            let quoted: Vec<_> = s.split('"').skip(1).step_by(2).collect();
            if quoted.len() < 4 {
                continue;
            }
            let (name, path, guid) = (quoted[1], quoted[2], quoted[3].to_ascii_lowercase());
            let folder = quoted[0].eq_ignore_ascii_case("{66A26720-8FB5-11D2-AA7E-00C04F688DDE}")
                || name == path;
            let id = f.node(
                name,
                if folder {
                    "solution_folder"
                } else {
                    "project_reference"
                },
                None,
                ln,
                json!({"path":if folder { None } else { local_target(&f.0.path,path) }}),
            );
            f.edge(&root, &id, "contains", ln);
            if !folder {
                f.reference(&id, path, "references", path_keys(&f.0.path, path), ln);
            }
            guids.insert(guid.clone(), id);
            current = Some(guid);
        } else if s == "EndProject" {
            current = None;
            deps = false;
        } else if s.contains("ProjectSection(ProjectDependencies)") {
            deps = true;
        } else if s == "EndProjectSection" {
            deps = false;
        } else if s.contains("GlobalSection(NestedProjects)") {
            nested = true;
        } else if s == "EndGlobalSection" {
            nested = false;
        } else if let Some((left, right)) = s.split_once('=') {
            if deps {
                if let Some(owner) = &current {
                    links.push((
                        owner.clone(),
                        left.trim().to_ascii_lowercase(),
                        "depends_on",
                        ln,
                    ));
                }
            } else if nested {
                links.push((
                    right.trim().to_ascii_lowercase(),
                    left.trim().to_ascii_lowercase(),
                    "contains",
                    ln,
                ));
            }
        }
    }
    for (from, to, relation, ln) in links {
        if let (Some(from), Some(to)) = (guids.get(&from), guids.get(&to)) {
            if relation == "contains" {
                f.0.edges
                    .retain(|e| !(e.source == root && e.target == *to && e.relation == "contains"));
            }
            f.edge(from, to, relation, ln);
        }
    }
}

pub(super) fn indexed_config_source(
    root: &Path,
    relative: &str,
) -> Result<(String, Option<String>)> {
    let mut path = root.to_path_buf();
    for component in relative.split('/') {
        path.push(component);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(("missing".into(), None));
            }
            Err(e) => return Err(e).context("cannot inspect indexed configuration"),
        };
        if meta.file_type().is_symlink() {
            return Ok(("symlink".into(), None));
        }
    }
    if !std::fs::symlink_metadata(&path)?.is_file() {
        return Ok(("not-file".into(), None));
    }
    let (hash, bytes) = ctx_graph_types::read_source(&path, CONFIG_MAX_BYTES as u64)?;
    Ok((hash, bytes.and_then(|bytes| String::from_utf8(bytes).ok())))
}

pub(super) fn terraform_target(path: &str, source: &str) -> Option<String> {
    if !(source.starts_with("./") || source.starts_with("../")) || source.contains('\\') {
        return None;
    }
    relative_path(directory(path), source)
}

pub(super) fn hcl_string(source: &str, mut node: Syntax<'_>) -> Option<String> {
    while matches!(node.kind(), "expression" | "literal_value") && node.named_child_count() == 1 {
        node = node.named_child(0)?;
    }
    if node.kind() != "string_lit" {
        return None;
    }
    // HCL adds eight-digit Unicode escapes to JSON's quoted-string syntax.
    let raw = source[node.byte_range()]
        .strip_prefix('"')?
        .strip_suffix('"')?;
    let mut chars = raw.chars();
    let mut text = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            text.push(c);
            continue;
        }
        text.push(match chars.next()? {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '"' => '"',
            '\\' => '\\',
            escape @ ('u' | 'U') => {
                let mut scalar = 0;
                for _ in 0..if escape == 'u' { 4 } else { 8 } {
                    scalar = scalar * 16 + chars.next()?.to_digit(16)?;
                }
                char::from_u32(scalar)?
            }
            _ => return None,
        });
    }
    Some(text.replace("$${", "${").replace("%%{", "%{"))
}

pub(super) fn hcl_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['_', '-'], "");
    [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "accesskey",
        "privatekey",
        "credential",
        "connectionstring",
        "auth",
        "passphrase",
    ]
    .iter()
    .any(|word| key.contains(word))
}

pub(super) fn hcl_credential(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("private key-----")
        || lower.contains("bearer ")
        || lower.contains("basic ")
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .any(|word| {
                (word.len() >= 20
                    && ["ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-", "sk-"]
                        .iter()
                        .any(|prefix| word.starts_with(prefix)))
                    || (word.len() == 20
                        && (word.starts_with("akia") || word.starts_with("asia"))
                        && word.bytes().all(|b| b.is_ascii_alphanumeric()))
            })
    {
        return true;
    }
    if text.split("://").skip(1).any(|rest| {
        rest.split(['/', '?', '#'])
            .next()
            .is_some_and(|authority| authority.contains('@'))
    }) {
        return true;
    }
    // Find the separator before trimming whitespace so quoted keys and ordinary
    // KEY = value assignments cannot lose their key/separator association.
    lower.match_indices(['=', ':']).any(|(offset, _)| {
        lower[..offset]
            .trim_end()
            .rsplit(|c: char| {
                c.is_whitespace() || ['&', '?', ';', ',', '{', '}', '[', ']', '=', ':'].contains(&c)
            })
            .next()
            .is_some_and(|key| hcl_sensitive_key(key.trim_matches(['"', '\'', '%'])))
    })
}

pub(super) fn hcl_unresolved(kind: &str) -> Value {
    json!({"$hcl":"unresolved", "kind":kind})
}

pub(super) fn hcl_value(source: &str, mut node: Syntax<'_>, depth: usize) -> Value {
    if depth > 32 {
        return hcl_unresolved("depth_limit");
    }
    while matches!(
        node.kind(),
        "expression" | "literal_value" | "collection_value"
    ) {
        let parts: Vec<_> = children(node)
            .into_iter()
            .filter(|n| n.kind() != "comment")
            .collect();
        let [child] = parts.as_slice() else { break };
        node = *child;
    }
    let raw = &source[node.byte_range()];
    match node.kind() {
        "bool_lit" => json!(raw == "true"),
        "null_lit" => Value::Null,
        "numeric_lit" => {
            // Do not silently round an integer beyond JSON's supported integer range.
            if raw.bytes().all(|b| b.is_ascii_digit()) {
                raw.parse::<u64>()
                    .map(|n| json!(n))
                    .unwrap_or_else(|_| hcl_unresolved("numeric_range"))
            } else {
                serde_json::from_str::<Value>(raw)
                    .ok()
                    .filter(Value::is_number)
                    .unwrap_or_else(|| hcl_unresolved("numeric_range"))
            }
        }
        "operation" if raw.starts_with('-') => {
            let number = raw[1..].trim();
            let signed = format!("-{number}");
            if number.bytes().all(|b| b.is_ascii_digit()) {
                signed
                    .parse::<i64>()
                    .map(|n| json!(n))
                    .unwrap_or_else(|_| hcl_unresolved("numeric_range"))
            } else {
                serde_json::from_str::<Value>(&signed)
                    .ok()
                    .filter(Value::is_number)
                    .unwrap_or_else(|| hcl_unresolved("operation"))
            }
        }
        "string_lit" => hcl_string(source, node)
            .map(|text| {
                if hcl_credential(&text) {
                    json!("[redacted]")
                } else {
                    json!(text)
                }
            })
            .unwrap_or_else(|| hcl_unresolved("string_escape")),
        "tuple" => Value::Array(
            children(node)
                .into_iter()
                .filter(|n| n.kind() == "expression")
                .map(|n| hcl_value(source, n, depth + 1))
                .collect(),
        ),
        "object" => {
            let mut values = serde_json::Map::new();
            for element in children(node)
                .into_iter()
                .filter(|n| n.kind() == "object_elem")
            {
                let (Some(key), Some(value)) = (
                    element.child_by_field_name("key"),
                    element.child_by_field_name("val"),
                ) else {
                    return hcl_unresolved("object_key");
                };
                // Only a bare identifier or quoted literal is a static object key.
                let key_text = &source[key.byte_range()];
                let key = if let Some(key) = hcl_string(source, key) {
                    key
                } else if key.named_child_count() == 1
                    && key.named_child(0).is_some_and(|n| {
                        n.kind() == "variable_expr" && &source[n.byte_range()] == key_text
                    })
                {
                    key_text.to_owned()
                } else {
                    return hcl_unresolved("object_key");
                };
                if hcl_credential(&key) {
                    return json!("[redacted]");
                }
                let value = if hcl_sensitive_key(&key) {
                    json!("[redacted]")
                } else {
                    hcl_value(source, value, depth + 1)
                };
                if values.insert(key, value).is_some() {
                    return hcl_unresolved("duplicate_key");
                }
            }
            Value::Object(values)
        }
        // No source-text fallback: templates, function arguments and computed keys
        // can contain secrets. References are collected independently by hcl_refs.
        kind => hcl_unresolved(kind),
    }
}

pub(super) fn hcl_attributes(source: &str, body: Syntax<'_>, sensitive: bool) -> Value {
    let mut values = serde_json::Map::new();
    for attr in children(body)
        .into_iter()
        .filter(|n| n.kind() == "attribute")
    {
        let parts: Vec<_> = children(attr)
            .into_iter()
            .filter(|n| n.kind() != "comment")
            .collect();
        let [key, value] = parts.as_slice() else {
            continue;
        };
        let key = &source[key.byte_range()];
        let value = if hcl_sensitive_key(key) || (sensitive && matches!(key, "default" | "value")) {
            json!("[redacted]")
        } else {
            hcl_value(source, *value, 0)
        };
        if values.contains_key(key) {
            values.insert(key.into(), hcl_unresolved("duplicate_attribute"));
        } else {
            values.insert(key.into(), value);
        }
    }
    Value::Object(values)
}

pub(super) fn hcl(f: &mut Facts, source: &str) -> Result<()> {
    let Some(tree) = tree(tree_sitter_hcl::LANGUAGE.into(), source, &mut f.0)? else {
        return Ok(());
    };
    let root = f.root("terraform", json!({"directory":directory(&f.0.path)}));
    // Variable-value files do not declare resources or module directories.
    if f.0.path.ends_with(".tfvars") {
        return Ok(());
    }
    let text = |n: Syntax<'_>| &source[n.byte_range()];
    let body = children(tree.root_node())
        .into_iter()
        .find(|n| n.kind() == "body")
        .unwrap_or(tree.root_node());
    for block in children(body).into_iter().filter(|n| n.kind() == "block") {
        let parts: Vec<_> = children(block)
            .into_iter()
            .filter(|n| n.kind() != "comment")
            .take_while(|n| !matches!(n.kind(), "block_start" | "body" | "block_end"))
            .map(|n| hcl_string(source, n).unwrap_or_else(|| text(n).trim_matches('"').to_string()))
            .collect();
        let Some(kind) = parts.first() else {
            continue;
        };
        let body = children(block).into_iter().find(|n| n.kind() == "body");
        if kind == "locals" {
            if let Some(body) = body {
                for a in children(body)
                    .into_iter()
                    .filter(|n| n.kind() == "attribute")
                {
                    if let Some(name) = children(a).first() {
                        let name = format!("local.{}", text(*name));
                        let id = hcl_node(f, &root, &name, "local", line(a));
                        hcl_refs(f, source, a, &id, "references", &[]);
                    }
                }
            }
            continue;
        }
        let name = match (kind.as_str(), parts.len()) {
            ("resource", n) if n >= 3 => format!("{}.{}", parts[1], parts[2]),
            ("data", n) if n >= 3 => format!("data.{}.{}", parts[1], parts[2]),
            ("variable", n) if n >= 2 => format!("var.{}", parts[1]),
            ("module" | "output" | "provider", n) if n >= 2 => format!("{kind}.{}", parts[1]),
            _ => continue,
        };
        let id = hcl_node(f, &root, &name, kind, line(block));
        if let Some(body) = body {
            let sensitive = matches!(kind.as_str(), "variable" | "output")
                && (parts.get(1).is_some_and(|name| hcl_sensitive_key(name))
                    || children(body).into_iter().any(|attr| {
                        let parts: Vec<_> = children(attr)
                            .into_iter()
                            .filter(|n| n.kind() != "comment")
                            .collect();
                        attr.kind() == "attribute"
                            && parts.len() == 2
                            && text(parts[0]) == "sensitive"
                            && hcl_value(source, parts[1], 0) != Value::Bool(false)
                    }));
            let attributes = hcl_attributes(source, body, sensitive);
            if let Some(n) = f.0.nodes.iter_mut().find(|n| n.id == id) {
                n.metadata["attributes"] = attributes;
            }
            if kind == "module" && f.0.path.ends_with(".tf") {
                let sources: Vec<_> = children(body)
                    .into_iter()
                    .filter(|n| {
                        n.kind() == "attribute"
                            && children(*n).first().is_some_and(|n| text(*n) == "source")
                    })
                    .collect();
                if let [attr] = sources.as_slice() {
                    let attr = *attr;
                    let parts: Vec<_> = children(attr)
                        .into_iter()
                        .filter(|n| n.kind() != "comment")
                        .collect();
                    if parts.len() >= 2
                        && text(parts[0]) == "source"
                        && let Some(module_source) = hcl_string(source, parts[1])
                    {
                        let redacted = hcl_credential(&module_source);
                        if let Some(n) = f.0.nodes.iter_mut().find(|n| n.id == id) {
                            n.metadata["module_source"] = json!(if redacted {
                                "[redacted]"
                            } else {
                                &module_source
                            });
                            n.metadata["module_source_line"] = json!(line(attr));
                        }
                        if !redacted
                            && (module_source.starts_with("./") || module_source.starts_with("../"))
                        {
                            let keys = terraform_target(&f.0.path, &module_source)
                                .map(|p| vec![format!("terraform:directory:{p}")])
                                .unwrap_or_default();
                            f.reference(&id, &module_source, "module_source", keys, line(attr));
                        }
                    }
                }
            }
            hcl_refs(f, source, body, &id, "references", &[]);
        }
    }
    Ok(())
}

pub(super) fn hcl_node(f: &mut Facts, root: &str, name: &str, kind: &str, ln: u32) -> String {
    let id = f.node(
        name,
        kind,
        Some(format!("terraform:{}:{name}", directory(&f.0.path))),
        ln,
        json!({"language":"terraform","directory":directory(&f.0.path)}),
    );
    f.edge(root, &id, "contains", ln);
    id
}

pub(super) fn hcl_refs(
    f: &mut Facts,
    source: &str,
    n: Syntax<'_>,
    owner: &str,
    relation: &str,
    shadowed: &[String],
) {
    let text = |n: Syntax<'_>| &source[n.byte_range()];
    let cs = children(n);
    let relation =
        if n.kind() == "attribute" && cs.first().is_some_and(|n| text(*n) == "depends_on") {
            "depends_on"
        } else {
            relation
        };
    let mut shadowed = shadowed.to_vec();
    // A for-expression's iterator identifiers are lexical locals, not resources.
    if n.kind().starts_with("for_") {
        for c in &cs {
            if c.kind() == "for_intro" {
                shadowed.extend(
                    children(*c)
                        .into_iter()
                        .filter(|n| n.kind() == "identifier")
                        .map(|n| text(n).to_owned()),
                );
            }
        }
    }
    for (i, c) in cs.iter().enumerate() {
        if c.kind() == "variable_expr" {
            let head = text(*c);
            if ["count", "each", "self", "path", "terraform"].contains(&head)
                || shadowed.iter().any(|s| s == head)
            {
                continue;
            }
            let attrs: Vec<_> = cs[i + 1..]
                .iter()
                .take_while(|n| matches!(n.kind(), "get_attr" | "index" | "splat"))
                .filter(|n| n.kind() == "get_attr")
                .filter_map(|n| children(*n).first().map(|n| text(*n).to_owned()))
                .collect();
            // Only a contiguous module.call.output traversal identifies an output.
            // Indexed/splat module collections require evaluation and stay on the call node.
            if head == "module"
                && cs.get(i + 1).is_some_and(|n| n.kind() == "get_attr")
                && cs.get(i + 2).is_some_and(|n| n.kind() == "get_attr")
                && attrs.len() >= 2
            {
                let label = format!("module.{}.{}", attrs[0], attrs[1]);
                f.reference(
                    owner,
                    &label,
                    relation,
                    vec![format!(
                        "terraform:module-output:{}:{}:{}",
                        directory(&f.0.path),
                        attrs[0],
                        attrs[1]
                    )],
                    line(*c),
                );
            }
            let address = if head == "data" && attrs.len() >= 2 {
                Some(format!("data.{}.{}", attrs[0], attrs[1]))
            } else if head != "data" && !attrs.is_empty() {
                Some(format!("{head}.{}", attrs[0]))
            } else {
                None
            };
            if let Some(address) = address {
                f.reference(
                    owner,
                    &address,
                    relation,
                    vec![format!("terraform:{}:{address}", directory(&f.0.path))],
                    line(*c),
                );
            }
        }
        hcl_refs(f, source, *c, owner, relation, &shadowed);
    }
}

pub(super) fn sql_tokens(source: &str) -> Vec<SqlToken<'_>> {
    let b = source.as_bytes();
    let mut i = 0;
    let mut out = vec![];
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b.get(i..i + 2) == Some(b"--") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b.get(i..i + 2) == Some(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b.get(i..i + 2) == Some(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b.get(i..i + 2) == Some(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let start = i;
        let kind;
        if matches!(b[i], b'\'' | b'"' | b'`' | b'[') {
            let open = b[i];
            let close = if open == b'[' { b']' } else { open };
            kind = if open == b'\'' { b's' } else { b'i' };
            i += 1;
            while i < b.len() {
                if b[i] == close {
                    i += 1;
                    if b.get(i) == Some(&close) {
                        i += 1;
                    } else {
                        break;
                    }
                } else if b[i] == b'\\' && open == b'\'' {
                    i = (i + 2).min(b.len());
                } else {
                    i += 1;
                }
            }
        } else if b[i] == b'$'
            && b.get(i + 1)
                .is_some_and(|c| *c == b'$' || c.is_ascii_alphabetic() || *c == b'_')
        {
            let mut tag_end = i + 1;
            while tag_end < b.len() && (b[tag_end].is_ascii_alphanumeric() || b[tag_end] == b'_') {
                tag_end += 1;
            }
            if b.get(tag_end) == Some(&b'$') {
                let tag = &source[i..tag_end + 1];
                i = source[tag_end + 1..]
                    .find(tag)
                    .map(|end| tag_end + 1 + end + tag.len())
                    .unwrap_or(b.len());
                kind = b'd';
            } else {
                i += 1;
                kind = b'p';
            }
        } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 128 {
            i += 1;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'$') || b[i] >= 128)
            {
                i += 1;
            }
            kind = b'w';
        } else {
            i += 1;
            kind = b'p';
        }
        out.push(SqlToken {
            text: &source[start..i],
            start,
            end: i,
            kind,
        });
    }
    out
}

pub(super) fn sql_identifier(t: &SqlToken<'_>) -> Option<(String, String)> {
    if t.kind == b'w' {
        return Some((t.text.into(), t.text.to_ascii_lowercase()));
    }
    if t.kind != b'i' || t.text.len() < 2 {
        return None;
    }
    let raw = &t.text[1..t.text.len() - 1];
    let name = match t.text.as_bytes()[0] {
        b'[' => raw.replace("]]", "]"),
        b'`' => raw.replace("``", "`"),
        _ => raw.replace("\"\"", "\""),
    };
    Some((name.clone(), name))
}

pub(super) fn sql_name(tokens: &[SqlToken<'_>], start: usize) -> Option<(String, String, usize)> {
    let mut i = start;
    let mut labels = vec![];
    let mut keys = vec![];
    loop {
        let (label, key) = sql_identifier(tokens.get(i)?)?;
        labels.push(label);
        keys.push(key);
        i += 1;
        if tokens.get(i).is_some_and(|t| t.text == ".") {
            i += 1;
        } else {
            break;
        }
    }
    Some((labels.join("."), serde_json::to_string(&keys).ok()?, i))
}

pub(super) fn sql_relation_key(key: &str) -> String {
    format!("sql:relation:{key}")
}
