use super::*;

pub(super) fn pascal_package_xml(path: &str, source: &str, hash: &str) -> FileFacts {
    use quick_xml::{Reader, events::Event};
    let mut f = asset_facts(path, hash);
    if source.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut f, None, "Package exceeds the 4 MiB indexing limit");
        return f;
    }
    let root = data_node(&mut f, "pascal", path, "asset", 1, None);
    let fallback = module_path(path)
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string();
    let package = data_node(&mut f, "pascal", &fallback, "package", 1, Some(root));
    let result = (|| -> Result<()> {
        let mut reader = Reader::from_str(source);
        let mut stack = Vec::<String>::new();
        let mut roots = 0;
        let mut packages = 0;
        let mut unit = None::<(usize, String, Option<String>, u32)>;
        let mut previous_offset = 0;
        let mut line = 1;
        loop {
            let offset = reader.buffer_position() as usize;
            line += source[previous_offset..offset]
                .bytes()
                .filter(|b| *b == b'\n')
                .count() as u32;
            previous_offset = offset;
            match reader.read_event()? {
                event @ (Event::Start(_) | Event::Empty(_)) => {
                    let empty = matches!(event, Event::Empty(_));
                    let element = match event {
                        Event::Start(e) | Event::Empty(e) => e,
                        _ => unreachable!(),
                    };
                    let name = std::str::from_utf8(element.name().as_ref())?.to_string();
                    if stack.is_empty() {
                        roots += 1;
                        anyhow::ensure!(roots == 1, "Multiple XML roots");
                    }
                    let mut value = None;
                    for attr in element.attributes() {
                        let attr = attr?;
                        if attr.key.as_ref() == b"Value" {
                            value = Some(
                                attr.decoded_and_normalized_value(
                                    quick_xml::XmlVersion::Implicit1_0,
                                    reader.decoder(),
                                )?
                                .into_owned(),
                            );
                        }
                    }
                    if name == "Package" {
                        packages += 1;
                        anyhow::ensure!(packages == 1, "Multiple package declarations");
                    }
                    if stack.last().is_some_and(|p| p == "Package")
                        && name == "Name"
                        && let Some(value) = &value
                    {
                        let n = f.nodes.iter_mut().find(|n| n.id == package).unwrap();
                        n.label = value.clone();
                        n.binding_key = Some(format!("pascal:package:{}", value.to_lowercase()));
                    }
                    if stack.last().is_some_and(|p| p == "Files") && name.starts_with("Item") {
                        unit = Some((stack.len(), String::new(), None, line));
                    }
                    if let Some((depth, unit_name, filename, _)) = &mut unit
                        && stack.len() == *depth + 1
                    {
                        match (name.as_str(), value.as_ref()) {
                            ("UnitName", Some(value)) => *unit_name = value.clone(),
                            ("Filename", Some(value)) => *filename = Some(value.clone()),
                            _ => {}
                        }
                    }
                    if name == "PackageName"
                        && stack.len() >= 2
                        && stack[stack.len() - 2] == "RequiredPkgs"
                        && let Some(value) = value.filter(|v| !v.is_empty())
                    {
                        data_reference(
                            &mut f,
                            &package,
                            &value,
                            "imports",
                            line,
                            vec![format!("pascal:package:{}", value.to_lowercase())],
                            "required package is unavailable or ambiguous",
                        );
                    }
                    if !empty {
                        stack.push(name);
                        anyhow::ensure!(stack.len() <= 256, "XML nesting exceeds indexing limit");
                    } else if unit.as_ref().is_some_and(|u| u.0 == stack.len()) {
                        let (_, name, filename, line) = unit.take().unwrap();
                        pascal_unit_reference(&mut f, &package, &name, filename.as_deref(), line);
                    }
                }
                Event::End(_) => {
                    anyhow::ensure!(!stack.is_empty(), "Unmatched XML end");
                    stack.pop();
                    if unit.as_ref().is_some_and(|u| u.0 == stack.len()) {
                        let (_, name, filename, line) = unit.take().unwrap();
                        pascal_unit_reference(&mut f, &package, &name, filename.as_deref(), line);
                    }
                }
                Event::DocType(_) => anyhow::bail!("Package XML with DOCTYPE is unsupported"),
                Event::Text(t) if stack.is_empty() => anyhow::ensure!(
                    t.iter().all(u8::is_ascii_whitespace),
                    "Text outside XML root"
                ),
                Event::Eof => {
                    anyhow::ensure!(
                        roots == 1 && packages == 1 && stack.is_empty(),
                        "Incomplete Lazarus package XML"
                    );
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        invalid_data(&mut f, &format!("Invalid Lazarus package: {e}"));
    }
    f
}

pub(super) fn pascal_package_source(path: &str, source: &str, hash: &str) -> FileFacts {
    let mut f = asset_facts(path, hash);
    if source.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut f, None, "Package exceeds the 4 MiB indexing limit");
        return f;
    }
    let result = (|| -> Result<()> {
        let ts = pascal_tokens(source.trim_start_matches('\u{feff}'))?;
        anyhow::ensure!(
            ts.len() >= 4
                && ts[0].text.eq_ignore_ascii_case("package")
                && pascal_identifier(ts[1].text)
                && ts[2].text == ";",
            "Expected a Delphi package declaration"
        );
        let root = data_node(&mut f, "pascal", path, "asset", 1, None);
        let package = data_node(
            &mut f,
            "pascal",
            ts[1].text,
            "package",
            ts[0].line,
            Some(root),
        );
        f.nodes.last_mut().unwrap().binding_key =
            Some(format!("pascal:package:{}", ts[1].text.to_lowercase()));
        let mut i = 3;
        while i < ts.len() && !ts[i].text.eq_ignore_ascii_case("end.") {
            let contains = ts[i].text.eq_ignore_ascii_case("contains");
            anyhow::ensure!(
                contains || ts[i].text.eq_ignore_ascii_case("requires"),
                "Expected requires or contains in package"
            );
            i += 1;
            loop {
                anyhow::ensure!(
                    i < ts.len() && pascal_identifier(ts[i].text),
                    "Expected package or unit name"
                );
                let name = &ts[i];
                i += 1;
                let mut filename = None;
                if contains && ts.get(i).is_some_and(|t| t.text.eq_ignore_ascii_case("in")) {
                    i += 1;
                    anyhow::ensure!(
                        ts.get(i).is_some_and(|t| t.quoted),
                        "Expected quoted unit filename"
                    );
                    filename = Some(pascal_string(&ts[i]));
                    i += 1;
                }
                if contains {
                    pascal_unit_reference(
                        &mut f,
                        &package,
                        name.text,
                        filename.as_deref(),
                        name.line,
                    );
                } else {
                    data_reference(
                        &mut f,
                        &package,
                        name.text,
                        "imports",
                        name.line,
                        vec![format!("pascal:package:{}", name.text.to_lowercase())],
                        "required package is unavailable or ambiguous",
                    );
                }
                anyhow::ensure!(
                    i < ts.len() && matches!(ts[i].text, "," | ";"),
                    "Expected package list delimiter"
                );
                let end = ts[i].text == ";";
                i += 1;
                if end {
                    break;
                }
            }
        }
        anyhow::ensure!(
            i + 1 == ts.len() && ts[i].text.eq_ignore_ascii_case("end."),
            "Expected package end"
        );
        Ok(())
    })();
    if let Err(e) = result {
        invalid_data(&mut f, &format!("Invalid Delphi package: {e}"));
    }
    f
}

pub(super) fn dart_string(raw: &str) -> Option<String> {
    let (raw, literal) = raw.strip_prefix('r').map_or((raw, false), |s| (s, true));
    let quote = raw.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let value = raw.strip_prefix(quote)?.strip_suffix(quote)?;
    if !literal && value.contains('$') {
        return None;
    }
    if literal {
        return Some(value.into());
    }
    let mut result = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        result.push(if c != '\\' {
            c
        } else {
            match chars.next()? {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '\\' => '\\',
                '\'' => '\'',
                '"' => '"',
                _ => return None,
            }
        });
    }
    Some(result)
}

pub(super) fn project_definition(node: &crate::model::Node) -> bool {
    node.metadata["objc_class"].is_string()
        || node.metadata["objc_owner"].is_string()
        || node.metadata["pascal_class"].is_string()
        || node.metadata["pascal_owner"].is_string()
}

pub(super) fn project_key(node: &crate::model::Node) -> String {
    format!("extended:definition:{}", node.id)
}

pub(super) fn project_reference(
    facts: &mut FileFacts,
    source: &crate::model::Node,
    target: &crate::model::Node,
    relation: &str,
) {
    let id = format!("extended:{relation}:{}:{}", source.id, target.id);
    if facts.references.iter().any(|r| r.id == id) {
        return;
    }
    facts.references.push(crate::model::Reference {
        id,
        source: source.id.clone(),
        label: target.label.clone(),
        relation: relation.into(),
        file: facts.path.clone(),
        line: source.line.unwrap_or(1),
        candidate_keys: vec![project_key(target)],
        reason: "unique declaration proven by explicit project sources".into(),
    });
}

pub(super) fn groovy_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '_' | '$'))
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
        && !matches!(
            text,
            "def"
                | "class"
                | "interface"
                | "enum"
                | "trait"
                | "return"
                | "new"
                | "this"
                | "super"
                | "true"
                | "false"
                | "null"
                | "if"
                | "else"
                | "for"
                | "while"
                | "switch"
                | "case"
                | "break"
                | "continue"
                | "throw"
                | "try"
                | "catch"
                | "finally"
                | "import"
                | "package"
                | "extends"
                | "implements"
                | "public"
                | "private"
                | "protected"
                | "static"
                | "final"
                | "void"
                | "int"
                | "boolean"
                | "long"
                | "short"
                | "byte"
                | "char"
                | "float"
                | "double"
        )
}

pub(super) fn groovy_tokens(source: &str) -> Option<Vec<GroovyToken<'_>>> {
    let bytes = source.as_bytes();
    let mut tokens: Vec<GroovyToken<'_>> = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"//") || (i == 0 && bytes.starts_with(b"#!")) {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i += 2 + source[i + 2..].find("*/")? + 2;
            continue;
        }
        let start = i;
        let mut quoted = false;
        if bytes[i..].starts_with(b"$/") {
            // Dollar-slashy escaping is not needed for header recovery. Decline
            // this view rather than expose possible code-like string contents.
            return None;
        } else if matches!(bytes[i], b'\'' | b'"') {
            let quote = bytes[i];
            let triple = bytes.get(i..i + 3).is_some_and(|s| s == [quote; 3]);
            let width = if triple { 3 } else { 1 };
            i += width;
            loop {
                let byte = *bytes.get(i)?;
                if byte == b'\\' {
                    i = i.checked_add(2)?;
                    continue;
                }
                if quote == b'"' && byte == b'$' {
                    return None;
                }
                if !triple && matches!(byte, b'\r' | b'\n') {
                    return None;
                }
                if byte == quote
                    && (!triple || bytes.get(i..i + 3).is_some_and(|s| s == [quote; 3]))
                {
                    i += width;
                    break;
                }
                i += 1;
            }
            quoted = !triple;
        } else if bytes[i] == b'/'
            && tokens.last().is_none_or(|t| {
                matches!(
                    t.text,
                    "=" | "(" | "[" | "," | ":" | "return" | "throw" | "case" | "~" | "?"
                )
            })
        {
            // Slash literals in expression-start positions are opaque. Division
            // after an operand stays punctuation and cannot consume declarations.
            let mut end = i + 1;
            while end < bytes.len() && bytes[end] != b'/' {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            if end < bytes.len() {
                i = end + 1;
            } else {
                i += 1;
            }
        } else if bytes[i].is_ascii_alphanumeric()
            || matches!(bytes[i], b'_' | b'$')
            || bytes[i] >= 128
        {
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric()
                    || matches!(bytes[i], b'_' | b'$')
                    || bytes[i] >= 128)
            {
                i += 1;
            }
        } else {
            i += 1;
        }
        tokens.push(GroovyToken {
            text: &source[start..i],
            start,
            end: i,
            quoted,
        });
    }
    Some(tokens)
}

pub(super) fn normalize_groovy(source: &str) -> Option<GroovySource> {
    let tokens = groovy_tokens(source)?;
    let mut pairs = HashMap::new();
    let mut stack: Vec<(usize, &str)> = vec![];
    for (i, token) in tokens.iter().enumerate() {
        match token.text {
            "(" | "[" | "{" => {
                if stack.len() >= 256 {
                    return None;
                }
                stack.push((i, token.text));
            }
            ")" | "]" | "}" => {
                let (start, open) = stack.pop()?;
                if !matches!((open, token.text), ("(", ")") | ("[", "]") | ("{", "}")) {
                    return None;
                }
                pairs.insert(start, i);
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return None;
    }
    let mut normalized = GroovySource {
        source: source.into(),
        parameters: HashMap::new(),
        spans: vec![],
    };
    let mut edits = vec![];
    for (i, token) in tokens.iter().enumerate() {
        if token.text != "def" || i > 0 && tokens[i - 1].text == "." {
            continue;
        }
        let Some(name) = tokens.get(i + 1) else {
            continue;
        };
        if !(name.quoted || groovy_identifier(name.text)) || tokens.get(i + 2)?.text != "(" {
            continue;
        }
        let end = *pairs.get(&(i + 2))?;
        if tokens.get(end + 1).is_none_or(|t| t.text != "{") {
            continue;
        }
        // Only simple bare parameters are erased. Typed parameters stay under
        // the grammar's control; defaults/destructuring/varargs are not erased.
        let mut segments = vec![];
        let mut first = i + 3;
        for (j, token) in tokens.iter().enumerate().take(end).skip(first) {
            if token.text == "," {
                segments.push((first, j));
                first = j + 1;
            }
        }
        if first < end {
            segments.push((first, end));
        }
        let simple = first == end && end == i + 3
            || !segments.is_empty()
                && first < end
                && segments.iter().all(|(a, b)| {
                    *b == *a + 1 && groovy_identifier(tokens[*a].text)
                        || *b == *a + 2
                            && (groovy_identifier(tokens[*a].text)
                                || matches!(
                                    tokens[*a].text,
                                    "def"
                                        | "int"
                                        | "boolean"
                                        | "long"
                                        | "short"
                                        | "byte"
                                        | "char"
                                        | "float"
                                        | "double"
                                ))
                            && groovy_identifier(tokens[*a + 1].text)
                });
        if !simple {
            continue;
        }
        let mut removed = vec![];
        let mut retained = false;
        for (a, b) in segments {
            if b == a + 1 {
                removed.push(tokens[a].text.into());
                edits.push((tokens[a].start, tokens[a].end, b' '));
                if a > i + 3 {
                    let comma = &tokens[a - 1];
                    edits.push((comma.start, comma.end, b' '));
                }
            } else {
                // Keep one separator between retained typed parameters.
                if a > i + 3 && !retained {
                    let comma = &tokens[a - 1];
                    edits.push((comma.start, comma.end, b' '));
                }
                retained = true;
            }
        }
        if !removed.is_empty() {
            normalized.parameters.insert(name.start, removed);
            normalized.spans.push(serde_json::json!({"kind":"groovy_untyped_parameters", "start_byte":tokens[i + 2].end, "end_byte":tokens[end].start}));
        }
        if name.quoted {
            edits.push((name.start, name.end, b'_'));
            normalized.spans.push(serde_json::json!({"kind":"groovy_quoted_method", "start_byte":name.start, "end_byte":name.end}));
        }
        // Groovy terminates a final statement at `}`; this grammar requires
        // a separator for returns and class-method expression statements. Use
        // existing horizontal trivia only, inside this proven complete body.
        let body_end = *pairs.get(&(end + 1))?;
        if body_end > end + 2 {
            let last = &tokens[body_end - 1];
            let gap = &source[last.end..tokens[body_end].start];
            if !matches!(last.text, ";" | "}")
                && !gap.is_empty()
                && gap.bytes().all(|b| matches!(b, b' ' | b'\t'))
            {
                edits.push((last.end, last.end + 1, b';'));
                normalized.spans.push(serde_json::json!({"kind":"groovy_terminal_statement", "start_byte":last.end, "end_byte":last.end + 1}));
            }
        }
    }
    if edits.is_empty() {
        return None;
    }
    let mut bytes = source.as_bytes().to_vec();
    for (start, end, replacement) in edits {
        for byte in &mut bytes[start..end] {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = replacement;
            }
        }
    }
    normalized.source = String::from_utf8(bytes).ok()?;
    Some(normalized)
}
