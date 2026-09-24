use super::*;

pub(super) fn grammar(path: &str) -> Option<(&'static str, Language)> {
    Some(match path.rsplit_once('.')?.1 {
        "scala" | "sc" => ("scala", tree_sitter_scala::LANGUAGE.into()),
        "dart" => ("dart", tree_sitter_dart::LANGUAGE.into()),
        "m" | "mm" | "h" => ("objc", tree_sitter_objc::LANGUAGE.into()),
        "jl" => ("julia", tree_sitter_julia::LANGUAGE.into()),
        "f" | "F" | "f90" | "F90" | "f95" | "F95" | "f03" | "F03" | "f08" | "F08" => {
            ("fortran", tree_sitter_fortran::LANGUAGE.into())
        }
        "ml" => ("ocaml", tree_sitter_ocaml::LANGUAGE_OCAML.into()),
        "mli" => ("ocaml", tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into()),
        "pas" | "pp" | "dpr" | "dpk" | "lpr" | "inc" => {
            ("pascal", tree_sitter_pascal::LANGUAGE.into())
        }
        "lisp" | "cl" | "lsp" | "asd" => (
            "commonlisp",
            tree_sitter_commonlisp::LANGUAGE_COMMONLISP.into(),
        ),
        "v" | "sv" | "svh" => ("verilog", tree_sitter_verilog::LANGUAGE.into()),
        "zig" => ("zig", tree_sitter_zig::LANGUAGE.into()),
        "cls" | "trigger" => ("apex", tree_sitter_sfapex::apex::LANGUAGE.into()),
        "groovy" | "gvy" | "gy" | "gsh" | "gradle" => {
            ("groovy", tree_sitter_groovy::LANGUAGE.into())
        }
        "dm" | "dme" => ("dm", tree_sitter_dm::LANGUAGE.into()),
        _ => return None,
    })
}

pub(super) fn parse_grammar(
    path: &str,
    source: &str,
    hash: &str,
    lang: &'static str,
    language: Language,
) -> Result<Option<FileFacts>> {
    let mut e = Extractor::new(path, source, hash, lang, module_path(path));
    // Sniff parsed Objective-C constructs, not keywords inside strings/comments.
    // A C header and a MATLAB .m file must remain available to other dispatchers.
    if lang == "objc" && !path.ends_with(".mm") {
        if source.len() > crate::parser::MAX_SOURCE_BYTES {
            diagnostic(
                &mut e.facts,
                None,
                "Source exceeds the 4 MiB indexing limit",
            );
            return Ok(Some(e.facts));
        }
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language)?;
        let Some(t) = parser.parse(source, None) else {
            return Ok(None);
        };
        if find(
            t.root_node(),
            &[
                "class_interface",
                "class_implementation",
                "protocol_declaration",
                "module_import",
                "message_expression",
                "class_declaration",
            ],
        )
        .is_none()
        {
            return Ok(None);
        }
    }
    let normalized = if lang == "groovy" && source.len() <= crate::parser::MAX_SOURCE_BYTES {
        normalize_groovy(source)
    } else {
        None
    };
    let parse_source = normalized.as_ref().map_or(source, |n| n.source.as_str());
    let Some(t) = tree(language, parse_source, &mut e.facts)? else {
        return Ok(Some(e.facts));
    };
    let root = t.root_node();
    e.root(root, format!("{lang}:file:{path}"));
    if let Some(normalized) = &normalized {
        e.facts.nodes[0].metadata["normalization"] = serde_json::json!(normalized.spans);
    }
    let mut prefix = format!("@{path}");
    if matches!(lang, "scala" | "groovy")
        && let Some(pkg) = child(root, &["package_clause", "package_declaration"])
        && let Some(n) = pkg
            .child_by_field_name("name")
            .or_else(|| child(pkg, &["identifier", "scoped_identifier"]))
        && pkg.child_by_field_name("body").is_none()
    {
        prefix = e.text(n).into();
    }
    let mut x = Extended {
        e,
        prefixes: vec![prefix],
        pending: vec![],
        contextual: vec![],
        value_types: HashMap::new(),
        dart_packages: HashSet::new(),
        dart_local_types: HashSet::new(),
        dart_bloc_types: HashMap::new(),
        cpp_defines: HashMap::new(),
        groovy_parameters: normalized.map_or_else(HashMap::new, |n| n.parameters),
    };
    if lang == "dart" {
        x.dart_environment(root);
    }
    let mut current = 0;
    for n in children(root) {
        if lang == "commonlisp" && n.kind() == "list_lit" {
            let values = children(n);
            if values.first().is_some_and(|v| x.text(*v) == "in-package") {
                if let Some(name) = values.get(1) {
                    let package = x.text(*name).trim_matches([':', '"']).to_string();
                    current = x.e.scope(0, package.clone(), None, false);
                    x.prefixes.push(package);
                }
                continue;
            }
        }
        x.visit(n, current);
    }
    for (n, scope, label, relation, parts, context) in std::mem::take(&mut x.contextual) {
        let keys = x.e.resolve(scope, &parts);
        x.e.reference(n, scope, label, relation, keys, &context);
    }
    for (n, scope, label, relation, parts) in std::mem::take(&mut x.pending) {
        let mut keys = x.e.resolve(scope, &parts);
        if keys.is_empty() && parts.len() == 1 {
            let mut parent = Some(scope);
            while let Some(s) = parent {
                if x.e.scopes[s].uncertain {
                    break;
                }
                if let Some(binding) = x.e.scopes[s].bindings.get(&parts[0]) {
                    if let Binding::Namespace { prefixes, .. } = binding {
                        keys = prefixes
                            .iter()
                            .map(|p| p.trim_end_matches('.').into())
                            .collect();
                    }
                    break;
                }
                parent = x.e.scopes[s].parent;
            }
        }
        x.e.reference(
            n,
            scope,
            label,
            relation,
            keys,
            "target is unavailable, dynamic, or ambiguous",
        );
    }
    Ok(Some(x.e.finish()))
}

pub(super) fn child<'t>(n: Syntax<'t>, kinds: &[&str]) -> Option<Syntax<'t>> {
    children(n).into_iter().find(|c| kinds.contains(&c.kind()))
}

pub(super) fn find<'t>(n: Syntax<'t>, kinds: &[&str]) -> Option<Syntax<'t>> {
    let mut stack = vec![(n, 0)];
    while let Some((n, depth)) = stack.pop() {
        if kinds.contains(&n.kind()) {
            return Some(n);
        }
        if depth < 256 {
            stack.extend(children(n).into_iter().rev().map(|c| (c, depth + 1)));
        }
    }
    None
}

pub(super) fn field<'t>(n: Syntax<'t>, fields: &[&str]) -> Option<Syntax<'t>> {
    fields.iter().find_map(|f| n.child_by_field_name(f))
}

pub(super) fn names(n: Syntax<'_>) -> bool {
    matches!(
        n.kind(),
        "identifier"
            | "type_identifier"
            | "name"
            | "sym_lit"
            | "value_name"
            | "value_pattern"
            | "module_name"
            | "type_constructor"
            | "constructor_name"
            | "simple_identifier"
            | "escaped_identifier"
            | "field_identifier"
    )
}

pub(super) fn asset_facts(path: &str, hash: &str) -> FileFacts {
    FileFacts {
        path: path.into(),
        hash: hash.into(),
        module: module_path(path),
        nodes: vec![],
        edges: vec![],
        references: vec![],
        diagnostics: vec![],
    }
}

pub(super) fn asset_node(
    f: &mut FileFacts,
    label: &str,
    kind: &str,
    line: u32,
    parent: Option<String>,
) -> String {
    data_node(f, "dm", label, kind, line, parent)
}

pub(super) fn data_node(
    f: &mut FileFacts,
    language: &str,
    label: &str,
    kind: &str,
    line: u32,
    parent: Option<String>,
) -> String {
    let id = format!("{language}:{}:{kind}:{}", f.path, f.nodes.len());
    f.nodes.push(crate::model::Node {
        id: id.clone(),
        label: label.into(),
        kind: kind.into(),
        file: f.path.clone(),
        line: Some(line),
        end_line: Some(line),
        qualified_name: None,
        binding_key: if parent.is_none() {
            Some(format!("{language}:file:{}", f.path))
        } else {
            None
        },
        metadata: serde_json::json!({"language":language}),
    });
    if let Some(parent) = parent {
        f.edges.push(crate::model::Edge {
            id: format!("contains:{id}"),
            source: parent,
            target: id.clone(),
            relation: "contains".into(),
            directed: true,
            file: Some(f.path.clone()),
            line: Some(line),
            confidence: "static".into(),
            metadata: serde_json::Value::Null,
        });
    }
    id
}

pub(super) fn asset_tokens(source: &str) -> Option<Vec<Token<'_>>> {
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut line = 1;
    let mut tokens = vec![];
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            line += u32::from(b == b'\n');
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i += 2;
            while i < bytes.len() && !bytes[i..].starts_with(b"*/") {
                line += u32::from(bytes[i] == b'\n');
                i += 1;
            }
            if i == bytes.len() {
                return None;
            }
            i += 2;
            continue;
        }
        let start = i;
        let first_line = line;
        let quoted = b == b'"' || b == b'\'';
        if quoted {
            i += 1;
            while i < bytes.len() && bytes[i] != b {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                if i < bytes.len() {
                    line += u32::from(bytes[i] == b'\n');
                    i += 1;
                }
            }
            if i == bytes.len() {
                return None;
            }
            i += 1;
        } else if b"=(),{}".contains(&b) {
            i += 1;
        } else {
            i += 1;
            while i < bytes.len()
                && !bytes[i].is_ascii_whitespace()
                && !b"=(),{}\"'".contains(&bytes[i])
            {
                i += 1;
            }
        }
        tokens.push(Token {
            text: &source[start..i],
            line: first_line,
            quoted,
        });
    }
    Some(tokens)
}

pub(super) fn asset(path: &str, source: &str, hash: &str) -> FileFacts {
    let mut f = asset_facts(path, hash);
    if source.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut f, None, "Source exceeds the 4 MiB indexing limit");
        return f;
    }
    let Some(tokens) = asset_tokens(source) else {
        diagnostic(&mut f, None, "Unterminated asset string or comment");
        return f;
    };
    let root = asset_node(&mut f, path, "asset", 1, None);
    if path.ends_with(".dmm") {
        let mut i = 0;
        while i + 2 < tokens.len() {
            if !(tokens[i].quoted && tokens[i + 1].text == "=" && tokens[i + 2].text == "(") {
                i += 1;
                continue;
            }
            i += 3;
            let mut parens = 1;
            let mut braces = 0;
            let mut entry = true;
            while i < tokens.len() && parens > 0 {
                let t = &tokens[i];
                if entry
                    && braces == 0
                    && parens == 1
                    && !t.quoted
                    && t.text.starts_with('/')
                    && t.text.split('/').skip(1).all(|p| {
                        !p.is_empty() && p.chars().all(|c| c.is_alphanumeric() || c == '_')
                    })
                {
                    f.references.push(crate::model::Reference {
                        id: format!("uses:{root}:{i}"),
                        source: root.clone(),
                        label: t.text.into(),
                        relation: "uses".into(),
                        file: path.into(),
                        line: t.line,
                        candidate_keys: vec![format!("dm:symbol:{}", t.text)],
                        reason: "map type is unavailable or ambiguous".into(),
                    });
                }
                if !t.quoted {
                    match t.text {
                        "(" => parens += 1,
                        ")" => parens -= 1,
                        "{" => braces += 1,
                        "}" => braces -= 1,
                        _ => {}
                    }
                    if braces < 0 {
                        break;
                    }
                    entry = t.text == "," && braces == 0 && parens == 1;
                } else {
                    entry = false;
                }
                i += 1;
            }
            if parens != 0 || braces != 0 {
                f.nodes.clear();
                f.edges.clear();
                f.references.clear();
                diagnostic(&mut f, None, "Unbalanced map tile definition");
                break;
            }
        }
    } else {
        let mut window = root.clone();
        let mut element = None;
        for ts in tokens.chunk_by(|a, b| a.line == b.line) {
            if ts.len() >= 2 && matches!(ts[0].text, "window" | "macro" | "menu") && ts[1].quoted {
                window = asset_node(
                    &mut f,
                    ts[1].text.trim_matches('"'),
                    ts[0].text,
                    ts[0].line,
                    Some(root.clone()),
                );
                element = None;
            } else if ts.len() >= 2 && ts[0].text == "elem" && ts[1].quoted {
                element = Some(asset_node(
                    &mut f,
                    ts[1].text.trim_matches('"'),
                    "control",
                    ts[0].line,
                    Some(window.clone()),
                ));
            } else if ts.len() == 3
                && ts[0].text == "type"
                && ts[1].text == "="
                && let Some(id) = &element
                && let Some(node) = f.nodes.iter_mut().find(|n| &n.id == id)
            {
                node.metadata["control_type"] = serde_json::json!(ts[2].text);
            }
        }
    }
    f
}

pub(super) fn pascal_identifier(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|part| {
            let mut cs = part.chars();
            cs.next().is_some_and(|c| c.is_alphabetic() || c == '_')
                && cs.all(|c| c.is_alphanumeric() || c == '_')
        })
}

pub(super) fn pascal_tokens(source: &str) -> Result<Vec<Token<'_>>> {
    let b = source.as_bytes();
    let mut i = 0;
    let mut line = 1;
    let mut tokens = vec![];
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            line += u32::from(b[i] == b'\n');
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i] == b'{' || b[i..].starts_with(b"(*") {
            let end: &[u8] = if b[i] == b'{' { b"}" } else { b"*)" };
            i += if b[i] == b'{' { 1 } else { 2 };
            while i < b.len() && !b[i..].starts_with(end) {
                line += u32::from(b[i] == b'\n');
                i += 1;
            }
            anyhow::ensure!(i < b.len(), "Unterminated Pascal comment or property blob");
            i += end.len();
            continue;
        }
        let start = i;
        let first_line = line;
        let quoted = b[i] == b'\'';
        if quoted {
            i += 1;
            loop {
                anyhow::ensure!(i < b.len(), "Unterminated Pascal string");
                if b[i] == b'\'' {
                    i += 1;
                    if i < b.len() && b[i] == b'\'' {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    line += u32::from(b[i] == b'\n');
                    i += 1;
                }
            }
        } else if b"=:;(),[]<>".contains(&b[i]) {
            i += 1;
        } else {
            i += 1;
            while i < b.len() && !b[i].is_ascii_whitespace() && !b"=:;(),[]<>{'".contains(&b[i]) {
                i += 1;
            }
        }
        tokens.push(Token {
            text: &source[start..i],
            line: first_line,
            quoted,
        });
    }
    Ok(tokens)
}

pub(super) fn pascal_string(token: &Token<'_>) -> String {
    if token.quoted {
        token.text[1..token.text.len() - 1].replace("''", "'")
    } else {
        token.text.into()
    }
}

pub(super) fn data_reference(
    f: &mut FileFacts,
    owner: &str,
    label: &str,
    relation: &str,
    line: u32,
    keys: Vec<String>,
    reason: &str,
) {
    f.references.push(crate::model::Reference {
        id: format!("{relation}:{owner}:{}", f.references.len()),
        source: owner.into(),
        label: label.into(),
        relation: relation.into(),
        file: f.path.clone(),
        line,
        candidate_keys: keys,
        reason: reason.into(),
    });
}

pub(super) fn invalid_data(f: &mut FileFacts, message: &str) {
    f.nodes.clear();
    f.edges.clear();
    f.references.clear();
    diagnostic(f, None, message);
}

pub(super) fn pascal_unit_reference(
    f: &mut FileFacts,
    package: &str,
    name: &str,
    filename: Option<&str>,
    line: u32,
) {
    let label = if name.is_empty() {
        filename.unwrap_or("")
    } else {
        name
    };
    if label.is_empty() {
        return;
    }
    let id = data_node(
        f,
        "pascal",
        label,
        "unit_reference",
        line,
        Some(package.into()),
    );
    let key = if let Some(filename) = filename {
        let normalized = filename.replace('\\', "/");
        if normalized
            .rsplit('/')
            .next()
            .is_none_or(|p| matches!(p, "" | "." | ".."))
            || normalized.starts_with('/')
            || normalized.contains(':')
            || normalized.contains('$')
            || normalized.contains('%')
        {
            None
        } else {
            relative_path(f.path.rsplit_once('/').map_or("", |p| p.0), &normalized)
                .filter(|p| !p.is_empty())
                .map(|p| format!("pascal:file:{p}"))
        }
    } else if pascal_identifier(name) {
        Some(format!("pascal:symbol:{}", name.to_lowercase()))
    } else {
        None
    };
    data_reference(
        f,
        &id,
        label,
        "imports",
        line,
        key.into_iter().collect(),
        "package unit path is outside the root, unknown, or unavailable",
    );
}
