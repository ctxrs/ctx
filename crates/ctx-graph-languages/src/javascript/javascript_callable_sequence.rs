use super::*;

impl Javascript<'_> {
    pub(super) fn callable_sequence(mut node: Syntax<'_>) -> bool {
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
    pub(super) fn declare_callable(&mut self, node: Syntax<'_>, scope: usize) {
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
    pub(super) fn assign_callable(&mut self, node: Syntax<'_>, scope: usize) {
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
    pub(super) fn receiver(&mut self, scope: usize, name: &str, at: usize, evidence: Receiver) {
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
    pub(super) fn annotated_type(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        let node = if node.kind() == "type_annotation" {
            node.named_child(0)?
        } else {
            node
        };
        self.type_path(node)
    }
    pub(super) fn typed_binding(
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
    pub(super) fn factory_binding(&mut self, node: Syntax<'_>, scope: usize) -> bool {
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
    pub(super) fn class_fields(&mut self, class: usize, body: Syntax<'_>) {
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
    pub(super) fn bind_type(&mut self, scope: usize, name: &str, binding: Binding) {
        self.type_bindings
            .entry((scope, identifier(name)))
            .and_modify(|b| *b = Binding::Unknown)
            .or_insert(binding);
    }
    pub(super) fn type_path(&self, node: Syntax<'_>) -> Option<Vec<String>> {
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
    pub(super) fn type_keys(&self, scope: usize, parts: &[String]) -> Vec<String> {
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
    pub(super) fn type_parameters(&mut self, node: Syntax<'_>, scope: usize) {
        if let Some(params) = node.child_by_field_name("type_parameters") {
            for param in children(params) {
                if let Some(name) = param.child_by_field_name("name") {
                    self.bind_type(scope, self.e.text(name), Binding::Unknown);
                }
            }
            self.type_refs(params, scope, "references_type");
        }
    }
    pub(super) fn import_type(&self, node: Syntax<'_>) -> Option<(String, Vec<String>)> {
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
    pub(super) fn type_refs(&mut self, node: Syntax<'_>, scope: usize, relation: &str) {
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
    pub(super) fn exact_key(&self, key: &str) -> String {
        if key.starts_with(&format!("javascript:local:{}:", self.e.facts.path)) {
            return key.into();
        }
        key.strip_prefix(&format!("javascript:{}:", self.e.facts.module))
            .map_or_else(
                || key.into(),
                |name| format!("javascript:file:{}:{name}", self.e.facts.path),
            )
    }
    pub(super) fn string(&self, node: Syntax<'_>) -> Option<String> {
        let text = self.e.text(node);
        // Escaped module specifiers need JavaScript string decoding; never guess their target.
        if text.len() < 2 || text.contains('\\') {
            return None;
        }
        Some(text[1..text.len() - 1].into())
    }
    pub(super) fn modules(&self, name: &str) -> Vec<String> {
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
    pub(super) fn exports(&mut self, root: Syntax<'_>) {
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
    pub(super) fn key(&self, scope: usize, name: &str) -> String {
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
    pub(super) fn aliases(&mut self, root: Syntax<'_>) {
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
}
