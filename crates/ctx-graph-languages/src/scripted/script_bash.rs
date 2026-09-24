use super::*;

impl<'a> Script<'a> {
    pub(super) fn bash(&mut self, n: Syntax<'_>, scope: usize) {
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
    pub(super) fn powershell_type(&mut self, n: Syntax<'_>, scope: usize, context: &str) {
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
    pub(super) fn powershell(&mut self, n: Syntax<'_>, scope: usize) {
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
