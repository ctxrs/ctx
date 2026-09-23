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
impl Javascript<'_> {
    fn callable_sequence(mut node: Syntax<'_>) -> bool {
        while let Some(parent) = node.parent() {
            match parent.kind() {
                "statement_block"
                | "expression_statement"
                | "return_statement"
                | "lexical_declaration"
                | "variable_declaration" => {}
                "function_declaration"
                | "generator_function_declaration"
                | "function_expression"
                | "generator_function"
                | "arrow_function"
                | "method_definition" => {
                    return parent.child_by_field_name("body") == Some(node);
                }
                _ => return false,
            }
            node = parent;
        }
        false
    }
    fn declare_callable(&mut self, node: Syntax<'_>, scope: usize) {
        if let Some(name) = node
            .child_by_field_name("name")
            .filter(|n| n.kind() == "identifier")
            && node
                .child_by_field_name("value")
                .is_some_and(|n| n.kind() == "identifier")
        {
            self.e.declare_callable_local(scope, self.e.text(name));
        }
    }
    fn assign_callable(&mut self, node: Syntax<'_>, scope: usize) {
        let declaration = node.kind() == "variable_declarator";
        if let Some(name) = node
            .child_by_field_name(if declaration { "name" } else { "left" })
            .filter(|n| n.kind() == "identifier")
        {
            let rhs = node
                .child_by_field_name(if declaration { "value" } else { "right" })
                .filter(|n| n.kind() == "identifier" && Self::callable_sequence(node))
                .map(|n| self.e.text(n));
            self.e.assign_callable_local(scope, self.e.text(name), rhs);
        }
    }
    fn receiver(&mut self, scope: usize, name: &str, at: usize, evidence: Receiver) {
        let marker = format!("javascript:receiver:{}:{scope}:{at}", self.e.facts.path);
        self.receivers.insert(marker.clone(), evidence);
        self.e.bind(
            scope,
            name,
            Binding::Namespace {
                prefixes: vec![format!("{marker}:")],
                separator: ".",
            },
        );
    }
    fn annotated_type(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        let node = if node.kind() == "type_annotation" {
            node.named_child(0)?
        } else {
            node
        };
        self.type_path(node)
    }
    fn typed_binding(
        &mut self,
        name: Syntax<'_>,
        annotation: Option<Syntax<'_>>,
        value: Option<Syntax<'_>>,
        scope: usize,
        value_scope: usize,
    ) -> bool {
        if name.kind() != "identifier" {
            return false;
        }
        let evidence = if let Some(annotation) = annotation {
            let Some(parts) = self.annotated_type(annotation) else {
                return false;
            };
            Receiver::Written { scope, parts }
        } else if let Some(value) = value.filter(|n| n.kind() == "new_expression") {
            if value
                .child_by_field_name("constructor")
                .and_then(|n| self.dotted(n))
                .is_none()
            {
                return false;
            }
            Receiver::Constructed(format!(
                "call:{}:{}-{}",
                self.e.scopes[value_scope].owner,
                value.start_byte(),
                value.end_byte()
            ))
        } else {
            return false;
        };
        self.receiver(scope, self.e.text(name), name.start_byte(), evidence);
        true
    }
    fn factory_binding(&mut self, node: Syntax<'_>, scope: usize) -> bool {
        let Some(name) = node.child_by_field_name("name") else {
            return false;
        };
        let Some(value) = node.child_by_field_name("value") else {
            return false;
        };
        if name.kind() != "identifier"
            || !node
                .parent()
                .is_some_and(|p| p.kind() == "lexical_declaration" && token(p, "const"))
            || value.kind() != "call_expression"
            || optional_chain(value)
        {
            return false;
        }
        let Some(mut target) = value.child_by_field_name("function") else {
            return false;
        };
        // Only ordinary named factories. Return proofs are joined separately;
        // computed/optional selection and call chains remain unsupported.
        while target.kind() == "member_expression" {
            if optional_chain(target)
                || !target
                    .child_by_field_name("property")
                    .is_some_and(|n| n.kind() == "property_identifier")
            {
                return false;
            }
            let Some(object) = target.child_by_field_name("object") else {
                return false;
            };
            target = object;
        }
        if target.kind() != "identifier" {
            return false;
        }
        // Keep explicit receiver annotations available for existing member
        // navigation; the extra evidence concerns only this const binding.
        if !self.typed_binding(name, node.child_by_field_name("type"), None, scope, scope) {
            self.receiver(
                scope,
                self.e.text(name),
                name.start_byte(),
                Receiver::FactoryValue,
            );
        }
        let key = format!("{}{DECLARED_CALLEE}", self.key(scope, self.e.text(name)));
        let exact_key = self.exact_key(&key);
        let index = self.e.facts.nodes.len();
        self.e
            .define(node, scope, self.e.text(name), "constant", Some(key), false);
        let probe = format!(
            "call:{}:{}-{}",
            self.e.scopes[scope].owner,
            name.start_byte(),
            name.end_byte()
        );
        self.e.call(
            name,
            scope,
            name,
            Some(vec![self.e.text(name).into(), DECLARED_CALLEE.into()]),
        );
        self.callee_declarations.insert(
            format!(
                "javascript:receiver:{}:{scope}:{}",
                self.e.facts.path,
                name.start_byte()
            ),
            CalleeDeclaration {
                node: index,
                key: exact_key,
                probe,
                initializer: format!(
                    "call:{}:{}-{}",
                    self.e.scopes[scope].owner,
                    value.start_byte(),
                    value.end_byte()
                ),
            },
        );
        true
    }
    fn class_fields(&mut self, class: usize, body: Syntax<'_>) {
        for member in children(body) {
            let fields = if matches!(
                member.kind(),
                "public_field_definition" | "field_definition"
            ) && !token(member, "static")
            {
                vec![member]
            } else if member.kind() == "method_definition"
                && member
                    .child_by_field_name("name")
                    .is_some_and(|n| self.e.text(n) == "constructor")
            {
                member
                    .child_by_field_name("parameters")
                    .map(children)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|p| {
                        token(*p, "readonly")
                            || children(*p)
                                .iter()
                                .any(|n| n.kind() == "accessibility_modifier")
                    })
                    .collect()
            } else {
                vec![]
            };
            for field in fields {
                let Some(name) = field
                    .child_by_field_name("name")
                    .or_else(|| field.child_by_field_name("pattern"))
                else {
                    continue;
                };
                if !matches!(
                    name.kind(),
                    "identifier" | "property_identifier" | "private_property_identifier"
                ) {
                    continue;
                }
                let parts = field
                    .child_by_field_name("type")
                    .and_then(|ty| self.annotated_type(ty));
                let name = identifier(self.e.text(name));
                let class = self.classes.get_mut(&class).unwrap();
                class.methods.insert((false, name.clone()), None);
                class
                    .fields
                    .entry(name)
                    .and_modify(|p| *p = None)
                    .or_insert(parts);
            }
        }
    }
    fn bind_type(&mut self, scope: usize, name: &str, binding: Binding) {
        self.type_bindings
            .entry((scope, identifier(name)))
            .and_modify(|b| *b = Binding::Unknown)
            .or_insert(binding);
    }
    fn type_path(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        match node.kind() {
            "identifier" | "type_identifier" => Some(vec![identifier(self.e.text(node))]),
            "nested_type_identifier" | "nested_identifier" | "member_expression" => {
                let left = node
                    .child_by_field_name("module")
                    .or_else(|| node.child_by_field_name("object"))?;
                let right = node
                    .child_by_field_name("name")
                    .or_else(|| node.child_by_field_name("property"))?;
                let mut parts = self.type_path(left)?;
                parts.push(identifier(self.e.text(right)));
                Some(parts)
            }
            "generic_type" => self.type_path(node.child_by_field_name("name")?),
            _ => None,
        }
    }
    fn type_keys(&self, scope: usize, parts: &[String]) -> Vec<String> {
        let Some(name) = parts.first() else {
            return vec![];
        };
        let mut current = Some(scope);
        while let Some(i) = current {
            if self.e.scopes[i].uncertain {
                return vec![];
            }
            if let Some(binding) = self.type_bindings.get(&(i, identifier(name))) {
                return match binding {
                    Binding::Symbol { keys, .. } => keys
                        .iter()
                        .map(|key| {
                            if parts.len() == 1 {
                                key.clone()
                            } else {
                                format!("{key}.{}", parts[1..].join("."))
                            }
                        })
                        .collect(),
                    Binding::Namespace {
                        prefixes,
                        separator,
                    } if parts.len() > 1 => prefixes
                        .iter()
                        .map(|p| format!("{p}{}", parts[1..].join(separator)))
                        .collect(),
                    _ => vec![],
                };
            }
            current = self.e.scopes[i].parent;
        }
        vec![]
    }
    fn type_parameters(&mut self, node: Syntax<'_>, scope: usize) {
        if let Some(params) = node.child_by_field_name("type_parameters") {
            for param in children(params) {
                if let Some(name) = param.child_by_field_name("name") {
                    self.bind_type(scope, self.e.text(name), Binding::Unknown);
                }
            }
            self.type_refs(params, scope, "references_type");
        }
    }
    fn import_type(&self, node: Syntax<'_>) -> Option<(String, Vec<String>)> {
        if node.kind() == "member_expression" {
            let (module, mut parts) = self.import_type(node.child_by_field_name("object")?)?;
            parts.push(identifier(
                self.e.text(node.child_by_field_name("property")?),
            ));
            return Some((module, parts));
        }
        if node.kind() != "call_expression"
            || node.child_by_field_name("function")?.kind() != "import"
        {
            return None;
        }
        let args = node.child_by_field_name("arguments")?;
        if args.named_child_count() != 1 {
            return None;
        }
        let literal = args.named_child(0)?;
        (literal.kind() == "string")
            .then(|| self.string(literal))
            .flatten()
            .map(|module| (module, vec![]))
    }
    fn type_refs(&mut self, node: Syntax<'_>, scope: usize, relation: &str) {
        if let Some((module, parts)) = self.import_type(node) {
            let modules = self.modules(&module);
            self.e.reference(
                node,
                scope,
                module,
                "imports",
                modules.iter().map(|m| module_key(m)).collect(),
                "type import module is external, unavailable, or ambiguous",
            );
            if !parts.is_empty() {
                self.e.reference(
                    node,
                    scope,
                    self.e.text(node).into(),
                    relation,
                    modules
                        .iter()
                        .map(|m| format!("javascript:{m}:{}", parts.join(".")))
                        .collect(),
                    "type import is external, unavailable, or ambiguous",
                );
            }
            return;
        }
        if matches!(node.kind(), "type_identifier" | "nested_type_identifier") {
            if let Some(parts) = self.type_path(node) {
                let index = self.e.facts.references.len();
                self.e.reference(
                    node,
                    scope,
                    self.e.text(node).into(),
                    relation,
                    vec![],
                    "explicit type is external, unavailable, or ambiguous",
                );
                self.type_references.push((index, scope, parts));
            }
            return;
        }
        for n in children(node) {
            if node.kind() == "type_parameter" && Some(n) == node.child_by_field_name("name") {
                continue;
            }
            self.type_refs(
                n,
                scope,
                if node.kind() == "type_arguments" {
                    "type_argument"
                } else {
                    relation
                },
            );
        }
    }
    fn exact_key(&self, key: &str) -> String {
        if key.starts_with(&format!("javascript:local:{}:", self.e.facts.path)) {
            return key.into();
        }
        key.strip_prefix(&format!("javascript:{}:", self.e.facts.module))
            .map_or_else(
                || key.into(),
                |name| format!("javascript:file:{}:{name}", self.e.facts.path),
            )
    }
    fn string(&self, node: Syntax<'_>) -> Option<String> {
        let text = self.e.text(node);
        // Escaped module specifiers need JavaScript string decoding; never guess their target.
        if text.len() < 2 || text.contains('\\') {
            return None;
        }
        Some(text[1..text.len() - 1].into())
    }
    fn modules(&self, name: &str) -> Vec<String> {
        if !name.starts_with("./") && !name.starts_with("../") {
            return vec![format!("import:{name}")];
        }
        let base = self.e.facts.path.rsplit_once('/').map_or("", |(p, _)| p);
        let Some(path) = relative_path(base, name) else {
            return vec![];
        };
        let extension = path.rsplit('.').next().unwrap_or("");
        if matches!(
            extension,
            "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts"
        ) {
            // Keep the requested extension until project context chooses an actual file.
            vec![path]
        } else {
            vec![path.clone(), format!("{path}/index")]
        }
    }
    fn exports(&mut self, root: Syntax<'_>) {
        for node in children(root)
            .into_iter()
            .filter(|n| n.kind() == "export_statement")
        {
            if token(node, "type") || node.child_by_field_name("source").is_some() {
                continue;
            }
            let default = token(node, "default");
            if let Some(declaration) = node
                .child_by_field_name("declaration")
                .or_else(|| node.child_by_field_name("value"))
            {
                if let Some(name) = declaration.child_by_field_name("name") {
                    if name.kind() == "identifier" || name.kind() == "type_identifier" {
                        let name = identifier(self.e.text(name));
                        self.exports
                            .entry(name.clone())
                            .or_default()
                            .push(if default { "default".into() } else { name });
                    }
                } else if matches!(
                    declaration.kind(),
                    "lexical_declaration" | "variable_declaration"
                ) {
                    for var in children(declaration) {
                        if let Some(name) = var
                            .child_by_field_name("name")
                            .filter(|n| n.kind() == "identifier")
                        {
                            let name = identifier(self.e.text(name));
                            self.exports.entry(name.clone()).or_default().push(name);
                        }
                    }
                } else if default && declaration.kind() == "identifier" {
                    self.exports
                        .entry(identifier(self.e.text(declaration)))
                        .or_default()
                        .push("default".into());
                }
            }
            for clause in children(node)
                .into_iter()
                .filter(|n| n.kind() == "export_clause")
            {
                for spec in children(clause) {
                    if token(spec, "type") {
                        continue;
                    }
                    if let Some(name) = spec.child_by_field_name("name") {
                        let alias = spec.child_by_field_name("alias").unwrap_or(name);
                        self.exports
                            .entry(identifier(self.e.text(name)))
                            .or_default()
                            .push(identifier(self.e.text(alias)));
                    }
                }
            }
        }
    }
    fn key(&self, scope: usize, name: &str) -> String {
        let name = identifier(name);
        if scope == 0
            && let Some(names) = self.exports.get(&name).filter(|n| !n.is_empty())
        {
            return format!("javascript:{}:{}", self.e.facts.module, names[0]);
        }
        if let Some((prefix, exported)) = self.namespaces.get(&scope)
            && exported.contains(&name)
        {
            return format!("{prefix}.{name}");
        }
        self.e.local_key(scope, &name)
    }
    fn aliases(&mut self, root: Syntax<'_>) {
        for export in children(root)
            .into_iter()
            .filter(|n| n.kind() == "export_statement")
        {
            let type_only = token(export, "type");
            if type_only {
                continue;
            }
            if token(export, "default")
                && let Some(value) = export
                    .child_by_field_name("value")
                    .filter(|n| n.kind() == "identifier")
            {
                let targets = self.e.resolve(0, &[self.e.text(value).into()]);
                let alias_key = format!("javascript:{}:default", self.e.facts.module);
                if targets != [alias_key.clone()] {
                    let targets = self.e.resolve_reference(0, &[self.e.text(value).into()]);
                    let child = self
                        .e
                        .define(value, 0, "default", "alias", Some(alias_key), false);
                    self.e.reference(
                        value,
                        child,
                        self.e.text(value).into(),
                        "aliases",
                        targets,
                        "default export target is dynamic, unavailable, or ambiguous",
                    );
                }
            }
            let modules = export
                .child_by_field_name("source")
                .and_then(|n| self.string(n))
                .map(|s| self.modules(&s));
            for clause in children(export)
                .into_iter()
                .filter(|n| n.kind() == "export_clause")
            {
                for spec in children(clause) {
                    if token(spec, "type") {
                        continue;
                    }
                    let Some(name) = spec.child_by_field_name("name") else {
                        continue;
                    };
                    let local = self.e.text(name);
                    let exported = self
                        .e
                        .text(spec.child_by_field_name("alias").unwrap_or(name));
                    let alias_key = format!("javascript:{}:{exported}", self.e.facts.module);
                    let targets = modules.as_ref().map_or_else(
                        || self.e.resolve(0, &[local.into()]),
                        |m| {
                            m.iter()
                                .map(|m| format!("javascript:{m}:{local}"))
                                .collect()
                        },
                    );
                    if targets == [alias_key.clone()] {
                        continue;
                    }
                    let targets = if modules.is_none() {
                        self.e.resolve_reference(0, &[local.into()])
                    } else {
                        targets
                    };
                    let child = self
                        .e
                        .define(spec, 0, exported, "alias", Some(alias_key), false);
                    self.e.reference(
                        spec,
                        child,
                        local.into(),
                        "aliases",
                        targets,
                        "export target is dynamic, unavailable, or ambiguous",
                    );
                }
            }
        }
    }
    fn comments(&mut self, root: Syntax<'_>) {
        let mut pending = vec![root];
        let mut document_refs: HashMap<String, String> = HashMap::new();
        while let Some(comment) = pending.pop() {
            if comment.kind() != "comment" {
                pending.extend(children(comment).into_iter().rev());
                continue;
            }
            let raw = self.e.text(comment);
            let doc = raw.starts_with("/**");
            let text = raw
                .trim_start_matches('/')
                .trim_start_matches('*')
                .trim_end_matches("*/")
                .lines()
                .map(|line| line.trim().trim_start_matches('*').trim())
                .collect::<Vec<_>>()
                .join("\n");
            let lower = text.to_ascii_lowercase();
            let marked = lower.lines().any(|line| {
                [
                    "note:",
                    "important:",
                    "hack:",
                    "why:",
                    "rationale:",
                    "todo:",
                    "fixme:",
                ]
                .iter()
                .any(|marker| line.trim_start().starts_with(marker))
            });
            let rationale = marked
                || [
                    "rationale:",
                    "decision:",
                    "because ",
                    "trade-off",
                    "tradeoff",
                    "we chose ",
                ]
                .iter()
                .any(|m| lower.contains(m));
            let citations = citations(raw);
            let has_reference = !citations.is_empty()
                || text.contains("@see")
                || text.contains("@link")
                || text.contains("](");
            if !doc && !rationale && !has_reference {
                continue;
            }
            let mut owner = self
                .e
                .facts
                .nodes
                .iter()
                .filter(|n| {
                    matches!(n.kind.as_str(), "module" | "class" | "function" | "method")
                        && n.metadata["start_byte"]
                            .as_u64()
                            .is_some_and(|p| p <= comment.start_byte() as u64)
                        && n.metadata["end_byte"]
                            .as_u64()
                            .is_some_and(|p| p >= comment.end_byte() as u64)
                })
                .min_by_key(|n| {
                    n.metadata["end_byte"].as_u64().unwrap_or(u64::MAX)
                        - n.metadata["start_byte"].as_u64().unwrap_or(0)
                })
                .map(|n| n.id.clone())
                .unwrap_or_else(|| self.e.scopes[0].owner.clone());
            if doc && let Some(mut next) = comment.next_named_sibling() {
                if next.kind() == "export_statement" {
                    next = next
                        .child_by_field_name("declaration")
                        .or_else(|| next.child_by_field_name("value"))
                        .unwrap_or(next);
                }
                if matches!(next.kind(), "lexical_declaration" | "variable_declaration")
                    && let Some(value) = next
                        .named_child(0)
                        .and_then(|v| v.child_by_field_name("value"))
                        .filter(|n| {
                            matches!(n.kind(), "arrow_function" | "function_expression" | "class")
                        })
                {
                    next = value;
                }
                if (self.e.source[comment.end_byte()..next.start_byte()]
                    .trim()
                    .is_empty()
                    || comment.next_named_sibling().is_some_and(|n| {
                        self.e.source[comment.end_byte()..n.start_byte()]
                            .trim()
                            .is_empty()
                    }))
                    && let Some(node) = self.e.facts.nodes.iter().find(|n| {
                        n.kind != "module"
                            && n.metadata["start_byte"].as_u64() == Some(next.start_byte() as u64)
                    })
                {
                    owner = node.id.clone();
                }
            }
            let scope = self
                .e
                .scopes
                .iter()
                .position(|s| s.owner == owner)
                .unwrap_or(0);
            let label: String = text
                .lines()
                .find(|line| !line.is_empty())
                .unwrap_or("Source documentation")
                .chars()
                .take(160)
                .collect();
            let child = self.e.define(
                comment,
                scope,
                &label,
                if rationale {
                    "rationale"
                } else {
                    "documentation"
                },
                None,
                false,
            );
            let node = self.e.facts.nodes.last_mut().unwrap();
            node.metadata["evidence"] = raw.into();
            node.metadata["provenance"] = if rationale {
                "source_rationale_marker"
            } else {
                "source_doc_comment"
            }
            .into();
            if rationale {
                self.e.facts.edges.last_mut().unwrap().relation = "explains".into();
            }
            let mut links = HashSet::new();
            for event in pulldown_cmark::Parser::new(&text) {
                if let pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link {
                    dest_url, ..
                }) = event
                {
                    links.insert(dest_url.into_string());
                }
            }
            for line in text.lines() {
                let mut words = line.split_whitespace();
                while let Some(word) = words.next() {
                    if matches!(word.trim_start_matches('{'), "@see" | "@link")
                        && let Some(target) = words.next()
                    {
                        links.insert(
                            target
                                .trim_matches(|c| matches!(c, '}' | '`' | '<' | '>'))
                                .to_owned(),
                        );
                    }
                }
            }
            for (start, end, canonical) in citations {
                let start = comment.start_byte() + start;
                let end = comment.start_byte() + end;
                let line = 1 + self.e.source[..start]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count() as u32;
                let target = if let Some(id) = document_refs.get(&canonical) {
                    id.clone()
                } else {
                    self.e.define(
                        comment,
                        0,
                        &canonical,
                        "doc_ref",
                        Some(format!("docref:{}:{canonical}", self.e.facts.path)),
                        false,
                    );
                    let doc = self.e.facts.nodes.last_mut().unwrap();
                    doc.line = Some(line);
                    doc.end_line = Some(line);
                    doc.metadata["start_byte"] = start.into();
                    doc.metadata["end_byte"] = end.into();
                    let column = self.e.source[..start].rsplit('\n').next().unwrap().len();
                    doc.metadata["start_column"] = column.into();
                    doc.metadata["end_column"] = (column + end - start).into();
                    doc.metadata["citation_id"] = canonical.clone().into();
                    doc.metadata["spelling"] = self.e.source[start..end].into();
                    let id = doc.id.clone();
                    let edge = self.e.facts.edges.last_mut().unwrap();
                    edge.relation = "cites".into();
                    edge.line = Some(line);
                    document_refs.insert(canonical.clone(), id.clone());
                    id
                };
                self.e.facts.edges.push(Edge {
                    id: format!("cites:{}:{start}:{canonical}", self.e.scopes[child].owner),
                    source: self.e.scopes[child].owner.clone(),
                    target,
                    relation: "cites".into(),
                    directed: true,
                    file: Some(self.e.facts.path.clone()),
                    line: Some(line),
                    confidence: "static".into(),
                    metadata: serde_json::json!({"spelling": &self.e.source[start..end]}),
                });
            }
            let mut links: Vec<_> = links.into_iter().collect();
            links.sort();
            for target in links {
                let path = target.split('#').next().unwrap_or("");
                let local = !path.contains(':')
                    && !path.starts_with('/')
                    && !path.contains('\\')
                    && matches!(
                        path.rsplit('.').next(),
                        Some("md" | "mdx" | "rst" | "adoc" | "txt")
                    );
                let keys = if local {
                    relative_path(
                        self.e.facts.path.rsplit_once('/').map_or("", |(d, _)| d),
                        path,
                    )
                    .map(|p| vec![format!("file:{p}")])
                    .unwrap_or_default()
                } else {
                    vec![]
                };
                self.e.reference(comment, child, target, "references", keys, "literal source-comment document reference; unresolved identifiers are not guessed");
            }
        }
    }
    fn require_target<'t>(
        &self,
        node: Syntax<'t>,
    ) -> Option<(Syntax<'t>, Option<String>, Option<String>)> {
        let (call, imported) = if node.kind() == "member_expression" {
            let property = node.child_by_field_name("property")?;
            (
                node.child_by_field_name("object")?,
                Some(identifier(self.e.text(property))),
            )
        } else {
            (node, None)
        };
        if call.kind() != "call_expression" {
            return None;
        }
        let function = call.child_by_field_name("function")?;
        if function.kind() != "identifier" || identifier(self.e.text(function)) != "require" {
            return None;
        }
        let args = children(call.child_by_field_name("arguments")?)
            .into_iter()
            .filter(|n| n.kind() != "comment")
            .collect::<Vec<_>>();
        let module = if args.len() == 1 && args[0].kind() == "string" {
            self.string(args[0])
        } else {
            None
        };
        Some((call, imported, module))
    }
    fn require_declaration(
        &mut self,
        name: Syntax<'_>,
        value: Syntax<'_>,
        binding_scope: usize,
        scope: usize,
    ) -> bool {
        let Some((_, imported, module)) = self.require_target(value) else {
            return false;
        };
        let modules = module.map(|m| self.modules(&m)).unwrap_or_default();
        let mut bindings = vec![];
        if name.kind() == "identifier" {
            bindings.push((self.e.text(name).to_owned(), imported));
        } else if name.kind() == "object_pattern" && imported.is_none() {
            for item in children(name) {
                if item.kind() == "shorthand_property_identifier_pattern" {
                    let local = self.e.text(item).to_owned();
                    bindings.push((local.clone(), Some(identifier(&local))));
                } else if item.kind() == "pair_pattern" {
                    let Some(key) = item.child_by_field_name("key") else {
                        continue;
                    };
                    let Some(value) = item
                        .child_by_field_name("value")
                        .filter(|n| n.kind() == "identifier")
                    else {
                        continue;
                    };
                    let key = match key.kind() {
                        "property_identifier" => Some(identifier(self.e.text(key))),
                        "string" => self.string(key),
                        _ => None,
                    };
                    if let Some(key) = key {
                        bindings.push((self.e.text(value).into(), Some(key)));
                    }
                }
            }
            // Defaults, rest and computed destructuring are not fixed imported symbols.
            let previous: HashSet<_> = self.e.scopes[binding_scope]
                .bindings
                .keys()
                .cloned()
                .collect();
            self.pattern(name, binding_scope, false);
            for (local, _) in &bindings {
                if previous.contains(&identifier(local)) {
                    continue;
                }
                self.e.scopes[binding_scope]
                    .bindings
                    .remove(&identifier(local));
            }
        } else {
            self.pattern(name, binding_scope, false);
            return true;
        }
        for (local, imported) in bindings {
            self.e.bind(
                binding_scope,
                &local,
                Binding::Require {
                    modules: modules.clone(),
                    imported,
                },
            );
            self.require_bindings.push((scope, binding_scope, local));
        }
        true
    }
    fn export_value(&mut self, value: Syntax<'_>, name: String, scope: usize) {
        if matches!(
            value.kind(),
            "function_expression" | "arrow_function" | "generator_function" | "method_definition"
        ) {
            let first = self.e.facts.nodes.len();
            self.function(value, scope, None);
            let node = &mut self.e.facts.nodes[first];
            if node.label.starts_with("<anonymous@") {
                node.label = name.clone();
            }
            self.cjs_exports.push(CjsExport {
                name,
                target: ExportTarget::Node(node.id.clone()),
            });
        } else if matches!(value.kind(), "identifier" | "shorthand_property_identifier") {
            self.cjs_exports.push(CjsExport {
                name,
                target: ExportTarget::Local(identifier(self.e.text(value))),
            });
        } else {
            self.cjs_exports.push(CjsExport {
                name,
                target: ExportTarget::Local(String::new()),
            });
            self.visit(value, scope);
        }
    }
    fn commonjs_export(&mut self, node: Syntax<'_>, scope: usize) -> bool {
        let Some(left) = node.child_by_field_name("left") else {
            return false;
        };
        let parts = self
            .dotted(left)
            .map(|p| p.iter().map(|s| identifier(s)).collect::<Vec<_>>());
        let Some(parts) = parts else {
            if left.kind() == "subscript_expression"
                && left
                    .child_by_field_name("object")
                    .and_then(|o| self.dotted(o))
                    .is_some_and(|p| p == ["exports"] || p == ["module", "exports"])
            {
                self.cjs_dynamic = true;
            }
            return false;
        };
        let (whole, name) = match parts
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["module", "exports"] => (true, "default".to_owned()),
            ["module", "exports", name] => (false, (*name).to_owned()),
            ["exports", name] => {
                if self.cjs_whole > 0 {
                    return false;
                }
                (false, (*name).to_owned())
            }
            _ => return false,
        };
        let top_level = node.parent().is_some_and(|p| {
            p.kind() == "expression_statement" && p.parent().is_some_and(|p| p.kind() == "program")
        });
        if scope != 0 || !top_level || node.kind() != "assignment_expression" {
            self.cjs_dynamic = true;
            return false;
        }
        let Some(value) = node.child_by_field_name("right") else {
            return false;
        };
        if whole {
            self.cjs_whole += 1;
            if self.cjs_whole > 1 {
                self.cjs_dynamic = true;
            }
            self.cjs_exports.clear();
            self.cjs_forward.clear();
            if value.kind() == "object" {
                for member in children(value) {
                    match member.kind() {
                        "pair" => {
                            let Some(key) = member.child_by_field_name("key") else {
                                continue;
                            };
                            let name = match key.kind() {
                                "property_identifier" => Some(identifier(self.e.text(key))),
                                "string" => self.string(key),
                                _ => None,
                            };
                            if let (Some(name), Some(value)) =
                                (name, member.child_by_field_name("value"))
                            {
                                self.export_value(value, name, scope);
                            } else {
                                self.cjs_dynamic = true;
                                self.visit(member, scope);
                            }
                        }
                        "shorthand_property_identifier" => {
                            self.export_value(member, identifier(self.e.text(member)), scope)
                        }
                        "method_definition" => {
                            if let Some(name) = member
                                .child_by_field_name("name")
                                .filter(|n| n.kind() == "property_identifier")
                            {
                                self.export_value(member, identifier(self.e.text(name)), scope);
                            } else {
                                self.cjs_dynamic = true;
                                self.visit(member, scope);
                            }
                        }
                        "comment" => {}
                        _ => {
                            self.cjs_dynamic = true;
                            self.visit(member, scope);
                        }
                    }
                }
                return true;
            }
            if let Some((_, None, Some(module))) = self.require_target(value) {
                self.cjs_forward.push(module);
                self.visit(value, scope);
                return true;
            }
        }
        self.export_value(value, name, scope);
        true
    }
    fn finish(mut self) -> FileFacts {
        for (index, scope, parts) in &self.type_references {
            self.e.facts.references[*index].candidate_keys = self.type_keys(*scope, parts);
        }
        for (scope, binding_scope, name) in &self.require_bindings {
            if !self.e.unbound(*scope, "require") {
                self.e.scopes[*binding_scope]
                    .bindings
                    .insert(identifier(name), Binding::Unknown);
            }
        }
        for (scope, index) in &self.require_references {
            if !self.e.unbound(*scope, "require") {
                self.e.facts.references[*index].candidate_keys.clear();
                self.e.facts.references[*index].reason =
                    "shadowed or dynamic require binding".into();
            }
        }
        let valid =
            !self.cjs_dynamic && self.e.unbound(0, "module") && self.e.unbound(0, "exports");
        let mut targets = vec![];
        if valid {
            let mut names = HashSet::new();
            let mut duplicate = HashSet::new();
            for export in &self.cjs_exports {
                if !names.insert(export.name.clone()) {
                    duplicate.insert(export.name.clone());
                }
            }
            for export in std::mem::take(&mut self.cjs_exports) {
                if duplicate.contains(&export.name) {
                    continue;
                }
                let target = match export.target {
                    ExportTarget::Node(id) => Some((id, None)),
                    ExportTarget::Local(name) => {
                        let keys = self.e.resolve(0, &[name]);
                        self.e
                            .facts
                            .nodes
                            .iter()
                            .find(|n| n.binding_key.as_ref().is_some_and(|k| keys.contains(k)))
                            .map(|n| (n.id.clone(), n.binding_key.clone()))
                    }
                };
                if let Some((id, key)) = target {
                    targets.push((export.name, id, key));
                }
            }
        }
        let forward = valid && self.e.unbound(0, "require");
        let receiver_types: HashMap<_, _> = self
            .receivers
            .iter()
            .filter_map(|(marker, evidence)| match evidence {
                Receiver::Written { scope, parts } => {
                    Some((marker.clone(), self.type_keys(*scope, parts)))
                }
                _ => None,
            })
            .collect();
        let mut field_types = HashMap::new();
        for (scope, class) in &self.classes {
            for (name, parts) in &class.fields {
                field_types.insert(
                    (*scope, name.clone()),
                    parts
                        .as_ref()
                        .map(|p| self.type_keys(*scope, p))
                        .unwrap_or_default(),
                );
            }
        }
        let mut facts = self.e.finish();
        let valid_classes: HashSet<_> = facts
            .nodes
            .iter()
            .filter(|n| {
                n.kind == "class"
                    && n.binding_key.is_some()
                    && self
                        .classes
                        .values()
                        .any(|c| c.id == n.id && !c.dynamic_members)
            })
            .map(|n| n.id.clone())
            .collect();
        for node in &mut facts.nodes {
            if let Some(id) = node.metadata["declaring_class"].as_str() {
                let member = (node.metadata["static"] == true, identifier(&node.label));
                if !valid_classes.contains(id)
                    || !self
                        .classes
                        .values()
                        .any(|c| c.id == id && c.methods.get(&member).is_some_and(Option::is_some))
                {
                    node.binding_key = None;
                }
            }
        }
        let probes: HashMap<_, _> = facts
            .references
            .iter()
            .map(|r| (r.id.clone(), r.candidate_keys.clone()))
            .collect();
        let valid_callees: HashMap<_, _> = self
            .callee_declarations
            .iter()
            .filter(|(marker, declaration)| {
                probes
                    .get(&declaration.probe)
                    .is_some_and(|keys| keys == &[format!("{marker}:{DECLARED_CALLEE}")])
            })
            .map(|(marker, declaration)| (marker.clone(), declaration))
            .collect();
        for (marker, declaration) in &self.callee_declarations {
            let node = &mut facts.nodes[declaration.node];
            if valid_callees.contains_key(marker) {
                node.metadata["declared_callee_binding"] = true.into();
                node.metadata["factory_initializer"] = declaration.initializer.clone().into();
            } else {
                node.binding_key = None;
            }
        }
        let remove: HashSet<_> = self
            .member_calls
            .iter()
            .map(|(_, probe, _)| probe.clone())
            .chain(self.callee_calls.values().cloned())
            .chain(self.callee_declarations.values().map(|d| d.probe.clone()))
            .chain(self.factory_returns.iter().filter_map(|r| r.probe.clone()))
            .collect();
        let members: HashMap<_, _> = self
            .member_calls
            .into_iter()
            .map(|(call, probe, tail)| (call, (probe, tail)))
            .collect();
        for reference in &mut facts.references {
            if let Some((probe, tail)) = members.get(&reference.id) {
                let unresolved_member = reference.candidate_keys.is_empty();
                for key in probes.get(probe).into_iter().flatten() {
                    if unresolved_member {
                        reference
                            .candidate_keys
                            .push(format!("{key}.{}", tail.join(".")));
                    }
                    // A named value or imported namespace may denote a class. Its
                    // static declarations use separate keys from instance methods.
                    reference
                        .candidate_keys
                        .push(format!("{key}#static.{}", tail.join(".")));
                }
            }
        }
        facts.references.retain(|r| !remove.contains(&r.id));
        let mut declarations = vec![];
        for reference in &mut facts.references {
            if let Some(probe) = self.callee_calls.get(&reference.id) {
                let keys: Vec<_> = probes
                    .get(probe)
                    .into_iter()
                    .flatten()
                    .filter_map(|key| key.rsplit_once(':'))
                    .filter(|(_, member)| *member == DECLARED_CALLEE)
                    .filter_map(|(marker, _)| valid_callees.get(marker))
                    .map(|declaration| declaration.key.clone())
                    .collect();
                if !keys.is_empty() {
                    // Clone the written callsite, not the internal binding probe.
                    reference.candidate_keys.clear();
                    let mut declaration = reference.clone();
                    declaration.id.push_str(":declared_callee");
                    declaration.relation = "declared_callee".into();
                    declaration.candidate_keys = keys;
                    declaration.reason = "written immutable callee binding; factory result and runtime dispatch are unresolved".into();
                    declarations.push(declaration);
                }
            }
            let mut replaced = false;
            let mut declared_keys = vec![];
            reference.candidate_keys = reference
                .candidate_keys
                .iter()
                .flat_map(|key| {
                    let Some((marker, member)) = key.rsplit_once(':') else {
                        return vec![key.clone()];
                    };
                    let Some(evidence) = self.receivers.get(marker) else {
                        return vec![key.clone()];
                    };
                    replaced = true;
                    let method = |keys: &[String]| {
                        keys.iter()
                            .map(|key| format!("{key}#instance.{member}"))
                            .collect::<Vec<_>>()
                    };
                    match evidence {
                        Receiver::Written { .. } if !member.contains('.') => {
                            let types =
                                receiver_types.get(marker).map(Vec::as_slice).unwrap_or(&[]);
                            if reference.relation == "calls" {
                                declared_keys.extend(
                                    types.iter().map(|ty| format!("{ty}#declared.{member}")),
                                );
                            }
                            method(types)
                        }
                        Receiver::Constructed(call) if !member.contains('.') => {
                            method(probes.get(call).map(Vec::as_slice).unwrap_or(&[]))
                        }
                        Receiver::This {
                            class,
                            static_member,
                            property_written,
                        } if valid_classes.contains(&self.classes[class].id) => {
                            if let Some((field, target)) = member.split_once('.') {
                                if *property_written || *static_member || target.contains('.') {
                                    return vec![];
                                }
                                field_types
                                    .get(&(*class, field.into()))
                                    .into_iter()
                                    .flatten()
                                    .map(|key| format!("{key}#instance.{target}"))
                                    .collect()
                            } else {
                                let keys: Vec<_> = self.classes[class]
                                    .methods
                                    .get(&(*static_member, identifier(member)))
                                    .into_iter()
                                    .flatten()
                                    .cloned()
                                    .collect();
                                if *property_written {
                                    if reference.relation == "calls" {
                                        declared_keys.extend(keys);
                                    }
                                    vec![]
                                } else {
                                    keys
                                }
                            }
                        }
                        _ => vec![],
                    }
                })
                .collect();
            if replaced {
                reference.reason =
                    "written receiver type; target is unavailable, private, or ambiguous".into();
            }
            if !declared_keys.is_empty() {
                let mut declaration = reference.clone();
                declaration.id.push_str(":declared_member");
                declaration.relation = "declared_member".into();
                declaration.candidate_keys = declared_keys;
                declaration.reason =
                    "written receiver member declaration; runtime dispatch is unresolved".into();
                declarations.push(declaration);
            }
        }
        facts.references.extend(declarations);

        for reference in &mut facts.references {
            if self.component_references.contains(&reference.id) {
                reference.relation = "uses_component".into();
            }
        }
        for node in &mut facts.nodes {
            if let Some(name) = node
                .binding_key
                .as_deref()
                .filter(|key| !key.starts_with(&format!("javascript:local:{}:", facts.path)))
                .and_then(|key| key.strip_prefix(&format!("javascript:{}:", facts.module)))
            {
                let alias = format!("javascript:file:{}:{name}", facts.path);
                if !node.metadata["binding_aliases"].is_array() {
                    node.metadata["binding_aliases"] = serde_json::json!([]);
                }
                node.metadata["binding_aliases"]
                    .as_array_mut()
                    .unwrap()
                    .push(alias.into());
            }
        }
        for (name, id, original_key) in targets {
            if let Some(node) = facts.nodes.iter_mut().find(|n| {
                n.id == id
                    && original_key
                        .as_ref()
                        .is_none_or(|k| n.binding_key.as_ref() == Some(k))
            }) {
                let alias = commonjs_key(&facts.module, &name);
                if !node.metadata["binding_aliases"].is_array() {
                    node.metadata["binding_aliases"] = serde_json::json!([]);
                }
                node.metadata["binding_aliases"]
                    .as_array_mut()
                    .unwrap()
                    .push(alias.into());
                node.metadata["commonjs_export"] = true.into();
            }
        }
        let class_aliases: HashMap<_, Vec<String>> = facts
            .nodes
            .iter()
            .filter(|n| n.kind == "class" && n.binding_key.is_some())
            .map(|n| {
                (
                    n.id.clone(),
                    n.metadata["binding_aliases"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                )
            })
            .collect();
        for node in &mut facts.nodes {
            if node.binding_key.is_some()
                && node.metadata["visibility"] == "public"
                && let Some(aliases) = node.metadata["declaring_class"]
                    .as_str()
                    .and_then(|id| class_aliases.get(id))
            {
                let mode = if node.metadata["static"] == true {
                    "static"
                } else {
                    "instance"
                };
                let mut keys: Vec<_> = node.metadata["binding_aliases"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect();
                keys.extend(
                    aliases
                        .iter()
                        .map(|key| format!("{key}#{mode}.{}", identifier(&node.label))),
                );
                keys.sort();
                keys.dedup();
                node.metadata["binding_aliases"] = serde_json::json!(keys);
            }
        }
        // Contract signatures remain uncallable. Only typed-receiver declaration
        // references use these aliases; static and constructed receivers do not.
        let interfaces: HashMap<_, Vec<String>> = facts
            .nodes
            .iter()
            .filter(|n| n.kind == "interface" && n.binding_key.is_some())
            .filter(|n| {
                facts
                    .nodes
                    .iter()
                    .filter(|other| other.binding_key == n.binding_key)
                    .count()
                    == 1
            })
            .map(|n| {
                (
                    n.id.clone(),
                    n.binding_key
                        .iter()
                        .cloned()
                        .chain(
                            n.metadata["binding_aliases"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|v| v.as_str().map(str::to_owned)),
                        )
                        .collect(),
                )
            })
            .collect();
        let parents: HashMap<_, _> = facts
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .map(|e| (e.target.clone(), e.source.clone()))
            .collect();
        for node in &mut facts.nodes {
            if node.metadata["interface_signature"] == true
                && let Some(keys) = parents.get(&node.id).and_then(|id| interfaces.get(id))
            {
                node.metadata["binding_aliases"] = serde_json::json!(
                    keys.iter()
                        .map(|key| format!("{key}#declared.{}", identifier(&node.label)))
                        .collect::<Vec<_>>()
                );
            }
        }
        if forward && !self.cjs_forward.is_empty() {
            facts.nodes[0].metadata["commonjs_reexports"] = serde_json::json!(self.cjs_forward);
        }
        for returned in &self.factory_returns {
            let factory = &facts.nodes[returned.factory];
            let body = &facts.nodes[returned.returned];
            if factory.binding_key.is_none()
                || body.binding_key.is_none()
                || returned.probe.as_ref().is_some_and(|probe| {
                    probes.get(probe).is_none_or(|keys| {
                        keys.as_slice() != [body.binding_key.as_ref().unwrap().clone()]
                    })
                })
            {
                continue;
            }
            let key = format!(
                "javascript:local:{}:#factory-return:{}",
                facts.path, body.id
            );
            let proof = serde_json::json!({"target": body.id, "key": key});
            let body = &mut facts.nodes[returned.returned];
            if !body.metadata["binding_aliases"].is_array() {
                body.metadata["binding_aliases"] = serde_json::json!([]);
            }
            body.metadata["binding_aliases"]
                .as_array_mut()
                .unwrap()
                .push(key.into());
            facts.nodes[returned.factory].metadata["factory_return"] = proof;
        }
        if !self.callback_arguments.is_empty() {
            // The shared deferred resolver has now applied all lexical writes and
            // shadows. Only a source-proved function value gets a target; imports
            // and factory results do not establish callability here.
            let local = format!("javascript:local:{}:", facts.path);
            let exact = format!("javascript:file:{}:", facts.path);
            let functions: HashSet<_> = facts
                .nodes
                .iter()
                .filter(|n| n.kind == "function" && n.binding_key.is_some())
                .flat_map(|n| {
                    n.binding_key.as_deref().into_iter().chain(
                        n.metadata["binding_aliases"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(serde_json::Value::as_str),
                    )
                })
                .filter(|key| key.starts_with(&local) || key.starts_with(&exact))
                .collect();
            for reference in &mut facts.references {
                if self.callback_arguments.contains(&reference.id) {
                    reference.id = reference.id.replacen("call:", "references:", 1);
                    reference.relation = "references".into();
                    reference
                        .candidate_keys
                        .retain(|key| functions.contains(key.as_str()));
                    reference.reason = if reference.candidate_keys.is_empty() {
                        "callback argument; function binding is unproved, shadowed, or reassigned; invocation is not implied"
                    } else {
                        "callback argument; written function value, invocation is not implied"
                    }
                    .into();
                }
            }
        }
        facts
    }
    fn pattern(&mut self, node: Syntax<'_>, scope: usize, write: bool) {
        match node.kind() {
            "this" if write && self.write_this_property(scope) => {}
            "identifier" | "shorthand_property_identifier_pattern" | "this" => {
                if write {
                    if matches!(
                        identifier(self.e.text(node)).as_str(),
                        "require" | "module" | "exports"
                    ) {
                        self.e.bind(0, self.e.text(node), Binding::Unknown);
                    }
                    self.e.invalidate(scope, self.e.text(node));
                } else {
                    self.e.bind(scope, self.e.text(node), Binding::Unknown);
                }
            }
            "pair_pattern" => {
                if let Some(n) = node.child_by_field_name("value") {
                    self.pattern(n, scope, write);
                }
            }
            "assignment_pattern" | "object_assignment_pattern" => {
                if let Some(n) = node.child_by_field_name("left") {
                    self.pattern(n, scope, write);
                }
            }
            "required_parameter" | "optional_parameter" => {
                if let Some(n) = node
                    .child_by_field_name("pattern")
                    .or_else(|| node.child_by_field_name("name"))
                    && (write
                        || node.kind() != "required_parameter"
                        || !self.typed_binding(
                            n,
                            node.child_by_field_name("type"),
                            None,
                            scope,
                            scope,
                        ))
                {
                    self.pattern(n, scope, write);
                }
            }
            "member_expression" | "subscript_expression" if write => {
                if let Some(n) = node.child_by_field_name("object") {
                    self.pattern(n, scope, true);
                }
            }
            "formal_parameters" | "object_pattern" | "array_pattern" | "rest_pattern" => {
                for n in children(node) {
                    self.pattern(n, scope, write);
                }
            }
            _ => {}
        }
    }
    fn write_this_property(&mut self, scope: usize) -> bool {
        let mut current = Some(scope);
        while let Some(i) = current {
            if let Some(binding) = self.e.scopes[i].bindings.get("this") {
                if let Binding::Namespace { prefixes, .. } = binding
                    && prefixes.len() == 1
                    && let Some(marker) = prefixes[0].strip_suffix(':')
                    && let Some(Receiver::This {
                        property_written, ..
                    }) = self.receivers.get_mut(marker)
                {
                    // A property write cannot replace lexical `this`. Retain its
                    // declaration owner, but stop claiming runtime call targets.
                    *property_written = true;
                    return true;
                }
                return false;
            }
            current = self.e.scopes[i].parent;
        }
        false
    }
    fn dotted(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        match node.kind() {
            "identifier" | "this" => Some(vec![self.e.text(node).into()]),
            "member_expression" => {
                let mut p = self.dotted(node.child_by_field_name("object")?)?;
                p.push(self.e.text(node.child_by_field_name("property")?).into());
                Some(p)
            }
            "parenthesized_expression"
            | "non_null_expression"
            | "as_expression"
            | "satisfies_expression" => self.dotted(node.named_child(0)?),
            _ => None,
        }
    }
    fn call(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        target: Syntax<'_>,
        parts: Option<Vec<String>>,
    ) {
        if let Some(parts) = parts.as_ref().filter(|p| p.len() == 1)
            && matches!(node.kind(), "call_expression" | "new_expression")
            && !optional_chain(node)
        {
            let owner = &self.e.scopes[scope].owner;
            self.callee_calls.insert(
                format!("call:{owner}:{}-{}", node.start_byte(), node.end_byte()),
                format!("call:{owner}:{}-{}", target.start_byte(), target.end_byte()),
            );
            self.e.call(
                target,
                scope,
                target,
                Some(vec![parts[0].clone(), DECLARED_CALLEE.into()]),
            );
        }
        if let Some(parts) = parts.as_ref().filter(|p| p.len() > 1) {
            // Named imported namespaces still use symbol bindings. Probe their head
            // through the shared deferred shadow/write checks before adding members.
            let owner = &self.e.scopes[scope].owner;
            self.member_calls.push((
                format!("call:{owner}:{}-{}", node.start_byte(), node.end_byte()),
                format!("call:{owner}:{}-{}", target.start_byte(), target.end_byte()),
                vec![parts.last().unwrap().clone()],
            ));
            self.e.call(
                target,
                scope,
                target,
                Some(parts[..parts.len() - 1].to_vec()),
            );
        }
        if node.kind() == "call_expression"
            && target.kind() == "identifier"
            && Self::callable_sequence(node)
            && !optional_chain(node)
            && node
                .child_by_field_name("arguments")
                .is_some_and(|args| children(args).iter().all(|n| n.kind() == "comment"))
        {
            self.e.call_with_callable_local(node, scope, target, parts);
        } else {
            self.e.call(node, scope, target, parts);
        }
    }
    fn single_factory_return<'a>(&self, node: Syntax<'a>) -> Option<(Syntax<'a>, Syntax<'a>)> {
        if !matches!(node.kind(), "function_declaration" | "function_expression")
            || token(node, "async")
        {
            return None;
        }
        let body = node.child_by_field_name("body")?;
        let statements = children(body);
        let last = *statements.iter().rfind(|n| n.kind() != "comment")?;
        if last.kind() != "return_statement" {
            return None;
        }
        // Nested callable/class bodies have their own returns. Any other return,
        // including a conditional one, invalidates this deliberately small proof.
        let mut pending = statements.clone();
        while let Some(statement) = pending.pop() {
            if statement.kind() == "with_statement" {
                return None;
            }
            if matches!(
                statement.kind(),
                "function_declaration"
                    | "function_expression"
                    | "arrow_function"
                    | "generator_function_declaration"
                    | "generator_function"
                    | "method_definition"
                    | "class_declaration"
                    | "class"
            ) {
                continue;
            }
            if statement.kind() == "return_statement" {
                if statement != last {
                    return None;
                }
            } else {
                pending.extend(children(statement));
            }
        }
        let mut value = last.named_child(0)?;
        while matches!(
            value.kind(),
            "as_expression" | "parenthesized_expression" | "satisfies_expression"
        ) {
            value = value.named_child(0)?;
        }
        let target = if value.kind() == "identifier" {
            let mut targets = statements.iter().filter(|n| {
                n.kind() == "function_declaration"
                    && !token(**n, "async")
                    && n.child_by_field_name("name").is_some_and(|name| {
                        identifier(self.e.text(name)) == identifier(self.e.text(value))
                    })
            });
            let target = *targets.next()?;
            if targets.next().is_some() {
                return None;
            }
            target
        } else if value.kind() == "function_expression" && !token(value, "async") {
            value
        } else {
            return None;
        };
        Some((value, target))
    }
    fn function(&mut self, node: Syntax<'_>, scope: usize, assigned: Option<&str>) {
        let name_node = node.child_by_field_name("name");
        let name = assigned
            .map(str::to_owned)
            .or_else(|| name_node.map(|n| self.e.text(n).into()))
            .unwrap_or_else(|| format!("<anonymous@{}>", node.start_byte()));
        let method = node.kind() == "method_definition";
        let declaration = matches!(
            node.kind(),
            "function_declaration" | "generator_function_declaration"
        );
        let bind = assigned.is_some() || declaration;
        let static_member = token(node, "static");
        let visibility = children(node)
            .into_iter()
            .find(|n| n.kind() == "accessibility_modifier")
            .map(|n| self.e.text(n))
            .unwrap_or(if name.starts_with('#') {
                "private"
            } else {
                "public"
            });
        let plain_method = method
            && name != "constructor"
            && !token(node, "get")
            && !token(node, "set")
            && name_node.is_some_and(|n| {
                matches!(
                    n.kind(),
                    "property_identifier" | "private_property_identifier"
                )
            })
            && !children(node).iter().any(|n| n.kind() == "decorator");
        let key = if plain_method
            && let Some(class) = self.classes.get(&scope).filter(|c| !c.dynamic_members)
        {
            let mode = if static_member { "static" } else { "instance" };
            Some(if visibility == "public" {
                format!("{}#{mode}.{}", class.key, identifier(&name))
            } else {
                self.e
                    .local_key(scope, &format!("#{mode}.{}", identifier(&name)))
            })
        } else if bind || (!method && name_node.is_some()) {
            Some(self.key(scope, &name))
        } else {
            None
        };
        if method {
            for n in children(node)
                .into_iter()
                .filter(|n| matches!(n.kind(), "computed_property_name" | "decorator"))
            {
                self.visit(n, scope);
            }
        }
        let factory = self.e.facts.nodes.len();
        let child = self.e.define(
            node,
            scope,
            &name,
            if method { "method" } else { "function" },
            key.clone(),
            bind,
        );
        let exact_member_key = key.as_deref().map(|k| self.exact_key(k));
        if method && let Some(class) = self.classes.get_mut(&scope) {
            class
                .methods
                .entry((static_member, identifier(&name)))
                .and_modify(|k| *k = None)
                .or_insert(exact_member_key);
            let item = self.e.facts.nodes.last_mut().unwrap();
            item.metadata["declaring_class"] = class.id.clone().into();
            item.metadata["static"] = static_member.into();
            item.metadata["visibility"] = visibility.into();
            self.receiver(
                child,
                "this",
                node.start_byte(),
                Receiver::This {
                    class: scope,
                    static_member,
                    property_written: false,
                },
            );
        } else if node.kind() != "arrow_function" {
            self.e.bind(child, "this", Binding::Unknown);
        }
        if !declaration
            && !method
            && let (Some(n), Some(key)) = (name_node, key)
        {
            self.e.bind(
                child,
                self.e.text(n),
                Binding::Symbol {
                    keys: vec![key],
                    id: Some(self.e.scopes[child].owner.clone()),
                },
            );
        }
        self.type_parameters(node, child);
        if let Some(result) = node.child_by_field_name("return_type") {
            self.type_refs(result, child, "return_type");
        }
        for field in ["parameters", "parameter"] {
            if let Some(parameters) = node.child_by_field_name(field) {
                self.pattern(parameters, child, false);
                // Defaults execute in the function's parameter environment.
                self.visit(parameters, child);
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            let body_scope = self.e.scopes.len();
            self.visit(body, child);
            if let Some((value, target)) = self.single_factory_return(node)
                && let Some(returned) = self.e.facts.nodes.iter().position(|n| {
                    n.kind == "function" && n.metadata["start_byte"] == target.start_byte()
                })
            {
                let probe = if value.kind() == "identifier" {
                    self.e.call(
                        value,
                        body_scope,
                        value,
                        Some(vec![self.e.text(value).into()]),
                    );
                    Some(format!(
                        "call:{}:{}-{}",
                        self.e.scopes[body_scope].owner,
                        value.start_byte(),
                        value.end_byte()
                    ))
                } else {
                    None
                };
                self.factory_returns.push(FactoryReturn {
                    factory,
                    returned,
                    probe,
                });
            }
        }
    }
    fn import(&mut self, node: Syntax<'_>, scope: usize) {
        let source = node
            .child_by_field_name("source")
            .and_then(|n| self.string(n));
        let modules = source
            .as_deref()
            .map(|s| self.modules(s))
            .unwrap_or_default();
        self.e.reference(
            node,
            scope,
            source.clone().unwrap_or_else(|| self.e.text(node).into()),
            "imports",
            modules.iter().map(|m| module_key(m)).collect(),
            "module is external, unavailable, or ambiguous",
        );
        let type_only = token(node, "type");
        for clause in children(node)
            .into_iter()
            .filter(|n| n.kind() == "import_clause")
        {
            for part in children(clause) {
                match part.kind() {
                    "identifier" => self.import_binding(
                        part,
                        scope,
                        self.e.text(part),
                        "default",
                        &modules,
                        type_only,
                    ),
                    "namespace_import" => {
                        if let Some(name) = part.named_child(0) {
                            self.bind_type(
                                scope,
                                self.e.text(name),
                                Binding::Namespace {
                                    prefixes: modules
                                        .iter()
                                        .map(|m| format!("javascript:{m}:"))
                                        .collect(),
                                    separator: ".",
                                },
                            );
                            self.e.bind(
                                scope,
                                self.e.text(name),
                                if type_only {
                                    Binding::Unknown
                                } else {
                                    Binding::Namespace {
                                        prefixes: modules
                                            .iter()
                                            .map(|m| format!("javascript:{m}:"))
                                            .collect(),
                                        separator: ".",
                                    }
                                },
                            );
                        }
                    }
                    "named_imports" => {
                        for spec in children(part) {
                            if let Some(name) = spec.child_by_field_name("name") {
                                let alias = spec.child_by_field_name("alias").unwrap_or(name);
                                self.import_binding(
                                    spec,
                                    scope,
                                    self.e.text(alias),
                                    self.e.text(name),
                                    &modules,
                                    type_only || token(spec, "type"),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    fn import_binding(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        local: &str,
        name: &str,
        modules: &[String],
        type_only: bool,
    ) {
        let name = identifier(name);
        let keys: Vec<_> = modules
            .iter()
            .map(|m| format!("javascript:{m}:{name}"))
            .collect();
        self.bind_type(
            scope,
            local,
            Binding::Symbol {
                keys: keys.clone(),
                id: None,
            },
        );
        self.e.bind(
            scope,
            local,
            if type_only {
                Binding::Unknown
            } else {
                Binding::Symbol {
                    keys: keys.clone(),
                    id: None,
                }
            },
        );
        self.e.reference(
            node,
            scope,
            format!("{name} as {local}"),
            "imports",
            keys,
            "import is external, unavailable, or ambiguous",
        );
    }
    fn visit(&mut self, node: Syntax<'_>, scope: usize) {
        match node.kind() {
            "internal_module" | "module" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = self.e.text(name_node).to_owned();
                    let ambient = name_node.kind() == "string"
                        || node
                            .parent()
                            .is_some_and(|p| p.kind() == "ambient_declaration");
                    let key = self.key(scope, &name);
                    let child = self.e.define(
                        node,
                        scope,
                        name.trim_matches(['\'', '"']),
                        "namespace",
                        (!ambient).then(|| key.clone()),
                        false,
                    );
                    if !ambient {
                        let parts = self
                            .type_path(name_node)
                            .unwrap_or_else(|| vec![name.clone()]);
                        let root = parts.first().unwrap();
                        let root_key = self.key(scope, root);
                        let binding = Binding::Namespace {
                            prefixes: vec![format!("{}.", self.exact_key(&root_key))],
                            separator: ".",
                        };
                        self.e.bind(scope, root, binding.clone());
                        self.bind_type(scope, root, binding);
                    } else {
                        self.e.scopes[child].uncertain = true;
                    }
                    if let Some(body) = node.child_by_field_name("body") {
                        let exported = children(body)
                            .into_iter()
                            .filter(|n| n.kind() == "export_statement")
                            .filter_map(|n| n.child_by_field_name("declaration"))
                            .flat_map(|n| {
                                if matches!(
                                    n.kind(),
                                    "lexical_declaration" | "variable_declaration"
                                ) {
                                    children(n)
                                } else {
                                    vec![n]
                                }
                            })
                            .filter_map(|n| n.child_by_field_name("name"))
                            .map(|n| identifier(self.e.text(n)))
                            .collect();
                        self.namespaces.insert(child, (key, exported));
                        for n in children(body) {
                            self.visit(n, child);
                        }
                    }
                }
                return;
            }
            "function_signature" | "method_signature" | "abstract_method_signature" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let child = self.e.define(
                        node,
                        scope,
                        self.e.text(name),
                        if node.kind() == "function_signature" {
                            "function"
                        } else {
                            "method"
                        },
                        None,
                        false,
                    );
                    self.e.facts.nodes.last_mut().unwrap().metadata["interface_signature"] =
                        (node.kind() == "method_signature"
                            && name.kind() == "property_identifier"
                            && !token(node, "static")
                            && !token(node, "?")
                            && !token(node, "get")
                            && !token(node, "set")
                            && node.parent().is_some_and(|body| {
                                children(body)
                                    .iter()
                                    .filter(|member| {
                                        member.child_by_field_name("name").is_some_and(|other| {
                                            identifier(self.e.text(other))
                                                == identifier(self.e.text(name))
                                        })
                                    })
                                    .count()
                                    == 1
                            }))
                        .into();
                    self.type_parameters(node, child);
                    if let Some(params) = node.child_by_field_name("parameters") {
                        self.type_refs(params, child, "parameter_type");
                    }
                    if let Some(result) = node.child_by_field_name("return_type") {
                        self.type_refs(result, child, "return_type");
                    }
                }
                return;
            }
            "import_statement" => {
                self.import(node, scope);
                return;
            }
            "export_statement" => {
                if let Some(source) = node.child_by_field_name("source") {
                    if token(node, "*")
                        && !token(node, "type")
                        && !children(node)
                            .iter()
                            .any(|n| n.kind() == "namespace_export")
                        && let Some(module) = self.string(source)
                    {
                        if !self.e.facts.nodes[0].metadata["star_reexports"].is_array() {
                            self.e.facts.nodes[0].metadata["star_reexports"] =
                                serde_json::json!([]);
                        }
                        self.e.facts.nodes[0].metadata["star_reexports"]
                            .as_array_mut()
                            .unwrap()
                            .push(module.into());
                    }
                    self.e.reference(
                        node,
                        scope,
                        self.e.text(source).into(),
                        "imports",
                        self.string(source)
                            .map(|s| {
                                self.modules(&s)
                                    .into_iter()
                                    .map(|m| module_key(&m))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        "re-export forwarding is not resolved",
                    );
                    return;
                }
                if token(node, "default")
                    && let Some(value) = node
                        .child_by_field_name("value")
                        .or_else(|| node.child_by_field_name("declaration"))
                    && matches!(
                        value.kind(),
                        "function_expression" | "arrow_function" | "generator_function"
                    )
                    && value.child_by_field_name("name").is_none()
                {
                    self.exports
                        .insert("default".into(), vec!["default".into()]);
                    self.function(value, scope, Some("default"));
                    return;
                }
            }
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function"
            | "method_definition" => {
                self.function(node, scope, None);
                return;
            }
            "class_declaration"
            | "class"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration" => {
                let name =
                    node.child_by_field_name("name")
                        .map(|n| self.e.text(n).to_owned())
                        .unwrap_or_else(|| {
                            if node.parent().is_some_and(|p| {
                                p.kind() == "export_statement" && token(p, "default")
                            }) {
                                "default".into()
                            } else {
                                format!("<class@{}>", node.start_byte())
                            }
                        });
                if name == "default" {
                    self.exports.insert(name.clone(), vec![name.clone()]);
                }
                let kind = match node.kind() {
                    "interface_declaration" => "interface",
                    "type_alias_declaration" => "type",
                    "enum_declaration" => "enum",
                    _ => "class",
                };
                let type_only = matches!(kind, "interface" | "type");
                let key = self.key(scope, &name);
                let child = self
                    .e
                    .define(node, scope, &name, kind, Some(key.clone()), !type_only);
                self.bind_type(scope, &name, Binding::symbol(self.exact_key(&key)));
                self.type_parameters(node, child);
                if kind == "class" {
                    self.classes.insert(
                        child,
                        ClassMembers {
                            id: self.e.scopes[child].owner.clone(),
                            key,
                            dynamic_members: node.child_by_field_name("decorator").is_some()
                                || node.child_by_field_name("body").is_some_and(|body| {
                                    children(body).iter().any(|member| {
                                        member.kind() == "decorator"
                                            || member.child_by_field_name("decorator").is_some()
                                            || member.child_by_field_name("name").is_some_and(
                                                |name| name.kind() == "computed_property_name",
                                            )
                                    })
                                }),
                            methods: HashMap::new(),
                            fields: HashMap::new(),
                        },
                    );
                    if let Some(body) = node.child_by_field_name("body") {
                        self.class_fields(child, body);
                    }
                }
                for n in children(node) {
                    if Some(n) == node.child_by_field_name("body") {
                        if kind != "type" {
                            self.visit(n, child);
                        }
                    } else if Some(n) == node.child_by_field_name("value") {
                        self.type_refs(n, child, "references_type");
                    } else if n.kind() == "extends_type_clause" {
                        self.type_refs(n, child, "inherits");
                    } else if n.kind() == "class_heritage" {
                        for clause in children(n) {
                            let relation = if clause.kind() == "implements_clause" {
                                "implements"
                            } else {
                                "inherits"
                            };
                            // JavaScript has a direct heritage expression; TypeScript
                            // wraps bases in extends/implements clauses.
                            let bases = if matches!(
                                clause.kind(),
                                "extends_clause" | "implements_clause"
                            ) {
                                children(clause)
                            } else {
                                vec![clause]
                            };
                            for base in bases {
                                if let Some(parts) = self.type_path(base) {
                                    let index = self.e.facts.references.len();
                                    self.e.reference(
                                        base,
                                        child,
                                        self.e.text(base).into(),
                                        relation,
                                        vec![],
                                        "explicit heritage type is unavailable or ambiguous",
                                    );
                                    self.type_references.push((index, child, parts));
                                    if let Some(args) = base.child_by_field_name("type_arguments") {
                                        self.type_refs(args, child, "type_argument");
                                    }
                                } else if base.kind() == "type_arguments" {
                                    self.type_refs(base, child, "type_argument");
                                } else {
                                    self.visit(base, scope);
                                }
                            }
                        }
                    } else if n.kind() == "decorator" {
                        self.visit(n, scope);
                    }
                }
                return;
            }
            "variable_declaration" => {
                let mut function_scope = scope;
                while !self.e.scopes[function_scope].function {
                    function_scope = self.e.scopes[function_scope].parent.unwrap_or(0);
                }
                for var in children(node) {
                    self.declare_callable(var, function_scope);
                    if let (Some(name), Some(value)) = (
                        var.child_by_field_name("name"),
                        var.child_by_field_name("value"),
                    ) && self.require_declaration(name, value, function_scope, scope)
                    {
                        self.visit(value, scope);
                        continue;
                    }
                    if let Some(name) = var.child_by_field_name("name")
                        && !self.typed_binding(
                            name,
                            var.child_by_field_name("type"),
                            var.child_by_field_name("value"),
                            function_scope,
                            scope,
                        )
                    {
                        self.pattern(name, function_scope, false);
                    }
                    if let Some(value) = var.child_by_field_name("value") {
                        self.visit(value, scope);
                    }
                    self.assign_callable(var, scope);
                }
                return;
            }
            "variable_declarator" => {
                self.declare_callable(node, scope);
                if let (Some(name), Some(value)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("value"),
                ) && self.require_declaration(name, value, scope, scope)
                {
                    self.visit(value, scope);
                    return;
                }
                if let Some(name) = node.child_by_field_name("name") {
                    if let Some(value) = node.child_by_field_name("value")
                        && name.kind() == "identifier"
                        && matches!(
                            value.kind(),
                            "arrow_function" | "function_expression" | "generator_function"
                        )
                    {
                        self.function(value, scope, Some(self.e.text(name)));
                        return;
                    }
                    if !self.factory_binding(node, scope)
                        && !self.typed_binding(
                            name,
                            node.child_by_field_name("type"),
                            node.child_by_field_name("value"),
                            scope,
                            scope,
                        )
                    {
                        self.pattern(name, scope, false);
                    }
                }
            }
            "assignment_expression" | "augmented_assignment_expression" => {
                if self.commonjs_export(node, scope) {
                    return;
                }
                if let Some(n) = node.child_by_field_name("left") {
                    self.pattern(n, scope, true);
                }
            }
            "update_expression" => {
                if let Some(n) = node.child_by_field_name("argument") {
                    self.pattern(n, scope, true);
                }
            }
            "call_expression" | "new_expression" => {
                if let Some(arguments) = node.child_by_field_name("arguments") {
                    for argument in children(arguments)
                        .into_iter()
                        .filter(|n| n.kind() == "identifier")
                    {
                        self.callback_arguments.insert(format!(
                            "call:{}:{}-{}",
                            self.e.scopes[scope].owner,
                            argument.start_byte(),
                            argument.end_byte()
                        ));
                        // Reuse the final lexical/write resolver without treating
                        // the argument as an invocation in the returned facts.
                        self.e.call(
                            argument,
                            scope,
                            argument,
                            Some(vec![self.e.text(argument).into()]),
                        );
                    }
                }
                if let Some(target) = node
                    .child_by_field_name("function")
                    .or_else(|| node.child_by_field_name("constructor"))
                {
                    let parts = self.dotted(target);
                    if let Some((_, _, module)) = self
                        .require_target(node)
                        .filter(|_| target.kind() == "identifier")
                    {
                        let index = self.e.facts.references.len();
                        let keys = module
                            .as_deref()
                            .map(|m| self.modules(m).iter().map(|m| module_key(m)).collect())
                            .unwrap_or_default();
                        self.e.reference(
                            node,
                            scope,
                            module.unwrap_or_else(|| self.e.text(node).into()),
                            "imports",
                            keys,
                            "CommonJS require target is unavailable or dynamic",
                        );
                        self.require_references.push((scope, index));
                    }
                    if let Some((_, imported, module)) = self.require_target(target) {
                        let index = self.e.facts.references.len();
                        let symbol = imported.as_deref().unwrap_or("default");
                        let keys = module
                            .as_deref()
                            .map(|m| {
                                self.modules(m)
                                    .iter()
                                    .map(|m| commonjs_key(m, symbol))
                                    .collect()
                            })
                            .unwrap_or_default();
                        self.e.reference(
                            node,
                            scope,
                            self.e.text(target).into(),
                            "calls",
                            keys,
                            "CommonJS target is unavailable or dynamic",
                        );
                        self.require_references.push((scope, index));
                    } else {
                        self.call(node, scope, target, parts.clone());
                    }
                    if target.kind() == "import" {
                        let argument = node
                            .child_by_field_name("arguments")
                            .and_then(|n| n.named_child(0));
                        let module = argument
                            .filter(|n| n.kind() == "string")
                            .and_then(|n| self.string(n));
                        self.e.reference(
                            node,
                            scope,
                            module.clone().unwrap_or_else(|| self.e.text(node).into()),
                            "imports",
                            module
                                .map(|m| self.modules(&m).iter().map(|m| module_key(m)).collect())
                                .unwrap_or_default(),
                            "dynamic import path is unavailable or not a static string",
                        );
                    }
                    if parts
                        .as_ref()
                        .is_some_and(|p| p.len() == 1 && p[0] == "eval")
                    {
                        let mut ancestor = Some(scope);
                        while let Some(i) = ancestor {
                            self.e.scopes[i].uncertain = true;
                            ancestor = self.e.scopes[i].parent;
                        }
                    }
                }
            }
            "statement_block" | "for_statement" | "for_in_statement" | "catch_clause"
            | "switch_statement" => {
                let child = self.e.block(scope, node);
                if let Some(p) = node.child_by_field_name("parameter") {
                    self.pattern(p, child, false);
                }
                if node.kind() == "for_in_statement"
                    && let Some(p) = node.child_by_field_name("left")
                {
                    let kind = node.child_by_field_name("kind").map(|n| n.kind());
                    let mut binding_scope = child;
                    if kind == Some("var") {
                        while !self.e.scopes[binding_scope].function {
                            binding_scope = self.e.scopes[binding_scope].parent.unwrap_or(0);
                        }
                    }
                    self.pattern(p, binding_scope, kind.is_none());
                }
                for n in children(node) {
                    self.visit(n, child);
                }
                return;
            }
            "with_statement" => {
                let child = self.e.block(scope, node);
                self.e.scopes[child].uncertain = true;
                for n in children(node) {
                    self.visit(n, child);
                }
                return;
            }
            "type_annotation" | "type_arguments" => {
                self.type_refs(node, scope, "references_type");
                return;
            }
            "type_parameters" | "export_clause" => return,
            "jsx_opening_element" | "jsx_self_closing_element" => {
                if let Some(name) = node.child_by_field_name("name")
                    && let Some(parts) = self.dotted(name)
                    && (parts.len() > 1 || parts[0].chars().next().is_some_and(char::is_uppercase))
                {
                    self.component_references.insert(format!(
                        "call:{}:{}-{}",
                        self.e.scopes[scope].owner,
                        node.start_byte(),
                        node.end_byte()
                    ));
                    self.call(node, scope, name, Some(parts));
                }
            }
            _ => {}
        }
        for n in children(node) {
            self.visit(n, scope);
        }
        if matches!(node.kind(), "variable_declarator" | "assignment_expression") {
            self.assign_callable(node, scope);
        }
    }
}
