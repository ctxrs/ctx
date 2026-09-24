use super::common::*;
use crate::model::{Edge, FileFacts};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use tree_sitter::Node as Syntax;

pub(super) fn parse(path: &str, source: &str, hash: &str) -> Result<FileFacts> {
    let language = match path.rsplit('.').next().unwrap() {
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        _ => tree_sitter_javascript::LANGUAGE.into(),
    };
    let mut e = Extractor::new(path, source, hash, "javascript", module_path(path));
    let Some(tree) = javascript_tree(language, source, &mut e.facts)? else {
        return Ok(e.facts);
    };
    let root = tree.root_node();
    e.root(root, format!("javascript:module:{}", e.facts.module));
    let mut js = Javascript {
        e,
        exports: HashMap::new(),
        require_bindings: vec![],
        require_references: vec![],
        cjs_exports: vec![],
        cjs_dynamic: false,
        cjs_whole: 0,
        cjs_forward: vec![],
        type_bindings: HashMap::new(),
        type_references: vec![],
        component_references: HashSet::new(),
        callback_arguments: HashSet::new(),
        member_calls: vec![],
        callee_calls: HashMap::new(),
        callee_declarations: HashMap::new(),
        factory_returns: vec![],
        namespaces: HashMap::new(),
        receivers: HashMap::new(),
        classes: HashMap::new(),
    };
    if children(root)
        .iter()
        .any(|n| matches!(n.kind(), "import_statement" | "export_statement"))
    {
        js.e.facts.nodes[0].metadata["module_syntax"] = "esm".into();
    }
    js.exports(root);
    js.visit(root, 0);
    js.aliases(root);
    js.comments(root);
    Ok(js.finish())
}

fn javascript_tree(
    language: tree_sitter::Language,
    source: &str,
    facts: &mut FileFacts,
) -> Result<Option<tree_sitter::Tree>> {
    let parsed = tree(language.clone(), source, facts)?;
    if parsed.is_some()
        || source.len() > crate::parser::MAX_SOURCE_BYTES
        || !matches!(
            facts.path.rsplit('.').next(),
            Some("ts" | "tsx" | "mts" | "cts")
        )
    {
        return Ok(parsed);
    }
    // The bundled TypeScript grammar lacks variance modifiers. Recover only
    // declaration type parameters, then require a completely clean reparse.
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language)?;
    let Some(raw) = parser.parse(source, None) else {
        return Ok(None);
    };
    let Some(normalized) = variance_source(raw.root_node(), source) else {
        return Ok(None);
    };
    let diagnostics = facts.diagnostics.len();
    let recovered = tree(language, &normalized, facts)?;
    if recovered.is_some() {
        facts.diagnostics.clear();
    } else {
        facts.diagnostics.truncate(diagnostics);
    }
    Ok(recovered)
}

fn variance_source(root: Syntax<'_>, source: &str) -> Option<String> {
    let mut masked = source.as_bytes().to_vec();
    let mut changed = false;
    let mut pending = vec![(root, 0)];
    while let Some((node, depth)) = pending.pop() {
        if depth > 256 {
            return None;
        }
        pending.extend(children(node).into_iter().map(|n| (n, depth + 1)));
        if node.kind() != "type_parameters"
            || !node.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    "interface_declaration"
                        | "type_alias_declaration"
                        | "class_declaration"
                        | "abstract_class_declaration"
                        | "class"
                ) && p.child_by_field_name("type_parameters") == Some(node)
            })
        {
            continue;
        }
        // Only commas owned by this parameter list split parameters. Commas
        // inside constraints/defaults, strings and comments cannot start one.
        let mut cursor = node.walk();
        let items: Vec<_> = node.children(&mut cursor).collect();
        for parameter in items.split(|n| matches!(n.kind(), "<" | "," | ">")) {
            let mut leaves = vec![];
            let mut stack: Vec<_> = parameter.iter().rev().map(|n| (*n, 0)).collect();
            while let Some((n, depth)) = stack.pop() {
                if depth > 256 {
                    return None;
                }
                if n.kind() == "comment" {
                    continue;
                }
                if n.child_count() == 0 {
                    leaves.push(n);
                    if leaves.len() == 3 {
                        break;
                    }
                } else {
                    let mut cursor = n.walk();
                    let children: Vec<_> = n.children(&mut cursor).collect();
                    stack.extend(children.into_iter().rev().map(|n| (n, depth + 1)));
                }
            }
            let spelling = |i: usize| leaves.get(i).map(|n| &source[n.byte_range()]);
            let count = match (spelling(0), spelling(1)) {
                (Some("in"), Some("out")) => 2,
                (Some("in" | "out"), _) => 1,
                _ => continue,
            };
            if !leaves
                .get(count)
                .is_some_and(|n| matches!(n.kind(), "identifier" | "type_identifier"))
            {
                continue;
            }
            for modifier in &leaves[..count] {
                masked[modifier.byte_range()].fill(b' ');
                changed = true;
            }
        }
    }
    // Only ASCII modifier bytes change; all source offsets and line breaks stay.
    changed.then(|| String::from_utf8(masked).unwrap())
}
// Identifier escapes denote the same lexical binding as their literal spelling.
pub(super) fn identifier(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        if chars.next() != Some('u') {
            return raw.into();
        }
        let Some(first) = chars.next() else {
            return raw.into();
        };
        let digits: String = if first == '{' {
            let mut digits = String::new();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some(c) => digits.push(c),
                    None => return raw.into(),
                }
            }
            digits
        } else {
            std::iter::once(first)
                .chain(chars.by_ref().take(3))
                .collect()
        };
        let Some(c) = u32::from_str_radix(&digits, 16)
            .ok()
            .and_then(char::from_u32)
        else {
            return raw.into();
        };
        out.push(c);
    }
    out
}
fn token(node: Syntax<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|n| !n.is_named() && n.kind() == kind)
}
fn optional_chain(node: Syntax<'_>) -> bool {
    // JavaScript names this field; TypeScript optional calls use a bare token.
    node.child_by_field_name("optional_chain").is_some() || token(node, "?.")
}
fn module_key(module: &str) -> String {
    module.strip_prefix("import:").map_or_else(
        || format!("javascript:module:{module}"),
        |s| format!("javascript:import-module:{s}"),
    )
}
pub(super) fn commonjs_key(module: &str, name: &str) -> String {
    module.strip_prefix("import:").map_or_else(
        || format!("javascript:cjs:{module}:{name}"),
        |s| format!("javascript:cjs-import:{s}:{name}"),
    )
}
// Citation tokens are recognized only inside tree-sitter comment nodes.
fn citations(raw: &str) -> Vec<(usize, usize, String)> {
    let bytes = raw.as_bytes();
    let mut result = vec![];
    let mut start = 0;
    while start + 3 < bytes.len() {
        let kind = if bytes[start..start + 3].eq_ignore_ascii_case(b"ADR") {
            "ADR"
        } else if bytes[start..start + 3].eq_ignore_ascii_case(b"RFC") {
            "RFC"
        } else {
            start += 1;
            continue;
        };
        if raw[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            start += 3;
            continue;
        }
        let mut end = start + 3;
        if bytes.get(end) == Some(&b'-') {
            end += 1;
        } else {
            while matches!(bytes.get(end), Some(b' ' | b'\t')) {
                end += 1;
            }
        }
        let number = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if (1..=5).contains(&(end - number))
            && !raw[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            let digits = &raw[number..end];
            let label = if kind == "ADR" {
                format!("ADR-{digits:0>4}")
            } else {
                format!("RFC-{digits}")
            };
            result.push((start, end, label));
        }
        start = end.max(start + 3);
    }
    result
}
enum ExportTarget {
    Node(String),
    Local(String),
}
struct CjsExport {
    name: String,
    target: ExportTarget,
}
enum Receiver {
    FactoryValue,
    Written {
        scope: usize,
        parts: Vec<String>,
    },
    Constructed(String),
    This {
        class: usize,
        static_member: bool,
        property_written: bool,
    },
}
struct ClassMembers {
    id: String,
    key: String,
    dynamic_members: bool,
    methods: HashMap<(bool, String), Option<String>>,
    fields: HashMap<String, Option<Vec<String>>>,
}
// This suffix denotes a value declaration, never a runtime call target.
const DECLARED_CALLEE: &str = "#declared_callee";
struct CalleeDeclaration {
    node: usize,
    key: String,
    probe: String,
    initializer: String,
}
struct FactoryReturn {
    factory: usize,
    returned: usize,
    probe: Option<String>,
}
struct Javascript<'a> {
    e: Extractor<'a>,
    exports: HashMap<String, Vec<String>>,
    require_bindings: Vec<(usize, usize, String)>,
    require_references: Vec<(usize, usize)>,
    cjs_exports: Vec<CjsExport>,
    cjs_dynamic: bool,
    cjs_whole: usize,
    cjs_forward: Vec<String>,
    type_bindings: HashMap<(usize, String), Binding>,
    type_references: Vec<(usize, usize, Vec<String>)>,
    component_references: HashSet<String>,
    callback_arguments: HashSet<String>,
    member_calls: Vec<(String, String, Vec<String>)>,
    callee_calls: HashMap<String, String>,
    callee_declarations: HashMap<String, CalleeDeclaration>,
    factory_returns: Vec<FactoryReturn>,
    namespaces: HashMap<usize, (String, HashSet<String>)>,
    receivers: HashMap<String, Receiver>,
    classes: HashMap<usize, ClassMembers>,
}

mod javascript_callable_sequence;
mod javascript_comments;
mod javascript_finish;
mod javascript_function;
mod javascript_visit;
