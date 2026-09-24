use super::*;

impl<'a> Script<'a> {
    pub(super) fn new(e: Extractor<'a>) -> Self {
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
    pub(super) fn extract(mut self, root: Syntax<'_>) -> FileFacts {
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
    pub(super) fn name(&self, name: &str) -> String {
        if self.e.language == "powershell" {
            name.to_lowercase()
        } else {
            name.into()
        }
    }
    pub(super) fn key(&self, scope: usize, name: &str) -> String {
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
    pub(super) fn import(&mut self, n: Syntax<'_>, scope: usize, name: &str, modules: &[String]) {
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
    pub(super) fn lua_modules(&self, name: &str) -> Vec<String> {
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
    pub(super) fn lua_parts(&self, n: Syntax<'_>) -> Option<Vec<String>> {
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
    pub(super) fn lua_require(&self, n: Syntax<'_>, scope: usize) -> Option<(String, Vec<String>)> {
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
    pub(super) fn lua_assignment(&mut self, n: Syntax<'_>, scope: usize, local: bool) {
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
    pub(super) fn lua_function(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        name: Syntax<'_>,
        local: bool,
    ) {
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
    pub(super) fn call(
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
    pub(super) fn visit(&mut self, n: Syntax<'_>, scope: usize) {
        match self.e.language {
            "lua" | "luau" => self.lua(n, scope),
            "bash" => self.bash(n, scope),
            _ => self.powershell(n, scope),
        }
    }
    pub(super) fn lua(&mut self, n: Syntax<'_>, scope: usize) {
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
    pub(super) fn bash_path(&self, n: Syntax<'_>, scope: usize) -> Option<BashPath> {
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
}
