//! Additional grammar-backed languages. Binding keys use explicit namespaces or
//! complete file paths; unresolved receiver types never fall back to a bare name.
use super::common::{Binding, Extractor, children, diagnostic, module_path, relative_path, tree};
use crate::model::FileFacts;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use tree_sitter::{Language, Node as Syntax};

fn grammar(path: &str) -> Option<(&'static str, Language)> {
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

pub(super) fn supports(path: &str) -> bool {
    grammar(path).is_some()
        || matches!(
            path.rsplit('.').next(),
            Some("dmm" | "dmf" | "dmi" | "dfm" | "lfm" | "lpk")
        )
}

pub(super) fn parse(path: &str, source: &str, hash: &str) -> Result<Option<FileFacts>> {
    if path.ends_with(".dmi") {
        let mut f = asset_facts(path, hash);
        diagnostic(&mut f, None, "DMI requires binary PNG ingestion");
        return Ok(Some(f));
    }
    if matches!(path.rsplit('.').next(), Some("dmm" | "dmf")) {
        return Ok(Some(asset(path, source, hash)));
    }
    match path.rsplit('.').next() {
        Some("dfm" | "lfm") => {
            return Ok(Some(parse_pascal_form_bytes(
                path,
                source.as_bytes(),
                hash,
            )?));
        }
        Some("lpk") => return Ok(Some(pascal_package_xml(path, source, hash))),
        Some("dpk") => return Ok(Some(pascal_package_source(path, source, hash))),
        _ => {}
    }
    let Some((lang, language)) = grammar(path) else {
        return Ok(None);
    };
    parse_grammar(path, source, hash, lang, language)
}

/// Parse a language selected by the caller, preserving the original script path.
pub(super) fn parse_named(
    path: &str,
    source: &str,
    hash: &str,
    language: &str,
) -> Result<Option<FileFacts>> {
    if language != "julia" {
        return Ok(None);
    }
    parse_grammar(
        path,
        source,
        hash,
        "julia",
        tree_sitter_julia::LANGUAGE.into(),
    )
}

fn parse_grammar(
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

fn child<'t>(n: Syntax<'t>, kinds: &[&str]) -> Option<Syntax<'t>> {
    children(n).into_iter().find(|c| kinds.contains(&c.kind()))
}
fn find<'t>(n: Syntax<'t>, kinds: &[&str]) -> Option<Syntax<'t>> {
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
fn field<'t>(n: Syntax<'t>, fields: &[&str]) -> Option<Syntax<'t>> {
    fields.iter().find_map(|f| n.child_by_field_name(f))
}
fn names(n: Syntax<'_>) -> bool {
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

type ContextReference<'t> = (Syntax<'t>, usize, String, &'static str, Vec<String>, String);

struct Extended<'s, 't> {
    e: Extractor<'s>,
    prefixes: Vec<String>,
    pending: Vec<(Syntax<'t>, usize, String, &'static str, Vec<String>)>,
    contextual: Vec<ContextReference<'t>>,
    value_types: HashMap<(usize, String), Option<String>>,
    dart_packages: HashSet<String>,
    dart_local_types: HashSet<String>,
    dart_bloc_types: HashMap<String, String>,
    cpp_defines: HashMap<String, Option<String>>,
    groovy_parameters: HashMap<usize, Vec<String>>,
}
impl<'s, 't> Extended<'s, 't> {
    fn norm(&self, text: &str) -> String {
        if self.e.language == "commonlisp" && (text.contains(['|', '\\']) || text.starts_with('"'))
        {
            text.into()
        } else if matches!(self.e.language, "fortran" | "pascal" | "commonlisp") {
            text.to_lowercase()
        } else {
            text.into()
        }
    }
    fn key(&self, q: &str) -> String {
        format!("{}:symbol:{q}", self.e.language)
    }
    fn text(&self, n: Syntax<'_>) -> String {
        self.norm(self.e.text(n))
    }
    fn parts(&self, n: Syntax<'_>) -> Vec<String> {
        if names(n) {
            return vec![self.text(n)];
        }
        match n.kind() {
            "field_expression" | "member_expression" | "scoped_identifier" | "genericDot"
            | "exprDot" => {
                let Some(a) = field(n, &["object", "value", "scope", "lhs"]) else {
                    return vec![];
                };
                let Some(b) = field(n, &["field", "member", "property", "name", "rhs"]) else {
                    return vec![];
                };
                let mut p = self.parts(a);
                if p.is_empty() {
                    return p;
                }
                p.extend(self.parts(b));
                p
            }
            "value_path"
            | "module_path"
            | "package_identifier"
            | "moduleName"
            | "import_path"
            | "stable_type_identifier" => {
                let mut p = vec![];
                for c in children(n) {
                    if c.kind() != "kDot" {
                        let s = self.parts(c);
                        if s.is_empty() {
                            return vec![];
                        }
                        p.extend(s);
                    }
                }
                p
            }
            "type"
            | "generic_type"
            | "generic_function"
            | "instantiation_expression"
            | "type_name"
            | "typeref"
            | "class_identifier"
            | "function_identifier"
            | "task_identifier"
            | "module_name"
            | "class_type" => n
                .child_by_field_name("function")
                .or_else(|| n.named_child(0))
                .map_or_else(Vec::new, |c| self.parts(c)),
            _ => vec![],
        }
    }
    fn define(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        name: String,
        kind: &str,
        global: bool,
    ) -> usize {
        let prefix = if global {
            name.clone()
        } else {
            format!("{}.{}", self.prefixes[scope], name)
        };
        let key = self.key(&prefix);
        let index = self.e.define(
            n,
            scope,
            &name,
            kind,
            Some(key.clone()),
            !matches!(kind, "module" | "namespace"),
        );
        self.prefixes.push(prefix);
        if matches!(kind, "module" | "namespace") {
            self.e.bind(
                scope,
                &name,
                Binding::Namespace {
                    prefixes: vec![format!("{key}.")],
                    separator: ".",
                },
            );
        }
        index
    }
    fn unresolved(&mut self, n: Syntax<'t>, scope: usize, label: String, relation: &str) {
        self.e.reference(
            n,
            scope,
            label,
            relation,
            vec![],
            "receiver, namespace, or binding context is unknown",
        );
    }
    fn target(&mut self, n: Syntax<'t>, scope: usize, target: Syntax<'t>, relation: &'static str) {
        self.pending
            .push((n, scope, self.text(target), relation, self.parts(target)));
    }
    fn call(&mut self, n: Syntax<'t>, scope: usize, target: Syntax<'t>, dynamic: bool) {
        let mut parts = if dynamic { vec![] } else { self.parts(target) };
        if parts.len() == 1
            && matches!(
                self.e.language,
                "scala" | "dart" | "groovy" | "apex" | "objc" | "dm"
            )
        {
            let mut parent = Some(scope);
            while let Some(s) = parent {
                if self.e.scopes[s].class {
                    parts.clear();
                    break;
                }
                parent = self.e.scopes[s].parent;
            }
        }
        if self.e.language == "ocaml" {
            let mut keys = self.e.resolve(scope, &parts);
            if keys.is_empty() {
                keys = self.ocaml_external(scope, &parts, false);
            }
            self.e.reference(
                n,
                scope,
                self.text(target),
                "calls",
                keys,
                "target is unavailable, dynamic, or not yet bound",
            );
        } else {
            self.e.call(n, scope, target, Some(parts));
        }
    }
    fn walk(&mut self, n: Syntax<'t>, scope: usize) {
        for c in children(n) {
            self.visit(c, scope);
        }
    }
    fn mask(&mut self, n: Syntax<'t>, scope: usize) {
        if self.e.language == "pascal"
            && matches!(n.kind(), "declArg" | "declVar" | "declField" | "declProp")
        {
            let mut cursor = n.walk();
            for name in n
                .children_by_field_name("name", &mut cursor)
                .filter(|n| names(*n))
            {
                self.mask(name, scope);
            }
            return;
        }
        if names(n) {
            let name = self.text(n);
            self.e.bind(scope, &name, Binding::Unknown);
            if matches!(self.e.language, "dart" | "apex") {
                self.value_types.insert((scope, name), None);
            }
        } else if let Some(name) = field(n, &["name", "pattern", "declarator"]) {
            self.mask(name, scope);
        } else {
            for c in children(n) {
                self.mask(c, scope);
            }
        }
    }
    fn function(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        name: Syntax<'t>,
        header: Syntax<'t>,
        body: Option<Syntax<'t>>,
    ) {
        let s = self.define(
            n,
            scope,
            self.text(name),
            if self.e.scopes[scope].class {
                "method"
            } else {
                "function"
            },
            false,
        );
        if self.e.language == "pascal" {
            let parts = self.parts(name);
            let owner = if parts.len() > 1 {
                Some(parts[..parts.len() - 1].join("."))
            } else {
                self.owner_metadata(scope, "pascal_class")
            };
            if let (Some(owner), Some(method)) = (owner, parts.last()) {
                self.annotate(s, serde_json::json!({"pascal_owner":owner, "pascal_method":method, "pascal_unit":self.owner_metadata(scope, "pascal_unit"), "body":n.kind() == "defProc"}));
            }
        }
        if let Some(params) = field(header, &["parameters", "args", "lambda_list"]).or_else(|| {
            child(
                header,
                &[
                    "parameters",
                    "proc_parameters",
                    "formal_parameters",
                    "argument_list",
                    "tf_port_list",
                ],
            )
        }) {
            self.mask(params, s);
        }
        for p in children(header)
            .into_iter()
            .filter(|c| c.kind() == "parameter")
        {
            self.mask(p, s);
        }
        if let Some(parameters) = self.groovy_parameters.get(&name.start_byte()) {
            for parameter in parameters {
                self.e.bind(s, parameter, Binding::Unknown);
            }
        }
        if self.e.language == "ocaml" && n.kind() == "let_binding" {
            let recursive = n.parent().is_some_and(|p| {
                let mut cursor = p.walk();
                p.children(&mut cursor).any(|c| c.kind() == "rec")
            });
            if !recursive {
                let name = self.text(name);
                self.e.bind(s, &name, Binding::Unknown);
            }
        }
        self.declaration_annotations(n, s);
        if matches!(self.e.language, "dart" | "apex") {
            self.parameter_types(header, s);
        }
        for f in ["return_type", "type"] {
            if let Some(ty) = header.child_by_field_name(f) {
                self.heritage(ty, s, "uses_type");
            }
        }
        if self.e.language == "pascal" && n.kind() == "defProc" {
            let mut cursor = n.walk();
            for local in n.children_by_field_name("local", &mut cursor) {
                self.visit(local, s);
            }
        }
        if let Some(body) = body {
            self.visit(body, s);
        } else {
            for c in children(n) {
                if c.id() != header.id() && c.id() != name.id() && !c.kind().ends_with("statement")
                {
                    self.visit(c, s);
                }
            }
        }
    }
    fn heritage(&mut self, n: Syntax<'t>, scope: usize, relation: &'static str) {
        if self.e.language == "scala" && n.kind() == "extends_clause" {
            let mut relation = "inherits";
            let mut cursor = n.walk();
            for c in n.children(&mut cursor) {
                if c.kind() == "with" {
                    relation = "mixes_in";
                } else if c.is_named() && !matches!(c.kind(), "arguments" | "type_arguments") {
                    self.heritage(c, scope, relation);
                }
            }
            return;
        }
        let relation = if self.e.language == "dart" && n.kind() == "mixins" {
            "mixes_in"
        } else {
            relation
        };
        if !self.parts(n).is_empty() {
            self.target(n, scope, n, relation);
        } else {
            for c in children(n) {
                self.heritage(c, scope, relation);
            }
        }
    }
    fn import(&mut self, n: Syntax<'t>, scope: usize, label: String, key: Option<String>) {
        self.e.reference(
            n,
            scope,
            label,
            "imports",
            key.into_iter().collect(),
            "import target is unavailable or ambiguous",
        );
    }
    fn file_import(&mut self, n: Syntax<'t>, scope: usize, value: &str, alias: Option<String>) {
        let value = value.trim_matches(['\'', '"']);
        let key = if !value.contains(':') && !value.starts_with('/') {
            relative_path(
                self.e.facts.path.rsplit_once('/').map_or("", |p| p.0),
                value,
            )
        } else {
            None
        };
        self.import(
            n,
            scope,
            value.into(),
            key.as_ref()
                .map(|p| format!("{}:file:{p}", self.e.language)),
        );
        if let (Some(alias), Some(path)) = (alias, key) {
            self.e.bind(
                scope,
                &alias,
                Binding::Namespace {
                    prefixes: vec![self.key(&format!("@{path}."))],
                    separator: ".",
                },
            );
        }
    }

    fn visit(&mut self, n: Syntax<'t>, scope: usize) {
        let k = n.kind();
        let lang = self.e.language;
        if matches!(
            k,
            "comment"
                | "line_comment"
                | "block_comment"
                | "string"
                | "string_literal"
                | "str_lit"
                | "syn_quoting_lit"
                | "quote_expression"
                | "quoting_lit"
                | "quasiquoting_lit"
                | "dis_expr"
        ) {
            return;
        }
        if lang == "objc" && k == "declaration" {
            let mut cursor = n.walk();
            for declarator in n.children_by_field_name("declarator", &mut cursor) {
                self.mask(declarator, scope);
            }
        }
        if lang == "pascal" && matches!(k, "declVar" | "declField" | "declProp") {
            self.mask(n, scope);
        }
        if lang == "pascal" && k == "statement" {
            let expressions: Vec<_> = children(n)
                .into_iter()
                .filter(|c| !c.kind().starts_with('k') && c.kind() != "comment")
                .collect();
            if let [target] = expressions.as_slice()
                && matches!(target.kind(), "identifier" | "inherited")
            {
                self.pascal_call(n, scope, *target);
                return;
            }
        }
        if lang == "commonlisp" {
            self.lisp(n, scope);
            return;
        }
        if lang == "fortran" && self.fortran_directive(n, scope) {
            return;
        }
        if matches!(lang, "apex" | "dart")
            && matches!(
                k,
                "block" | "for_statement" | "enhanced_for_statement" | "catch_clause"
            )
        {
            let s = self.e.block(scope, n);
            self.prefixes
                .push(format!("{}.<{}>", self.prefixes[scope], n.start_byte()));
            if matches!(k, "enhanced_for_statement" | "catch_clause") {
                self.parameter_types(n, s);
            }
            self.walk(n, s);
            return;
        }
        if matches!(lang, "dart" | "apex") && self.typed_declaration(n, scope) {
            return;
        }
        if matches!(
            k,
            "function_expression" | "arrow_function_expression" | "fun_expression" | "lambda"
        ) {
            let s = self.e.block(scope, n);
            self.prefixes
                .push(format!("{}.<{}>", self.prefixes[scope], n.start_byte()));
            if let Some(p) = field(n, &["parameters", "args"]) {
                self.mask(p, s);
            }
            if lang == "dart" {
                self.parameter_types(n, s);
                if self.dart_bloc_scope(scope)
                    && let Some(call) = n
                        .parent()
                        .filter(|p| p.kind() == "arguments")
                        .and_then(|p| p.parent())
                    && let Some(function) = call.child_by_field_name("function")
                    && self.parts(function) == ["on"]
                    && self.e.unbound(scope, "on")
                    && let Some(params) = n.child_by_field_name("parameters")
                    && let Some(parameter) = params.named_child(1)
                    && let Some(name) = parameter
                        .child_by_field_name("name")
                        .or_else(|| child(parameter, &["identifier"]))
                {
                    self.value_types
                        .insert((s, self.text(name)), Some("Emitter".into()));
                }
            }
            for p in children(n).into_iter().filter(|p| p.kind() == "parameter") {
                self.mask(p, s);
            }
            if let Some(body) = n.child_by_field_name("body") {
                self.visit(body, s);
            } else {
                self.e.scopes[s].uncertain = true;
                self.walk(n, s);
            }
            return;
        }
        if lang == "julia" && matches!(k, "function_definition" | "macro_definition" | "assignment")
        {
            let header = if k == "assignment" {
                n.named_child(0)
            } else {
                child(n, &["signature"])
            };
            if let Some(header) = header
                && let Some(call) = if k == "assignment" {
                    (header.kind() == "call_expression").then_some(header)
                } else {
                    find(header, &["call_expression"])
                }
                && let Some(name) = call.named_child(0)
            {
                let s = self.define(
                    n,
                    scope,
                    self.text(name),
                    if k == "macro_definition" {
                        "macro"
                    } else {
                        "function"
                    },
                    false,
                );
                if let Some(args) = child(call, &["argument_list"]) {
                    self.mask(args, s);
                }
                for c in children(n) {
                    if c.id() != header.id() {
                        self.visit(c, s);
                    }
                }
                return;
            }
        }
        if lang == "julia"
            && matches!(
                k,
                "struct_definition" | "abstract_definition" | "primitive_definition"
            )
            && let Some(head) = child(n, &["type_head"])
            && let Some(name) = find(head, &["identifier"])
        {
            let s = self.define(
                n,
                scope,
                self.text(name),
                if k == "abstract_definition" {
                    "type"
                } else {
                    "struct"
                },
                false,
            );
            if let Some(binary) = child(head, &["binary_expression"])
                && self.e.text(binary).contains("<:")
                && let Some(base) = binary.named_child(2)
            {
                self.target(binary, s, base, "inherits");
            }
            for c in children(n) {
                if c.id() != head.id() {
                    self.visit(c, s);
                }
            }
            return;
        }
        if lang == "scala"
            && k == "package_clause"
            && let (Some(name), Some(body)) =
                (n.child_by_field_name("name"), n.child_by_field_name("body"))
        {
            let s = self.define(
                n,
                scope,
                self.text(name),
                "module",
                scope == 0 && self.prefixes[scope].starts_with('@'),
            );
            self.visit(body, s);
            return;
        }
        if lang == "fortran"
            && matches!(
                k,
                "module" | "program" | "subroutine" | "function" | "derived_type_definition"
            )
        {
            let header = child(
                n,
                &[
                    "module_statement",
                    "program_statement",
                    "subroutine_statement",
                    "function_statement",
                    "derived_type_statement",
                ],
            );
            if let Some(header) = header
                && let Some(name) =
                    field(header, &["name"]).or_else(|| child(header, &["name", "type_name"]))
            {
                let kind = match k {
                    "module" | "program" => "module",
                    "derived_type_definition" => "struct",
                    _ => "function",
                };
                let s = self.define(n, scope, self.text(name), kind, k == "module");
                if let Some(base) = header.child_by_field_name("base") {
                    self.heritage(base, s, "inherits");
                }
                if let Some(p) = header.child_by_field_name("parameters") {
                    self.mask(p, s);
                }
                for c in children(n) {
                    if c.id() != header.id() {
                        self.visit(c, s);
                    }
                }
                return;
            }
        }
        if lang == "pascal"
            && k == "defProc"
            && let Some(h) = n.child_by_field_name("header")
            && let Some(name) = h.child_by_field_name("name")
        {
            self.function(n, scope, name, h, n.child_by_field_name("body"));
            return;
        }
        if lang == "pascal"
            && k == "declType"
            && let Some(name) = n.child_by_field_name("name")
        {
            let body = n.child_by_field_name("type");
            let kind = match body.map(|b| b.kind()) {
                Some("declClass") => "class",
                Some("declIntf") => "interface",
                Some("declEnum") => "enum",
                _ => "type",
            };
            let s = self.define(n, scope, self.text(name), kind, false);
            if kind == "class" {
                let bases: Vec<String> = body
                    .map(children)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| c.kind() == "typeref")
                    .map(|c| self.text(c))
                    .collect();
                self.annotate(s, serde_json::json!({"pascal_class":self.text(name), "pascal_unit":self.owner_metadata(scope, "pascal_unit"), "pascal_bases":bases}));
            }
            if let Some(b) = body {
                self.walk(b, s);
                if kind == "class" {
                    let mut masked: Vec<_> = self.e.scopes[s]
                        .bindings
                        .iter()
                        .filter(|(_, binding)| matches!(binding, Binding::Unknown))
                        .map(|(name, _)| name.clone())
                        .collect();
                    masked.sort();
                    self.annotate(s, serde_json::json!({"pascal_shadowed_members":masked}));
                }
                if let Some(base) = b.child_by_field_name("parent") {
                    self.target(base, s, base, "inherits");
                }
            }
            return;
        }
        if lang == "zig"
            && k == "variable_declaration"
            && let Some(name) = child(n, &["identifier"])
        {
            if let Some(body) = child(
                n,
                &[
                    "struct_declaration",
                    "enum_declaration",
                    "union_declaration",
                    "opaque_declaration",
                ],
            ) {
                let s = self.define(
                    n,
                    scope,
                    self.text(name),
                    if body.kind() == "enum_declaration" {
                        "enum"
                    } else {
                        "struct"
                    },
                    false,
                );
                self.walk(body, s);
                return;
            }
            if let Some(builtin) = child(n, &["builtin_function"])
                && child(builtin, &["builtin_identifier"])
                    .is_some_and(|b| self.e.text(b) == "@import")
                && let Some(value) = find(builtin, &["string"])
            {
                self.file_import(builtin, scope, self.e.text(value), Some(self.text(name)));
                return;
            }
            let value = self.text(name);
            self.e.bind(scope, &value, Binding::Unknown);
        }
        if lang == "objc" && matches!(k, "method_definition" | "method_declaration") {
            let selector = self.selector(n, false);
            if !selector.is_empty() {
                let class_method = self.e.text(n).trim_start().starts_with('+');
                let s = self.define(
                    n,
                    scope,
                    format!("{}{selector}", if class_method { "+" } else { "-" }),
                    "method",
                    false,
                );
                if self.owner_metadata(scope, "objc_class").is_some() {
                    let owner = self.e.scopes[scope].owner.clone();
                    self.annotate(s, serde_json::json!({"objc_owner":owner, "objc_method":self.e.scopes[s].qualified.rsplit('.').next(), "objc_method_kind":if class_method { "+" } else { "-" }, "body":k == "method_definition"}));
                }
                for p in children(n)
                    .into_iter()
                    .filter(|c| c.kind() == "method_parameter")
                {
                    if let Some(name) = child(p, &["identifier"]) {
                        self.mask(name, s);
                    }
                }
                if let Some(body) = child(n, &["compound_statement"]) {
                    self.visit(body, s);
                }
            }
            return;
        }
        if lang == "dm"
            && k == "type_definition"
            && let Some(name) = child(n, &["type_path"])
        {
            let raw = self.e.text(name);
            let name = if raw.starts_with('/') {
                raw.into()
            } else if self.prefixes[scope].starts_with('/') {
                format!("{}/{raw}", self.prefixes[scope])
            } else {
                format!("/{raw}")
            };
            let s = self.define(n, scope, name.clone(), "class", true);
            if let Some((base, _)) = name.rsplit_once('/')
                && !base.is_empty()
            {
                self.e.reference(
                    n,
                    s,
                    base.into(),
                    "inherits",
                    vec![self.key(base)],
                    "parent type is unavailable or ambiguous",
                );
            }
            for c in children(n) {
                if c.kind() == "type_body" {
                    self.visit(c, s);
                }
            }
            return;
        }
        if lang == "dm"
            && matches!(
                k,
                "proc_definition" | "proc_override" | "type_proc_definition" | "type_proc_override"
            )
            && let Some(name) = n.child_by_field_name("name")
        {
            let owner = child(n, &["type_path"]).map(|p| self.text(p));
            let prefix = owner.unwrap_or_else(|| {
                if self.prefixes[scope].starts_with('/') {
                    self.prefixes[scope].clone()
                } else {
                    "/proc".into()
                }
            });
            let full = format!("{prefix}/{}", self.text(name));
            let s = self.define(n, scope, full.clone(), "function", true);
            if prefix == "/proc" {
                let short = self.text(name);
                self.e.bind(scope, &short, Binding::symbol(self.key(&full)));
            }
            if let Some(params) = child(n, &["proc_parameters"]) {
                self.mask(params, s);
            }
            if let Some(body) = child(n, &["block"]) {
                self.visit(body, s);
            }
            return;
        }

        let definition = match (lang, k) {
            (
                "scala",
                "class_definition" | "object_definition" | "trait_definition" | "enum_definition",
            ) => Some((
                n.child_by_field_name("name"),
                match k {
                    "trait_definition" => "trait",
                    "enum_definition" => "enum",
                    "object_definition" => "module",
                    _ => "class",
                },
                false,
            )),
            (
                "dart" | "groovy" | "apex",
                "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "mixin_declaration"
                | "extension_declaration"
                | "extension_type_declaration",
            ) => Some((
                n.child_by_field_name("name"),
                match k {
                    "interface_declaration" => "interface",
                    "enum_declaration" => "enum",
                    "mixin_declaration" => "trait",
                    _ => "class",
                },
                lang == "apex" && scope == 0,
            )),
            ("apex", "trigger_declaration") => {
                Some((n.child_by_field_name("name"), "trigger", true))
            }
            ("julia", "module_definition") => {
                Some((n.child_by_field_name("name"), "module", scope == 0))
            }
            ("ocaml", "module_binding") => Some((child(n, &["module_name"]), "module", false)),
            ("ocaml", "type_binding") => Some((n.child_by_field_name("name"), "type", false)),
            ("ocaml", "constructor_declaration") => {
                Some((child(n, &["constructor_name"]), "variant", false))
            }
            ("ocaml", "value_specification") => {
                Some((child(n, &["value_name"]), "function", false))
            }
            ("pascal", "unit" | "program" | "library") => {
                Some((child(n, &["moduleName"]), "module", true))
            }
            ("objc", "class_interface" | "class_implementation" | "protocol_declaration") => {
                Some((
                    child(n, &["identifier"]),
                    if k == "protocol_declaration" {
                        "interface"
                    } else {
                        "class"
                    },
                    true,
                ))
            }
            ("verilog", "module_declaration") => Some((
                child(n, &["module_header"])
                    .and_then(|h| child(h, &["simple_identifier", "escaped_identifier"])),
                "module",
                true,
            )),
            (
                "verilog",
                "package_declaration" | "class_declaration" | "interface_class_declaration",
            ) => Some((
                child(n, &["package_identifier", "class_identifier"]),
                if k == "package_declaration" {
                    "module"
                } else {
                    "class"
                },
                k == "package_declaration",
            )),
            _ => None,
        };
        if let Some((Some(name), kind, global)) = definition {
            let s = self.define(n, scope, self.text(name), kind, global);
            if lang == "objc" && kind == "class" {
                let mut cursor = n.walk();
                let category = n.children(&mut cursor).any(|c| c.kind() == "(");
                self.annotate(s, serde_json::json!({"objc_class":self.text(name), "objc_category":category, "objc_role":k}));
            }
            if lang == "pascal" && kind == "module" {
                self.annotate(s, serde_json::json!({"pascal_unit":self.text(name)}));
            }
            self.declaration_annotations(n, s);
            if lang == "ocaml" {
                for parameter in children(n)
                    .into_iter()
                    .filter(|c| c.kind() == "module_parameter")
                {
                    if let Some(name) = child(parameter, &["module_name"]) {
                        self.mask(name, s);
                    }
                }
                if k == "module_binding"
                    && let Some(body) = n.child_by_field_name("body")
                {
                    let parts = self.parts(body);
                    if !parts.is_empty() {
                        let mut prefixes = self.e.resolve(scope, &parts);
                        if prefixes.is_empty() {
                            prefixes = self.ocaml_external(scope, &parts, true);
                        }
                        for prefix in &mut prefixes {
                            if let Some(path) = prefix.strip_prefix("ocaml:file:") {
                                *prefix = self.key(&format!("@{path}"));
                            }
                        }
                        if !prefixes.is_empty() {
                            let alias = self.text(name);
                            self.e.scopes[scope].bindings.insert(
                                alias,
                                Binding::Namespace {
                                    prefixes: prefixes.iter().map(|p| format!("{p}.")).collect(),
                                    separator: ".",
                                },
                            );
                        }
                    }
                }
            }
            for f in ["extend", "superclass"] {
                if let Some(h) = n.child_by_field_name(f) {
                    self.heritage(h, s, "inherits");
                }
            }
            if let Some(h) = n.child_by_field_name("interfaces") {
                self.heritage(h, s, "implements");
            }
            if lang == "verilog" {
                for h in children(n).into_iter().filter(|c| c.kind() == "class_type") {
                    self.heritage(h, s, "inherits");
                }
            }
            if lang == "objc" {
                for h in children(n).into_iter().filter(|c| {
                    matches!(
                        c.kind(),
                        "parameterized_arguments" | "protocol_reference_list"
                    )
                }) {
                    self.heritage(h, s, "implements");
                }
            }
            if lang == "apex"
                && k == "trigger_declaration"
                && let Some(object) = n.child_by_field_name("object")
            {
                self.unresolved(object, s, self.text(object), "uses");
            }
            for c in children(n) {
                if c.id() != name.id() {
                    self.visit(c, s);
                }
            }
            return;
        }
        if lang == "ocaml"
            && k == "let_binding"
            && child(n, &["parameter"]).is_none()
            && let (Some(name), Some(body)) = (
                n.child_by_field_name("pattern"),
                n.child_by_field_name("body"),
            )
            && name.kind() == "value_name"
        {
            let kind = if matches!(body.kind(), "fun_expression" | "function_expression") {
                "function"
            } else {
                "variable"
            };
            let s = self.define(n, scope, self.text(name), kind, false);
            if kind == "variable" {
                let name = self.text(name);
                self.e.invalidate(scope, &name);
            }
            self.visit(body, s);
            return;
        }
        let function = match (lang, k) {
            ("scala" | "zig" | "groovy", "function_definition" | "function_declaration")
            | ("apex" | "groovy", "method_declaration" | "constructor_declaration") => n
                .child_by_field_name("name")
                .map(|name| (name, n, n.child_by_field_name("body"))),
            (
                "dart",
                "function_declaration"
                | "method_declaration"
                | "local_function_declaration"
                | "getter_declaration"
                | "setter_declaration",
            ) => find(
                n.child_by_field_name("signature").unwrap_or(n),
                &[
                    "function_signature",
                    "getter_signature",
                    "setter_signature",
                    "constructor_signature",
                ],
            )
            .and_then(|h| {
                h.child_by_field_name("name").map(|name| {
                    (
                        name,
                        h,
                        n.child_by_field_name("body")
                            .or_else(|| child(n, &["function_body"])),
                    )
                })
            }),
            ("ocaml", "let_binding") => n
                .child_by_field_name("pattern")
                .filter(|p| p.kind() == "value_name")
                .map(|name| (name, n, n.child_by_field_name("body"))),
            ("pascal", "declProc") => n.child_by_field_name("name").map(|name| (name, n, None)),
            ("verilog", "function_declaration" | "task_declaration") => {
                child(n, &["function_body_declaration", "task_body_declaration"]).and_then(|h| {
                    child(h, &["function_identifier", "task_identifier"])
                        .map(|name| (name, h, Some(h)))
                })
            }
            _ => None,
        };
        if let Some((name, h, body)) = function {
            self.function(n, scope, name, h, body);
            return;
        }
        if self.imports(n, scope) {
            return;
        }
        if matches!(k, "preproc_if" | "preproc_ifdef" | "preproc_ifndef") {
            diagnostic(
                &mut self.e.facts,
                Some(super::common::line(n)),
                "Conditional compilation requires a build environment; branch omitted",
            );
            return;
        }
        if matches!(k, "annotation" | "marker_annotation")
            && let Some(name) = field(n, &["name"]).or_else(|| child(n, &["identifier"]))
        {
            self.target(n, scope, name, "annotated_by");
        }
        if lang == "scala"
            && matches!(k, "class_parameter" | "val_definition" | "var_definition")
            && self.e.scopes[scope].class
            && let Some(name) = field(n, &["name", "pattern"]).filter(|n| names(*n))
        {
            let s = self.define(n, scope, self.text(name), "field", false);
            if let Some(ty) = n.child_by_field_name("type") {
                self.heritage(ty, s, "uses_type");
            }
            if let Some(value) = n.child_by_field_name("value") {
                self.visit(value, s);
            }
            return;
        }
        if lang == "apex" && k == "from_clause" {
            for storage in children(n)
                .into_iter()
                .filter(|n| n.kind() == "storage_identifier")
            {
                self.unresolved(storage, scope, self.text(storage), "uses");
            }
        }
        if lang == "apex" && k == "dml_expression" {
            self.apex_dml(n, scope);
        }

        if lang == "julia"
            && k == "typed_expression"
            && self.e.scopes[scope].class
            && let (Some(name), Some(ty)) = (n.named_child(0), n.named_child(1))
            && names(name)
        {
            let s = self.define(n, scope, self.text(name), "field", false);
            self.target(ty, s, ty, "uses_type");
            return;
        }
        if lang == "zig"
            && k == "container_field"
            && let Some(name) = n.child_by_field_name("name")
        {
            let s = self.define(n, scope, self.text(name), "field", false);
            if let Some(ty) = n.child_by_field_name("type") {
                self.target(ty, s, ty, "uses_type");
            }
        }
        if lang == "objc" && k == "property_declaration" {
            if let Some(declaration) = child(n, &["struct_declaration"])
                && let Some(declarator) = child(declaration, &["struct_declarator"])
                && let Some(name) = find(declarator, &["identifier"])
            {
                let s = self.define(n, scope, self.text(name), "property", false);
                if let Some(ty) = child(declaration, &["type_identifier"]) {
                    self.target(ty, s, ty, "uses_type");
                }
            }
            return;
        }
        if matches!(k, "extends_interfaces" | "inheritance_definition") {
            self.heritage(n, scope, "inherits");
        }
        if matches!(
            k,
            "parameter"
                | "formal_parameter"
                | "field_declaration"
                | "variable_declaration"
                | "declField"
                | "declArg"
        ) && let Some(ty) = n.child_by_field_name("type")
        {
            self.heritage(ty, scope, "uses_type");
        }
        match (lang, k) {
            (_, "call_expression") => {
                if lang == "dart" {
                    self.dart_call(n, scope);
                }
                let target = field(n, &["function", "name"]).or_else(|| {
                    if matches!(lang, "julia" | "fortran") {
                        n.named_child(0)
                    } else {
                        None
                    }
                });
                if let Some(target) = target {
                    self.call(n, scope, target, false);
                }
            }
            ("fortran", "subroutine_call") => {
                if let Some(t) = n.child_by_field_name("subroutine") {
                    self.call(n, scope, t, false);
                }
            }
            ("ocaml", "application_expression") => {
                if !self.e.facts.path.ends_with(".mli")
                    && let Some(t) = n.child_by_field_name("function")
                    && t.kind() != "application_expression"
                {
                    self.call(n, scope, t, false);
                }
            }
            ("pascal", "exprCall") => {
                if let Some(t) = n.child_by_field_name("entity") {
                    self.pascal_call(n, scope, t);
                }
            }
            ("apex" | "groovy", "method_invocation") => {
                if let Some(t) = n.child_by_field_name("name") {
                    self.call(n, scope, t, n.child_by_field_name("object").is_some());
                }
            }
            ("groovy", "juxt_function_call") => {
                if let Some(t) = n.child_by_field_name("name") {
                    self.call(n, scope, t, false);
                }
            }
            ("objc", "message_expression") => self.objc_message(n, scope),
            ("dm", "field_proc_expression") => {
                if let Some(t) = n.child_by_field_name("proc") {
                    self.call(n, scope, t, true);
                }
            }
            ("dm", "new_expression") => {
                if let Some(t) = child(n, &["type_path"]) {
                    let name = self.text(t);
                    self.e.reference(
                        n,
                        scope,
                        name.clone(),
                        "instantiates",
                        vec![self.key(&name)],
                        "type is unavailable or ambiguous",
                    );
                }
            }
            ("verilog", "tf_call") => {
                if let Some(t) = child(n, &["simple_identifier", "escaped_identifier"]) {
                    self.call(n, scope, t, false);
                }
            }
            ("verilog", "method_call") => {
                self.unresolved(n, scope, self.text(n), "calls");
                return;
            }
            ("verilog", "module_instantiation" | "checker_instantiation") => {
                if let Some(t) = child(
                    n,
                    &[
                        "simple_identifier",
                        "escaped_identifier",
                        "checker_identifier",
                    ],
                ) {
                    let name = self.text(t);
                    self.e.reference(
                        n,
                        scope,
                        name.clone(),
                        "instantiates",
                        vec![self.key(&name)],
                        "module is unavailable or ambiguous",
                    );
                }
            }
            _ => {}
        }
        if lang == "fortran" && k == "variable_declaration" {
            let mut cursor = n.walk();
            for d in n.children_by_field_name("declarator", &mut cursor) {
                if let Some(name) = find(d, &["identifier"]) {
                    self.mask(name, scope);
                }
            }
        }
        // Writes and parameters mask lexical functions; no name-only fallback.
        if matches!(
            k,
            "parameter"
                | "formal_parameter"
                | "proc_parameter"
                | "variable_declarator"
                | "initialized_variable_definition"
                | "binding"
                | "declVar"
                | "declArg"
        ) && let Some(name) = field(n, &["name", "pattern"])
        {
            self.mask(name, scope);
        }
        if matches!(k, "val_definition" | "var_definition")
            && lang == "scala"
            && let Some(p) = n.child_by_field_name("pattern")
        {
            self.mask(p, scope);
        }
        if matches!(
            k,
            "assignment_expression" | "assignment_statement" | "assignment"
        ) && let Some(lhs) = field(n, &["left", "lhs"]).or_else(|| n.named_child(0))
            && names(lhs)
        {
            let name = self.text(lhs);
            self.e.invalidate(scope, &name);
        }
        self.walk(n, scope);
    }

    fn imports(&mut self, n: Syntax<'t>, scope: usize) -> bool {
        let k = n.kind();
        match (self.e.language, k) {
            ("scala" | "groovy", "import_declaration") => {
                let parts: Vec<_> = children(n)
                    .into_iter()
                    .filter(|c| matches!(c.kind(), "identifier" | "scoped_identifier"))
                    .flat_map(|c| self.parts(c))
                    .collect();
                if !parts.is_empty() {
                    let full = parts.join(".");
                    let key = self.key(&full);
                    self.import(n, scope, full, Some(key.clone()));
                    if !children(n).iter().any(|c| {
                        matches!(
                            c.kind(),
                            "namespace_selectors"
                                | "namespace_wildcard"
                                | "asterisk"
                                | "as_renamed_identifier"
                        )
                    }) {
                        self.e.bind(
                            scope,
                            parts.last().unwrap(),
                            Binding::Namespace {
                                prefixes: vec![format!("{key}.")],
                                separator: ".",
                            },
                        );
                    }
                }
                true
            }
            ("dart", "import_specification" | "part_directive" | "part_of_directive") => {
                if let Some(uri) = find(n, &["string_literal"]) {
                    self.file_import(
                        n,
                        scope,
                        self.e.text(uri),
                        n.child_by_field_name("alias").map(|n| self.text(n)),
                    );
                } else {
                    self.unresolved(n, scope, self.text(n), "imports");
                }
                true
            }
            ("objc", "preproc_include") => {
                if let Some(p) = n.child_by_field_name("path") {
                    if p.kind() == "string_literal" {
                        self.file_import(n, scope, self.e.text(p), None);
                    } else {
                        self.import(n, scope, self.text(p), None);
                    }
                }
                true
            }
            ("objc", "module_import") => {
                if let Some(p) = n.child_by_field_name("path") {
                    self.import(n, scope, self.text(p), None);
                }
                true
            }
            ("dm", "preproc_include") => {
                if let Some(p) = n.child_by_field_name("file") {
                    self.file_import(n, scope, self.e.text(p), None);
                }
                true
            }
            ("fortran", "use_statement") => {
                if let Some(module) = child(n, &["module_name"]) {
                    let module = self.text(module);
                    self.import(n, scope, module.clone(), Some(self.key(&module)));
                    if let Some(items) = child(n, &["included_items"]) {
                        for item in children(items) {
                            if item.kind() == "identifier" {
                                let name = self.text(item);
                                self.e.bind(
                                    scope,
                                    &name,
                                    Binding::symbol(self.key(&format!("{module}.{name}"))),
                                );
                            } else if item.kind() == "use_alias" {
                                let ids: Vec<_> =
                                    children(item).into_iter().filter(|c| names(*c)).collect();
                                if ids.len() == 2 {
                                    let alias = self.text(ids[0]);
                                    let name = self.text(ids[1]);
                                    self.e.bind(
                                        scope,
                                        &alias,
                                        Binding::symbol(self.key(&format!("{module}.{name}"))),
                                    );
                                }
                            }
                        }
                    }
                }
                true
            }
            ("julia", "using_statement" | "import_statement") => {
                for item in children(n) {
                    if item.kind() == "selected_import" {
                        let items = children(item);
                        if let Some(module) = items.first() {
                            let module = self.text(*module);
                            self.import(n, scope, module.clone(), Some(self.key(&module)));
                            if !module.starts_with('.') {
                                for item in items.iter().skip(1) {
                                    if names(*item) {
                                        let name = self.text(*item);
                                        self.e.bind(
                                            scope,
                                            &name,
                                            Binding::symbol(self.key(&format!("{module}.{name}"))),
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        let parts = self.parts(item);
                        let name = parts.join(".");
                        if !name.is_empty() {
                            self.import(n, scope, self.text(item), Some(self.key(&name)));
                            if !self.e.text(item).starts_with('.') {
                                self.e.bind(
                                    scope,
                                    parts.last().unwrap(),
                                    Binding::Namespace {
                                        prefixes: vec![format!("{}.", self.key(&name))],
                                        separator: ".",
                                    },
                                );
                            }
                        }
                    }
                }
                true
            }
            ("ocaml", "open_module" | "open_module_signature" | "include_module") => {
                if let Some(m) = n.child_by_field_name("module") {
                    let parts = self.parts(m);
                    let mut keys = self.e.resolve(scope, &parts);
                    if keys.is_empty() {
                        keys = self.ocaml_external(scope, &parts, true);
                    }
                    self.e.reference(n, scope, self.text(m), "imports", keys, "explicit sibling compilation-unit candidate; load-path overrides are not inferred");
                }
                true
            }
            ("pascal", "declUses") => {
                for m in children(n).into_iter().filter(|c| c.kind() == "moduleName") {
                    let name = self.text(m);
                    self.import(m, scope, name.clone(), Some(self.key(&name)));
                    self.e.bind(
                        scope,
                        &name,
                        Binding::Namespace {
                            prefixes: vec![format!("{}.", self.key(&name))],
                            separator: ".",
                        },
                    );
                }
                true
            }
            ("verilog", "package_import_declaration") => {
                for item in children(n) {
                    if let Some(p) = child(item, &["package_identifier"]) {
                        let package = self.text(p);
                        self.import(item, scope, package.clone(), Some(self.key(&package)));
                        if let Some(name) =
                            child(item, &["simple_identifier", "escaped_identifier"])
                        {
                            let name = self.text(name);
                            self.e.bind(
                                scope,
                                &name,
                                Binding::symbol(self.key(&format!("{package}.{name}"))),
                            );
                        }
                    }
                }
                true
            }
            ("zig", "builtin_function") => {
                if let Some(name) = child(n, &["builtin_identifier"])
                    && matches!(self.e.text(name), "@import" | "@cImport" | "@cInclude")
                {
                    if let Some(value) = find(n, &["string"]) {
                        self.file_import(n, scope, self.e.text(value), None);
                    } else {
                        self.unresolved(n, scope, self.text(name), "imports");
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }
    fn selector(&self, n: Syntax<'_>, message: bool) -> String {
        let mut result = String::new();
        let mut cursor = n.walk();
        for (i, c) in n.children(&mut cursor).enumerate() {
            let is_method = if message {
                n.field_name_for_child(i as u32) == Some("method")
            } else {
                c.kind() == "identifier"
            };
            if is_method {
                result.push_str(self.e.text(c));
                if self.e.source[c.end_byte()..n.end_byte()]
                    .trim_start()
                    .starts_with(':')
                    || (!message
                        && c.next_named_sibling()
                            .is_some_and(|s| s.kind() == "method_parameter"))
                {
                    result.push(':');
                }
            }
        }
        result
    }
    fn lisp(&mut self, n: Syntax<'t>, scope: usize) {
        if n.kind() == "include_reader_macro" {
            // Retain the written form without evaluating the reader feature.
            // An uncertain scope prevents conditional definitions leaking into
            // unconditional bindings or calls to an arbitrary active branch.
            let first = self.e.facts.nodes.len();
            let first_reference = self.e.facts.references.len();
            let s = self.e.block(scope, n);
            self.e.scopes[s].uncertain = true;
            self.prefixes.push(self.prefixes[scope].clone());
            let mut cursor = n.walk();
            for target in n
                .children_by_field_name("target", &mut cursor)
                .filter(|n| n.is_named())
            {
                self.visit(target, s);
            }
            let condition = serde_json::json!({
                "marker": n.child_by_field_name("marker").map(|v| self.e.text(v)),
                "feature": n.child_by_field_name("condition").map(|v| self.e.text(v)),
            });
            for node in &mut self.e.facts.nodes[first..] {
                node.binding_key = None;
                if !node.metadata["reader_conditions"].is_array() {
                    node.metadata["reader_conditions"] = serde_json::json!([]);
                }
                node.metadata["reader_conditions"]
                    .as_array_mut()
                    .unwrap()
                    .push(condition.clone());
            }
            for reference in &mut self.e.facts.references[first_reference..] {
                reference.candidate_keys.clear();
            }
            return;
        }
        if n.kind() == "defun" {
            if let Some(h) = child(n, &["defun_header"])
                && let Some(name) = h.child_by_field_name("function_name")
            {
                let keyword = h
                    .child_by_field_name("keyword")
                    .map(|k| self.text(k))
                    .unwrap_or_default();
                let kind = if keyword == "defmacro" {
                    "macro"
                } else if keyword == "defmethod" {
                    "method"
                } else {
                    "function"
                };
                let s = self.define(n, scope, self.text(name), kind, false);
                if let Some(p) = h.child_by_field_name("lambda_list") {
                    self.mask(p, s);
                }
                if let Some(doc) = children(n)
                    .into_iter()
                    .find(|c| c.id() != h.id() && c.kind() != "comment")
                    && doc.kind() == "str_lit"
                {
                    self.lisp_rationale(doc, s);
                }
                for c in children(n) {
                    if c.id() != h.id() {
                        self.visit(c, s);
                    }
                }
            }
            return;
        }
        if n.kind() != "list_lit" {
            self.walk(n, scope);
            return;
        }
        let cs = children(n);
        let Some(head) = cs.first() else { return };
        if head.kind() == "defun" {
            self.visit(*head, scope);
            return;
        }
        let op = self.text(*head);
        if matches!(op.as_str(), "quote" | "function") {
            return;
        }
        if matches!(op.as_str(), "in-package" | "defpackage") {
            if let Some(name) = cs.get(1) {
                let package = self.text(*name).trim_matches([':', '"']).to_string();
                if op == "in-package" {
                    self.prefixes[scope] = package;
                } else {
                    let s = self.define(n, scope, package, "module", true);
                    for option in cs.iter().skip(2) {
                        let values = children(*option);
                        if values.first().is_some_and(|v| self.text(*v) == ":use") {
                            for value in values.iter().skip(1) {
                                let name = self.text(*value).trim_start_matches(':').to_string();
                                self.import(*value, s, name.clone(), Some(self.key(&name)));
                            }
                        }
                    }
                }
            }
            return;
        }
        if matches!(
            op.as_str(),
            "defclass" | "defstruct" | "defgeneric" | "defvar" | "defparameter" | "defconstant"
        ) || (op.starts_with("def")
            && !op.starts_with("default")
            && op != "define"
            && head.kind() == "sym_lit")
        {
            if let Some(name) = cs.get(1).filter(|n| names(**n)) {
                let kind = match op.as_str() {
                    "defclass" | "define-condition" => "class",
                    "defstruct" => "struct",
                    "deftype" => "type",
                    "defgeneric" => "function",
                    "defvar" | "defparameter" | "defconstant" => "variable",
                    _ if cs.get(2).is_some_and(|n| n.kind() == "list_lit") => "function",
                    _ => "variable",
                };
                let s = self.define(n, scope, self.text(*name), kind, false);
                if matches!(op.as_str(), "defvar" | "defparameter" | "defconstant") {
                    if let Some(doc) = cs.get(3).filter(|c| c.kind() == "str_lit") {
                        self.lisp_rationale(*doc, s);
                    }
                } else if op == "defstruct" {
                    if let Some(doc) = cs.get(2).filter(|c| c.kind() == "str_lit") {
                        self.lisp_rationale(*doc, s);
                    }
                } else if op == "deftype"
                    && let Some(doc) = cs.get(3).filter(|c| c.kind() == "str_lit")
                {
                    self.lisp_rationale(*doc, s);
                }
                for option in cs.iter().skip(2).filter(|c| c.kind() == "list_lit") {
                    let values = children(*option);
                    if values
                        .first()
                        .is_some_and(|v| self.text(*v) == ":documentation")
                        && let Some(doc) = values.get(1).filter(|c| c.kind() == "str_lit")
                    {
                        self.lisp_rationale(*doc, s);
                    }
                }
                if matches!(op.as_str(), "defclass" | "define-condition")
                    && let Some(bases) = cs.get(2)
                {
                    for base in children(*bases) {
                        self.target(base, s, base, "inherits");
                    }
                }
                if kind == "function" {
                    if let Some(params) = cs.get(2) {
                        self.mask(*params, s);
                    }
                    for c in cs.iter().skip(3) {
                        self.visit(*c, s);
                    }
                } else if kind == "variable" {
                    for c in cs.iter().skip(2) {
                        self.visit(*c, s);
                    }
                }
            }
            return;
        }
        if matches!(
            op.as_str(),
            "let" | "let*" | "flet" | "labels" | "lambda" | "macrolet" | "symbol-macrolet"
        ) {
            let index = self.e.block(scope, n);
            self.prefixes
                .push(format!("{}.<{}>", self.prefixes[scope], n.start_byte()));
            // Binding initializers and local macros require evaluation order and a
            // separate function namespace. Retain calls, but do not guess targets.
            self.e.scopes[index].uncertain = true;
            for c in cs.iter().skip(2) {
                self.visit(*c, index);
            }
            return;
        }
        if !matches!(
            op.as_str(),
            "if" | "when"
                | "unless"
                | "cond"
                | "case"
                | "progn"
                | "prog1"
                | "prog2"
                | "and"
                | "or"
                | "setq"
                | "setf"
                | "return"
                | "return-from"
                | "block"
                | "catch"
                | "throw"
                | "unwind-protect"
                | "tagbody"
                | "go"
                | "the"
                | "declare"
                | "declaim"
                | "eval-when"
                | "multiple-value-bind"
                | "dolist"
                | "dotimes"
                | "loop"
        ) {
            if head.kind() == "package_lit" {
                let text = self.text(*head);
                let p: Vec<_> = text.split(':').filter(|s| !s.is_empty()).collect();
                if p.len() == 2 {
                    self.e.reference(
                        n,
                        scope,
                        text.clone(),
                        "calls",
                        vec![self.key(&format!("{}.{}", p[0], p[1]))],
                        "package target is unavailable or ambiguous",
                    );
                }
            } else if head.kind() == "sym_lit" {
                self.call(n, scope, *head, false);
            }
        }
        for c in cs.iter().skip(1) {
            self.visit(*c, scope);
        }
    }
}

fn asset_facts(path: &str, hash: &str) -> FileFacts {
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
fn asset_node(
    f: &mut FileFacts,
    label: &str,
    kind: &str,
    line: u32,
    parent: Option<String>,
) -> String {
    data_node(f, "dm", label, kind, line, parent)
}
fn data_node(
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
#[derive(Clone)]
struct Token<'a> {
    text: &'a str,
    line: u32,
    quoted: bool,
}
fn asset_tokens(source: &str) -> Option<Vec<Token<'_>>> {
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
fn asset(path: &str, source: &str, hash: &str) -> FileFacts {
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

/// Read only PNG metadata. Bitmap data is skipped, never decoded into pixels.
pub fn parse_dmi(path: &str, bytes: &[u8], hash: &str) -> Result<FileFacts> {
    anyhow::ensure!(
        !path.starts_with('/')
            && !path.contains('\\')
            && !path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == ".."),
        "source path must be a normalized relative POSIX path"
    );
    let mut f = asset_facts(path, hash);
    if bytes.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut f, None, "DMI exceeds the 4 MiB indexing limit");
        return Ok(f);
    }
    let descriptions = (|| -> Result<Vec<String>> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_limits(png::Limits {
            bytes: crate::parser::MAX_SOURCE_BYTES,
        });
        let mut reader = decoder.read_info()?;
        reader.finish()?;
        let info = reader.info();
        let mut descriptions = vec![];
        let mut remaining = 256 * 1024;
        for text in &info.uncompressed_latin1_text {
            if text.keyword == "Description" {
                anyhow::ensure!(
                    text.text.len() <= remaining,
                    "DMI description exceeds limit"
                );
                remaining -= text.text.len();
                descriptions.push(text.text.clone());
            }
        }
        for text in &info.compressed_latin1_text {
            if text.keyword == "Description" {
                let mut text = text.clone();
                text.decompress_text_with_limit(remaining)?;
                let value = text.get_text()?;
                anyhow::ensure!(value.len() <= remaining, "DMI description exceeds limit");
                remaining -= value.len();
                descriptions.push(value);
            }
        }
        for text in &info.utf8_text {
            if text.keyword == "Description" {
                let mut text = text.clone();
                text.decompress_text_with_limit(remaining)?;
                let value = text.get_text()?;
                anyhow::ensure!(value.len() <= remaining, "DMI description exceeds limit");
                remaining -= value.len();
                descriptions.push(value);
            }
        }
        anyhow::ensure!(
            descriptions.iter().map(String::len).sum::<usize>() <= 256 * 1024,
            "DMI description exceeds limit"
        );
        Ok(descriptions)
    })();
    let descriptions = match descriptions {
        Ok(d) => d,
        Err(_) => {
            diagnostic(
                &mut f,
                None,
                "Invalid PNG or DMI metadata exceeds the indexing limit",
            );
            return Ok(f);
        }
    };
    let root = asset_node(&mut f, path, "asset", 1, None);
    let mut current = None;
    for description in descriptions {
        for (i, line) in description.lines().enumerate() {
            let Some((name, value)) = line.trim().split_once('=') else {
                continue;
            };
            let value = value.trim();
            if name.trim() == "state"
                && value.starts_with('"')
                && value.ends_with('"')
                && value.len() >= 2
            {
                current = Some(asset_node(
                    &mut f,
                    &value[1..value.len() - 1],
                    "icon_state",
                    i as u32 + 1,
                    Some(root.clone()),
                ));
            } else if matches!(name.trim(), "dirs" | "frames" | "width" | "height")
                && let Ok(value) = value.parse::<u32>()
            {
                let id = current.as_ref().unwrap_or(&root);
                if let Some(n) = f.nodes.iter_mut().find(|n| &n.id == id) {
                    n.metadata[name.trim()] = serde_json::json!(value);
                }
            }
        }
    }
    Ok(f)
}

fn pascal_identifier(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|part| {
            let mut cs = part.chars();
            cs.next().is_some_and(|c| c.is_alphabetic() || c == '_')
                && cs.all(|c| c.is_alphanumeric() || c == '_')
        })
}

// Pascal strings, comments, and property blobs stay opaque. This lexer is used
// only by declarative forms and Delphi package headers, not Pascal source code.
fn pascal_tokens(source: &str) -> Result<Vec<Token<'_>>> {
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

fn pascal_string(token: &Token<'_>) -> String {
    if token.quoted {
        token.text[1..token.text.len() - 1].replace("''", "'")
    } else {
        token.text.into()
    }
}

fn data_reference(
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

fn invalid_data(f: &mut FileFacts, message: &str) {
    f.nodes.clear();
    f.edges.clear();
    f.references.clear();
    diagnostic(f, None, message);
}

/// Parse text Delphi/Lazarus forms and diagnose binary Delphi resources before
/// attempting UTF-8 decoding.
pub fn parse_pascal_form_bytes(path: &str, bytes: &[u8], hash: &str) -> Result<FileFacts> {
    anyhow::ensure!(
        !path.starts_with('/')
            && !path.contains('\\')
            && !path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == ".."),
        "source path must be a normalized relative POSIX path"
    );
    let mut f = asset_facts(path, hash);
    if bytes.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut f, None, "Form exceeds the 4 MiB indexing limit");
        return Ok(f);
    }
    if bytes.starts_with(b"TPF0") || bytes.starts_with(&[0xff, 0x0a]) {
        diagnostic(
            &mut f,
            None,
            "Binary DFM is unsupported; save the form as text in Delphi to index it",
        );
        return Ok(f);
    }
    let source = match std::str::from_utf8(bytes) {
        Ok(s) => s.trim_start_matches('\u{feff}'),
        Err(_) => {
            diagnostic(&mut f, None, "Form is not UTF-8 text");
            return Ok(f);
        }
    };
    let tokens = match pascal_tokens(source) {
        Ok(t) => t,
        Err(e) => {
            diagnostic(&mut f, None, &e.to_string());
            return Ok(f);
        }
    };
    let root = data_node(&mut f, "pascal", path, "asset", 1, None);
    let mut stack: Vec<(String, String)> = vec![];
    let mut instances = std::collections::HashMap::<String, Vec<String>>::new();
    let mut properties = vec![];
    let result = (|| -> Result<()> {
        let mut i = 0;
        while i < tokens.len() {
            let t = &tokens[i];
            if !t.quoted
                && matches!(
                    t.text.to_ascii_lowercase().as_str(),
                    "object" | "inherited" | "inline"
                )
            {
                anyhow::ensure!(
                    i + 3 < tokens.len()
                        && pascal_identifier(tokens[i + 1].text)
                        && tokens[i + 2].text == ":"
                        && pascal_identifier(tokens[i + 3].text),
                    "Invalid form component declaration"
                );
                anyhow::ensure!(stack.len() < 256, "Form nesting exceeds indexing limit");
                let instance = tokens[i + 1].text;
                let class = tokens[i + 3].text;
                let parent = stack.last().map_or(&root, |p| &p.0).clone();
                let qualified = stack
                    .last()
                    .map_or_else(|| instance.into(), |p| format!("{}.{}", p.1, instance));
                let id = data_node(&mut f, "pascal", class, "component", t.line, Some(parent));
                let key = format!("pascal:form:{path}:{}", qualified.to_lowercase());
                let node = f.nodes.last_mut().unwrap();
                node.qualified_name = Some(qualified.clone());
                node.binding_key = Some(key.clone());
                node.metadata["name"] = serde_json::json!(instance);
                node.metadata["class"] = serde_json::json!(class);
                node.metadata["declaration"] = serde_json::json!(t.text.to_ascii_lowercase());
                node.metadata["properties"] = serde_json::json!({});
                instances
                    .entry(instance.to_lowercase())
                    .or_default()
                    .push(key);
                data_reference(
                    &mut f,
                    &id,
                    class,
                    "uses_type",
                    t.line,
                    vec![],
                    "component class unit is unknown",
                );
                stack.push((id, qualified));
                i += 4;
                // Streaming form files may carry an inherited component index.
                if tokens.get(i).is_some_and(|t| t.text == "[") {
                    i += 1;
                    while i < tokens.len() && tokens[i].text != "]" {
                        i += 1;
                    }
                    anyhow::ensure!(i < tokens.len(), "Unterminated component index");
                    i += 1;
                }
            } else if !t.quoted && t.text.eq_ignore_ascii_case("end") {
                let (id, _) = stack
                    .pop()
                    .ok_or_else(|| anyhow::anyhow!("Unmatched form end"))?;
                f.nodes.iter_mut().find(|n| n.id == id).unwrap().end_line = Some(t.line);
                i += 1;
            } else if i + 1 < tokens.len() && pascal_identifier(t.text) && tokens[i + 1].text == "="
            {
                let (owner, _) = stack
                    .last()
                    .ok_or_else(|| anyhow::anyhow!("Property outside a form component"))?;
                let owner = owner.clone();
                i += 2;
                let start = i;
                let mut closing = vec![];
                while i < tokens.len() {
                    let value = &tokens[i];
                    if closing.is_empty() && value.line > t.line && i > start {
                        break;
                    }
                    if closing.is_empty()
                        && i == start
                        && value.line > t.line
                        && !matches!(value.text, "(" | "<" | "[")
                    {
                        break;
                    }
                    if !value.quoted {
                        match value.text {
                            "(" => closing.push(")"),
                            "<" => closing.push(">"),
                            "[" => closing.push("]"),
                            ")" | ">" | "]" => anyhow::ensure!(
                                closing.pop() == Some(value.text),
                                "Unbalanced form property"
                            ),
                            _ => {}
                        }
                        anyhow::ensure!(
                            closing.len() <= 256,
                            "Property nesting exceeds indexing limit"
                        );
                    }
                    i += 1;
                    if closing.is_empty() && tokens.get(i).is_none_or(|v| v.line > value.line) {
                        break;
                    }
                }
                anyhow::ensure!(closing.is_empty(), "Unterminated form property");
                let values = &tokens[start..i];
                if values.len() == 1 {
                    let v = &values[0];
                    let raw = pascal_string(v);
                    let value = if v.quoted {
                        serde_json::json!(raw)
                    } else if raw.eq_ignore_ascii_case("true") {
                        serde_json::json!(true)
                    } else if raw.eq_ignore_ascii_case("false") {
                        serde_json::json!(false)
                    } else if let Ok(n) = raw.parse::<i64>() {
                        serde_json::json!(n)
                    } else {
                        serde_json::json!(raw)
                    };
                    f.nodes.iter_mut().find(|n| n.id == owner).unwrap().metadata["properties"]
                        [t.text] = value;
                    if !v.quoted
                        && pascal_identifier(&raw)
                        && !matches!(raw.to_ascii_lowercase().as_str(), "true" | "false" | "nil")
                    {
                        let event = t.text.to_ascii_lowercase().starts_with("on");
                        properties.push((owner, raw, t.line, event, t.text.to_string()));
                    }
                }
            } else {
                anyhow::bail!("Unsupported or malformed form statement at line {}", t.line);
            }
        }
        anyhow::ensure!(stack.is_empty(), "Unterminated form component");
        Ok(())
    })();
    if let Err(e) = result {
        invalid_data(&mut f, &e.to_string());
        return Ok(f);
    }
    for (owner, value, line, event, property) in properties {
        let keys = if event {
            vec![]
        } else {
            instances
                .get(&value.to_lowercase())
                .filter(|v| v.len() == 1)
                .cloned()
                .unwrap_or_default()
        };
        data_reference(
            &mut f,
            &owner,
            &value,
            "references",
            line,
            keys,
            &if event {
                format!("event property {property}; handler unit is unknown")
            } else {
                format!("property {property}; target is unavailable or ambiguous")
            },
        );
    }
    Ok(f)
}

fn pascal_unit_reference(
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

fn pascal_package_xml(path: &str, source: &str, hash: &str) -> FileFacts {
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

fn pascal_package_source(path: &str, source: &str, hash: &str) -> FileFacts {
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

impl<'s, 't> Extended<'s, 't> {
    fn fact_node(&mut self, n: Syntax<'t>, label: &str, kind: &str, context: &str) -> String {
        let id = format!(
            "{}:{}:{context}:{label}",
            self.e.language, self.e.facts.path
        );
        if !self.e.facts.nodes.iter().any(|v| v.id == id) {
            self.e.facts.nodes.push(crate::model::Node {
                id: id.clone(),
                label: label.into(),
                kind: kind.into(),
                file: self.e.facts.path.clone(),
                line: Some(super::common::line(n)),
                end_line: Some(super::common::end_line(n)),
                qualified_name: None,
                binding_key: None,
                metadata: serde_json::json!({"context": context}),
            });
        }
        id
    }
    fn fact_edge(
        &mut self,
        n: Syntax<'t>,
        source: String,
        target: String,
        relation: &str,
        context: &str,
    ) {
        self.e.facts.edges.push(crate::model::Edge {
            id: format!("{relation}:{source}:{target}:{}", n.start_byte()),
            source,
            target,
            relation: relation.into(),
            directed: true,
            file: Some(self.e.facts.path.clone()),
            line: Some(super::common::line(n)),
            confidence: "static".into(),
            metadata: serde_json::json!({"context": context}),
        });
    }
    fn contextual_target(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        target: Syntax<'t>,
        relation: &'static str,
        context: &str,
    ) {
        self.contextual.push((
            n,
            scope,
            self.text(target),
            relation,
            self.parts(target),
            context.into(),
        ));
    }
    fn lisp_rationale(&mut self, n: Syntax<'t>, scope: usize) {
        let raw = self.e.text(n);
        let Some(raw) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
            return;
        };
        let mut chars = raw.chars();
        let mut doc = String::new();
        while let Some(c) = chars.next() {
            doc.push(if c == '\\' {
                chars.next().unwrap_or(c)
            } else {
                c
            });
        }
        let label: String = doc.chars().take(120).collect();
        let id = self.fact_node(
            n,
            &label,
            "rationale",
            &format!("docstring:{}", n.start_byte()),
        );
        if let Some(node) = self.e.facts.nodes.iter_mut().find(|v| v.id == id) {
            node.metadata["text"] = doc.into();
        }
        self.fact_edge(
            n,
            id,
            self.e.scopes[scope].owner.clone(),
            "rationale_for",
            "docstring",
        );
    }
    // OCaml compilation units use the capitalized source basename. Keep sibling
    // candidates qualified by directory; a compiler load path is not available.
    fn ocaml_external(&self, scope: usize, parts: &[String], module: bool) -> Vec<String> {
        let Some(first) = parts.first() else {
            return vec![];
        };
        if parts.len() == 1 && module {
            let mut current = Some(scope);
            while let Some(s) = current {
                if self.e.scopes[s].uncertain {
                    return vec![];
                }
                if let Some(b) = self.e.scopes[s].bindings.get(first) {
                    return match b {
                        Binding::Namespace { prefixes, .. } => prefixes
                            .iter()
                            .map(|p| p.trim_end_matches('.').into())
                            .collect(),
                        _ => vec![],
                    };
                }
                current = self.e.scopes[s].parent;
            }
        }
        if (!module && parts.len() < 2)
            || !self.e.unbound(scope, first)
            || !first.starts_with(|c: char| c.is_ascii_uppercase())
        {
            return vec![];
        }
        let base = format!("{}{}", first[..1].to_ascii_lowercase(), &first[1..]);
        let dir = self.e.facts.path.rsplit_once('/').map_or("", |p| p.0);
        ["ml", "mli"]
            .into_iter()
            .filter_map(|ext| {
                let path = relative_path(dir, &format!("{base}.{ext}"))?;
                Some(if module && parts.len() == 1 {
                    format!("ocaml:file:{path}")
                } else {
                    self.key(&format!("@{path}.{}", parts[1..].join(".")))
                })
            })
            .collect()
    }
    fn fortran_directive(&mut self, n: Syntax<'t>, scope: usize) -> bool {
        match n.kind() {
            "preproc_def" => {
                if let Some(name) = n.child_by_field_name("name") {
                    self.cpp_defines.insert(
                        self.e.text(name).into(),
                        Some(
                            n.child_by_field_name("value")
                                .map_or("", |v| self.e.text(v))
                                .trim()
                                .into(),
                        ),
                    );
                }
            }
            "preproc_function_def" => {}
            "preproc_call" => {
                if field(n, &["directive"]).is_some_and(|d| self.e.text(d).trim() == "#undef")
                    && let Some(name) = field(n, &["argument"])
                {
                    self.cpp_defines
                        .insert(self.e.text(name).trim().into(), None);
                }
            }
            "preproc_include" => {
                if let Some(path) = n.child_by_field_name("path") {
                    if path.kind() == "string_literal" {
                        self.file_import(n, scope, self.e.text(path), None);
                    } else {
                        self.unresolved(n, scope, self.text(path), "imports");
                    }
                }
            }
            "preproc_if" | "preproc_ifdef" | "preproc_elif" | "preproc_elifdef" => {
                let condition = field(n, &["condition", "name"]);
                let known = condition.and_then(|c| {
                    let text = self.e.text(c).trim();
                    if n.kind().ends_with("ifdef") || n.kind().ends_with("elifdef") {
                        self.cpp_defines.get(text).map(|v| {
                            v.is_some() != self.e.text(n).trim_start().starts_with("#ifndef")
                        })
                    } else {
                        let value = self
                            .cpp_defines
                            .get(text)
                            .and_then(|v| v.as_deref())
                            .unwrap_or(text);
                        match value {
                            "0" => Some(false),
                            "1" => Some(true),
                            _ => None,
                        }
                    }
                });
                let alternative = n.child_by_field_name("alternative");
                if known != Some(false) {
                    self.fortran_branch(n, scope, known.is_none(), condition, alternative);
                }
                if known != Some(true)
                    && let Some(other) = alternative
                {
                    if known.is_none() {
                        self.fortran_branch(other, scope, true, None, None);
                    } else {
                        self.visit(other, scope);
                    }
                }
            }
            "preproc_else" => self.walk(n, scope),
            _ => return false,
        }
        true
    }
    fn fortran_branch(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        conditional: bool,
        condition: Option<Syntax<'t>>,
        alternative: Option<Syntax<'t>>,
    ) {
        let condition = condition.or_else(|| field(n, &["condition", "name"]));
        let first_node = self.e.facts.nodes.len();
        let first_ref = self.e.facts.references.len();
        let saved = self.cpp_defines.clone();
        let s = if conditional {
            let s = self.e.block(scope, n);
            self.e.scopes[s].uncertain = true;
            self.prefixes.push(self.prefixes[scope].clone());
            s
        } else {
            scope
        };
        for c in children(n) {
            if condition.is_none_or(|v| v.id() != c.id())
                && alternative.is_none_or(|v| v.id() != c.id())
            {
                self.visit(c, s);
            }
        }
        if conditional {
            self.cpp_defines = saved;
            for node in &mut self.e.facts.nodes[first_node..] {
                node.binding_key = None;
                node.metadata["conditional_compilation"] = true.into();
            }
            for reference in &mut self.e.facts.references[first_ref..] {
                reference.candidate_keys.clear();
            }
        }
    }
    fn type_name(&self, n: Syntax<'t>) -> Option<String> {
        let mut parts = self.parts(n);
        if self.e.language == "apex"
            && parts
                .first()
                .is_some_and(|p| p.eq_ignore_ascii_case("List") || p.eq_ignore_ascii_case("Set"))
        {
            parts = child(n, &["type_arguments"])
                .and_then(|v| v.named_child(0))
                .map_or_else(Vec::new, |v| self.parts(v));
        }
        (!parts.is_empty()).then(|| parts.join("."))
    }
    fn value_type(&self, scope: usize, name: &str) -> Option<String> {
        let mut current = Some(scope);
        while let Some(s) = current {
            if self.e.scopes[s].uncertain {
                return None;
            }
            if let Some(ty) = self.value_types.get(&(s, name.into())) {
                return ty.clone();
            }
            if self.e.scopes[s].bindings.contains_key(name) {
                return None;
            }
            current = self.e.scopes[s].parent;
        }
        None
    }
    fn parameter_types(&mut self, header: Syntax<'t>, scope: usize) {
        let mut stack = vec![header];
        while let Some(n) = stack.pop() {
            if matches!(
                n.kind(),
                "formal_parameter" | "catch_formal_parameter" | "enhanced_for_statement"
            ) {
                let name = n
                    .child_by_field_name("name")
                    .or_else(|| child(n, &["identifier"]));
                if let Some(name) = name {
                    let ty = n
                        .child_by_field_name("type")
                        .or_else(|| child(n, &["type"]))
                        .and_then(|v| self.type_name(v));
                    let name = self.text(name);
                    self.e.bind(scope, &name, Binding::Unknown);
                    self.value_types.insert((scope, name), ty);
                }
            } else {
                stack.extend(children(n).into_iter().filter(|c| {
                    !matches!(
                        c.kind(),
                        "block" | "function_body" | "function_expression_body" | "class_body"
                    )
                }));
            }
        }
    }
    fn typed_declaration(&mut self, n: Syntax<'t>, scope: usize) -> bool {
        let apex = self.e.language == "apex";
        if !(apex && matches!(n.kind(), "local_variable_declaration" | "field_declaration"))
            && !(!apex
                && matches!(
                    n.kind(),
                    "initialized_variable_definition"
                        | "top_level_variable_declaration"
                        | "declaration"
                ))
        {
            return false;
        }
        if n.kind() == "declaration"
            && child(
                n,
                &[
                    "initialized_identifier_list",
                    "static_final_declaration_list",
                ],
            )
            .is_none()
        {
            return false;
        }
        let ty = n
            .child_by_field_name("type")
            .or_else(|| child(n, &["type"]))
            .and_then(|v| self.type_name(v));
        let mut stack = vec![n];
        while let Some(d) = stack.pop() {
            if let Some(name) = d.child_by_field_name("name") {
                let name = self.text(name);
                let value = d.child_by_field_name("value");
                let provider = !apex
                    && self.dart_has(&["riverpod", "flutter_riverpod", "hooks_riverpod"])
                    && value
                        .and_then(|v| v.child_by_field_name("function"))
                        .is_some_and(|function| {
                            let parts = self.parts(function);
                            parts.len() == 1
                                && self.dart_framework_name(scope, &parts[0])
                                && matches!(
                                    parts[0].as_str(),
                                    "Provider"
                                        | "StateProvider"
                                        | "FutureProvider"
                                        | "StreamProvider"
                                        | "NotifierProvider"
                                        | "StateNotifierProvider"
                                )
                        });
                if provider {
                    // A real provider declaration installs its symbol exactly once.
                    self.define(d, scope, name.clone(), "variable", false);
                } else {
                    self.e.bind(scope, &name, Binding::Unknown);
                }
                self.value_types.insert((scope, name), ty.clone());
                if let Some(value) = value {
                    self.visit(value, scope);
                }
                for c in children(d)
                    .into_iter()
                    .filter(|c| c.kind() == "initialized_identifier")
                {
                    stack.push(c);
                }
            } else {
                stack.extend(
                    children(d)
                        .into_iter()
                        .filter(|c| !matches!(c.kind(), "type" | "generic_type" | "annotation")),
                );
            }
        }
        true
    }
    fn declaration_annotations(&mut self, n: Syntax<'t>, scope: usize) {
        if !matches!(self.e.language, "apex" | "dart") {
            return;
        }
        let mut annotations = children(n);
        if let Some(modifiers) = child(n, &["modifiers"]) {
            annotations.extend(children(modifiers));
        }
        for annotation in annotations
            .into_iter()
            .filter(|c| matches!(c.kind(), "annotation" | "marker_annotation"))
        {
            let Some(name) = annotation.child_by_field_name("name") else {
                continue;
            };
            let label = self.text(name);
            let id = self.fact_node(annotation, &label, "annotation", "annotation");
            let owner = self.e.scopes[scope].owner.clone();
            self.fact_edge(annotation, id, owner.clone(), "configures", "annotation");
            if self.e.language == "apex"
                && matches!(
                    label.to_ascii_lowercase().as_str(),
                    "auraenabled" | "invocablemethod"
                )
            {
                self.fact_edge(
                    annotation,
                    self.e.scopes[0].owner.clone(),
                    owner,
                    "exposes",
                    "apex_entrypoint",
                );
            }
            if self.e.language == "dart"
                && label == "riverpod"
                && self.dart_has(&["riverpod_annotation"])
                && self.dart_framework_name(scope, &label)
            {
                let Some(parent) = self.e.scopes[scope].parent else {
                    continue;
                };
                let Some(definition) = self
                    .e
                    .facts
                    .nodes
                    .iter()
                    .find(|v| v.id == self.e.scopes[scope].owner)
                else {
                    continue;
                };
                let mut chars = definition.label.chars();
                let Some(first) = chars.next() else { continue };
                let generated = format!("{}{}Provider", first.to_lowercase(), chars.as_str());
                let s = self.define(annotation, parent, generated, "variable", false);
                let generated_id = self.e.scopes[s].owner.clone();
                self.fact_edge(
                    annotation,
                    self.e.scopes[scope].owner.clone(),
                    generated_id.clone(),
                    "defines",
                    "riverpod_generator",
                );
                if let Some(node) = self.e.facts.nodes.iter_mut().find(|v| v.id == generated_id) {
                    node.metadata["generated"] = true.into();
                }
            }
        }
    }
    fn apex_dml(&mut self, n: Syntax<'t>, scope: usize) {
        let Some(operation) = child(n, &["dml_type"]) else {
            return;
        };
        let op = self.text(operation).to_ascii_lowercase();
        let id = self.fact_node(operation, &op, "operation", "dml");
        self.fact_edge(
            n,
            self.e.scopes[scope].owner.clone(),
            id,
            "uses",
            "dml_operation",
        );
        for target in ["target", "merge_with"]
            .into_iter()
            .filter_map(|f| n.child_by_field_name(f))
        {
            let ty = if target.kind() == "object_creation_expression" {
                target
                    .child_by_field_name("type")
                    .and_then(|v| self.type_name(v))
            } else if target.kind() == "identifier" {
                self.value_type(scope, self.e.text(target))
            } else {
                None
            };
            let ty = ty.filter(|v| {
                !matches!(
                    v.to_ascii_lowercase().as_str(),
                    "sobject" | "object" | "string" | "integer" | "list" | "set"
                )
            });
            if let Some(ty) = ty {
                self.e.reference(
                    target,
                    scope,
                    ty.clone(),
                    "uses",
                    vec![self.key(&ty)],
                    &format!("dml_{op}: explicitly declared operand type"),
                );
            } else {
                self.e.reference(
                    target,
                    scope,
                    self.text(target),
                    "uses",
                    vec![],
                    &format!("dml_{op}: operand type is unknown"),
                );
            }
        }
    }
    fn dart_has(&self, packages: &[&str]) -> bool {
        packages.iter().any(|p| self.dart_packages.contains(*p))
    }
    fn dart_framework_name(&self, scope: usize, name: &str) -> bool {
        !self.dart_local_types.contains(name) && self.e.unbound(scope, name)
    }
    fn dart_environment(&mut self, root: Syntax<'t>) {
        for n in children(root).into_iter().filter(|n| {
            matches!(
                n.kind(),
                "class_declaration" | "mixin_declaration" | "enum_declaration" | "type_alias"
            )
        }) {
            if let Some(name) = n.child_by_field_name("name") {
                self.dart_local_types.insert(self.text(name));
            }
        }
        for n in children(root) {
            if n.kind() == "import_or_export" || n.kind() == "import_specification" {
                let import = if n.kind() == "import_specification" {
                    Some(n)
                } else {
                    find(n, &["import_specification"])
                };
                if let Some(import) = import
                    && import.child_by_field_name("alias").is_none()
                    && child(import, &["combinator"]).is_none()
                    && let Some(literal) = find(import, &["string_literal"])
                    && let Some(uri) = dart_string(self.e.text(literal))
                    && let Some(package) = uri
                        .strip_prefix("package:")
                        .and_then(|s| s.split('/').next())
                {
                    self.dart_packages.insert(package.into());
                }
            }
        }
        if self.dart_has(&["bloc", "flutter_bloc"]) {
            for n in children(root)
                .into_iter()
                .filter(|n| n.kind() == "class_declaration")
            {
                if let (Some(name), Some(base)) = (
                    n.child_by_field_name("name"),
                    n.child_by_field_name("superclass"),
                ) && let Some(base) = find(base, &["type_identifier"])
                    && matches!(self.e.text(base), "Bloc" | "Cubit")
                    && !self.dart_local_types.contains(self.e.text(base))
                {
                    self.dart_bloc_types
                        .insert(self.text(name), self.text(base));
                }
            }
        }
    }
    fn dart_bloc_scope(&self, scope: usize) -> bool {
        let mut current = Some(scope);
        while let Some(s) = current {
            if self.e.scopes[s].class {
                return self
                    .e
                    .facts
                    .nodes
                    .iter()
                    .find(|n| n.id == self.e.scopes[s].owner)
                    .is_some_and(|n| self.dart_bloc_types.contains_key(&n.label));
            }
            current = self.e.scopes[s].parent;
        }
        false
    }
    fn dart_call(&mut self, n: Syntax<'t>, scope: usize) {
        let Some(function) = n.child_by_field_name("function") else {
            return;
        };
        let parts = self.parts(function);
        let Some(method) = parts.last().map(String::as_str) else {
            return;
        };
        let args = n
            .child_by_field_name("arguments")
            .map(children)
            .unwrap_or_default();
        let types = function
            .child_by_field_name("type_arguments")
            .map(children)
            .unwrap_or_default();
        let receiver = (parts.len() == 2)
            .then(|| self.value_type(scope, &parts[0]))
            .flatten();
        let bloc = self.dart_has(&["bloc", "flutter_bloc"]);
        let riverpod = self.dart_has(&[
            "riverpod",
            "flutter_riverpod",
            "hooks_riverpod",
            "riverpod_annotation",
        ]);
        if bloc
            && parts.len() == 1
            && method == "on"
            && self.e.unbound(scope, "on")
            && self.dart_bloc_scope(scope)
            && let Some(event) = types.first()
        {
            self.contextual_target(n, scope, *event, "calls", "bloc_event");
        }
        if bloc
            && method == "emit"
            && parts.len() == 1
            && ((self.dart_bloc_scope(scope) && self.e.unbound(scope, method))
                || self.value_type(scope, method).as_deref() == Some("Emitter"))
            && let Some(arg) = args.first().and_then(|v| v.child_by_field_name("function"))
        {
            self.contextual_target(n, scope, arg, "calls", "emit_state");
        }
        if bloc
            && method == "add"
            && receiver
                .as_ref()
                .is_some_and(|t| self.dart_bloc_types.contains_key(t) || t == "Bloc")
            && let Some(arg) = args.first().and_then(|v| v.child_by_field_name("function"))
        {
            self.contextual_target(n, scope, arg, "calls", "bloc_add_event");
        }
        if bloc
            && parts.len() == 1
            && matches!(
                method,
                "BlocBuilder" | "BlocListener" | "BlocConsumer" | "BlocProvider" | "BlocSelector"
            )
            && self.dart_framework_name(scope, method)
            && let Some(ty) = types.first()
        {
            self.contextual_target(n, scope, *ty, "references", "bloc_widget_binding");
        }
        if bloc
            && ((receiver.as_deref() == Some("BuildContext")
                && self.dart_framework_name(scope, "BuildContext")
                && matches!(method, "read" | "watch" | "select"))
                || (parts == ["BlocProvider", "of"]
                    && self.dart_framework_name(scope, "BlocProvider")))
            && let Some(ty) = types.first()
        {
            self.contextual_target(n, scope, *ty, "references", "bloc_lookup");
        }
        if riverpod
            && matches!(receiver.as_deref(), Some("WidgetRef" | "Ref"))
            && receiver
                .as_ref()
                .is_some_and(|name| self.dart_framework_name(scope, name))
            && matches!(method, "watch" | "read" | "listen")
            && let Some(provider) = args.first().filter(|a| !self.parts(**a).is_empty())
        {
            self.contextual_target(n, scope, *provider, "references", "riverpod_reference");
        }
        let navigator = parts.len() == 2
            && parts[0] == "Navigator"
            && self.dart_has(&["flutter"])
            && self.dart_framework_name(scope, "Navigator");
        let router = self.dart_has(&["go_router"])
            && matches!(receiver.as_deref(), Some("BuildContext" | "GoRouter"))
            && receiver
                .as_ref()
                .is_some_and(|name| self.dart_framework_name(scope, name));
        if ((router
            && matches!(
                method,
                "go" | "push" | "goNamed" | "pushNamed" | "replace" | "replaceNamed"
            ))
            || (navigator
                && matches!(
                    method,
                    "pushNamed" | "pushReplacementNamed" | "popAndPushNamed"
                )))
            && let Some(target) = args.get(usize::from(navigator))
        {
            let context = if method.contains("Named") {
                "route_name"
            } else {
                "route_path"
            };
            if target.kind() == "string_literal"
                && let Some(value) = dart_string(self.e.text(*target))
            {
                let id = self.fact_node(*target, &value, "route", context);
                self.fact_edge(
                    n,
                    self.e.scopes[scope].owner.clone(),
                    id,
                    "navigates",
                    context,
                );
            } else if !self.parts(*target).is_empty() {
                self.contextual_target(n, scope, *target, "navigates", "route_const");
            }
        }
        if self.dart_has(&["go_router"])
            && parts == ["GoRoute"]
            && self.dart_framework_name(scope, "GoRoute")
        {
            for arg in args.iter().filter(|a| a.kind() == "named_argument") {
                let cs = children(*arg);
                if let [label, value] = cs.as_slice()
                    && matches!(self.e.text(*label).trim_end_matches(':'), "path" | "name")
                    && value.kind() == "string_literal"
                    && let Some(value) = dart_string(self.e.text(*value))
                {
                    let context = if self.e.text(*label).trim_end_matches(':') == "name" {
                        "route_name"
                    } else {
                        "route_path"
                    };
                    let id = self.fact_node(*arg, &value, "route", context);
                    self.fact_edge(
                        n,
                        self.e.scopes[scope].owner.clone(),
                        id,
                        "defines",
                        context,
                    );
                }
            }
        }
    }
}

fn dart_string(raw: &str) -> Option<String> {
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

impl<'s, 't> Extended<'s, 't> {
    fn owner_metadata(&self, mut scope: usize, field: &str) -> Option<String> {
        loop {
            if let Some(value) = self
                .e
                .facts
                .nodes
                .iter()
                .find(|n| n.id == self.e.scopes[scope].owner)
                .and_then(|n| n.metadata[field].as_str())
            {
                return Some(value.into());
            }
            scope = self.e.scopes[scope].parent?;
        }
    }
    fn annotate(&mut self, scope: usize, values: serde_json::Value) {
        if let Some(node) = self
            .e
            .facts
            .nodes
            .iter_mut()
            .find(|n| n.id == self.e.scopes[scope].owner)
        {
            for (key, value) in values.as_object().unwrap() {
                node.metadata[key] = value.clone();
            }
        }
    }
    fn local_shadow(&self, mut scope: usize, name: &str) -> bool {
        while let Some(parent) = self.e.scopes[scope].parent {
            let current = &self.e.scopes[scope];
            if current.class
                || self
                    .e
                    .facts
                    .nodes
                    .iter()
                    .any(|n| n.id == current.owner && n.kind == "module")
            {
                break;
            }
            if current.bindings.contains_key(name) {
                return true;
            }
            scope = parent;
        }
        false
    }
    fn navigation_hint(
        &mut self,
        n: Syntax<'t>,
        scope: usize,
        label: String,
        mut hint: serde_json::Value,
    ) {
        self.e.reference(
            n,
            scope,
            label,
            "calls",
            vec![],
            "explicit receiver requires unique project declaration",
        );
        hint["reference"] = self.e.facts.references.last().unwrap().id.clone().into();
        let metadata = &mut self.e.facts.nodes[0].metadata;
        if !metadata["member_navigation"].is_array() {
            metadata["member_navigation"] = serde_json::json!([]);
        }
        metadata["member_navigation"]
            .as_array_mut()
            .unwrap()
            .push(hint);
    }
    fn pascal_call(&mut self, n: Syntax<'t>, scope: usize, target: Syntax<'t>) {
        let inherited = target.kind() == "inherited";
        let parts = if inherited {
            child(target, &["identifier"])
                .map(|n| vec![self.text(n)])
                .unwrap_or_default()
        } else {
            self.parts(target)
        };
        let method = parts.last().cloned().or_else(|| {
            if inherited {
                self.owner_metadata(scope, "pascal_method")
            } else {
                None
            }
        });
        if let (Some(owner), Some(method)) = (self.owner_metadata(scope, "pascal_owner"), method)
            && (inherited || parts.len() == 1 || parts.len() == 2 && parts[0] == "self")
            && (inherited || !self.local_shadow(scope, &method))
        {
            self.navigation_hint(n, scope, method.clone(), serde_json::json!({"language":"pascal", "class":owner, "member":method, "inherited":inherited}));
            if !inherited {
                let keys = self.e.resolve(scope, &parts);
                self.e.facts.references.last_mut().unwrap().candidate_keys = keys;
            }
        } else {
            self.call(n, scope, target, inherited);
        }
    }
    fn objc_message(&mut self, n: Syntax<'t>, scope: usize) {
        let selector = self.selector(n, true);
        let Some(receiver) = n
            .child_by_field_name("receiver")
            .filter(|n| n.kind() == "identifier")
        else {
            self.unresolved(n, scope, selector, "calls");
            return;
        };
        let receiver = self.text(receiver);
        let mut parent = Some(scope);
        let mut masked = false;
        while let Some(s) = parent {
            masked |= matches!(
                self.e.scopes[s].bindings.get(&receiver),
                Some(Binding::Unknown)
            );
            parent = self.e.scopes[s].parent;
        }
        if masked || self.local_shadow(scope, &receiver) {
            self.unresolved(n, scope, selector, "calls");
            return;
        }
        let owner = self.owner_metadata(scope, "objc_owner");
        if receiver == "self" && owner.is_none() || receiver == "super" {
            self.unresolved(n, scope, selector, "calls");
            return;
        }
        let sign = if receiver == "self" {
            self.owner_metadata(scope, "objc_method_kind")
                .unwrap_or_else(|| "-".into())
        } else {
            "+".into()
        };
        self.navigation_hint(n, scope, selector.clone(), serde_json::json!({"language":"objc", "receiver":receiver, "owner":owner, "member":format!("{sign}{selector}")}));
    }
}

/// File families whose explicit declarations can supply extended project links.
pub fn applies(path: &str) -> bool {
    matches!(
        path.rsplit('.').next(),
        Some("h" | "m" | "mm" | "pas" | "pp" | "dpr" | "lpr" | "inc" | "dfm" | "lfm")
    )
}

/// Source-only project navigation. Declarations are retained separately; an
/// ambiguous class, implementation, overload or ancestor never becomes a link.
#[derive(Default)]
pub struct ExtendedContext {
    nodes: HashMap<String, crate::model::Node>,
    imports: HashMap<String, Vec<String>>,
    groups: HashMap<String, String>,
    bindings: HashMap<String, Option<String>>,
    source_hashes: HashMap<String, String>,
    fingerprint: String,
}
impl ExtendedContext {
    pub fn discover(root: &std::path::Path, paths: &[String]) -> Result<Self> {
        anyhow::ensure!(
            std::fs::symlink_metadata(root)?.is_dir(),
            "source root must be a directory, not a symlink"
        );
        let mut facts = vec![];
        let ordered: std::collections::BTreeSet<_> = paths.iter().filter(|p| applies(p)).collect();
        for path in ordered {
            anyhow::ensure!(
                !path.starts_with('/')
                    && !path.contains(['\\', ':'])
                    && !path
                        .split('/')
                        .any(|p| p.is_empty() || p == "." || p == ".."),
                "source path must be a normalized relative POSIX path"
            );
            let mut cursor = root.to_path_buf();
            let mut eligible = true;
            for component in path.split('/') {
                cursor.push(component);
                match std::fs::symlink_metadata(&cursor) {
                    Ok(metadata) if !metadata.file_type().is_symlink() => {}
                    Ok(_) => {
                        eligible = false;
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        eligible = false;
                        break;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            if !eligible || !std::fs::symlink_metadata(&cursor)?.is_file() {
                facts.push(asset_facts(path, "unavailable"));
                continue;
            }
            let (hash, bytes) =
                crate::index::read_source(&cursor, crate::parser::MAX_SOURCE_BYTES as u64)?;
            let parsed = if let Some(bytes) = bytes {
                if matches!(path.rsplit('.').next(), Some("dfm" | "lfm")) {
                    Some(parse_pascal_form_bytes(path, &bytes, &hash)?)
                } else if let Ok(source) = std::str::from_utf8(&bytes) {
                    parse(path, source, &hash)?
                } else {
                    None
                }
            } else {
                None
            };
            facts.push(parsed.unwrap_or_else(|| asset_facts(path, &hash)));
        }
        Ok(Self::from_facts(&facts))
    }
    /// Build the same context from already parsed, bounded indexed inputs.
    pub fn from_facts(files: &[FileFacts]) -> Self {
        let mut result = Self::default();
        let mut hash = blake3::Hasher::new();
        hash.update(b"extended-context-v1\0");
        let mut ordered: Vec<_> = files.iter().filter(|f| applies(&f.path)).collect();
        ordered.sort_by(|a, b| a.path.cmp(&b.path));
        for facts in ordered {
            result
                .source_hashes
                .insert(facts.path.clone(), facts.hash.clone());
            hash.update(facts.path.as_bytes());
            hash.update(&[0]);
            hash.update(facts.hash.as_bytes());
            hash.update(&[0]);
            if !facts.diagnostics.is_empty() {
                continue;
            }
            let imports = result.imports.entry(facts.path.clone()).or_default();
            for reference in facts.references.iter().filter(|r| r.relation == "imports") {
                if reference.source.starts_with("objc:") {
                    imports.extend(
                        reference
                            .candidate_keys
                            .iter()
                            .filter_map(|k| k.strip_prefix("objc:file:").map(str::to_owned)),
                    );
                } else if reference.source.starts_with("pascal:") {
                    imports.push(reference.label.to_lowercase());
                }
            }
            for node in &facts.nodes {
                if node.metadata["objc_class"].is_string()
                    || node.metadata["objc_owner"].is_string()
                    || node.metadata["pascal_class"].is_string()
                    || node.metadata["pascal_unit"].is_string()
                {
                    result.nodes.insert(node.id.clone(), node.clone());
                }
            }
        }
        result.fingerprint = hash.finalize().to_hex().to_string();
        for node in result.nodes.values().filter(|n| project_definition(n)) {
            if let Some(key) = &node.binding_key {
                result
                    .bindings
                    .entry(key.clone())
                    .and_modify(|value| *value = None)
                    .or_insert_with(|| Some(project_key(node)));
            }
        }
        // First anchor ordinary interfaces; pair implementations with one
        // explicit imported/sibling interface, then attach category declarations.
        let classes: Vec<_> = result
            .nodes
            .values()
            .filter(|n| n.metadata["objc_class"].is_string())
            .cloned()
            .collect();
        for class in classes.iter().filter(|n| {
            n.metadata["objc_category"] != true && n.metadata["objc_role"] == "class_interface"
        }) {
            result.groups.insert(class.id.clone(), class.id.clone());
        }
        for class in classes.iter().filter(|n| {
            n.metadata["objc_category"] != true && n.metadata["objc_role"] == "class_implementation"
        }) {
            let Some(visible) = result.visible_files(&class.file) else {
                result.groups.insert(class.id.clone(), class.id.clone());
                continue;
            };
            let candidates: Vec<_> = classes
                .iter()
                .filter(|n| {
                    n.metadata["objc_role"] == "class_interface"
                        && n.metadata["objc_category"] != true
                        && n.label == class.label
                        && (visible.contains(&n.file)
                            || module_path(&n.file) == module_path(&class.file))
                })
                .collect();
            let anchor = if candidates.len() == 1 {
                candidates[0].id.clone()
            } else {
                class.id.clone()
            };
            result.groups.insert(class.id.clone(), anchor);
        }
        for class in classes
            .iter()
            .filter(|n| n.metadata["objc_category"] == true)
        {
            let Some(visible) = result.visible_files(&class.file) else {
                continue;
            };
            let stem = module_path(&class.file);
            let base_stem = stem.rsplit_once('+').map(|(base, _)| base);
            let candidates: HashSet<_> = classes
                .iter()
                .filter(|n| {
                    n.metadata["objc_category"] != true
                        && n.label == class.label
                        && (visible.contains(&n.file)
                            || base_stem.is_some_and(|s| module_path(&n.file) == s))
                })
                .filter_map(|n| result.groups.get(&n.id).cloned())
                .collect();
            if candidates.len() == 1 {
                result
                    .groups
                    .insert(class.id.clone(), candidates.into_iter().next().unwrap());
            }
        }
        result
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    /// Compare the raw bounded-reader hash, before any project decoration.
    pub fn validate_source(&self, path: &str, content_hash: &str) -> Result<()> {
        anyhow::ensure!(
            !applies(path)
                || self
                    .source_hashes
                    .get(path)
                    .is_some_and(|hash| hash == content_hash),
            "source changed during context discovery; retry indexing"
        );
        Ok(())
    }
    fn visible_files(&self, path: &str) -> Option<HashSet<String>> {
        let mut seen = HashSet::new();
        let mut pending = vec![path.to_string()];
        while let Some(path) = pending.pop() {
            if !seen.insert(path.clone()) {
                continue;
            }
            if seen.len() > 4096 {
                // Incomplete evidence must also suppress sibling pairing.
                return None;
            }
            if let Some(imports) = self.imports.get(&path) {
                pending.extend(imports.iter().cloned());
            }
        }
        Some(seen)
    }
    fn unique_objc_group(&self, file: &str, name: &str) -> Option<String> {
        let visible = self.visible_files(file)?;
        let groups: HashSet<_> = self
            .nodes
            .values()
            .filter(|n| n.metadata["objc_class"] == name && visible.contains(&n.file))
            .filter_map(|n| self.groups.get(&n.id).cloned())
            .collect();
        (groups.len() == 1).then(|| groups.into_iter().next().unwrap())
    }
    fn objc_members(&self, group: &str, member: &str) -> Vec<&crate::model::Node> {
        self.nodes
            .values()
            .filter(|n| {
                n.metadata["objc_method"] == member
                    && n.metadata["objc_owner"]
                        .as_str()
                        .and_then(|owner| self.groups.get(owner))
                        .is_some_and(|g| g == group)
            })
            .collect()
    }
    fn method_target<'a>(
        members: &[&'a crate::model::Node],
    ) -> Result<Option<&'a crate::model::Node>, ()> {
        let bodies: Vec<_> = members
            .iter()
            .copied()
            .filter(|n| n.metadata["body"] == true)
            .collect();
        let declarations: Vec<_> = members
            .iter()
            .copied()
            .filter(|n| n.metadata["body"] != true)
            .collect();
        if bodies.len() > 1 || declarations.len() > 1 {
            return Err(());
        }
        Ok(bodies.first().or(declarations.first()).copied())
    }
    fn pascal_class(&self, file: &str, name: &str) -> Option<&crate::model::Node> {
        let name = name.to_lowercase();
        let (unit, name) = name
            .rsplit_once('.')
            .map_or((None, name.as_str()), |(u, n)| (Some(u), n));
        let local: Vec<_> = self
            .nodes
            .values()
            .filter(|n| {
                n.file == file
                    && n.metadata["pascal_class"] == name
                    && unit.is_none_or(|u| n.metadata["pascal_unit"] == u)
            })
            .collect();
        if !local.is_empty() {
            return (local.len() == 1).then_some(local[0]);
        }
        let mut candidates = vec![];
        for imported in self
            .imports
            .get(file)
            .into_iter()
            .flatten()
            .filter(|u| unit.is_none_or(|v| v == u.as_str()))
        {
            let units: Vec<_> = self
                .nodes
                .values()
                .filter(|n| n.kind == "module" && n.metadata["pascal_unit"] == imported.as_str())
                .collect();
            if units.len() != 1 {
                return None;
            }
            candidates.extend(
                self.nodes
                    .values()
                    .filter(|n| n.file == units[0].file && n.metadata["pascal_class"] == name),
            );
        }
        candidates.sort_by(|a, b| a.id.cmp(&b.id));
        candidates.dedup_by(|a, b| a.id == b.id);
        (candidates.len() == 1).then(|| candidates[0])
    }
    fn pascal_base(&self, class: &crate::model::Node) -> Result<Option<&crate::model::Node>, ()> {
        let bases = class.metadata["pascal_bases"].as_array().ok_or(())?;
        if bases.is_empty() {
            return Ok(None);
        }
        if bases.len() != 1 {
            return Err(());
        }
        self.pascal_class(&class.file, bases[0].as_str().ok_or(())?)
            .map(Some)
            .ok_or(())
    }
    fn pascal_member(
        &self,
        class: &crate::model::Node,
        name: &str,
        inherited: bool,
    ) -> Result<Option<&crate::model::Node>, ()> {
        let mut owner = if inherited {
            self.pascal_base(class)?
        } else {
            self.nodes.get(&class.id)
        };
        let mut seen = HashSet::new();
        while let Some(class) = owner {
            if seen.len() >= 256 || !seen.insert(class.id.clone()) {
                return Err(());
            }
            if class.metadata["pascal_shadowed_members"]
                .as_array()
                .is_some_and(|names| names.iter().any(|value| value == name))
            {
                return Err(());
            }
            let members: Vec<_> = self
                .nodes
                .values()
                .filter(|n| {
                    n.file == class.file
                        && n.metadata["pascal_method"] == name
                        && n.metadata["pascal_owner"]
                            .as_str()
                            .is_some_and(|v| v.rsplit('.').next() == Some(class.label.as_str()))
                })
                .collect();
            if !members.is_empty() {
                return Self::method_target(&members);
            }
            owner = self.pascal_base(class)?;
        }
        Ok(None)
    }
    pub fn apply(&self, facts: &mut FileFacts) {
        if !applies(&facts.path) || !facts.diagnostics.is_empty() {
            return;
        }
        for reference in &mut facts.references {
            for key in &mut reference.candidate_keys {
                if let Some(Some(replacement)) = self.bindings.get(key) {
                    *key = replacement.clone();
                }
            }
        }
        for node in &mut facts.nodes {
            if project_definition(node) && self.nodes.contains_key(&node.id) {
                node.binding_key = Some(project_key(node));
            }
        }
        let hints: Vec<_> = facts
            .nodes
            .first()
            .and_then(|n| n.metadata["member_navigation"].as_array())
            .cloned()
            .unwrap_or_default();
        for hint in hints {
            let Some(reference) = facts
                .references
                .iter_mut()
                .find(|r| Some(r.id.as_str()) == hint["reference"].as_str())
            else {
                continue;
            };
            let member = hint["member"].as_str().unwrap_or("");
            if hint["language"] == "objc" {
                let group = if hint["receiver"] == "self" {
                    hint["owner"]
                        .as_str()
                        .and_then(|id| self.groups.get(id).cloned())
                } else {
                    self.unique_objc_group(&facts.path, hint["receiver"].as_str().unwrap_or(""))
                };
                let target = group.and_then(|g| {
                    Self::method_target(&self.objc_members(&g, member))
                        .ok()
                        .flatten()
                });
                reference.candidate_keys = target.map(project_key).into_iter().collect();
            } else if hint["language"] == "pascal" {
                let target = self
                    .pascal_class(&facts.path, hint["class"].as_str().unwrap_or(""))
                    .map(|c| self.pascal_member(c, member, hint["inherited"] == true))
                    .unwrap_or(Err(()));
                match target {
                    Ok(Some(target)) => reference.candidate_keys = vec![project_key(target)],
                    Err(()) => reference.candidate_keys.clear(),
                    Ok(None) => {}
                }
            }
        }
        // Link separately retained declarations, never merge their identities.
        let own: Vec<_> = facts
            .nodes
            .iter()
            .filter(|n| self.nodes.contains_key(&n.id))
            .cloned()
            .collect();
        for node in own {
            if let Some(anchor) = self
                .groups
                .get(&node.id)
                .filter(|anchor| *anchor != &node.id)
                .and_then(|id| self.nodes.get(id))
            {
                project_reference(
                    facts,
                    &node,
                    anchor,
                    if node.metadata["objc_category"] == true {
                        "extends"
                    } else {
                        "implements"
                    },
                );
            }
            if let Some(owner) = node.metadata["objc_owner"]
                .as_str()
                .and_then(|id| self.groups.get(id))
                && node.metadata["body"] == true
            {
                let members =
                    self.objc_members(owner, node.metadata["objc_method"].as_str().unwrap_or(""));
                let declarations: Vec<_> = members
                    .into_iter()
                    .filter(|n| n.metadata["body"] != true)
                    .collect();
                if declarations.len() == 1 {
                    project_reference(facts, &node, declarations[0], "implements");
                }
            }
            if node.metadata["pascal_class"].is_string()
                && let Ok(Some(base)) = self.pascal_base(&node)
            {
                project_reference(facts, &node, base, "inherits");
            }
            if let Some(owner) = node.metadata["pascal_owner"].as_str()
                && let Some(class) = self.pascal_class(&node.file, owner)
                && class.file == node.file
            {
                project_reference(facts, class, &node, "method");
                if node.metadata["body"] == true {
                    let declarations: Vec<_> = self
                        .nodes
                        .values()
                        .filter(|n| {
                            n.file == node.file
                                && n.metadata["pascal_owner"] == owner
                                && n.metadata["pascal_method"] == node.metadata["pascal_method"]
                                && n.metadata["body"] != true
                        })
                        .collect();
                    if declarations.len() == 1 {
                        project_reference(facts, &node, declarations[0], "implements");
                    }
                }
            }
        }
        if matches!(facts.path.rsplit('.').next(), Some("dfm" | "lfm")) {
            let roots: Vec<_> = facts
                .nodes
                .iter()
                .filter(|n| {
                    n.kind == "component"
                        && n.qualified_name.as_ref().is_some_and(|q| !q.contains('.'))
                })
                .collect();
            if roots.len() != 1 {
                return;
            }
            let class_name = roots[0].metadata["class"]
                .as_str()
                .unwrap_or("")
                .to_lowercase();
            let classes: Vec<_> = self
                .nodes
                .values()
                .filter(|n| {
                    n.metadata["pascal_class"] == class_name
                        && module_path(&n.file) == module_path(&facts.path)
                })
                .collect();
            if classes.len() != 1 {
                return;
            }
            for reference in &mut facts.references {
                if reference.reason.starts_with("event property ") {
                    reference.candidate_keys = self
                        .pascal_member(classes[0], &reference.label.to_lowercase(), false)
                        .ok()
                        .flatten()
                        .map(project_key)
                        .into_iter()
                        .collect();
                }
            }
        }
    }
}
fn project_definition(node: &crate::model::Node) -> bool {
    node.metadata["objc_class"].is_string()
        || node.metadata["objc_owner"].is_string()
        || node.metadata["pascal_class"].is_string()
        || node.metadata["pascal_owner"].is_string()
}
fn project_key(node: &crate::model::Node) -> String {
    format!("extended:definition:{}", node.id)
}
fn project_reference(
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

// A parse-only view of syntax missing from the released Groovy grammar. Every
// replacement has the original byte length, so Extractor reads original text.
struct GroovySource {
    source: String,
    parameters: HashMap<usize, Vec<String>>,
    spans: Vec<serde_json::Value>,
}
struct GroovyToken<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    quoted: bool,
}
fn groovy_identifier(text: &str) -> bool {
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
fn groovy_tokens(source: &str) -> Option<Vec<GroovyToken<'_>>> {
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
fn normalize_groovy(source: &str) -> Option<GroovySource> {
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

#[cfg(test)]
mod named_tests {
    #[test]
    fn named_julia_preserves_extensionless_script_path_and_locations() {
        let facts = super::parse_named(
            "scripts/run",
            "#!/usr/bin/env julia\nfunction helper()\n  1\nend\nhelper()\n",
            "hash",
            "julia",
        )
        .unwrap()
        .unwrap();
        assert!(facts.diagnostics.is_empty(), "{:?}", facts.diagnostics);
        assert_eq!(facts.path, "scripts/run");
        assert_eq!(
            facts
                .nodes
                .iter()
                .find(|n| n.label == "helper")
                .unwrap()
                .line,
            Some(2)
        );
        assert!(
            facts
                .references
                .iter()
                .any(|r| r.relation == "calls" && r.label == "helper")
        );
        assert!(
            super::parse_named("scripts/run", "", "h", "unknown")
                .unwrap()
                .is_none()
        );
    }
}
