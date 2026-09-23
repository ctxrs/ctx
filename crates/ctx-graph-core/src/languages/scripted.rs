//! Native scripted-language extraction. Parsing never evaluates code or follows imports.
use super::common::*;
use crate::model::{FileFacts, Node, Reference};
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Language, Node as Syntax};

pub fn supports(path: &str) -> bool {
    language(path).is_some()
}
fn language(path: &str) -> Option<&'static str> {
    Some(match path.rsplit('.').next()? {
        "rb" | "rake" | "gemspec" => "ruby",
        "php" | "phtml" | "php3" | "php4" | "php5" | "php7" | "phps" => "php",
        "lua" => "lua",
        "toc" => "lua-manifest",
        "luau" => "luau",
        "sh" | "bash" | "zsh" | "ksh" => "bash",
        "ps1" | "psm1" | "psd1" => "powershell",
        "ex" | "exs" => "elixir",
        _ if matches!(
            path.rsplit('/').next(),
            Some("Gemfile" | "Rakefile" | "Guardfile")
        ) =>
        {
            "ruby"
        }
        _ => return None,
    })
}
pub fn parse(path: &str, source: &str, hash: &str) -> Result<Option<FileFacts>> {
    let Some(lang) = language(path).or_else(|| shebang_language(source)) else {
        return Ok(None);
    };
    parse_named(path, source, hash, lang)
}
/// Recognize a literal interpreter name without invoking env or a shell.
pub fn shebang_language(source: &str) -> Option<&'static str> {
    let mut words = source
        .lines()
        .next()?
        .strip_prefix("#!")?
        .split_whitespace();
    let interpreter = words.next()?.rsplit('/').next()?;
    let interpreter = if interpreter == "env" {
        let mut command = words.next()?;
        if matches!(command, "-S" | "--") {
            command = words.next()?;
        }
        if command.starts_with('-') || command.contains('=') {
            return None;
        }
        command.rsplit('/').next()?
    } else {
        interpreter
    };
    Some(match interpreter {
        "bash" | "sh" | "dash" => "bash",
        "ruby" => "ruby",
        "php" => "php",
        "lua" | "luajit" => "lua",
        "luau" => "luau",
        "pwsh" | "powershell" => "powershell",
        "elixir" => "elixir",
        "node" | "nodejs" => "javascript",
        "julia" => "julia",
        "python" | "python2" | "python3" => "python",
        _ => return None,
    })
}
/// Select a native grammar while preserving the actual source path and ranges.
pub fn parse_named(
    path: &str,
    source: &str,
    hash: &str,
    language: &str,
) -> Result<Option<FileFacts>> {
    let lang = match language {
        "ruby" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "luau" => "luau",
        "bash" | "sh" => "bash",
        "powershell" | "pwsh" => "powershell",
        "elixir" => "elixir",
        "lua-manifest" => "lua-manifest",
        _ => return Ok(None),
    };
    if path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        bail!("source path must be a normalized relative POSIX path");
    }
    if lang == "lua-manifest" {
        return Ok(Some(lua_manifest(path, source, hash)));
    }
    let grammar: Language = match lang {
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "luau" => tree_sitter_luau::LANGUAGE.into(),
        "bash" => tree_sitter_bash::LANGUAGE.into(),
        "powershell" => tree_sitter_powershell::LANGUAGE.into(),
        _ => tree_sitter_elixir::LANGUAGE.into(),
    };
    let mut e = Extractor::new(path, source, hash, lang, module_path(path));
    let Some(tree) = tree(grammar, source, &mut e.facts)? else {
        return Ok(Some(e.facts));
    };
    let root = tree.root_node();
    e.root(root, format!("{lang}:module:{}", e.facts.module));
    let mut facts = match lang {
        "ruby" => Ruby::new(e).extract(root),
        "php" => Php::new(e).extract(root),
        "elixir" => Elixir::new(e).extract(root),
        _ => Script::new(e).extract(root),
    };
    if lang == "php" {
        let regions = descendants(root, "text")
            .into_iter()
            .map(|n| n.byte_range())
            .collect::<Vec<_>>();
        super::templates::append_inline_javascript(&mut facts, source, &regions)?;
    }
    Ok(Some(facts))
}
fn child<'a>(n: Syntax<'a>, kind: &str) -> Option<Syntax<'a>> {
    children(n).into_iter().find(|n| n.kind() == kind)
}
fn field<'a>(n: Syntax<'a>, name: &str) -> Option<Syntax<'a>> {
    n.child_by_field_name(name)
}
fn descendants<'a>(n: Syntax<'a>, kind: &str) -> Vec<Syntax<'a>> {
    let mut found = vec![];
    let mut pending = vec![n];
    while let Some(n) = pending.pop() {
        if n.kind() == kind {
            found.push(n);
        } else {
            pending.extend(children(n).into_iter().rev());
        }
    }
    found
}
fn literal(e: &Extractor<'_>, n: Syntax<'_>) -> Option<String> {
    let text = e.text(n);
    if matches!(
        n.kind(),
        "word" | "command_name" | "generic_token" | "path_command_name" | "path_command_name_token"
    ) && n.named_child_count() == 0
    {
        return (!text.is_empty() && !text.contains(['$', '`', '\\', '*', '?']))
            .then(|| text.into());
    }
    if matches!(
        n.kind(),
        "string" | "raw_string" | "string_literal" | "encapsed_string"
    ) {
        if children(n).iter().any(|c| {
            matches!(
                c.kind(),
                "interpolation"
                    | "string_interpolation"
                    | "simple_expansion"
                    | "expansion"
                    | "command_substitution"
                    | "variable_name"
                    | "variable"
                    | "sub_expression"
            )
        }) {
            return None;
        }
        if text.len() >= 2
            && matches!(text.as_bytes()[0], b'\'' | b'"')
            && text.as_bytes().last() == text.as_bytes().first()
            && !text.contains(['\\', '`'])
        {
            let inner = &text[1..text.len() - 1];
            if text.starts_with('"') && (inner.contains('$') || inner.contains("#{")) {
                return None;
            }
            return Some(inner.into());
        }
        if let Some(content) = field(n, "content") {
            return Some(e.text(content).into());
        }
    }
    if n.named_child_count() == 1
        && matches!(
            n.kind(),
            "argument"
                | "arguments"
                | "command_name"
                | "command_name_expr"
                | "path_command_name"
                | "array_literal_expression"
                | "unary_expression"
                | "parenthesized_expression"
        )
    {
        return literal(e, n.named_child(0)?);
    }
    None
}
fn path_modules(e: &Extractor<'_>, name: &str, relative: bool) -> Vec<String> {
    if name.starts_with('/') || name.contains(['\\', '$', '`']) {
        return vec![];
    }
    let base = if relative {
        e.facts.path.rsplit_once('/').map_or("", |(p, _)| p)
    } else {
        ""
    };
    relative_path(base, name)
        .map(|p| {
            let known = p.rsplit_once('.').is_some_and(|(_, ext)| {
                matches!(
                    ext,
                    "rb" | "php" | "phtml" | "sh" | "bash" | "ps1" | "psm1" | "psd1"
                )
            });
            vec![if known { module_path(&p) } else { p }]
        })
        .unwrap_or_default()
}
fn unknown_parameters(e: &mut Extractor<'_>, n: Syntax<'_>, scope: usize, kinds: &[&str]) {
    for kind in kinds {
        for name in descendants(n, kind) {
            e.bind(scope, e.text(name), Binding::Unknown);
        }
    }
}
fn qualified(prefix: &str, name: &str, sep: &str) -> String {
    if prefix.is_empty() {
        name.into()
    } else {
        format!("{prefix}{sep}{name}")
    }
}

// Lua, Bash and PowerShell have file-scoped exports; imported filenames are
// normalized lexically, never read. All ordinary calls use the shared scope resolver.
#[derive(Clone)]
struct BashPath {
    value: String,
    anchored: bool,
}
struct Script<'a> {
    e: Extractor<'a>,
    exported_table: Option<String>,
    sourced: HashMap<usize, Vec<String>>,
    type_calls: Vec<(usize, usize, String, String)>,
    invalidated_prefixes: Vec<String>,
    invalidated_members: HashSet<String>,
    manifest_modules: Vec<String>,
    bash_paths: HashMap<(usize, String), Option<BashPath>>,
    bash_functions: HashSet<String>,
    import_scopes: Vec<(usize, usize)>,
}
impl<'a> Script<'a> {
    fn new(e: Extractor<'a>) -> Self {
        Self {
            e,
            exported_table: None,
            sourced: HashMap::new(),
            type_calls: vec![],
            invalidated_prefixes: vec![],
            invalidated_members: HashSet::new(),
            manifest_modules: vec![],
            bash_paths: HashMap::new(),
            bash_functions: HashSet::new(),
            import_scopes: vec![],
        }
    }
    fn extract(mut self, root: Syntax<'_>) -> FileFacts {
        if self.e.language == "bash" {
            let mut pending = vec![root];
            while let Some(n) = pending.pop() {
                if n.kind() == "function_definition"
                    && let Some(name) = field(n, "name")
                {
                    self.bash_functions.insert(self.e.text(name).into());
                }
                pending.extend(children(n));
            }
        }
        if matches!(self.e.language, "lua" | "luau")
            && let Some(ret) = children(root)
                .into_iter()
                .rev()
                .find(|n| n.kind() == "return_statement")
            && let Some(values) = child(ret, "expression_list")
            && values.named_child_count() == 1
        {
            self.exported_table = values
                .named_child(0)
                .filter(|n| n.kind() == "identifier")
                .map(|n| self.e.text(n).into());
        }
        if self.e.language == "powershell" && self.e.facts.path.ends_with(".psd1") {
            self.e.facts.nodes[0].kind = "manifest".into();
            self.e.facts.nodes[0].binding_key =
                Some(format!("powershell:manifest:{}", self.e.facts.module));
            for entry in descendants(root, "hash_entry") {
                if child(entry, "key_expression").is_some_and(|k| {
                    matches!(
                        self.e
                            .text(k)
                            .trim_matches(['\'', '"'])
                            .to_ascii_lowercase()
                            .as_str(),
                        "rootmodule" | "nestedmodules"
                    )
                }) && let Some(value) = child(entry, "pipeline")
                {
                    for node in descendants(value, "string_literal") {
                        if let Some(value) = literal(&self.e, node) {
                            self.manifest_modules
                                .extend(path_modules(&self.e, &value, true));
                        }
                    }
                }
            }
        }
        self.visit(root, 0);
        for (index, scope, class, method) in &self.type_calls {
            let mut types = self.e.resolve(*scope, std::slice::from_ref(class));
            if types.is_empty() {
                let mut current = Some(*scope);
                while let Some(i) = current {
                    if self.e.scopes[i].bindings.contains_key(class) || self.e.scopes[i].uncertain {
                        break;
                    }
                    if let Some(modules) = self.sourced.get(&i) {
                        types = modules
                            .iter()
                            .rev()
                            .map(|m| format!("powershell:{m}:{class}"))
                            .collect();
                        break;
                    }
                    current = self.e.scopes[i].parent;
                }
            }
            self.e.facts.references[*index].candidate_keys = types
                .into_iter()
                .map(|key| {
                    if method == "new" {
                        key
                    } else {
                        format!("{key}::{method}")
                    }
                })
                .collect();
        }
        for (index, scope) in &self.import_scopes {
            let mut current = Some(*scope);
            while let Some(i) = current {
                if self.e.scopes[i].uncertain {
                    self.e.facts.references[*index].candidate_keys.clear();
                    self.e.facts.references[*index].reason =
                        "dynamic execution changes import lookup".into();
                    break;
                }
                current = self.e.scopes[i].parent;
            }
        }
        // Calls into explicitly sourced files are represented by import-specific
        // bindings, never by searching every function with the same label.
        let mut facts = self.e.finish();
        for node in &mut facts.nodes {
            if node.binding_key.as_ref().is_some_and(|k| {
                self.invalidated_members.contains(k)
                    || self.invalidated_prefixes.iter().any(|p| k.starts_with(p))
            }) {
                node.binding_key = None;
            }
        }
        for reference in &mut facts.references {
            if reference.candidate_keys.iter().any(|k| {
                self.invalidated_members.contains(k)
                    || self.invalidated_prefixes.iter().any(|p| k.starts_with(p))
            }) {
                reference.candidate_keys.clear();
                reference.reason = "table member is reassigned dynamically".into();
            }
        }
        facts.references.sort_by_key(|r| (r.line, r.id.clone()));
        facts
    }
    fn name(&self, name: &str) -> String {
        if self.e.language == "powershell" {
            name.to_lowercase()
        } else {
            name.into()
        }
    }
    fn key(&self, scope: usize, name: &str) -> String {
        if scope == 0 {
            format!(
                "{}:{}:{}",
                self.e.language,
                self.e.facts.module,
                self.name(name)
            )
        } else {
            self.e.local_key(scope, &self.name(name))
        }
    }
    fn import(&mut self, n: Syntax<'_>, scope: usize, name: &str, modules: &[String]) {
        let keys = modules
            .iter()
            .flat_map(|m| {
                if matches!(self.e.language, "lua" | "luau") {
                    vec![
                        format!("{}:module:{m}", self.e.language),
                        format!(
                            "{}:module:{m}",
                            if self.e.language == "lua" {
                                "luau"
                            } else {
                                "lua"
                            }
                        ),
                    ]
                } else {
                    vec![format!(
                        "{}:{}:{m}",
                        self.e.language,
                        if self.e.language == "powershell" && name.ends_with(".psd1") {
                            "manifest"
                        } else {
                            "module"
                        }
                    )]
                }
            })
            .collect();
        self.import_scopes
            .push((self.e.facts.references.len(), scope));
        self.e.reference(
            n,
            scope,
            name.into(),
            "imports",
            keys,
            "import target is dynamic or outside the indexed project",
        );
    }
    fn lua_modules(&self, name: &str) -> Vec<String> {
        if name.contains(['/', '\\']) || name.split('.').any(|p| p.is_empty()) {
            return vec![];
        }
        let p = name.replace('.', "/");
        vec![
            p.clone(),
            format!("{p}/init"),
            format!("lua/{p}"),
            format!("lua/{p}/init"),
        ]
    }
    fn lua_parts(&self, n: Syntax<'_>) -> Option<Vec<String>> {
        match n.kind() {
            "identifier" => Some(vec![self.e.text(n).into()]),
            "bracket_index_expression" => {
                let mut parts = self.lua_parts(field(n, "table")?)?;
                parts.push(literal(&self.e, field(n, "field")?)?);
                Some(parts)
            }
            "dot_index_expression" | "method_index_expression" => {
                let mut parts = self.lua_parts(field(n, "table")?)?;
                parts.push(
                    self.e
                        .text(field(n, "field").or_else(|| field(n, "method"))?)
                        .into(),
                );
                Some(parts)
            }
            _ => None,
        }
    }
    fn lua_require(&self, n: Syntax<'_>, scope: usize) -> Option<(String, Vec<String>)> {
        if n.kind() != "function_call"
            || field(n, "name")
                .is_none_or(|t| t.kind() != "identifier" || self.e.text(t) != "require")
        {
            return None;
        }
        let mut current = Some(scope);
        while let Some(s) = current {
            if self.e.scopes[s].bindings.contains_key("require") {
                return None;
            }
            current = self.e.scopes[s].parent;
        }
        let args = field(n, "arguments")?;
        if args.named_child_count() != 1 {
            return None;
        }
        let value = literal(&self.e, args.named_child(0)?)?;
        let modules = self.lua_modules(&value);
        Some((value, modules))
    }
    fn lua_assignment(&mut self, n: Syntax<'_>, scope: usize, local: bool) {
        let assignment = child(n, "assignment_statement").unwrap_or(n);
        let names = child(assignment, "variable_list")
            .map(children)
            .unwrap_or_default();
        let values = child(assignment, "expression_list")
            .map(children)
            .unwrap_or_default();
        for (i, name) in names.iter().enumerate() {
            let parts = self.lua_parts(*name).or_else(|| {
                field(*name, "table")
                    .and_then(|table| self.lua_parts(table))
                    .map(|mut p| {
                        p.truncate(1);
                        p
                    })
            });
            let Some(parts) = parts else { continue };
            let value = values.get(i).copied();
            let bare = parts.len() == 1;
            if let Some(value) = value.filter(|v| v.kind() == "function_definition") {
                self.lua_function(value, scope, *name, local);
                continue;
            }
            if bare && local {
                let binding =
                    if let Some((_, modules)) = value.and_then(|v| self.lua_require(v, scope)) {
                        Binding::Namespace {
                            prefixes: modules
                                .iter()
                                .flat_map(|m| {
                                    vec![
                                        format!("{}:{m}:", self.e.language),
                                        format!(
                                            "{}:{m}:",
                                            if self.e.language == "lua" {
                                                "luau"
                                            } else {
                                                "lua"
                                            }
                                        ),
                                    ]
                                })
                                .collect(),
                            separator: ".",
                        }
                    } else if value.is_some_and(|v| v.kind() == "table_constructor") {
                        let prefix =
                            if scope == 0 && self.exported_table.as_deref() == Some(&parts[0]) {
                                format!("{}:{}:", self.e.language, self.e.facts.module)
                            } else {
                                format!("{}.", self.e.local_key(scope, &parts[0]))
                            };
                        Binding::Namespace {
                            prefixes: vec![prefix],
                            separator: ".",
                        }
                    } else {
                        Binding::Unknown
                    };
                self.e.bind(scope, &parts[0], binding);
            } else if !bare {
                self.invalidated_members
                    .extend(self.e.resolve(scope, &parts));
            } else {
                let mut current = Some(scope);
                while let Some(i) = current {
                    if let Some(binding) = self.e.scopes[i].bindings.get(&parts[0]) {
                        if let Binding::Namespace { prefixes, .. } = binding {
                            self.invalidated_prefixes.extend(prefixes.clone());
                        }
                        break;
                    }
                    current = self.e.scopes[i].parent;
                }
                self.e.invalidate(scope, &parts[0]);
            }
        }
        for v in values {
            if v.kind() != "function_definition" {
                self.visit(v, scope);
            }
        }
    }
    fn lua_function(&mut self, n: Syntax<'_>, scope: usize, name: Syntax<'_>, local: bool) {
        let parts = self.lua_parts(name).unwrap_or_default();
        let label = parts.last().cloned().unwrap_or_else(|| "<function>".into());
        let keys = if parts.len() > 1 {
            self.e.resolve(scope, &parts)
        } else {
            vec![self.e.local_key(scope, &label)]
        };
        let key = (keys.len() == 1).then(|| keys[0].clone());
        let nested = self.e.define(
            n,
            scope,
            &label,
            if parts.len() > 1 {
                "method"
            } else {
                "function"
            },
            key,
            parts.len() == 1,
        );
        if !local && scope != 0 && parts.len() == 1 {
            // Conditional/global assignments inside functions cannot be treated as exports.
            self.e.facts.nodes.last_mut().unwrap().binding_key = None;
        }
        if let Some(p) = field(n, "parameters") {
            if self.e.language == "luau" {
                for p in children(p) {
                    // Luau names are anonymous grammar tokens; use their AST-delimited prefix.
                    let name = self.e.text(p).split(':').next().unwrap_or("").trim();
                    if !name.is_empty() {
                        self.e.bind(nested, name, Binding::Unknown);
                    }
                }
            } else {
                unknown_parameters(&mut self.e, p, nested, &["identifier"]);
            }
        }
        if name.kind() == "method_index_expression" {
            self.e.bind(nested, "self", Binding::Unknown);
        }
        if let Some(body) = field(n, "body") {
            self.visit(body, nested);
        }
    }
    fn call(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        target: Syntax<'_>,
        parts: Option<Vec<String>>,
    ) {
        // A source/import enables lookup only in that lexical scope's imported files.
        if let Some(parts) = &parts
            && parts.len() == 1
            && self.e.resolve(scope, parts).is_empty()
        {
            let mut s = Some(scope);
            while let Some(i) = s {
                if self.e.scopes[i].bindings.contains_key(&parts[0]) {
                    break;
                }
                if let Some(modules) = self.sourced.get(&i) {
                    let keys = modules
                        .iter()
                        .rev()
                        .map(|m| format!("{}:{m}:{}", self.e.language, parts[0]))
                        .collect();
                    self.e
                        .bind(scope, &parts[0], Binding::Symbol { keys, id: None });
                    break;
                }
                s = self.e.scopes[i].parent;
            }
        }
        // Lua locals become visible at their declaration, unlike hoisted JS
        // declarations. Shell top-level commands also cannot see later functions.
        let parts = if matches!(self.e.language, "lua" | "luau")
            || (scope == 0 && self.e.language == "bash")
        {
            parts.filter(|p| !self.e.resolve(scope, p).is_empty())
        } else {
            parts
        };
        self.e.call(n, scope, target, parts);
    }
    fn visit(&mut self, n: Syntax<'_>, scope: usize) {
        match self.e.language {
            "lua" | "luau" => self.lua(n, scope),
            "bash" => self.bash(n, scope),
            _ => self.powershell(n, scope),
        }
    }
    fn lua(&mut self, n: Syntax<'_>, scope: usize) {
        match n.kind() {
            "variable_declaration" => {
                self.lua_assignment(n, scope, true);
                return;
            }
            "assignment_statement" | "update_statement" => {
                self.lua_assignment(n, scope, false);
                return;
            }
            "function_declaration" => {
                if let Some(name) = field(n, "name") {
                    self.lua_function(
                        n,
                        scope,
                        name,
                        self.e.text(n).trim_start().starts_with("local "),
                    );
                }
                return;
            }
            "function_definition" => {
                let name = format!("<function@{}>", n.start_byte());
                let nested = self.e.define(n, scope, &name, "function", None, false);
                if let Some(p) = field(n, "parameters") {
                    unknown_parameters(&mut self.e, p, nested, &["identifier"]);
                }
                if let Some(body) = field(n, "body") {
                    self.visit(body, nested);
                }
                return;
            }
            "type_definition" => {
                if let Some(name) = field(n, "name") {
                    self.e.define(
                        n,
                        scope,
                        self.e.text(name),
                        "type",
                        Some(self.key(scope, self.e.text(name))),
                        false,
                    );
                }
                return;
            }
            "function_call" => {
                if let Some((name, modules)) = self.lua_require(n, scope) {
                    self.import(n, scope, &name, &modules);
                } else if let Some(name) = field(n, "name") {
                    self.call(n, scope, name, self.lua_parts(name));
                }
            }
            "for_statement" => {
                let nested = self.e.block(scope, n);
                if let Some(clause) = field(n, "clause") {
                    if let Some(name) = field(clause, "name") {
                        self.e.bind(nested, self.e.text(name), Binding::Unknown);
                    }
                    if let Some(vars) = child(clause, "variable_list") {
                        unknown_parameters(&mut self.e, vars, nested, &["identifier"]);
                    }
                    self.visit(clause, scope);
                }
                if let Some(body) = field(n, "body") {
                    self.visit(body, nested);
                }
                return;
            }
            "repeat_statement" => {
                let nested = self.e.block(scope, n);
                if let Some(body) = field(n, "body") {
                    for c in children(body) {
                        self.visit(c, nested);
                    }
                }
                if let Some(condition) = field(n, "condition") {
                    self.visit(condition, nested);
                }
                return;
            }
            "block" => {
                let nested = self.e.block(scope, n);
                for c in children(n) {
                    self.visit(c, nested);
                }
                return;
            }
            _ => {}
        }
        for c in children(n) {
            self.visit(c, scope);
        }
    }
    fn bash_path(&self, n: Syntax<'_>, scope: usize) -> Option<BashPath> {
        if let Some(value) = literal(&self.e, n) {
            return Some(BashPath {
                value,
                anchored: false,
            });
        }
        match n.kind() {
            "expansion" | "simple_expansion" => {
                let text = self.e.text(n);
                if text == "${BASH_SOURCE[0]}" {
                    return Some(BashPath {
                        value: self.e.facts.path.clone(),
                        anchored: true,
                    });
                }
                if field(n, "operator").is_some() || child(n, "subscript").is_some() {
                    return None;
                }
                let name = child(n, "variable_name")?;
                let mut current = Some(scope);
                while let Some(i) = current {
                    if self.e.scopes[i].uncertain {
                        return None;
                    }
                    if let Some(value) = self.bash_paths.get(&(i, self.e.text(name).into())) {
                        return value.clone();
                    }
                    current = self.e.scopes[i].parent;
                }
                None
            }
            "string_content" => {
                let value = self.e.text(n);
                (!value.contains(['\\', '$', '`'])).then(|| BashPath {
                    value: value.into(),
                    anchored: false,
                })
            }
            "string" | "concatenation" => {
                let mut value = String::new();
                let mut anchored = false;
                for part in children(n) {
                    let part = self.bash_path(part, scope)?;
                    if part.anchored && (!value.is_empty() || anchored) {
                        return None;
                    }
                    anchored |= part.anchored;
                    value.push_str(&part.value);
                }
                Some(BashPath { value, anchored })
            }
            "command_substitution" => {
                let statement = n.named_child(0)?;
                if n.named_child_count() != 1 {
                    return None;
                }
                if statement.kind() == "command" {
                    if children(statement).iter().any(|c| {
                        matches!(
                            c.kind(),
                            "file_redirect" | "herestring_redirect" | "variable_assignment"
                        )
                    }) {
                        return None;
                    }
                    let name = field(statement, "name").and_then(|n| literal(&self.e, n))?;
                    if name != "dirname" || self.bash_functions.contains("dirname") {
                        return None;
                    }
                    let mut cursor = statement.walk();
                    let args: Vec<_> = statement
                        .children_by_field_name("argument", &mut cursor)
                        .collect();
                    let args = if args.first().is_some_and(|a| self.e.text(*a) == "--") {
                        &args[1..]
                    } else {
                        &args[..]
                    };
                    if args.len() != 1 {
                        return None;
                    }
                    let path = self.bash_path(args[0], scope)?;
                    if !path.anchored {
                        return None;
                    }
                    let normalized = relative_path("", &path.value)?;
                    let parent = normalized
                        .rsplit_once('/')
                        .map_or(".", |(base, _)| if base.is_empty() { "." } else { base });
                    return Some(BashPath {
                        value: parent.into(),
                        anchored: true,
                    });
                }
                if statement.kind() == "list" {
                    let commands = children(statement);
                    if commands.len() != 2 || commands.iter().any(|c| c.kind() != "command") {
                        return None;
                    }
                    let (cd, pwd) = (commands[0], commands[1]);
                    if commands.iter().any(|c| {
                        children(*c).iter().any(|n| {
                            matches!(
                                n.kind(),
                                "file_redirect" | "herestring_redirect" | "variable_assignment"
                            )
                        })
                    }) {
                        return None;
                    }
                    if self.e.source[cd.end_byte()..pwd.start_byte()].trim() != "&&"
                        || field(cd, "name").is_none_or(|n| self.e.text(n) != "cd")
                        || field(pwd, "name").is_none_or(|n| self.e.text(n) != "pwd")
                        || self.bash_functions.contains("cd")
                        || self.bash_functions.contains("pwd")
                    {
                        return None;
                    }
                    let mut cursor = cd.walk();
                    let args: Vec<_> = cd.children_by_field_name("argument", &mut cursor).collect();
                    let mut cursor = pwd.walk();
                    if args.len() != 1
                        || pwd
                            .children_by_field_name("argument", &mut cursor)
                            .next()
                            .is_some()
                    {
                        return None;
                    }
                    let path = self.bash_path(args[0], scope)?;
                    if !path.anchored {
                        return None;
                    }
                    let value = relative_path("", &path.value)?;
                    return Some(BashPath {
                        value: if value.is_empty() { ".".into() } else { value },
                        anchored: true,
                    });
                }
                None
            }
            _ => None,
        }
    }
    fn bash(&mut self, n: Syntax<'_>, scope: usize) {
        if n.kind() == "variable_assignment"
            && let Some(name) = field(n, "name").filter(|n| n.kind() == "variable_name")
        {
            let mut value = field(n, "value").and_then(|v| self.bash_path(v, scope));
            let mut parent = n.parent();
            while let Some(p) = parent {
                if matches!(
                    p.kind(),
                    "if_statement"
                        | "case_statement"
                        | "while_statement"
                        | "for_statement"
                        | "list"
                ) {
                    value = None;
                    break;
                }
                if p.kind() == "function_definition" {
                    break;
                }
                parent = p.parent();
            }
            self.bash_paths.insert(
                (scope, self.e.text(name).into()),
                value.filter(|v| v.anchored),
            );
        }
        if n.kind() == "function_definition" {
            if let Some(name) = field(n, "name") {
                let label = self.e.text(name);
                if scope != 0 {
                    self.e.invalidate(0, label);
                }
                let nested = self.e.define(
                    n,
                    scope,
                    label,
                    "function",
                    Some(self.key(scope, label)),
                    true,
                );
                if let Some(body) = field(n, "body") {
                    self.visit(body, nested);
                }
            }
            return;
        }
        if matches!(n.kind(), "subshell" | "command_substitution") {
            let nested = self.e.block(scope, n);
            for c in children(n) {
                self.visit(c, nested);
            }
            return;
        }
        if n.kind() == "command"
            && let Some(target) = field(n, "name")
        {
            let name = literal(&self.e, target);
            if matches!(name.as_deref(), Some("source" | "."))
                && !self.bash_functions.contains(name.as_deref().unwrap())
            {
                let arg = n.children_by_field_name("argument", &mut n.walk()).next();
                let evaluated = arg.and_then(|a| self.bash_path(a, scope));
                let value =
                    arg.map(|a| literal(&self.e, a).unwrap_or_else(|| self.e.text(a).into()));
                let modules = evaluated
                    .map(|p| {
                        if p.anchored {
                            relative_path("", &p.value)
                                .map(|v| vec![module_path(&v)])
                                .unwrap_or_default()
                        } else {
                            path_modules(&self.e, &p.value, p.value.starts_with('.'))
                        }
                    })
                    .unwrap_or_default();
                self.import(
                    n,
                    scope,
                    value.as_deref().unwrap_or(self.e.text(n)),
                    &modules,
                );
                if modules.is_empty() {
                    self.e.scopes[scope].uncertain = true;
                }
                self.sourced.entry(scope).or_default().extend(modules);
            } else {
                if matches!(
                    name.as_deref(),
                    Some("eval" | "alias" | "unalias" | "unset")
                ) {
                    self.e.scopes[scope].uncertain = true;
                }
                let parts = name.map(|s| vec![s]);
                self.call(n, scope, target, parts);
            }
        }
        for c in children(n) {
            self.visit(c, scope);
        }
    }
    fn powershell_type(&mut self, n: Syntax<'_>, scope: usize, context: &str) {
        for name in descendants(n, "type_identifier") {
            let label = self.e.text(name);
            let normalized = self.name(label);
            let builtin = matches!(
                normalized.as_str(),
                "string"
                    | "int"
                    | "long"
                    | "bool"
                    | "byte"
                    | "void"
                    | "object"
                    | "hashtable"
                    | "array"
                    | "double"
                    | "decimal"
                    | "float"
                    | "char"
                    | "scriptblock"
                    | "datetime"
            );
            let index = self.e.facts.references.len();
            self.e
                .reference(name, scope, label.into(), "references", vec![], context);
            if !builtin {
                self.type_calls
                    .push((index, scope, normalized, "new".into()));
            }
            let owner = self.e.scopes[scope].owner.clone();
            if let Some(node) = self.e.facts.nodes.iter_mut().find(|n| n.id == owner) {
                if !node.metadata["type_contexts"].is_array() {
                    node.metadata["type_contexts"] = serde_json::json!([]);
                }
                node.metadata["type_contexts"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({"context":context,"type":label,"line":line(name)}));
            }
        }
    }
    fn powershell(&mut self, n: Syntax<'_>, scope: usize) {
        match n.kind() {
            "function_statement"
            | "class_statement"
            | "class_method_definition"
            | "enum_statement"
            | "enum_member" => {
                let name_kind = if n.kind() == "function_statement" {
                    "function_name"
                } else {
                    "simple_name"
                };
                if let Some(name) = child(n, name_kind) {
                    let label = self.e.text(name);
                    let kind = match n.kind() {
                        "class_statement" => "class",
                        "enum_statement" => "enum",
                        "enum_member" => "constant",
                        "class_method_definition" => "method",
                        _ => "function",
                    };
                    let key = if kind == "method" {
                        self.e
                            .facts
                            .nodes
                            .iter()
                            .find(|n| n.id == self.e.scopes[scope].owner)
                            .and_then(|n| n.binding_key.as_ref())
                            .map(|k| format!("{k}::{}", self.name(label)))
                            .unwrap_or_else(|| self.key(scope, label))
                    } else {
                        self.key(scope, label)
                    };
                    let nested = self
                        .e
                        .define(n, scope, label, kind, Some(key.clone()), false);
                    self.e.bind(scope, &self.name(label), Binding::symbol(key));
                    self.e.scopes[nested].class = false;
                    if kind == "class" {
                        for (i, base) in children(n)
                            .into_iter()
                            .filter(|c| c.kind() == "simple_name" && c.id() != name.id())
                            .enumerate()
                        {
                            let keys = self.e.resolve(scope, &[self.name(self.e.text(base))]);
                            let index = self.e.facts.references.len();
                            self.e.reference(
                                base,
                                nested,
                                self.e.text(base).into(),
                                if i == 0 { "inherits" } else { "implements" },
                                keys,
                                "type is unavailable or dynamically imported",
                            );
                            self.type_calls.push((
                                index,
                                scope,
                                self.name(self.e.text(base)),
                                "new".into(),
                            ));
                        }
                    }
                    for c in children(n) {
                        if c.kind() != "simple_name" && c.kind() != "function_name" {
                            self.visit(c, nested);
                        }
                    }
                }
                return;
            }
            "class_property_definition" => {
                if let Some(name) = child(n, "variable") {
                    let label = self.e.text(name);
                    let key = self
                        .e
                        .facts
                        .nodes
                        .iter()
                        .find(|n| n.id == self.e.scopes[scope].owner)
                        .and_then(|n| n.binding_key.as_ref())
                        .map(|k| format!("{k}::{}", self.name(label)));
                    let nested = self.e.define(n, scope, label, "property", key, false);
                    for c in children(n) {
                        if c.kind() == "type_literal" {
                            self.powershell_type(c, nested, "field");
                        } else if c.kind() != "variable" {
                            self.visit(c, nested);
                        }
                    }
                }
                return;
            }
            "type_literal" => {
                let parent = n.parent().map(|p| p.kind()).unwrap_or("");
                let context = if parent == "class_method_definition" {
                    "return_type"
                } else if matches!(
                    parent,
                    "class_method_parameter" | "script_parameter" | "attribute"
                ) {
                    "parameter_type"
                } else {
                    "type"
                };
                self.powershell_type(n, scope, context);
                return;
            }
            "script_block_expression" => {
                let label = format!("<scriptblock@{}>", n.start_byte());
                let nested = self.e.define(n, scope, &label, "function", None, false);
                for c in children(n) {
                    self.visit(c, nested);
                }
                return;
            }
            "command" => {
                if let Some(target) = field(n, "command_name") {
                    let name = literal(&self.e, target);
                    let normalized = name.as_deref().map(str::to_lowercase);
                    let op = child(n, "command_invokation_operator").map(|n| self.e.text(n));
                    let args = field(n, "command_elements")
                        .map(children)
                        .unwrap_or_default();
                    let shadowed_import = normalized.as_deref() == Some("import-module")
                        && !self.e.resolve(scope, &["import-module".into()]).is_empty();
                    if (normalized.as_deref() == Some("import-module") && !shadowed_import)
                        || normalized.as_deref() == Some("using")
                        || op == Some(".")
                    {
                        let arg = if op == Some(".") {
                            Some(target)
                        } else {
                            let mut selected = None;
                            let mut skip_value = false;
                            for arg in &args {
                                if arg.kind() == "command_argument_sep" {
                                    continue;
                                }
                                if arg.kind() == "command_parameter" {
                                    let option = self.e.text(*arg).to_ascii_lowercase();
                                    skip_value = matches!(
                                        option.as_str(),
                                        "-minimumversion"
                                            | "-maximumversion"
                                            | "-requiredversion"
                                            | "-prefix"
                                            | "-scope"
                                            | "-argumentlist"
                                    );
                                    continue;
                                }
                                if skip_value {
                                    skip_value = false;
                                    continue;
                                }
                                if let Some(value) = literal(&self.e, *arg) {
                                    if matches!(
                                        value.to_ascii_lowercase().as_str(),
                                        "module" | "namespace" | "assembly"
                                    ) {
                                        continue;
                                    }
                                    selected = Some(*arg);
                                    break;
                                }
                            }
                            selected
                        };
                        let value = arg.and_then(|a| literal(&self.e, a));
                        let external_namespace = normalized.as_deref() == Some("using")
                            && args.iter().any(|a| {
                                literal(&self.e, *a).is_some_and(|s| {
                                    matches!(
                                        s.to_ascii_lowercase().as_str(),
                                        "namespace" | "assembly"
                                    )
                                })
                            });
                        let modules = if external_namespace {
                            vec![]
                        } else {
                            value
                                .as_deref()
                                .map(|s| path_modules(&self.e, s, true))
                                .unwrap_or_default()
                        };
                        self.import(
                            n,
                            scope,
                            value.as_deref().unwrap_or(self.e.text(n)),
                            &modules,
                        );
                        if modules.is_empty() && !external_namespace {
                            self.e.scopes[scope].uncertain = true;
                        }
                        self.sourced.entry(scope).or_default().extend(modules);
                    } else {
                        if matches!(
                            normalized.as_deref(),
                            Some(
                                "set-alias"
                                    | "new-alias"
                                    | "invoke-expression"
                                    | "remove-item"
                                    | "set-item"
                            )
                        ) {
                            self.e.scopes[scope].uncertain = true;
                        }
                        self.call(n, scope, target, normalized.map(|s| vec![s]));
                    }
                }
            }
            "invokation_expression" => {
                let parts = children(n);
                let object = parts.first().copied();
                let member = child(n, "member_name").and_then(|m| child(m, "simple_name"));
                let label = parts
                    .iter()
                    .take_while(|c| c.kind() != "argument_list")
                    .map(|c| self.e.text(*c))
                    .collect::<Vec<_>>()
                    .join(".");
                let index = self.e.facts.references.len();
                let static_type = object
                    .filter(|o| o.kind() == "type_literal")
                    .and_then(|o| descendants(o, "type_identifier").first().copied());
                let constructor = static_type.is_some()
                    && member.is_some_and(|m| self.e.text(m).eq_ignore_ascii_case("new"));
                self.e.reference(
                    n,
                    scope,
                    label,
                    if constructor { "instantiates" } else { "calls" },
                    vec![],
                    "runtime receiver or method dispatch",
                );
                if let (Some(class), Some(method)) = (static_type, member) {
                    self.type_calls.push((
                        index,
                        scope,
                        self.name(self.e.text(class)),
                        self.name(self.e.text(method)),
                    ));
                }
            }
            "hash_entry" if self.e.facts.path.ends_with(".psd1") => {
                if let Some(key) = child(n, "key_expression") {
                    let key = self.e.text(key).trim_matches(['\'', '"']).to_lowercase();
                    if matches!(key.as_str(), "functionstoexport" | "cmdletstoexport")
                        && let Some(value) = child(n, "pipeline")
                    {
                        for name in descendants(value, "string_literal") {
                            if let Some(label) = literal(&self.e, name) {
                                let keys = if label.contains(['*', '?', '[']) {
                                    vec![]
                                } else {
                                    self.manifest_modules
                                        .iter()
                                        .map(|m| format!("powershell:{m}:{}", self.name(&label)))
                                        .collect()
                                };
                                self.e.reference(
                                    name,
                                    scope,
                                    label,
                                    "exports",
                                    keys,
                                    "manifest export is wildcard, compiled, or unavailable",
                                );
                            }
                        }
                    }
                    if matches!(
                        key.as_str(),
                        "rootmodule" | "nestedmodules" | "requiredmodules"
                    ) {
                        for s in descendants(n, "string_literal") {
                            let mut parent = s.parent();
                            let mut permitted = true;
                            while let Some(p) = parent.filter(|p| p.id() != n.id()) {
                                if p.kind() == "key_expression" {
                                    permitted = false;
                                    break;
                                }
                                if p.kind() == "hash_entry" {
                                    permitted = child(p, "key_expression").is_some_and(|k| {
                                        self.e
                                            .text(k)
                                            .trim_matches(['\'', '"'])
                                            .eq_ignore_ascii_case("ModuleName")
                                    });
                                    break;
                                }
                                parent = p.parent();
                            }
                            if permitted && let Some(value) = literal(&self.e, s) {
                                let modules = path_modules(&self.e, &value, true);
                                self.import(s, scope, &value, &modules);
                            }
                        }
                    }
                }
                return;
            }
            _ => {}
        }
        for c in children(n) {
            self.visit(c, scope);
        }
    }
}

struct RubyOwner {
    count: usize,
    class: bool,
    bases: Vec<String>,
    dynamic_base: bool,
    instance_barrier: bool,
    singleton_barrier: bool,
    visibility: HashMap<String, bool>,
}

/// A writer-side snapshot of exact Ruby bindings. No source or dependency is loaded.
pub(crate) struct RubyContext {
    unsafe_lookup: bool,
    owners: HashMap<String, RubyOwner>,
    methods: HashMap<String, (usize, bool, bool)>,
    method_owners: HashMap<String, String>,
    self_calls: HashSet<String>,
}
impl RubyContext {
    pub(crate) fn from_nodes(nodes: &[Node]) -> Self {
        let mut context = Self {
            unsafe_lookup: false,
            owners: HashMap::new(),
            methods: HashMap::new(),
            method_owners: HashMap::new(),
            self_calls: HashSet::new(),
        };
        let mut roots = HashSet::new();
        let mut files = HashSet::new();
        for node in nodes.iter().filter(|n| n.metadata["language"] == "ruby") {
            files.insert(node.file.as_str());
            if node.id == format!("ruby:{}:module", node.file) {
                roots.insert(node.file.as_str());
                context.unsafe_lookup |= node.metadata["ruby_lookup_schema"] != 2
                    || node.metadata["ruby_lookup_unsafe"] != false;
                context.self_calls.extend(
                    node.metadata["ruby_self_calls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|id| id.as_str().map(str::to_owned)),
                );
            }
            let Some(key) = &node.binding_key else {
                continue;
            };
            if Self::method_parts(key).is_some() {
                context.method_owners.insert(node.id.clone(), key.clone());
            }
            if let Some(owner) = key.strip_prefix("ruby:type:") {
                let entry = context
                    .owners
                    .entry(owner.into())
                    .or_insert_with(|| RubyOwner {
                        count: 0,
                        class: node.kind == "class",
                        bases: node.metadata["ruby_bases"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|k| k.as_str().map(str::to_owned))
                            .collect(),
                        dynamic_base: node.metadata["ruby_dynamic_base"] != false,
                        instance_barrier: node.metadata["ruby_instance_barrier"] != false,
                        singleton_barrier: node.metadata["ruby_singleton_barrier"] != false,
                        visibility: node.metadata["ruby_visibility_overrides"]
                            .as_object()
                            .into_iter()
                            .flatten()
                            .map(|(key, value)| (key.clone(), value == "public"))
                            .collect(),
                    });
                entry.count += 1;
            }
            let aliases = node.metadata["binding_aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str());
            for key in std::iter::once(key.as_str()).chain(aliases) {
                if Self::method_parts(key).is_some() {
                    let entry = context.methods.entry(key.into()).or_insert((0, true, true));
                    entry.0 += 1;
                    entry.1 &= node.metadata["ruby_direct_method"] == true;
                    entry.2 &= node.metadata["ruby_visibility"] == "public"
                        || (node.metadata["ruby_module_function"] == true
                            && key.starts_with("ruby:singleton:"));
                }
            }
        }
        context.unsafe_lookup |= roots != files;
        context
    }
    fn method_parts(key: &str) -> Option<(bool, &str, &str)> {
        if let Some(key) = key.strip_prefix("ruby:instance:") {
            let (owner, method) = key.rsplit_once('#')?;
            Some((false, owner, method))
        } else {
            let (owner, method) = key.strip_prefix("ruby:singleton:")?.rsplit_once('.')?;
            Some((true, owner, method))
        }
    }
    fn select_owner<'b>(&self, keys: impl IntoIterator<Item = &'b str>) -> Option<String> {
        for key in keys {
            if self.owners.contains_key(key) {
                return Some(key.into());
            }
            // A missing nested constant must not fall through an enclosing class
            // whose inherited constants could provide a different binding.
            let mut prefix = key;
            while let Some((parent, _)) = prefix.rsplit_once("::") {
                if self.owners.get(parent).is_some_and(|o| {
                    o.count != 1 || o.dynamic_base || !o.bases.is_empty() || o.instance_barrier
                }) {
                    return None;
                }
                prefix = parent;
            }
        }
        None
    }
    /// None preserves ordinary resolution; Some is authoritative, including barriers.
    pub(crate) fn inherited_keys(&self, reference: &Reference) -> Option<Vec<String>> {
        if reference.relation == "inherits" {
            let keys = reference
                .candidate_keys
                .iter()
                .map(|key| key.strip_prefix("ruby:type:"))
                .collect::<Option<Vec<_>>>()?;
            if self.unsafe_lookup {
                return Some(vec![]);
            }
            return Some(
                self.select_owner(keys)
                    .filter(|owner| {
                        self.owners
                            .get(owner)
                            .is_some_and(|owner| owner.count == 1 && owner.class)
                    })
                    .map(|owner| vec![format!("ruby:type:{owner}")])
                    .unwrap_or_default(),
            );
        }
        if reference.relation != "calls" {
            return None;
        }
        let is_super = reference.label == "super" && reference.candidate_keys.is_empty();
        let super_key;
        let keys = if is_super {
            super_key = vec![self.method_owners.get(&reference.source)?.clone()];
            &super_key
        } else {
            &reference.candidate_keys
        };
        if keys.is_empty() {
            return None;
        }
        let parts: Vec<_> = keys
            .iter()
            .map(|key| Self::method_parts(key))
            .collect::<Option<_>>()?;
        let (singleton, _, method) = parts[0];
        if parts
            .iter()
            .any(|(kind, _, name)| *kind != singleton || *name != method)
        {
            return Some(vec![]);
        }
        if self.unsafe_lookup {
            return Some(vec![]);
        }
        let Some(mut owner) = self.select_owner(parts.iter().map(|(_, owner, _)| *owner)) else {
            return Some(vec![]);
        };
        let mut seen = HashSet::new();
        let mut visibility = None;
        let self_call = is_super || self.self_calls.contains(&reference.id);
        for depth in 0..64 {
            if !seen.insert(owner.clone()) {
                break;
            }
            let Some(class) = self.owners.get(&owner) else {
                break;
            };
            if class.count != 1
                || if singleton {
                    class.singleton_barrier
                } else {
                    class.instance_barrier
                }
            {
                break;
            }
            let key = if singleton {
                format!("ruby:singleton:{owner}.{method}")
            } else {
                format!("ruby:instance:{owner}#{method}")
            };
            if visibility.is_none() {
                visibility = class.visibility.get(&key).copied();
            }
            if let Some((count, direct, public)) = self.methods.get(&key)
                && !(is_super && depth == 0)
            {
                return Some(
                    if *count == 1 && *direct && (self_call || visibility.unwrap_or(*public)) {
                        vec![key]
                    } else {
                        vec![]
                    },
                );
            }
            if class.dynamic_base {
                break;
            }
            let Some(base) = self.select_owner(class.bases.iter().map(String::as_str)) else {
                break;
            };
            if !self.owners.get(&base).is_some_and(|owner| owner.class) {
                break;
            }
            owner = base;
        }
        Some(vec![])
    }
}

struct Ruby<'a> {
    e: Extractor<'a>,
    owners: HashMap<usize, String>,
    singleton: HashSet<usize>,
    unsafe_lookup: bool,
    lexical: HashMap<String, Vec<String>>,
    call_labels: HashMap<String, String>,
    module_functions: HashSet<usize>,
    exported_methods: HashSet<String>,
    extended_self: HashSet<String>,
    attribute_shadows: HashSet<String>,
    instance_barriers: HashSet<String>,
    singleton_barriers: HashSet<String>,
    inheritance_unsafe: bool,
    visibility: HashMap<usize, &'static str>,
    visibility_overrides: HashMap<String, HashMap<String, String>>,
    self_calls: HashSet<String>,
}
impl<'a> Ruby<'a> {
    fn new(e: Extractor<'a>) -> Self {
        Self {
            e,
            owners: HashMap::new(),
            singleton: HashSet::new(),
            unsafe_lookup: false,
            lexical: HashMap::new(),
            call_labels: HashMap::new(),
            module_functions: HashSet::new(),
            exported_methods: HashSet::new(),
            extended_self: HashSet::new(),
            attribute_shadows: HashSet::new(),
            instance_barriers: HashSet::new(),
            singleton_barriers: HashSet::new(),
            inheritance_unsafe: false,
            visibility: HashMap::new(),
            visibility_overrides: HashMap::new(),
            self_calls: HashSet::new(),
        }
    }
    fn extract(mut self, root: Syntax<'_>) -> FileFacts {
        let mut pending = vec![root];
        while let Some(n) = pending.pop() {
            if matches!(n.kind(), "method" | "singleton_method")
                && let Some(name) = field(n, "name")
                && matches!(
                    self.e.text(name),
                    "attr_reader"
                        | "attr_writer"
                        | "attr_accessor"
                        | "private"
                        | "protected"
                        | "public"
                        | "private_class_method"
                        | "public_class_method"
                )
            {
                self.attribute_shadows.insert(self.e.text(name).into());
                self.inheritance_unsafe = true;
            }
            pending.extend(children(n));
        }
        self.visit(root, 0, "", None);
        let unsafe_lookup = self.unsafe_lookup;
        let mut facts = self.e.finish();
        facts.nodes[0].metadata["ruby_lookup_schema"] = 2.into();
        let mut self_calls: Vec<_> = self.self_calls.into_iter().collect();
        self_calls.sort();
        facts.nodes[0].metadata["ruby_self_calls"] = serde_json::json!(self_calls);
        facts.nodes[0].metadata["ruby_lookup_unsafe"] =
            (unsafe_lookup || self.inheritance_unsafe).into();
        let mut methods: HashMap<String, Vec<usize>> = HashMap::new();
        let mut owner_counts: HashMap<String, usize> = HashMap::new();
        for (index, node) in facts.nodes.iter().enumerate() {
            if let Some(owner) = node
                .binding_key
                .as_deref()
                .and_then(|k| k.strip_prefix("ruby:type:"))
            {
                *owner_counts.entry(owner.into()).or_default() += 1;
            }
            if let Some(key) = &node.binding_key
                && RubyContext::method_parts(key).is_some()
            {
                methods.entry(key.clone()).or_default().push(index);
            }
        }
        for (key, indices) in methods {
            let (_, owner, _) = RubyContext::method_parts(&key).unwrap();
            let single_owner = owner_counts.get(owner) == Some(&1);
            if single_owner
                && indices.len() > 1
                && indices
                    .iter()
                    .any(|i| facts.nodes[*i].metadata["ruby_generated_by"].is_string())
                && indices
                    .iter()
                    .all(|i| facts.nodes[*i].metadata["ruby_direct_method"] == true)
            {
                let winner = *indices
                    .iter()
                    .max_by_key(|i| facts.nodes[**i].metadata["start_byte"].as_u64())
                    .unwrap();
                let winner_id = facts.nodes[winner].id.clone();
                for index in indices.into_iter().filter(|i| *i != winner) {
                    facts.nodes[index].binding_key = None;
                    facts.nodes[index].metadata["ruby_superseded_by"] = winner_id.clone().into();
                }
            }
        }
        for node in &mut facts.nodes {
            if let Some(owner) = node
                .binding_key
                .as_deref()
                .and_then(|k| k.strip_prefix("ruby:type:"))
            {
                node.metadata["ruby_instance_barrier"] =
                    self.instance_barriers.contains(owner).into();
                node.metadata["ruby_singleton_barrier"] =
                    self.singleton_barriers.contains(owner).into();
                node.metadata["ruby_visibility_overrides"] = serde_json::json!(
                    self.visibility_overrides
                        .get(owner)
                        .cloned()
                        .unwrap_or_default()
                );
            }
            if let Some(key) = &node.binding_key
                && let Some((owner, method)) = key
                    .strip_prefix("ruby:instance:")
                    .and_then(|k| k.split_once('#'))
                && (self.exported_methods.contains(key) || self.extended_self.contains(owner))
            {
                node.metadata["binding_aliases"] =
                    serde_json::json!([format!("ruby:singleton:{owner}.{method}")]);
                if self.exported_methods.contains(key) {
                    node.metadata["ruby_visibility"] = "private".into();
                    node.metadata["ruby_module_function"] = true.into();
                }
            }
        }
        for r in &mut facts.references {
            if let Some(label) = self.call_labels.get(&r.id) {
                r.label.clone_from(label);
            }
        }
        if unsafe_lookup {
            for node in &mut facts.nodes {
                if matches!(
                    node.kind.as_str(),
                    "method" | "class" | "module" | "function"
                ) && node.id != format!("ruby:{}:module", facts.path)
                {
                    node.binding_key = None;
                    if let Some(metadata) = node.metadata.as_object_mut() {
                        metadata.remove("binding_aliases");
                    }
                }
            }
            for r in &mut facts.references {
                if matches!(
                    r.relation.as_str(),
                    "calls" | "inherits" | "instantiates" | "mixes_in"
                ) {
                    r.candidate_keys.clear();
                    r.reason = "Ruby constant rebinding or metaprogramming changes lookup".into();
                }
            }
        }
        facts
    }
    fn constant(&self, n: Syntax<'_>, namespace: &str) -> Option<String> {
        match n.kind() {
            "constant" => Some(qualified(namespace, self.e.text(n), "::")),
            "scope_resolution" => {
                let name = field(n, "name")?;
                if let Some(scope) = field(n, "scope") {
                    Some(qualified(
                        &self.constant(scope, namespace)?,
                        self.e.text(name),
                        "::",
                    ))
                } else {
                    Some(self.e.text(name).into())
                }
            }
            _ => None,
        }
    }
    fn constants(&self, n: Syntax<'_>, namespace: &str) -> Vec<String> {
        if n.kind() != "constant" {
            return self.constant(n, namespace).into_iter().collect();
        }
        let mut candidates: Vec<_> = self
            .lexical
            .get(namespace)
            .into_iter()
            .flatten()
            .map(|p| qualified(p, self.e.text(n), "::"))
            .collect();
        candidates.push(self.e.text(n).into());
        candidates.dedup();
        candidates
    }
    fn is_singleton(&self, scope: usize) -> bool {
        let mut current = Some(scope);
        while let Some(i) = current {
            if self.singleton.contains(&i) {
                return true;
            }
            current = self.e.scopes[i].parent;
        }
        false
    }
    fn direct_declaration(n: Syntax<'_>) -> bool {
        n.parent().is_some_and(|p| {
            p.kind() == "program"
                || (p.kind() == "body_statement"
                    && p.parent().is_some_and(|owner| {
                        matches!(owner.kind(), "class" | "module")
                            && field(owner, "body").is_some_and(|body| body.id() == p.id())
                    }))
        })
    }
    fn self_call(&mut self, n: Syntax<'_>, scope: usize) {
        self.self_calls.insert(format!(
            "call:{}:{}-{}",
            self.e.scopes[scope].owner,
            n.start_byte(),
            n.end_byte()
        ));
    }
    fn set_visibility(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        owner: Option<&str>,
        name: &str,
        args: &[Syntax<'_>],
    ) {
        let Some(owner) = owner else { return };
        let singleton = name.ends_with("_class_method");
        let visibility = match name {
            "public" | "public_class_method" => "public",
            "protected" => "protected",
            _ => "private",
        };
        let names: Option<Vec<_>> = args
            .iter()
            .map(|arg| {
                if arg.kind() == "simple_symbol" {
                    self.e.text(*arg).strip_prefix(':').map(str::to_owned)
                } else {
                    literal(&self.e, *arg)
                }
            })
            .collect();
        if !Self::direct_declaration(n)
            || self.e.scopes[scope].function
            || names.is_none()
            || field(n, "block").is_some()
            || (singleton && args.is_empty())
        {
            if singleton {
                self.singleton_barriers.insert(owner.into());
            } else {
                self.instance_barriers.insert(owner.into());
            }
            return;
        }
        if args.is_empty() {
            self.visibility.insert(scope, visibility);
        } else {
            for method in names.unwrap() {
                let key = if singleton {
                    format!("ruby:singleton:{owner}.{method}")
                } else {
                    format!("ruby:instance:{owner}#{method}")
                };
                self.visibility_overrides
                    .entry(owner.into())
                    .or_default()
                    .insert(key, visibility.into());
            }
        }
    }
    fn attribute_name(&self, n: Syntax<'_>) -> Option<String> {
        let name = if n.kind() == "simple_symbol" {
            self.e.text(n).strip_prefix(':')?.to_owned()
        } else if n.kind() == "delimited_symbol" {
            if !descendants(n, "interpolation").is_empty() {
                return None;
            }
            let text = self.e.text(n).strip_prefix(':')?;
            let quote = text.chars().next()?;
            if !matches!(quote, '\'' | '"') || !text.ends_with(quote) || text.contains('\\') {
                return None;
            }
            text[1..text.len() - 1].to_owned()
        } else {
            literal(&self.e, n)?
        };
        let mut chars = name.chars();
        (chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
            && chars.all(|c| c == '_' || c.is_alphanumeric()))
        .then_some(name)
    }
    fn attributes(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        owner: Option<&str>,
        name: &str,
        args: &[Syntax<'_>],
    ) {
        let Some(owner) = owner else { return };
        let names: Option<Vec<_>> = args.iter().map(|arg| self.attribute_name(*arg)).collect();
        if !Self::direct_declaration(n)
            || self.e.scopes[scope].function
            || self.attribute_shadows.contains(name)
            || names.is_none()
            || field(n, "block").is_some()
        {
            self.instance_barriers.insert(owner.into());
            return;
        }
        for (arg, attribute) in args.iter().zip(names.unwrap()) {
            let mut methods = vec![];
            if name != "attr_writer" {
                methods.push(attribute.clone());
            }
            if name != "attr_reader" {
                methods.push(format!("{attribute}="));
            }
            for method in methods {
                let key = format!("ruby:instance:{owner}#{method}");
                if let Some(overrides) = self.visibility_overrides.get_mut(owner) {
                    overrides.remove(&key);
                }
                self.e
                    .define(*arg, scope, &method, "method", Some(key), false);
                let node = self.e.facts.nodes.last_mut().unwrap();
                node.metadata["ruby_direct_method"] = true.into();
                node.metadata["ruby_visibility"] = self
                    .visibility
                    .get(&scope)
                    .copied()
                    .unwrap_or("public")
                    .into();
                node.metadata["ruby_generated_by"] = name.into();
                node.metadata["ruby_attribute"] = attribute.clone().into();
            }
        }
    }
    fn method(&mut self, n: Syntax<'_>, scope: usize, namespace: &str, owner: Option<&str>) {
        let Some(name) = field(n, "name") else { return };
        let singleton = n.kind() == "singleton_method";
        let receiver = field(n, "object");
        let method_owner = if singleton {
            receiver.and_then(|r| {
                if r.kind() == "self" {
                    owner.map(str::to_owned)
                } else {
                    self.constant(r, namespace)
                }
            })
        } else {
            owner.map(str::to_owned)
        };
        let label = self.e.text(name);
        let key = method_owner
            .as_ref()
            .map(|o| {
                if singleton {
                    format!("ruby:singleton:{o}.{label}")
                } else {
                    format!("ruby:instance:{o}#{label}")
                }
            })
            .or_else(|| (!singleton).then(|| self.e.local_key(0, label)));
        if let Some(owner) = &method_owner
            && let Some(overrides) = self.visibility_overrides.get_mut(owner)
            && let Some(key) = &key
        {
            overrides.remove(key);
        }
        let nested = self.e.define(
            n,
            scope,
            label,
            if owner.is_some() || singleton {
                "method"
            } else {
                "function"
            },
            key,
            !singleton && owner.is_none(),
        );
        self.e.facts.nodes.last_mut().unwrap().metadata["ruby_direct_method"] =
            Self::direct_declaration(n).into();
        self.e.facts.nodes.last_mut().unwrap().metadata["ruby_visibility"] = if singleton {
            "public"
        } else {
            self.visibility.get(&scope).copied().unwrap_or("public")
        }
        .into();
        if singleton && receiver.is_some_and(|r| r.kind() != "self") {
            self.inheritance_unsafe = true;
        }
        if !singleton
            && self.module_functions.contains(&scope)
            && let Some(owner) = owner
        {
            self.exported_methods
                .insert(format!("ruby:instance:{owner}#{label}"));
        }
        // A def starts a fresh local-variable environment, even inside another def.
        self.e.scopes[nested].parent = None;
        self.e.scopes[nested].fallback = Some(self.e.local_key(0, ""));
        if let Some(o) = &method_owner {
            self.owners.insert(nested, o.clone());
            self.e.scopes[nested].fallback = Some(if singleton {
                format!("ruby:singleton:{o}.")
            } else {
                format!("ruby:instance:{o}#")
            });
        }
        if singleton {
            self.singleton.insert(nested);
        }
        if let Some(p) = field(n, "parameters") {
            unknown_parameters(&mut self.e, p, nested, &["identifier"]);
        }
        if let Some(body) = field(n, "body") {
            self.visit(body, nested, namespace, method_owner.as_deref());
        }
    }
    fn visit(&mut self, n: Syntax<'_>, scope: usize, namespace: &str, owner: Option<&str>) {
        match n.kind() {
            "class" | "module" => {
                let Some(name) = field(n, "name") else { return };
                let Some(full) = self.constant(name, namespace) else {
                    return;
                };
                let nested = self.e.define(
                    n,
                    scope,
                    self.e.text(name),
                    n.kind(),
                    Some(format!("ruby:type:{full}")),
                    false,
                );
                let base = field(n, "superclass").and_then(|n| n.named_child(0));
                let bases = base
                    .map(|base| self.constants(base, namespace))
                    .unwrap_or_default();
                let metadata = &mut self.e.facts.nodes.last_mut().unwrap().metadata;
                metadata["ruby_bases"] = serde_json::json!(bases);
                metadata["ruby_dynamic_base"] = (base.is_some() && bases.is_empty()).into();
                if !Self::direct_declaration(n) {
                    self.instance_barriers.insert(full.clone());
                    self.singleton_barriers.insert(full.clone());
                }
                self.owners.insert(nested, full.clone());
                self.singleton.insert(nested);
                self.e.scopes[nested].parent = None;
                self.e.scopes[nested].class = false;
                self.e.scopes[nested].fallback = Some(format!("ruby:singleton:{full}."));
                let mut lexical = vec![full.clone()];
                lexical.extend(self.lexical.get(namespace).cloned().unwrap_or_default());
                self.lexical.insert(full.clone(), lexical);
                if let Some(base) = base {
                    let keys = bases
                        .into_iter()
                        .map(|s| format!("ruby:type:{s}"))
                        .collect();
                    self.e.reference(
                        base,
                        nested,
                        self.e.text(base).into(),
                        "inherits",
                        keys,
                        "base constant is unavailable or dynamic",
                    );
                }
                if let Some(body) = field(n, "body") {
                    self.visit(body, nested, &full, Some(&full));
                }
                return;
            }
            "method" | "singleton_method" => {
                self.method(n, scope, namespace, owner);
                return;
            }
            "singleton_class" => {
                self.unsafe_lookup = true;
            }
            "lambda" | "block" | "do_block" => {
                let label = format!("<block@{}>", n.start_byte());
                let nested = self.e.define(n, scope, &label, "function", None, false);
                if let Some(p) = field(n, "parameters") {
                    unknown_parameters(&mut self.e, p, nested, &["identifier"]);
                }
                if let Some(body) = field(n, "body") {
                    self.visit(body, nested, namespace, owner);
                }
                return;
            }
            "assignment" | "operator_assignment" => {
                if let Some(left) = field(n, "left") {
                    if matches!(
                        left.kind(),
                        "left_assignment_list" | "destructured_left_assignment"
                    ) && !descendants(left, "constant").is_empty()
                    {
                        self.inheritance_unsafe = true;
                    }
                    if matches!(left.kind(), "constant" | "scope_resolution") {
                        let factory =
                            field(n, "right")
                                .filter(|r| r.kind() == "call")
                                .filter(|r| {
                                    let method = field(*r, "method").map(|m| self.e.text(m));
                                    let recv = field(*r, "receiver").map(|m| self.e.text(m));
                                    matches!(
                                        (recv, method),
                                        (Some("Class" | "Struct"), Some("new"))
                                            | (Some("Data"), Some("define"))
                                    )
                                });
                        if let Some(factory) = factory
                            && let Some(full) = self.constant(left, namespace)
                        {
                            let nested = self.e.define(
                                n,
                                scope,
                                self.e.text(left),
                                "class",
                                Some(format!("ruby:type:{full}")),
                                false,
                            );
                            if field(factory, "receiver").is_some_and(|r| self.e.text(r) == "Class")
                                && let Some(base) =
                                    field(factory, "arguments").and_then(|a| a.named_child(0))
                            {
                                let keys = self
                                    .constant(base, namespace)
                                    .map(|b| vec![format!("ruby:type:{b}")])
                                    .unwrap_or_default();
                                self.e.reference(
                                    base,
                                    nested,
                                    self.e.text(base).into(),
                                    "inherits",
                                    keys,
                                    "dynamic superclass",
                                );
                            }
                            if let Some(body) =
                                field(factory, "block").and_then(|b| field(b, "body"))
                            {
                                self.visit(body, nested, &full, Some(&full));
                            }
                            return;
                        }
                        self.unsafe_lookup = true;
                    } else if left.kind() == "identifier" {
                        let binding = field(n, "right")
                            .filter(|r| r.kind() == "call")
                            .filter(|r| {
                                field(*r, "method").is_some_and(|m| self.e.text(m) == "new")
                            })
                            .and_then(|r| field(r, "receiver"))
                            .map(|r| self.constants(r, namespace))
                            .filter(|names| !names.is_empty())
                            .map(|names| Binding::Namespace {
                                prefixes: names
                                    .into_iter()
                                    .map(|s| format!("ruby:instance:{s}#"))
                                    .collect(),
                                separator: ".",
                            })
                            .unwrap_or(Binding::Unknown);
                        self.e.bind(scope, self.e.text(left), binding);
                    }
                }
            }
            "call" => {
                let Some(method) = field(n, "method") else {
                    for c in children(n) {
                        self.visit(c, scope, namespace, owner);
                    }
                    return;
                };
                let setter = n.parent().is_some_and(|p| {
                    p.kind() == "assignment" && field(p, "left").is_some_and(|l| l.id() == n.id())
                });
                let method_name = if setter {
                    format!("{}=", self.e.text(method))
                } else {
                    self.e.text(method).into()
                };
                let name = method_name.as_str();
                let receiver = field(n, "receiver");
                let args = field(n, "arguments").map(children).unwrap_or_default();
                if matches!(
                    name,
                    "include"
                        | "extend"
                        | "prepend"
                        | "private"
                        | "protected"
                        | "public"
                        | "private_class_method"
                        | "public_class_method"
                ) && receiver.is_some_and(|r| r.kind() != "self")
                {
                    self.inheritance_unsafe = true;
                }
                if matches!(
                    name,
                    "private"
                        | "protected"
                        | "public"
                        | "private_class_method"
                        | "public_class_method"
                ) && (receiver.is_none() || receiver.is_some_and(|r| r.kind() == "self"))
                {
                    self.set_visibility(n, scope, owner, name, &args);
                } else if name == "super" && receiver.is_none() {
                    self.e.reference(
                        n,
                        scope,
                        "super".into(),
                        "calls",
                        vec![],
                        "superclass method is unavailable or dynamic",
                    );
                } else if matches!(name, "attr_reader" | "attr_writer" | "attr_accessor") {
                    if receiver.is_none() || receiver.is_some_and(|r| r.kind() == "self") {
                        self.attributes(n, scope, owner, name, &args);
                    } else {
                        self.inheritance_unsafe = true;
                    }
                    self.e.call(n, scope, method, Some(vec![name.into()]));
                } else if receiver.is_none()
                    && matches!(name, "require" | "require_relative" | "load")
                {
                    let value = args.first().and_then(|a| literal(&self.e, *a));
                    let modules = value
                        .as_deref()
                        .map(|v| path_modules(&self.e, v, name == "require_relative"))
                        .unwrap_or_default();
                    self.e.reference(
                        n,
                        scope,
                        value.unwrap_or_else(|| self.e.text(n).into()),
                        "imports",
                        modules
                            .into_iter()
                            .map(|m| format!("ruby:module:{m}"))
                            .collect(),
                        "dynamic require or unavailable load path",
                    );
                } else if receiver.is_none()
                    && name == "module_function"
                    && !self.e.scopes[scope].function
                {
                    if args.is_empty() {
                        self.module_functions.insert(scope);
                    } else if let Some(owner) = owner {
                        for arg in &args {
                            if arg.kind() == "simple_symbol" {
                                self.exported_methods.insert(format!(
                                    "ruby:instance:{owner}#{}",
                                    self.e.text(*arg).trim_start_matches(':')
                                ));
                            }
                        }
                    }
                } else if (receiver.is_none() || receiver.is_some_and(|r| r.kind() == "self"))
                    && matches!(name, "include" | "extend" | "prepend")
                {
                    if let Some(owner) = owner {
                        if name == "extend" {
                            if !args.iter().all(|a| a.kind() == "self") {
                                self.singleton_barriers.insert(owner.into());
                            }
                        } else {
                            self.instance_barriers.insert(owner.into());
                        }
                    }
                    for arg in &args {
                        if name == "extend" && arg.kind() == "self" {
                            if let Some(owner) = owner {
                                self.extended_self.insert(owner.into());
                            }
                            continue;
                        }
                        let keys = self
                            .constants(*arg, namespace)
                            .into_iter()
                            .map(|s| format!("ruby:type:{s}"))
                            .collect();
                        self.e.reference(
                            *arg,
                            scope,
                            self.e.text(*arg).into(),
                            "mixes_in",
                            keys,
                            "mixin is dynamic or unavailable",
                        );
                    }
                } else {
                    if matches!(
                        name,
                        "eval"
                            | "class_eval"
                            | "module_eval"
                            | "class_exec"
                            | "module_exec"
                            | "refine"
                            | "using"
                            | "define_method"
                            | "define_singleton_method"
                            | "remove_method"
                            | "undef_method"
                            | "const_set"
                            | "autoload"
                            | "alias_method"
                    ) {
                        self.unsafe_lookup = true;
                    }
                    if let Some(receiver) = receiver {
                        let label = format!("{}.{}", self.e.text(receiver), name);
                        let classes = self.constants(receiver, namespace);
                        if !classes.is_empty() {
                            let relation = if name == "new" {
                                "instantiates"
                            } else {
                                "calls"
                            };
                            let keys = classes
                                .into_iter()
                                .map(|class| {
                                    if name == "new" {
                                        format!("ruby:type:{class}")
                                    } else {
                                        format!("ruby:singleton:{class}.{name}")
                                    }
                                })
                                .collect();
                            self.e.reference(
                                n,
                                scope,
                                label,
                                relation,
                                keys,
                                "constant receiver target is unavailable or ambiguous",
                            );
                        } else if receiver.kind() == "self" && owner.is_some() {
                            let singleton = self.is_singleton(scope);
                            let o = owner.unwrap();
                            let key = if singleton {
                                format!("ruby:singleton:{o}.{name}")
                            } else {
                                format!("ruby:instance:{o}#{name}")
                            };
                            self.e.reference(
                                n,
                                scope,
                                label,
                                "calls",
                                vec![key],
                                "receiver method is unavailable or ambiguous",
                            );
                            self.self_calls
                                .insert(self.e.facts.references.last().unwrap().id.clone());
                        } else {
                            let parts = (receiver.kind() == "identifier")
                                .then(|| vec![self.e.text(receiver).into(), name.into()]);
                            self.call_labels.insert(
                                format!(
                                    "call:{}:{}-{}",
                                    self.e.scopes[scope].owner,
                                    n.start_byte(),
                                    n.end_byte()
                                ),
                                label,
                            );
                            self.e.call(n, scope, method, parts);
                        }
                    } else {
                        self.self_call(n, scope);
                        self.e.call(n, scope, method, Some(vec![name.into()]));
                    }
                }
                for arg in args {
                    self.visit(arg, scope, namespace, owner);
                }
                if let Some(recv) = receiver.filter(|r| r.kind() == "call") {
                    self.visit(recv, scope, namespace, owner);
                }
                if let Some(block) = field(n, "block") {
                    self.visit(block, scope, namespace, owner);
                }
                return;
            }
            "identifier" => {
                let name = self.e.text(n);
                if matches!(name, "private" | "protected" | "public") && owner.is_some() {
                    self.set_visibility(n, scope, owner, name, &[]);
                    return;
                }
                if name == "module_function" && owner.is_some() && !self.e.scopes[scope].function {
                    self.module_functions.insert(scope);
                    return;
                }
                if n.parent().is_some_and(|p| {
                    field(p, "left").is_some_and(|l| l.id() == n.id())
                        || field(p, "name").is_some_and(|l| l.id() == n.id())
                }) {
                    return;
                }
                let mut current = Some(scope);
                while let Some(i) = current {
                    if matches!(
                        self.e.scopes[i].bindings.get(name),
                        Some(Binding::Unknown | Binding::Namespace { .. })
                    ) {
                        return;
                    }
                    current = self.e.scopes[i].parent;
                }
                self.self_call(n, scope);
                self.e.call(n, scope, n, Some(vec![name.into()]));
                return;
            }
            "super" | "yield" => {
                self.e.reference(
                    n,
                    scope,
                    self.e.text(n).into(),
                    "calls",
                    vec![],
                    "inherited dispatch or runtime block",
                );
            }
            "alias" | "undef" => {
                self.unsafe_lookup = true;
            }
            _ => {}
        }
        for c in children(n) {
            self.visit(c, scope, namespace, owner);
        }
    }
}

#[derive(Clone, Default)]
struct PhpContext {
    namespace: String,
    class: Option<String>,
    aliases: HashMap<String, String>,
    functions: HashMap<String, String>,
}
struct Php<'a> {
    e: Extractor<'a>,
    semantic_sources: Vec<(usize, String, &'static str)>,
    config_uses: Vec<(usize, String)>,
}
impl<'a> Php<'a> {
    fn new(e: Extractor<'a>) -> Self {
        Self {
            e,
            semantic_sources: vec![],
            config_uses: vec![],
        }
    }
    fn extract(mut self, root: Syntax<'_>) -> FileFacts {
        self.sequence(root, 0, &PhpContext::default());
        for (index, source_key, fallback_relation) in &self.semantic_sources {
            let sources: Vec<_> = self
                .e
                .facts
                .nodes
                .iter()
                .filter(|n| n.binding_key.as_ref() == Some(source_key))
                .collect();
            if sources.len() == 1 {
                self.e.facts.references[*index].source = sources[0].id.clone();
            } else {
                self.e.facts.references[*index].relation = (*fallback_relation).into();
            }
        }
        for (index, namespace) in &self.config_uses {
            let local = format!(
                "php:function:{}",
                qualified(namespace, "config", "\\").to_ascii_lowercase()
            );
            if self.e.facts.nodes.iter().any(|n| {
                n.binding_key
                    .as_deref()
                    .is_some_and(|k| k == local || k == "php:function:config")
            }) {
                self.e.facts.references[*index].candidate_keys.clear();
                self.e.facts.references[*index].reason =
                    "config helper is shadowed by a project function".into();
            }
        }
        self.e.finish()
    }
    fn name(&self, n: Syntax<'_>, ctx: &PhpContext, function: bool) -> Option<String> {
        if !matches!(
            n.kind(),
            "name" | "qualified_name" | "namespace_name" | "relative_name" | "relative_scope"
        ) {
            return None;
        }
        let text = self.e.text(n);
        if text.eq_ignore_ascii_case("self") {
            return ctx.class.clone();
        }
        if matches!(text.to_lowercase().as_str(), "static" | "parent") {
            return None;
        }
        if text.starts_with('\\') {
            return Some(text.trim_start_matches('\\').to_lowercase());
        }
        if let Some(rest) = text.strip_prefix("namespace\\") {
            return Some(qualified(&ctx.namespace, rest, "\\").to_lowercase());
        }
        let (first, rest) = text.split_once('\\').unwrap_or((text, ""));
        let aliases = if function && rest.is_empty() {
            &ctx.functions
        } else {
            &ctx.aliases
        };
        if let Some(base) = aliases.get(&first.to_lowercase()) {
            return Some(
                qualified(base, rest, if rest.is_empty() { "" } else { "\\" }).to_lowercase(),
            );
        }
        Some(qualified(&ctx.namespace, text, "\\").to_lowercase())
    }
    fn sequence(&mut self, n: Syntax<'_>, scope: usize, context: &PhpContext) {
        let mut ctx = context.clone();
        let mut owner = scope;
        for c in children(n) {
            if c.kind() == "namespace_definition" {
                ctx = PhpContext {
                    namespace: field(c, "name")
                        .map(|n| self.e.text(n).into())
                        .unwrap_or_default(),
                    ..Default::default()
                };
                let label = if ctx.namespace.is_empty() {
                    "<global>"
                } else {
                    &ctx.namespace
                };
                owner = self.e.define(
                    c,
                    scope,
                    label,
                    "namespace",
                    Some(format!("php:namespace:{}", ctx.namespace.to_lowercase())),
                    false,
                );
                if let Some(body) = field(c, "body") {
                    self.sequence(body, owner, &ctx);
                    ctx = context.clone();
                    owner = scope;
                }
            } else if c.kind() == "namespace_use_declaration" {
                self.use_import(c, owner, &mut ctx);
            } else {
                self.visit(c, owner, &ctx);
            }
        }
    }
    fn use_import(&mut self, n: Syntax<'_>, scope: usize, ctx: &mut PhpContext) {
        let prefix = child(n, "namespace_name")
            .map(|p| self.e.text(p).trim_end_matches('\\'))
            .unwrap_or("");
        for clause in descendants(n, "namespace_use_clause") {
            let Some(target) = children(clause)
                .into_iter()
                .find(|c| Some(c.id()) != field(clause, "alias").map(|a| a.id()))
            else {
                continue;
            };
            let full = qualified(prefix, self.e.text(target).trim_start_matches('\\'), "\\")
                .to_lowercase();
            let alias = field(clause, "alias")
                .map(|a| self.e.text(a))
                .unwrap_or_else(|| self.e.text(target).rsplit('\\').next().unwrap_or(""))
                .to_lowercase();
            let kind = field(clause, "type")
                .or_else(|| field(n, "type"))
                .map(|n| self.e.text(n));
            let function = kind == Some("function");
            let key = format!(
                "php:{}:{full}",
                if function {
                    "function"
                } else if kind == Some("const") {
                    "constant"
                } else {
                    "type"
                }
            );
            self.e.reference(
                clause,
                scope,
                full.clone(),
                "imports",
                vec![key],
                "import target is unavailable or ambiguous",
            );
            if function {
                ctx.functions.insert(alias, full);
            } else if kind != Some("const") {
                ctx.aliases.insert(alias, full);
            }
        }
    }
    fn type_refs(&mut self, n: Syntax<'_>, scope: usize, ctx: &PhpContext, context: &str) {
        let mut types = descendants(n, "named_type");
        types.extend(descendants(n, "primitive_type"));
        for ty in types {
            if let Some(name) = if ty.kind() == "primitive_type" {
                Some(ty)
            } else {
                ty.named_child(0)
            } {
                let keys = self
                    .name(name, ctx, false)
                    .map(|s| vec![format!("php:type:{s}")])
                    .unwrap_or_default();
                self.e.reference(
                    name,
                    scope,
                    self.e.text(name).into(),
                    "references",
                    keys.clone(),
                    context,
                );
                let owner = self.e.scopes[scope].owner.clone();
                if let Some(node) = self.e.facts.nodes.iter_mut().find(|n| n.id == owner) {
                    if !node.metadata["type_contexts"].is_array() {
                        node.metadata["type_contexts"] = serde_json::json!([]);
                    }
                    node.metadata["type_contexts"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({"context":context,"keys":keys,"line":line(name)}));
                }
            }
        }
    }
    fn class_literal(&self, n: Syntax<'_>, ctx: &PhpContext) -> Option<String> {
        let n = if n.kind() == "argument" {
            n.named_child(0)?
        } else {
            n
        };
        if n.kind() != "class_constant_access_expression" {
            return None;
        }
        let parts = children(n);
        if !parts
            .last()
            .is_some_and(|c| self.e.text(*c).eq_ignore_ascii_case("class"))
        {
            return None;
        }
        self.name(*parts.first()?, ctx, false)
    }
    fn registration(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        source: &str,
        target: &str,
        relation: &'static str,
    ) {
        let source_key = format!("php:type:{source}");
        self.e.reference(
            n,
            scope,
            source.into(),
            if relation == "bound_to" {
                "binds"
            } else {
                "listens_for"
            },
            vec![source_key.clone()],
            "registered contract/event is external or ambiguous",
        );
        let index = self.e.facts.references.len();
        self.e.reference(
            n,
            scope,
            target.into(),
            relation,
            vec![format!("php:type:{target}")],
            "registered implementation/listener is external or ambiguous",
        );
        self.semantic_sources.push((
            index,
            source_key,
            if relation == "bound_to" {
                "registers_binding"
            } else {
                "registers_listener"
            },
        ));
    }
    fn visit(&mut self, n: Syntax<'_>, scope: usize, ctx: &PhpContext) {
        match n.kind() {
            "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration"
            | "anonymous_class" => {
                let label = field(n, "name")
                    .map(|n| self.e.text(n).to_owned())
                    .unwrap_or_else(|| format!("<class@{}>", n.start_byte()));
                let full = qualified(&ctx.namespace, &label, "\\").to_lowercase();
                let kind = match n.kind() {
                    "interface_declaration" => "interface",
                    "trait_declaration" => "trait",
                    "enum_declaration" => "enum",
                    _ => "class",
                };
                let nested = self.e.define(
                    n,
                    scope,
                    &label,
                    kind,
                    Some(format!("php:type:{full}")),
                    false,
                );
                let mut inner = ctx.clone();
                inner.class = Some(full);
                for base in children(n)
                    .into_iter()
                    .filter(|c| matches!(c.kind(), "base_clause" | "class_interface_clause"))
                {
                    for target in children(base) {
                        let keys = self
                            .name(target, ctx, false)
                            .map(|s| vec![format!("php:type:{s}")])
                            .unwrap_or_default();
                        self.e.reference(
                            target,
                            nested,
                            self.e.text(target).into(),
                            if base.kind() == "base_clause" {
                                "inherits"
                            } else {
                                "implements"
                            },
                            keys,
                            "base type is unavailable or dynamic",
                        );
                    }
                }
                if let Some(body) = field(n, "body") {
                    self.sequence(body, nested, &inner);
                }
                return;
            }
            "function_definition"
            | "method_declaration"
            | "anonymous_function"
            | "arrow_function" => {
                let label = field(n, "name")
                    .map(|n| self.e.text(n).to_owned())
                    .unwrap_or_else(|| format!("<function@{}>", n.start_byte()));
                let method = n.kind() == "method_declaration";
                let key = if method {
                    ctx.class
                        .as_ref()
                        .map(|c| format!("php:method:{c}::{}", label.to_lowercase()))
                } else if n.kind() == "function_definition" {
                    Some(format!(
                        "php:function:{}",
                        qualified(&ctx.namespace, &label, "\\").to_lowercase()
                    ))
                } else {
                    None
                };
                let nested = self.e.define(
                    n,
                    scope,
                    &label,
                    if method { "method" } else { "function" },
                    key,
                    false,
                );
                if let Some(params) = field(n, "parameters") {
                    unknown_parameters(&mut self.e, params, nested, &["variable_name"]);
                    self.type_refs(params, nested, ctx, "parameter_type");
                    for parameter in children(params) {
                        if let Some(value) = field(parameter, "default_value") {
                            self.visit(value, nested, ctx);
                        }
                    }
                }
                if let Some(ty) = field(n, "return_type") {
                    self.type_refs(ty, nested, ctx, "return_type");
                }
                if let Some(body) = field(n, "body") {
                    self.visit(body, nested, ctx);
                }
                return;
            }
            "property_declaration" => {
                for property in children(n)
                    .into_iter()
                    .filter(|p| p.kind() == "property_element")
                {
                    let Some(name) = field(property, "name") else {
                        continue;
                    };
                    let label = self.e.text(name);
                    let key = ctx
                        .class
                        .as_ref()
                        .map(|c| format!("php:property:{c}::{label}"));
                    let nested = self
                        .e
                        .define(property, scope, label, "property", key, false);
                    if let Some(ty) = field(n, "type") {
                        self.type_refs(ty, nested, ctx, "field");
                    }
                    if let Some(value) = field(property, "default_value") {
                        if matches!(label, "$listen" | "$subscribe")
                            && value.kind() == "array_creation_expression"
                        {
                            for entry in children(value)
                                .into_iter()
                                .filter(|e| e.kind() == "array_element_initializer")
                            {
                                let parts = children(entry);
                                if let (Some(event), Some(listeners)) = (
                                    parts.first().and_then(|n| self.class_literal(*n, ctx)),
                                    parts
                                        .get(1)
                                        .filter(|n| n.kind() == "array_creation_expression"),
                                ) {
                                    for listener in children(*listeners)
                                        .into_iter()
                                        .filter_map(|n| n.named_child(0))
                                    {
                                        if let Some(target) = self.class_literal(listener, ctx) {
                                            self.registration(
                                                listener,
                                                nested,
                                                &event,
                                                &target,
                                                "listened_by",
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        self.visit(value, nested, ctx);
                    }
                }
                return;
            }
            "scoped_property_access_expression" | "class_constant_access_expression" => {
                let target = field(n, "scope").or_else(|| n.named_child(0));
                let keys = target
                    .and_then(|t| self.name(t, ctx, false))
                    .map(|s| vec![format!("php:type:{s}")])
                    .unwrap_or_default();
                self.e.reference(
                    n,
                    scope,
                    self.e.text(n).into(),
                    if n.kind() == "scoped_property_access_expression" {
                        "uses_static_prop"
                    } else {
                        "references_constant"
                    },
                    keys,
                    "static owner is dynamic, external, or ambiguous",
                );
            }
            "use_declaration" => {
                for target in children(n).into_iter().filter(|c| c.kind() != "use_list") {
                    let keys = self
                        .name(target, ctx, false)
                        .map(|s| vec![format!("php:type:{s}")])
                        .unwrap_or_default();
                    self.e.reference(
                        target,
                        scope,
                        self.e.text(target).into(),
                        "mixes_in",
                        keys,
                        "trait is dynamic or unavailable",
                    );
                }
            }
            "function_call_expression" => {
                if let Some(target) = field(n, "function") {
                    if self
                        .e
                        .text(target)
                        .trim_start_matches('\\')
                        .eq_ignore_ascii_case("config")
                        && !ctx.functions.contains_key("config")
                        && let Some(value) = field(n, "arguments")
                            .and_then(|a| a.named_child(0))
                            .and_then(|a| literal(&self.e, a))
                    {
                        let section = value.split('.').next().unwrap_or("").to_owned();
                        if !section.is_empty()
                            && section.chars().all(|c| c.is_alphanumeric() || c == '_')
                        {
                            let index = self.e.facts.references.len();
                            self.e.reference(
                                n,
                                scope,
                                value,
                                "uses_config",
                                vec![
                                    format!("php:module:config/{section}"),
                                    format!(
                                        "php:type:{}",
                                        qualified(&ctx.namespace, &section, "\\")
                                            .to_ascii_lowercase()
                                    ),
                                ],
                                "static config convention target is external or ambiguous",
                            );
                            self.config_uses.push((index, ctx.namespace.clone()));
                        }
                    }
                    let keys = self
                        .name(target, ctx, true)
                        .map(|s| vec![format!("php:function:{s}")])
                        .unwrap_or_default();
                    self.e.reference(
                        n,
                        scope,
                        self.e.text(target).into(),
                        "calls",
                        keys,
                        "dynamic callable or unavailable namespaced function",
                    );
                }
            }
            "scoped_call_expression" => {
                if let (Some(class), Some(name)) = (field(n, "scope"), field(n, "name")) {
                    let keys = (name.kind() == "name")
                        .then(|| self.name(class, ctx, false))
                        .flatten()
                        .map(|c| {
                            vec![format!(
                                "php:method:{c}::{}",
                                self.e.text(name).to_lowercase()
                            )]
                        })
                        .unwrap_or_default();
                    self.e.reference(
                        n,
                        scope,
                        format!("{}::{}", self.e.text(class), self.e.text(name)),
                        "calls",
                        keys,
                        "dynamic dispatch or unavailable static method",
                    );
                }
            }
            "member_call_expression" | "nullsafe_member_call_expression" => {
                if let (Some(object), Some(name)) = (field(n, "object"), field(n, "name")) {
                    let container = object.kind() == "member_access_expression"
                        && field(object, "object").is_some_and(|o| self.e.text(o) == "$this")
                        && field(object, "name").is_some_and(|n| self.e.text(n) == "app");
                    if container
                        && matches!(
                            self.e.text(name),
                            "bind" | "singleton" | "scoped" | "instance"
                        )
                    {
                        let args = field(n, "arguments").map(children).unwrap_or_default();
                        if let (Some(contract), Some(implementation)) = (
                            args.first().and_then(|a| self.class_literal(*a, ctx)),
                            args.get(1).and_then(|a| self.class_literal(*a, ctx)),
                        ) {
                            self.registration(n, scope, &contract, &implementation, "bound_to");
                        }
                    }
                    let keys = if self.e.text(object) == "$this" && name.kind() == "name" {
                        ctx.class
                            .as_ref()
                            .map(|c| {
                                vec![format!(
                                    "php:method:{c}::{}",
                                    self.e.text(name).to_lowercase()
                                )]
                            })
                            .unwrap_or_default()
                    } else {
                        vec![]
                    };
                    self.e.reference(
                        n,
                        scope,
                        format!("{}->{}", self.e.text(object), self.e.text(name)),
                        "calls",
                        keys,
                        "runtime receiver type or dynamic method",
                    );
                }
            }
            "object_creation_expression" => {
                if let Some(target) = children(n).into_iter().find(|c| c.kind() != "arguments") {
                    let keys = self
                        .name(target, ctx, false)
                        .map(|s| vec![format!("php:type:{s}")])
                        .unwrap_or_default();
                    self.e.reference(
                        n,
                        scope,
                        self.e.text(target).into(),
                        "instantiates",
                        keys,
                        "runtime class or unavailable type",
                    );
                }
            }
            "include_expression"
            | "include_once_expression"
            | "require_expression"
            | "require_once_expression" => {
                let target = n.named_child(0);
                let value = target.and_then(|t| literal(&self.e, t));
                let keys = value
                    .as_deref()
                    .map(|s| {
                        path_modules(&self.e, s, true)
                            .into_iter()
                            .map(|m| format!("php:module:{m}"))
                            .collect()
                    })
                    .unwrap_or_default();
                self.e.reference(
                    n,
                    scope,
                    value.unwrap_or_else(|| self.e.text(n).into()),
                    "imports",
                    keys,
                    "dynamic include or unavailable path",
                );
            }
            _ => {}
        }
        for c in children(n) {
            self.visit(c, scope, ctx);
        }
    }
}

type ElixirSignatures = HashSet<(String, usize)>;

#[derive(Clone, Default)]
struct ElixirContext {
    module: String,
    aliases: HashMap<String, String>,
    imports: Vec<(String, Option<ElixirSignatures>, ElixirSignatures)>,
}
struct Elixir<'a> {
    e: Extractor<'a>,
    functions: HashMap<(String, String, usize), (String, usize)>,
    pending: Vec<(usize, String, String, usize, ElixirContext)>,
}
impl<'a> Elixir<'a> {
    fn new(e: Extractor<'a>) -> Self {
        Self {
            e,
            functions: HashMap::new(),
            pending: vec![],
        }
    }
    fn extract(mut self, root: Syntax<'_>) -> FileFacts {
        self.sequence(root, 0, &ElixirContext::default());
        for (index, module, name, arity, ctx) in &self.pending {
            let keys = if let Some((key, _)) =
                self.functions.get(&(module.clone(), name.clone(), *arity))
            {
                vec![key.clone()]
            } else {
                let matching: Vec<_> = ctx
                    .imports
                    .iter()
                    .filter(|(_, only, except)| {
                        only.as_ref()
                            .is_none_or(|o| o.contains(&(name.clone(), *arity)))
                            && !except.contains(&(name.clone(), *arity))
                    })
                    .collect();
                if matching.len() == 1 {
                    vec![format!("elixir:function:{}:{name}/{arity}", matching[0].0)]
                } else {
                    vec![]
                }
            };
            self.e.facts.references[*index].candidate_keys = keys;
        }
        self.e.finish()
    }
    fn module(&self, name: &str, ctx: &ElixirContext) -> String {
        if let Some(name) = name.strip_prefix("Elixir.") {
            return name.into();
        }
        if name == "__MODULE__" {
            return ctx.module.clone();
        }
        if let Some(rest) = name.strip_prefix("__MODULE__.") {
            return qualified(&ctx.module, rest, ".");
        }
        let (first, rest) = name.split_once('.').unwrap_or((name, ""));
        ctx.aliases
            .get(first)
            .map(|a| {
                if rest.is_empty() {
                    a.clone()
                } else {
                    format!("{a}.{rest}")
                }
            })
            .unwrap_or_else(|| name.into())
    }
    fn module_args(&self, n: Syntax<'_>, ctx: &ElixirContext) -> Vec<String> {
        let Some(first) = children(n).first().copied() else {
            return vec![];
        };
        if first.kind() == "alias" {
            return vec![self.module(self.e.text(first), ctx)];
        }
        if first.kind() == "dot"
            && let (Some(left), Some(right)) = (field(first, "left"), field(first, "right"))
            && left.kind() == "alias"
            && right.kind() == "tuple"
        {
            let base = self.module(self.e.text(left), ctx);
            return children(right)
                .into_iter()
                .filter(|c| c.kind() == "alias")
                .map(|c| format!("{base}.{}", self.e.text(c)))
                .collect();
        }
        vec![]
    }
    fn option<'b>(&self, n: Syntax<'b>, key: &str) -> Option<Syntax<'b>> {
        descendants(n, "pair")
            .into_iter()
            .find(|p| {
                field(*p, "key")
                    .is_some_and(|k| self.e.text(k).trim_end().trim_end_matches(':') == key)
            })
            .and_then(|p| field(p, "value"))
    }
    fn selectors(&self, n: Syntax<'_>) -> HashSet<(String, usize)> {
        descendants(n, "pair")
            .into_iter()
            .filter_map(|p| {
                let name = self
                    .e
                    .text(field(p, "key")?)
                    .trim_end()
                    .trim_end_matches(':')
                    .to_owned();
                let arity = self.e.text(field(p, "value")?).parse().ok()?;
                Some((name, arity))
            })
            .collect()
    }
    fn sequence(&mut self, n: Syntax<'_>, scope: usize, ctx: &ElixirContext) {
        let mut ctx = ctx.clone();
        for c in children(n) {
            if c.kind() == "call"
                && field(c, "target").is_some_and(|t| {
                    matches!(self.e.text(t), "alias" | "import" | "require" | "use")
                })
            {
                self.import(c, scope, &mut ctx);
            } else {
                self.visit(c, scope, &ctx);
            }
        }
    }
    fn import(&mut self, n: Syntax<'_>, scope: usize, ctx: &mut ElixirContext) {
        let keyword = field(n, "target").map(|n| self.e.text(n)).unwrap_or("");
        let Some(args) = child(n, "arguments") else {
            return;
        };
        let modules = self.module_args(args, ctx);
        if modules.is_empty() {
            self.e.reference(
                n,
                scope,
                self.e.text(n).into(),
                "imports",
                vec![],
                "dynamic module expression",
            );
        }
        for module in modules {
            self.e.reference(
                n,
                scope,
                module.clone(),
                "imports",
                vec![format!("elixir:module-name:{module}")],
                "module is external or ambiguous",
            );
            if keyword == "alias" {
                let alias = self
                    .option(args, "as")
                    .filter(|n| n.kind() == "alias")
                    .map(|n| self.e.text(n))
                    .unwrap_or_else(|| module.rsplit('.').next().unwrap_or(&module))
                    .to_owned();
                ctx.aliases.insert(alias, module);
            } else if keyword == "import" {
                let only = self.option(args, "only").map(|o| self.selectors(o));
                let except = self
                    .option(args, "except")
                    .map(|o| self.selectors(o))
                    .unwrap_or_default();
                ctx.imports.push((module, only, except));
            }
        }
    }
    fn head<'b>(&self, mut n: Syntax<'b>) -> Syntax<'b> {
        while n.kind() == "binary_operator"
            && field(n, "operator").is_some_and(|o| self.e.text(o) == "when")
        {
            let Some(left) = field(n, "left") else { break };
            n = left;
        }
        n
    }
    fn function(&mut self, n: Syntax<'_>, scope: usize, ctx: &ElixirContext, keyword: &str) {
        let Some(args) = child(n, "arguments") else {
            return;
        };
        let Some(head) = args.named_child(0).map(|n| self.head(n)) else {
            return;
        };
        let target = field(head, "target").unwrap_or(head);
        if target.kind() != "identifier" {
            return;
        }
        let name = self.e.text(target);
        let params = child(head, "arguments").map(children).unwrap_or_default();
        let arity = params.len();
        let defaults = params
            .iter()
            .filter(|p| {
                p.kind() == "binary_operator"
                    && field(**p, "operator").is_some_and(|o| self.e.text(o) == "\\\\")
            })
            .count();
        let public = !matches!(keyword, "defp" | "defmacrop" | "defguardp");
        let key = if public {
            format!("elixir:function:{}:{name}/{arity}", ctx.module)
        } else {
            format!(
                "elixir:private:{}:{}:{name}/{arity}",
                self.e.facts.path, ctx.module
            )
        };
        let slot = (ctx.module.clone(), name.into(), arity);
        let nested = if let Some((_, existing)) = self.functions.get(&slot) {
            let existing = *existing;
            let owner = self.e.scopes[existing].owner.clone();
            if let Some(node) = self.e.facts.nodes.iter_mut().find(|n| n.id == owner) {
                node.end_line = Some(end_line(n));
                node.metadata["end_byte"] = n.end_byte().into();
            }
            self.e.scope(
                scope,
                self.e.scopes[existing].qualified.clone(),
                Some(owner),
                false,
            )
        } else {
            let nested = self.e.define(
                n,
                scope,
                name,
                if keyword.contains("macro") {
                    "macro"
                } else {
                    "function"
                },
                Some(key.clone()),
                false,
            );
            self.functions.insert(slot, (key.clone(), nested));
            if defaults > 0 {
                let aliases: Vec<_> = (arity - defaults..arity)
                    .map(|a| {
                        let alias = if public {
                            format!("elixir:function:{}:{name}/{a}", ctx.module)
                        } else {
                            format!(
                                "elixir:private:{}:{}:{name}/{a}",
                                self.e.facts.path, ctx.module
                            )
                        };
                        self.functions.insert(
                            (ctx.module.clone(), name.into(), a),
                            (alias.clone(), nested),
                        );
                        alias
                    })
                    .collect();
                self.e.facts.nodes.last_mut().unwrap().metadata["binding_aliases"] =
                    serde_json::json!(aliases);
            }
            nested
        };
        for p in &params {
            unknown_parameters(&mut self.e, *p, nested, &["identifier"]);
        }
        if let Some(body) = child(n, "do_block") {
            self.sequence(body, nested, ctx);
        }
        if let Some(body) = self.option(args, "do") {
            self.visit(body, nested, ctx);
        }
        // Guard calls belong to the function, never the enclosing module.
        if let Some(original_head) = args
            .named_child(0)
            .filter(|h| h.kind() == "binary_operator")
            && let Some(guard) = field(original_head, "right")
        {
            self.visit(guard, nested, ctx);
        }
    }
    fn visit(&mut self, n: Syntax<'_>, scope: usize, ctx: &ElixirContext) {
        if n.kind() == "call" {
            let Some(target) = field(n, "target") else {
                return;
            };
            let keyword = self.e.text(target);
            if matches!(keyword, "defmodule" | "defprotocol" | "defimpl") {
                let Some(args) = child(n, "arguments") else {
                    return;
                };
                let Some(name) = args.named_child(0).filter(|n| n.kind() == "alias") else {
                    return;
                };
                let declared = self.e.text(name);
                let full = if keyword == "defimpl" {
                    let implementation = self
                        .option(args, "for")
                        .filter(|n| matches!(n.kind(), "alias" | "atom"))
                        .map(|n| self.module(self.e.text(n).trim_start_matches(':'), ctx));
                    implementation
                        .map(|target| format!("{}.{target}", self.module(declared, ctx)))
                        .unwrap_or_else(|| {
                            format!("{}.<implementation@{}>", self.e.facts.path, n.start_byte())
                        })
                } else if declared.starts_with("Elixir.") {
                    declared.trim_start_matches("Elixir.").into()
                } else {
                    qualified(&ctx.module, declared, ".")
                };
                let nested = self.e.define(
                    n,
                    scope,
                    declared,
                    if keyword == "defprotocol" {
                        "interface"
                    } else if keyword == "defimpl" {
                        "impl"
                    } else {
                        "module"
                    },
                    Some(format!("elixir:module-name:{full}")),
                    false,
                );
                if keyword == "defimpl" {
                    self.e.reference(
                        name,
                        nested,
                        declared.into(),
                        "implements",
                        vec![format!("elixir:module-name:{}", self.module(declared, ctx))],
                        "protocol is external or ambiguous",
                    );
                }
                let mut inner = ctx.clone();
                inner.module = full;
                if let Some(body) = child(n, "do_block") {
                    self.sequence(body, nested, &inner);
                }
                return;
            }
            if matches!(
                keyword,
                "def" | "defp" | "defmacro" | "defmacrop" | "defguard" | "defguardp"
            ) {
                self.function(n, scope, ctx, keyword);
                return;
            }
            if matches!(keyword, "alias" | "import" | "require" | "use") {
                self.import(n, scope, &mut ctx.clone());
                return;
            }
            let args = child(n, "arguments");
            let mut arity = args.map(|a| a.named_child_count()).unwrap_or(0);
            if n.parent().is_some_and(|p| {
                p.kind() == "binary_operator"
                    && field(p, "right").is_some_and(|r| r.id() == n.id())
                    && field(p, "operator").is_some_and(|o| self.e.text(o) == "|>")
            }) {
                arity += 1;
            }
            if !matches!(
                keyword,
                "if" | "unless"
                    | "case"
                    | "cond"
                    | "with"
                    | "for"
                    | "quote"
                    | "unquote"
                    | "defstruct"
                    | "raise"
                    | "try"
                    | "receive"
            ) {
                let keys = if target.kind() == "dot" {
                    match (field(target, "left"), field(target, "right")) {
                        (Some(left), Some(right))
                            if left.kind() == "alias" && right.kind() == "identifier" =>
                        {
                            vec![format!(
                                "elixir:function:{}:{}/{arity}",
                                self.module(self.e.text(left), ctx),
                                self.e.text(right)
                            )]
                        }
                        _ => vec![],
                    }
                } else {
                    vec![]
                };
                let index = self.e.facts.references.len();
                self.e.reference(
                    n,
                    scope,
                    keyword.into(),
                    "calls",
                    keys,
                    "dynamic callable, unavailable module, or ambiguous import",
                );
                if target.kind() == "identifier" {
                    self.pending.push((
                        index,
                        ctx.module.clone(),
                        keyword.into(),
                        arity,
                        ctx.clone(),
                    ));
                }
            }
            if let Some(args) = args {
                for arg in children(args) {
                    self.visit(arg, scope, ctx);
                }
            }
            if let Some(body) = child(n, "do_block") {
                let nested = self.e.block(scope, body);
                self.sequence(body, nested, ctx);
            }
            return;
        }
        if n.kind() == "binary_operator"
            && field(n, "operator").is_some_and(|o| self.e.text(o) == "=")
        {
            if let Some(right) = field(n, "right") {
                self.visit(right, scope, ctx);
            }
            if let Some(left) = field(n, "left") {
                unknown_parameters(&mut self.e, left, scope, &["identifier"]);
            }
            return;
        }
        if n.kind() == "identifier" {
            let name = self.e.text(n);
            let mut current = Some(scope);
            while let Some(i) = current {
                if self.e.scopes[i].bindings.contains_key(name) {
                    return;
                }
                current = self.e.scopes[i].parent;
            }
            if name != "__MODULE__" {
                let index = self.e.facts.references.len();
                self.e.reference(
                    n,
                    scope,
                    name.into(),
                    "calls",
                    vec![],
                    "unbound zero-arity function",
                );
                self.pending
                    .push((index, ctx.module.clone(), name.into(), 0, ctx.clone()));
            }
            return;
        }
        if n.kind() == "anonymous_function" {
            let label = format!("<fn@{}>", n.start_byte());
            let nested = self.e.define(n, scope, &label, "function", None, false);
            for clause in children(n) {
                self.visit(clause, nested, ctx);
            }
            return;
        }
        if n.kind() == "stab_clause" {
            let nested = self.e.block(scope, n);
            if let Some(params) = field(n, "left") {
                unknown_parameters(&mut self.e, params, nested, &["identifier"]);
            }
            if let Some(body) = field(n, "right") {
                self.sequence(body, nested, ctx);
            }
            return;
        }
        for c in children(n) {
            self.visit(c, scope, ctx);
        }
    }
}

// TOC is a line-oriented addon manifest, not Lua source. Entries never execute.
fn lua_manifest(path: &str, source: &str, hash: &str) -> FileFacts {
    use crate::model::{Node, Reference};
    let module = module_path(path);
    let owner = format!("lua:{path}:manifest");
    let mut facts = FileFacts {
        path: path.into(),
        hash: hash.into(),
        module: module.clone(),
        nodes: vec![],
        edges: vec![],
        references: vec![],
        diagnostics: vec![],
    };
    if source.len() > crate::parser::MAX_SOURCE_BYTES {
        diagnostic(&mut facts, None, "Source exceeds the 4 MiB indexing limit");
        return facts;
    }
    facts.nodes.push(Node { id: owner.clone(), label: module.clone(), kind: "manifest".into(), file: path.into(), line: Some(1), end_line: Some(source.lines().count().max(1) as u32), qualified_name: Some(module.clone()), binding_key: Some(format!("lua:manifest:{module}")), metadata: serde_json::json!({"language":"lua-manifest","start_byte":0,"end_byte":source.len(),"manifest":{}}) });
    let base = path.rsplit_once('/').map_or("", |(base, _)| base);
    for (row, line) in source.lines().enumerate() {
        let text = line.trim().trim_start_matches('\u{feff}');
        if text.is_empty() {
            continue;
        }
        if let Some(metadata) = text.strip_prefix("##") {
            if let Some((name, value)) = metadata.split_once(':') {
                facts.nodes[0].metadata["manifest"][name.trim()] = value.trim().into();
                if matches!(
                    name.trim(),
                    "Dependencies" | "RequiredDeps" | "OptionalDeps"
                ) {
                    for (index, dependency) in value
                        .split(',')
                        .map(str::trim)
                        .filter(|d| !d.is_empty())
                        .enumerate()
                    {
                        facts.references.push(Reference {
                            id: format!("imports:{owner}:{row}:{index}"),
                            source: owner.clone(),
                            label: dependency.into(),
                            relation: "imports".into(),
                            file: path.into(),
                            line: row as u32 + 1,
                            candidate_keys: vec![],
                            reason: "addon load order and installed dependencies are external"
                                .into(),
                        });
                    }
                }
            }
            continue;
        }
        if text.starts_with('#') {
            continue;
        }
        let normalized = text.replace('\\', "/");
        let target = (!normalized.starts_with('/') && !normalized.contains(['$', ':']))
            .then(|| relative_path(base, &normalized))
            .flatten();
        let keys = target
            .as_ref()
            .map(|p| match p.rsplit('.').next() {
                Some("lua") => vec![format!("lua:module:{}", module_path(p))],
                Some("luau") => vec![format!("luau:module:{}", module_path(p))],
                _ => vec![],
            })
            .unwrap_or_default();
        if target.is_none() {
            diagnostic(
                &mut facts,
                Some(row as u32 + 1),
                "Manifest entry is not a repository-relative static path",
            );
        }
        facts.references.push(Reference {
            id: format!("imports:{owner}:{row}"),
            source: owner.clone(),
            label: text.into(),
            relation: "imports".into(),
            file: path.into(),
            line: row as u32 + 1,
            candidate_keys: keys,
            reason: "manifest entry is dynamic, non-code, or unavailable".into(),
        });
    }
    facts
}
