use super::*;

pub(super) fn language(path: &str) -> Option<(&'static str, Language)> {
    Some(match path.rsplit_once('.')?.1 {
        "c" | "h" => ("c", tree_sitter_c::LANGUAGE.into()),
        "cu" | "cuh" => ("cpp", tree_sitter_cuda::LANGUAGE.into()),
        "cc" | "cpp" | "cxx" | "C" | "hh" | "hpp" | "hxx" | "H" | "metal" => {
            ("cpp", tree_sitter_cpp::LANGUAGE.into())
        }
        "java" => ("java", tree_sitter_java::LANGUAGE.into()),
        "cs" => ("csharp", tree_sitter_c_sharp::LANGUAGE.into()),
        "kt" | "kts" => ("kotlin", tree_sitter_kotlin_ng::LANGUAGE.into()),
        "swift" => ("swift", tree_sitter_swift::LANGUAGE.into()),
        _ => return None,
    })
}

pub(super) fn swift_body_statement(n: Syntax<'_>) -> bool {
    n.parent().is_some_and(|p| {
        p.kind() == "statements" && p.parent().is_some_and(|p| p.kind() == "function_body")
    })
}

pub(super) fn swift_assignment_name(n: Syntax<'_>) -> Option<Syntax<'_>> {
    let target = n.child_by_field_name("target")?;
    target.named_child(0).filter(|c| {
        target.kind() == "directly_assignable_expression"
            && target.named_child_count() == 1
            && c.kind() == "simple_identifier"
    })
}

pub(super) fn declarator_name(mut n: Syntax<'_>) -> Option<Syntax<'_>> {
    loop {
        if matches!(
            n.kind(),
            "identifier"
                | "field_identifier"
                | "qualified_identifier"
                | "operator_name"
                | "destructor_name"
        ) {
            return Some(n);
        }
        n = n.child_by_field_name("declarator").or_else(|| {
            if n.kind() == "parenthesized_declarator" {
                n.named_child(0)
            } else {
                None
            }
        })?;
    }
}

pub(super) fn contains_kind(n: Syntax<'_>, kind: &str) -> bool {
    n.kind() == kind || children(n).into_iter().any(|c| contains_kind(c, kind))
}

pub(super) fn simple_name(text: &str) -> Option<String> {
    let text = text.trim().replace("::", ".");
    let text = text
        .strip_prefix("global.")
        .unwrap_or(&text)
        .trim_start_matches('.');
    if text.is_empty() {
        return None;
    }
    // Only identifier paths. Calls, subscripts, generic dispatch, optional chaining,
    // pointer dereferences, and operators never collapse to a bare method name.
    let parts = text
        .split('.')
        .map(|p| p.trim().trim_matches('`').trim_start_matches('@'))
        .collect::<Vec<_>>();
    if parts.iter().any(|p| {
        p.is_empty()
            || !p
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_alphabetic() || (i > 0 && c.is_numeric()))
    }) {
        return None;
    }
    Some(parts.join("."))
}

pub(super) fn swift_module_key(kind: &str, module: &str, symbol: &str) -> String {
    format!("swift:{kind}:module:{}:{module}:{symbol}", module.len())
}

pub(super) fn swift_export_key(kind: &str, module: &str, symbol: &str) -> String {
    format!("swift:{kind}:export:{}:{module}:{symbol}", module.len())
}

pub(super) fn header_key(kind: &str, header: &str, symbol: &str) -> String {
    format!("c-cpp:header-{kind}:{header}:{symbol}")
}

pub(super) fn is_header(path: &str) -> bool {
    matches!(
        path.rsplit('.').next(),
        Some("h" | "hh" | "hpp" | "hxx" | "H" | "cuh")
    )
}

pub(super) fn cpp_header(source: &str) -> Result<bool> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_cpp::LANGUAGE.into())?;
    let Some(tree) = parser.parse(source, None) else {
        return Ok(false);
    };
    let mut pending = vec![(tree.root_node(), 0)];
    while let Some((node, depth)) = pending.pop() {
        if depth > 256 {
            return Ok(false);
        }
        if !node.has_error()
            && (matches!(
                node.kind(),
                "class_specifier"
                    | "namespace_definition"
                    | "template_declaration"
                    | "alias_declaration"
                    | "base_class_clause"
                    | "access_specifier"
                    | "qualified_identifier"
            ) || (node.kind() == "field_declaration"
                && children(node).into_iter().any(is_function_declarator)))
        {
            return Ok(true);
        }
        if !matches!(
            node.kind(),
            "comment" | "string_literal" | "raw_string_literal" | "preproc_arg"
        ) {
            pending.extend(children(node).into_iter().map(|c| (c, depth + 1)));
        }
    }
    Ok(false)
}

pub(super) fn include_guard(n: Syntax<'_>, source: &str) -> bool {
    n.kind() == "preproc_ifdef"
        && source[n.byte_range()].trim_start().starts_with("#ifndef")
        && n.parent().is_some_and(|p| p.kind() == "translation_unit")
        && n.child_by_field_name("alternative").is_none()
        && n.child_by_field_name("name").is_some_and(|name| {
            children(n)
                .into_iter()
                .find(|c| c.kind() == "preproc_def")
                .and_then(|d| d.child_by_field_name("name"))
                .is_some_and(|defined| source[name.byte_range()] == source[defined.byte_range()])
        })
}

pub(super) fn is_function_declarator(mut n: Syntax<'_>) -> bool {
    loop {
        if n.kind() == "function_declarator" {
            return n
                .child_by_field_name("declarator")
                .is_some_and(|d| d.kind() != "parenthesized_declarator");
        }
        let Some(inner) = n.child_by_field_name("declarator") else {
            return false;
        };
        n = inner;
    }
}

pub(super) fn is_type(kind: &str) -> bool {
    matches!(
        kind,
        "type_identifier"
            | "scoped_identifier"
            | "user_type"
            | "qualified_identifier"
            | "scoped_type_identifier"
            | "qualified_name"
            | "generic_name"
            | "generic_type"
            | "template_type"
            | "array_type"
            | "nullable_type"
            | "optional_type"
            | "pointer_type"
            | "ref_type"
            | "function_type"
            | "type_annotation"
            | "tuple_type"
            | "type"
            | "opaque_type"
            | "existential_type"
            | "struct_specifier"
            | "enum_specifier"
            | "union_specifier"
    )
}

pub(super) fn declared_type(n: Syntax<'_>) -> Option<Syntax<'_>> {
    let ty = n
        .child_by_field_name("type")
        .or_else(|| children(n).into_iter().find(|c| is_type(c.kind())));
    ty.and_then(|t| {
        if t.kind() == "type_annotation" {
            t.child_by_field_name("name")
        } else {
            Some(t)
        }
    })
}

pub(super) fn type_name(n: Syntax<'_>, source: &str) -> Option<String> {
    if !is_type(n.kind()) && !matches!(n.kind(), "identifier" | "simple_identifier") {
        return None;
    }
    if let Some(name) = simple_name(&source[n.byte_range()]) {
        return Some(name);
    }
    if matches!(
        n.kind(),
        "generic_type"
            | "generic_name"
            | "template_type"
            | "user_type"
            | "struct_specifier"
            | "enum_specifier"
            | "union_specifier"
    ) {
        if let Some(name) = n
            .child_by_field_name("name")
            .and_then(|n| simple_name(&source[n.byte_range()]))
        {
            return Some(name);
        }
        let names: Vec<_> = children(n)
            .into_iter()
            .filter(|n| {
                matches!(
                    n.kind(),
                    "identifier"
                        | "type_identifier"
                        | "scoped_type_identifier"
                        | "qualified_name"
                        | "qualified_identifier"
                )
            })
            .filter_map(|n| simple_name(&source[n.byte_range()]))
            .collect();
        if !names.is_empty() {
            return Some(names.join("."));
        }
    }
    None
}

pub(super) fn dialect_tokens(source: &str) -> Vec<DialectToken<'_>> {
    let bytes = source.as_bytes();
    let mut tokens = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if bytes[i..].starts_with(b"//") {
            i += 2;
            while i < bytes.len() {
                if bytes[i] == b'\n' && !source[..i].trim_end_matches('\r').ends_with('\\') {
                    break;
                }
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i = source[i + 2..]
                .find("*/")
                .map_or(bytes.len(), |n| i + 2 + n + 2);
            continue;
        }
        let mut opaque = false;
        if bytes[i] == b'#' && source[..i].rsplit('\n').next().unwrap().trim().is_empty() {
            opaque = true;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\n' && !source[..i].trim_end_matches('\r').ends_with('\\') {
                    break;
                }
                i += 1;
            }
        } else if let Some(prefix) = ["R\"", "u8R\"", "uR\"", "UR\"", "LR\""]
            .into_iter()
            .find(|prefix| source[i..].starts_with(prefix))
        {
            opaque = true;
            let delimiter_start = i + prefix.len();
            i = source[delimiter_start..]
                .find('(')
                .filter(|n| *n <= 16)
                .and_then(|n| {
                    let end = format!("){}\"", &source[delimiter_start..delimiter_start + n]);
                    source[delimiter_start + n + 1..]
                        .find(&end)
                        .map(|offset| delimiter_start + n + 1 + offset + end.len())
                })
                .unwrap_or(bytes.len());
        } else if matches!(bytes[i], b'\'' | b'"') {
            opaque = true;
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else {
                    let closed = bytes[i] == quote;
                    i += 1;
                    if closed {
                        break;
                    }
                }
            }
        } else if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 128 {
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 128)
            {
                i += 1;
            }
        } else {
            i += if [b"::", b"[[", b"]]", b"^=", b"%="]
                .iter()
                .any(|p| bytes[i..].starts_with(*p))
            {
                2
            } else {
                1
            };
        }
        tokens.push(DialectToken {
            text: &source[start..i],
            start,
            end: i,
            opaque,
        });
    }
    tokens
}

pub(super) fn matching_token(
    tokens: &[DialectToken<'_>],
    start: usize,
    open: &str,
    close: &str,
) -> Option<usize> {
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(start) {
        if token.opaque {
            continue;
        }
        if token.text == open {
            depth += 1;
        }
        if depth > 256 {
            return None;
        }
        if token.text == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

pub(super) fn normalize_dialect(source: &str, metal: bool) -> Option<DialectSource> {
    let tokens = dialect_tokens(source);
    let managed = !metal
        && (tokens.windows(2).any(|t| {
            !t[0].opaque
                && !t[1].opaque
                && matches!(t[0].text, "ref" | "value" | "interface")
                && matches!(t[1].text, "class" | "struct")
        }) || tokens.windows(2).any(|t| {
            !t[0].opaque && t[0].text == "gcnew" && !t[1].opaque && simple_name(t[1].text).is_some()
        }) || tokens.windows(3).any(|t| {
            t[0].text == "[" && matches!(t[1].text, "assembly" | "module") && t[2].text == ":"
        }));
    if !metal && !managed {
        return None;
    }
    let mut edits: Vec<(usize, usize, &'static str)> = vec![];
    if managed {
        for (i, token) in tokens.iter().enumerate().filter(|(_, t)| !t.opaque) {
            if matches!(token.text, "ref" | "value" | "interface")
                && tokens
                    .get(i + 1)
                    .is_some_and(|t| matches!(t.text, "class" | "struct"))
            {
                edits.push((token.start, token.end, "managed_type"));
                if i > 0 && matches!(tokens[i - 1].text, "public" | "private") {
                    edits.push((tokens[i - 1].start, tokens[i - 1].end, "managed_visibility"));
                }
            } else if token.text == "gcnew"
                && tokens
                    .get(i + 1)
                    .is_some_and(|t| !t.opaque && simple_name(t.text).is_some())
            {
                edits.push((token.start, token.end, "managed_allocation"));
            } else if token.text == "["
                && tokens
                    .get(i + 1)
                    .is_some_and(|t| matches!(t.text, "assembly" | "module"))
                && tokens.get(i + 2).is_some_and(|t| t.text == ":")
                && let Some(end) = matching_token(&tokens, i, "[", "]")
            {
                edits.push((token.start, tokens[end].end, "managed_attribute"));
            }
        }
    }
    // Suffixes are only removed from declarations outside executable bodies.
    // This deliberately excludes ambiguous local `Name ^ value` expressions.
    let mut declaration_scope = vec![true];
    let mut start = 0;
    for (i, token) in tokens.iter().enumerate() {
        if token.opaque {
            if token.text.starts_with('#') {
                start = i + 1;
            }
            continue;
        }
        if token.text == "{" {
            let prefix = &tokens[start..i];
            let nested_type = !prefix.iter().any(|t| t.text == "=")
                && prefix
                    .iter()
                    .any(|t| matches!(t.text, "class" | "struct" | "namespace" | "union"));
            if *declaration_scope.last().unwrap() && !nested_type {
                normalize_declaration(prefix, metal, managed, &mut edits);
            }
            declaration_scope.push(nested_type);
            start = i + 1;
        } else if token.text == "}" {
            if declaration_scope.len() > 1 {
                declaration_scope.pop();
            }
            start = i + 1;
        } else if token.text == ";" {
            if *declaration_scope.last().unwrap() {
                normalize_declaration(&tokens[start..i], metal, managed, &mut edits);
            }
            start = i + 1;
        } else if token.text == ":"
            && i == start + 1
            && matches!(tokens[start].text, "public" | "private" | "protected")
        {
            start = i + 1;
        }
    }
    if edits.is_empty() {
        return None;
    }
    edits.sort_unstable();
    edits.dedup();
    let mut bytes = source.as_bytes().to_vec();
    let mut spans = vec![];
    for (start, end, kind) in edits {
        for byte in &mut bytes[start..end] {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b' ';
            }
        }
        if kind == "managed_allocation" {
            bytes[start..start + 3].copy_from_slice(b"new");
        }
        spans.push(json!({"kind":kind,"start_byte":start,"end_byte":end}));
    }
    Some(DialectSource {
        source: String::from_utf8(bytes).expect("only complete tokens replaced"),
        dialect: if metal { "metal" } else { "cpp_cli" },
        spans,
    })
}

pub(super) fn normalize_declaration(
    tokens: &[DialectToken<'_>],
    metal: bool,
    managed: bool,
    edits: &mut Vec<(usize, usize, &'static str)>,
) {
    if tokens.is_empty() {
        return;
    }
    let mut pending = vec![];
    let mut at = 0;
    let shader = metal && matches!(tokens[0].text, "kernel" | "vertex" | "fragment");
    if shader {
        pending.push((tokens[0].start, tokens[0].end, "metal_stage"));
        at += 1;
    }
    let Some(next) = declaration_type(tokens, at, managed, &mut pending) else {
        return;
    };
    at = next;
    // A declarator must follow the type. Expression operators and initializers
    // cannot be used as evidence for another type suffix.
    if tokens
        .get(at)
        .is_none_or(|t| t.opaque || simple_name(t.text).is_none())
    {
        return;
    }
    at += 1;
    if tokens.get(at).is_some_and(|t| t.text == "(") {
        let Some(end) = matching_token(tokens, at, "(", ")") else {
            return;
        };
        let mut first = at + 1;
        let mut depth = 0usize;
        for i in at + 1..=end {
            if matches!(tokens[i].text, "<" | "(" | "[[") {
                depth += 1;
            }
            if (i == end || tokens[i].text == ",") && depth == 0 {
                normalize_parameter(&tokens[first..i], metal, managed, &mut pending);
                first = i + 1;
            }
            if matches!(tokens[i].text, ">" | ")" | "]]") {
                depth = depth.saturating_sub(1);
            }
        }
    } else if shader
        || !tokens
            .get(at)
            .is_none_or(|t| matches!(t.text, "=" | "[" | ","))
    {
        return;
    }
    edits.extend(pending);
}

pub(super) fn normalize_parameter(
    tokens: &[DialectToken<'_>],
    metal: bool,
    managed: bool,
    edits: &mut Vec<(usize, usize, &'static str)>,
) {
    let mut at = 0;
    if metal
        && tokens
            .first()
            .is_some_and(|t| matches!(t.text, "device" | "constant" | "thread" | "threadgroup"))
    {
        let t = tokens[0];
        edits.push((t.start, t.end, "metal_address_space"));
        at += 1;
    }
    if declaration_type(tokens, at, managed, edits).is_none() {
        return;
    }
    if metal {
        for (i, token) in tokens.iter().enumerate() {
            if token.text == "[["
                && let Some(end) = matching_token(tokens, i, "[[", "]]")
                && metal_attribute(&tokens[i + 1..end])
            {
                edits.push((token.start, tokens[end].end, "metal_attribute"));
            }
        }
    }
}

pub(super) fn metal_attribute(tokens: &[DialectToken<'_>]) -> bool {
    match tokens {
        [name] => matches!(
            name.text,
            "thread_position_in_grid"
                | "thread_position_in_threadgroup"
                | "threadgroup_position_in_grid"
                | "threads_per_threadgroup"
                | "stage_in"
                | "position"
        ),
        [name, open, number, close] => {
            matches!(name.text, "buffer" | "texture" | "sampler" | "color")
                && open.text == "("
                && close.text == ")"
                && !number.text.is_empty()
                && number.text.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    }
}

pub(super) fn declaration_type(
    tokens: &[DialectToken<'_>],
    mut at: usize,
    managed: bool,
    edits: &mut Vec<(usize, usize, &'static str)>,
) -> Option<usize> {
    while tokens.get(at).is_some_and(|t| {
        matches!(
            t.text,
            "static"
                | "inline"
                | "const"
                | "volatile"
                | "virtual"
                | "extern"
                | "constexpr"
                | "unsigned"
                | "signed"
                | "long"
                | "short"
        )
    }) {
        at += 1;
    }
    let name = tokens.get(at)?;
    if name.opaque
        || simple_name(name.text).is_none()
        || matches!(
            name.text,
            "return" | "throw" | "if" | "while" | "for" | "switch" | "delete" | "new"
        )
    {
        return None;
    }
    at += 1;
    loop {
        if tokens.get(at).is_some_and(|t| t.text == "::") {
            let name = tokens.get(at + 1)?;
            if name.opaque || simple_name(name.text).is_none() {
                return None;
            }
            at += 2;
        } else if tokens.get(at).is_some_and(|t| t.text == "<") {
            let end = matching_token(tokens, at, "<", ">")?;
            if managed {
                for i in at + 1..end {
                    if matches!(tokens[i].text, "^" | "%")
                        && matches!(tokens[i + 1].text, ">" | ",")
                    {
                        edits.push((tokens[i].start, tokens[i].end, "managed_type_suffix"));
                    }
                }
            }
            at = end + 1;
        } else {
            break;
        }
    }
    while let Some(token) = tokens.get(at) {
        if matches!(token.text, "*" | "&" | "const" | "volatile") {
            at += 1;
        } else if managed && matches!(token.text, "^" | "%") {
            edits.push((token.start, token.end, "managed_type_suffix"));
            at += 1;
        } else {
            break;
        }
    }
    Some(at)
}

pub(super) fn compact_kotlin(source: &str) -> Option<DialectSource> {
    let tokens = dialect_tokens(source);
    let mut stack = vec![false];
    let mut start = 0;
    let mut bytes = source.as_bytes().to_vec();
    let mut spans = vec![];
    for (i, t) in tokens.iter().enumerate().filter(|(_, t)| !t.opaque) {
        if t.text == "{" {
            stack.push(
                tokens[start..i]
                    .iter()
                    .any(|t| !t.opaque && t.text == "interface"),
            );
            start = i + 1;
        } else if t.text == "}" {
            if *stack.last().unwrap()
                && let Some(fun) = (start..i).find(|j| tokens[*j].text == "fun")
                && !tokens[fun..i]
                    .iter()
                    .any(|t| matches!(t.text, "=" | ";" | "{"))
                && let Some(open) = (fun + 1..i).find(|j| tokens[*j].text == "(")
                && matching_token(&tokens, open, "(", ")").is_some_and(|end| end < i)
                && let Some(previous) = i.checked_sub(1).map(|j| tokens[j])
                && !source[previous.end..t.start].contains(['\n', '\r'])
                && let Some(offset) = source.as_bytes()[previous.end..t.start]
                    .iter()
                    .rposition(|b| matches!(b, b' ' | b'\t'))
            {
                let at = previous.end + offset;
                bytes[at] = b';';
                spans.push(
                    json!({"kind":"kotlin_abstract_separator","start_byte":at,"end_byte":at+1}),
                );
            }
            if stack.len() > 1 {
                stack.pop();
            }
            start = i + 1;
        } else if t.text == ";" {
            start = i + 1;
        }
    }
    (!spans.is_empty()).then(|| DialectSource {
        source: String::from_utf8(bytes).unwrap(),
        dialect: "kotlin",
        spans,
    })
}
