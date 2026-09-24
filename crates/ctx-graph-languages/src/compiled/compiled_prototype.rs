use super::*;

impl<'s, 't> Compiled<'s, 't> {
    pub(super) fn prototype(
        &mut self,
        n: Syntax<'t>,
        declarator: Syntax<'t>,
        scope: usize,
        name: &str,
    ) {
        let parameterless = !n
            .parent()
            .is_some_and(|p| p.kind() == "template_declaration")
            && self.parameterless(declarator);
        let q = format!("{}.{name}", self.scopes[&scope].prefix);
        let child = self.define(n, scope, name, "declaration", q.clone(), false);
        let symbol = q
            .strip_prefix(&format!("@{}.", self.e.facts.path))
            .unwrap_or(&q)
            .to_owned();
        let member = self.scopes[&scope].class.is_some();
        let static_member = member && self.modifier(n, "static");
        let dynamic = self.modifier(n, "virtual")
            || self.modifier(n, "override")
            || self.scopes[&scope].abstract_members
            || (member && !static_member && self.e.facts.nodes[0].metadata["dialect"] == "cpp_cli");
        let safe = !dynamic && !self.uncertain(scope);
        let node = self.e.facts.nodes.last_mut().unwrap();
        node.metadata["header_declaration"] = json!(symbol);
        node.metadata["static"] = json!(static_member);
        node.metadata["parameterless"] = json!(parameterless);
        node.metadata["dynamic_dispatch"] = json!(dynamic);
        if safe && is_header(&self.e.facts.path) {
            node.binding_key = Some(header_key("declaration", &self.e.facts.path, &symbol));
            let mut aliases = vec![header_key("symbol", &self.e.facts.path, &symbol)];
            if member {
                aliases.push(header_key(
                    if static_member { "static" } else { "member" },
                    &self.e.facts.path,
                    &symbol,
                ));
            }
            node.metadata["binding_aliases"] = json!(aliases);
        }
        self.e.reference(
            n,
            child,
            name.into(),
            "implemented_by",
            vec![header_key("implementation", &self.e.facts.path, &symbol)],
            "matching definition must include this exact header",
        );
        self.scopes.get_mut(&child).unwrap().local = true;
        if let Some(ty) = n.child_by_field_name("type") {
            self.type_evidence(ty, child, "return_type");
        }
        let mut pending = vec![declarator];
        while let Some(d) = pending.pop() {
            if let Some(params) = d.child_by_field_name("parameters") {
                self.visit(params, child);
            }
            if let Some(d) = d.child_by_field_name("declarator") {
                pending.push(d);
            }
        }
    }
    // Only a written empty parameter list, without method type parameters,
    // supplies this declaration-shape proof. It is never overload resolution.
    pub(super) fn parameterless(&self, mut n: Syntax<'_>) -> bool {
        loop {
            if children(n)
                .iter()
                .any(|c| matches!(c.kind(), "type_parameters" | "type_parameter_list"))
                || n.parent()
                    .is_some_and(|p| p.kind() == "template_declaration")
            {
                return false;
            }
            if let Some(parameters) = n.child_by_field_name("parameters").or_else(|| {
                children(n)
                    .into_iter()
                    .find(|c| c.kind() == "function_value_parameters")
            }) {
                let mut cursor = parameters.walk();
                return parameters
                    .children(&mut cursor)
                    .all(|c| matches!(c.kind(), "(" | ")" | "comment"));
            }
            if self.e.language == "swift" {
                let mut cursor = n.walk();
                let tokens: Vec<_> = n
                    .children(&mut cursor)
                    .filter(|c| c.kind() != "comment")
                    .collect();
                return tokens
                    .windows(2)
                    .any(|pair| pair[0].kind() == "(" && pair[1].kind() == ")");
            }
            let Some(declarator) = n.child_by_field_name("declarator") else {
                return false;
            };
            n = declarator;
        }
    }
    pub(super) fn uncertain(&self, mut scope: usize) -> bool {
        loop {
            if self.scopes[&scope].uncertain {
                return true;
            }
            let Some(p) = self.e.scopes[scope].parent else {
                return false;
            };
            scope = p;
        }
    }
    pub(super) fn variable(&mut self, n: Syntax<'t>, scope: usize) {
        if n.kind() == "property_declaration" && self.e.language == "kotlin" {
            return;
        }
        let name = n
            .child_by_field_name("name")
            .or_else(|| n.child_by_field_name("left"))
            .or_else(|| {
                n.child_by_field_name("declarator")
                    .and_then(declarator_name)
            })
            .or_else(|| {
                children(n).into_iter().find(|c| {
                    matches!(
                        c.kind(),
                        "identifier" | "simple_identifier" | "type_identifier"
                    ) && Some(*c) != n.child_by_field_name("type")
                })
            });
        let Some(name) = name else {
            return;
        };
        if matches!(n.kind(), "type_parameter" | "type_parameter_declaration") {
            let name = self.e.text(name).to_owned();
            self.bind(scope, &name, Binding::TypeParameter);
            return;
        }
        self.variable_declarator(n, n, name, scope);
    }
    pub(super) fn variable_declarator(
        &mut self,
        declaration: Syntax<'t>,
        declarator: Syntax<'t>,
        name: Syntax<'t>,
        scope: usize,
    ) {
        let Some(name) = self.name(name) else {
            return;
        };
        let ty_node =
            declared_type(declaration).or_else(|| declaration.parent().and_then(declared_type));
        let field = self.scopes[&scope].class.is_some() && !self.scopes[&scope].local;
        let context = if field {
            "field"
        } else if matches!(
            declaration.kind(),
            "formal_parameter"
                | "parameter"
                | "parameter_declaration"
                | "optional_parameter_declaration"
                | "class_parameter"
        ) {
            "parameter_type"
        } else {
            "variable_type"
        };
        if let Some(ty) = ty_node {
            self.type_evidence(ty, scope, context);
        }
        let ty = ty_node
            .and_then(|n| {
                // An existential names a protocol declaration, not an implementor.
                let n = if self.e.language == "swift" && n.kind() == "existential_type" {
                    n.named_child(0).filter(|n| n.kind() == "user_type")?
                } else {
                    n
                };
                self.name(n)
            })
            .filter(|s| !matches!(s.as_str(), "var" | "auto" | "dynamic" | "Any" | "AnyObject"));
        let ty = if contains_kind(declarator, "function_declarator") {
            None
        } else {
            ty.and_then(|t| self.qualify(scope, &t))
        };
        self.bind(scope, &name, Binding::Value(ty));
        if field {
            let q = format!("{}.{name}", self.scopes[&scope].prefix);
            self.define(declaration, scope, &name, "field", q.clone(), false);
            let key = format!("{}:field:{q}", self.e.language);
            let safe = !self.uncertain(scope);
            self.e.facts.nodes.last_mut().unwrap().binding_key = safe.then_some(key);
        }
    }
    pub(super) fn inheritance(&mut self, n: Syntax<'t>, scope: usize) {
        for child in children(n) {
            if matches!(
                child.kind(),
                "superclass"
                    | "super_interfaces"
                    | "extends_interfaces"
                    | "base_list"
                    | "base_class_clause"
                    | "inheritance_specifier"
                    | "delegation_specifiers"
            ) {
                self.base_types(
                    child,
                    scope,
                    if child.kind() == "super_interfaces" {
                        "implements"
                    } else {
                        "inherits"
                    },
                );
            }
        }
    }
    pub(super) fn base_types(&mut self, n: Syntax<'t>, scope: usize, relation: &'static str) {
        if self.e.language == "kotlin" && n.kind() == "explicit_delegation" {
            if let Some(ty) = children(n).into_iter().find(|c| is_type(c.kind())) {
                self.base_types(ty, scope, "implements");
                self.pending.push(Pending {
                    node: n,
                    scope,
                    label: self.e.text(n).into(),
                    relation: "delegates_to",
                    target: children(n).last().and_then(|n| self.name(*n)),
                    constructor: false,
                    context: Some("delegation_type"),
                });
            }
            return;
        }
        let relation = if self.e.language == "kotlin"
            && n.kind() == "delegation_specifier"
            && !children(n)
                .iter()
                .any(|c| c.kind() == "constructor_invocation")
            && !self.scopes[&scope].abstract_members
        {
            "implements"
        } else {
            relation
        };
        if matches!(
            n.kind(),
            "type_identifier"
                | "user_type"
                | "scoped_type_identifier"
                | "qualified_identifier"
                | "identifier"
                | "qualified_name"
                | "generic_name"
                | "template_type"
                | "generic_type"
        ) {
            let target = type_name(n, self.e.source);
            for c in children(n).into_iter().filter(|c| {
                matches!(
                    c.kind(),
                    "type_arguments" | "type_argument_list" | "template_argument_list"
                )
            }) {
                for arg in children(c) {
                    self.type_evidence(arg, scope, "generic_arg");
                }
            }
            self.pending.push(Pending {
                node: n,
                scope,
                label: self.e.text(n).into(),
                relation,
                target,
                constructor: true,
                context: None,
            });
        } else {
            for c in children(n) {
                if c.kind() != "argument_list" && c.kind() != "value_arguments" {
                    self.base_types(c, scope, relation);
                }
            }
        }
    }
    pub(super) fn enum_case(&mut self, n: Syntax<'t>, scope: usize) {
        let mut cursor = n.walk();
        let mut names: Vec<_> = n.children_by_field_name("name", &mut cursor).collect();
        if names.is_empty() {
            names.extend(
                children(n)
                    .into_iter()
                    .find(|c| matches!(c.kind(), "identifier" | "simple_identifier")),
            );
        }
        for name in names {
            let Some(label) = self.name(name) else {
                continue;
            };
            let qualified = format!("{}.{label}", self.scopes[&scope].prefix);
            let owner = self.e.scopes[scope].owner.clone();
            let child = self.define(name, scope, &label, "enum_case", qualified, false);
            let id = self.e.scopes[child].owner.clone();
            self.e.facts.edges.push(crate::model::Edge {
                id: format!("case_of:{id}"),
                source: owner,
                target: id,
                relation: "case_of".into(),
                directed: true,
                file: Some(self.e.facts.path.clone()),
                line: Some(name.start_position().row as u32 + 1),
                confidence: "static".into(),
                metadata: json!({"syntax":"enum case"}),
            });
            for c in children(n) {
                if c.kind() == "enum_type_parameters" {
                    for ty in children(c).into_iter().filter(|c| is_type(c.kind())) {
                        self.type_evidence(ty, scope, "type");
                    }
                } else if matches!(c.kind(), "argument_list" | "value_arguments" | "class_body") {
                    self.visit(c, child);
                }
            }
        }
    }
    pub(super) fn import(&mut self, n: Syntax<'t>, scope: usize) {
        let text = self.e.text(n);
        self.e.facts.nodes[0].metadata["imports"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "syntax": text, "kind": n.kind(), "scope": self.e.scopes[scope].owner,
                "start_byte": n.start_byte(), "end_byte": n.end_byte()
            }));
        if n.kind() == "preproc_include" {
            let path = n.child_by_field_name("path");
            let label = path.map(|p| self.e.text(p)).unwrap_or(text).to_owned();
            let keys = if label.starts_with('"') {
                let relative = relative_path(
                    self.e.facts.path.rsplit_once('/').map_or("", |(p, _)| p),
                    label.trim_matches('"'),
                );
                if let Some(path) = &relative
                    && !self.uncertain(scope)
                    && !self.includes.contains(path)
                {
                    self.includes.push(path.clone());
                }
                relative
                    .map(|p| {
                        vec![format!(
                            "{}:file:{p}",
                            language(&p).map_or(self.e.language, |(lang, _)| lang)
                        )]
                    })
                    .unwrap_or_default()
            } else {
                vec![]
            };
            self.e.reference(
                n,
                scope,
                label,
                "includes",
                keys,
                "include target is external or unavailable; preprocessing is not performed",
            );
            return;
        }
        let raw = text.trim().trim_end_matches(';').trim();
        let mut raw = raw.strip_prefix("global ").unwrap_or(raw);
        for prefix in ["import ", "using ", "namespace "] {
            if let Some(s) = raw.strip_prefix(prefix) {
                raw = s.trim();
                break;
            }
        }
        let static_import = raw.starts_with("static ");
        raw = raw.strip_prefix("static ").unwrap_or(raw);
        let (target, alias) = if let Some((alias, target)) = raw.split_once('=') {
            (target.trim(), Some(alias.trim()))
        } else if let Some((target, alias)) = raw.split_once(" as ") {
            (target.trim(), Some(alias.trim()))
        } else {
            (raw, None)
        };
        let target = simple_name(target);
        let mut keys = vec![];
        if let Some(target) = target {
            keys.push(self.key(&target));
            if self.e.language == "swift" && !target.contains('.') {
                self.bind(scope, &target, Binding::Module(target.clone()));
                self.e.facts.nodes[0].metadata["swift_imports"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(target));
            }
            let alias = alias.unwrap_or_else(|| target.rsplit('.').next().unwrap());
            let is_namespace_using =
                self.e.language == "csharp" && !static_import && !raw.contains('=');
            if !is_namespace_using
                && self.e.language != "swift"
                && !(self.e.language == "csharp" && static_import)
            {
                let binding = if static_import
                    || (self.e.language == "kotlin"
                        && target
                            .rsplit('.')
                            .next()
                            .is_some_and(|s| s.chars().next().is_some_and(char::is_lowercase)))
                {
                    Binding::Symbol(self.key(&target))
                } else {
                    Binding::Type(target.clone())
                };
                if let Some(alias) = simple_name(alias) {
                    self.bind(scope, &alias, binding);
                }
            }
            // Namespace/wildcard imports cannot be priority candidates: Store resolves
            // the first available key, which would hide collisions across imports.
        }
        self.e.reference(
            n,
            scope,
            raw.into(),
            "imports",
            keys,
            "import target is external, a namespace, or unavailable",
        );
    }
    pub(super) fn resolve(&self, p: &Pending<'_>) -> Vec<String> {
        let Some(target) = &p.target else {
            return vec![];
        };
        if self.uncertain(p.scope) {
            return vec![];
        }
        if p.relation == "delegates_to" {
            return match self.lookup(p.scope, target) {
                Some(Binding::Value(Some(ty))) => vec![self.key(ty)],
                _ => vec![],
            };
        }
        if p.constructor {
            if matches!(
                self.lookup(p.scope, target.split('.').next().unwrap_or(target)),
                Some(Binding::TypeParameter)
            ) {
                return vec![];
            }
            let parent = if matches!(p.relation, "inherits" | "implements" | "extends") {
                self.e.scopes[p.scope].parent.unwrap_or(p.scope)
            } else {
                p.scope
            };
            return self
                .qualify(parent, target)
                .map(|q| vec![self.key(&q)])
                .unwrap_or_default();
        }
        let parts: Vec<_> = target.split('.').collect();
        let head = parts[0];
        if parts.len() == 1 {
            return match self.lookup(p.scope, head) {
                Some(Binding::Symbol(k)) => vec![k.clone()],
                Some(Binding::Type(q)) => vec![self.key(q)],
                Some(_) => vec![],
                None if matches!(self.e.language, "java" | "csharp" | "swift")
                    && self.scopes[&p.scope].class.is_some() =>
                {
                    vec![self.key(&format!(
                        "{}.{head}",
                        self.scopes[&p.scope].class.as_ref().unwrap().0
                    ))]
                }
                None => {
                    let mut keys =
                        vec![self.key(&format!("{}.{head}", self.scopes[&p.scope].prefix))];
                    if matches!(self.e.language, "c" | "cpp") && self.includes.len() == 1 {
                        keys.push(header_key("symbol", &self.includes[0], head));
                    }
                    keys
                }
            };
        }
        if matches!(head, "this" | "self") {
            if parts.len() == 3 {
                let mut scope = p.scope;
                loop {
                    if self.e.scopes[scope].class {
                        return match self.scopes[&scope].bindings.get(parts[1]) {
                            Some(Binding::Value(Some(q))) => {
                                vec![self.member_key("member", &format!("{q}.{}", parts[2]))]
                            }
                            _ => vec![],
                        };
                    }
                    let Some(parent) = self.e.scopes[scope].parent else {
                        return vec![];
                    };
                    scope = parent;
                }
            }
            if parts.len() != 2 {
                return vec![];
            }
            return self.scopes[&p.scope]
                .class
                .as_ref()
                .map(|(q, _)| vec![self.key(&format!("{q}.{}", parts[1]))])
                .unwrap_or_default();
        }
        if matches!(head, "base" | "super") && parts.len() == 2 {
            return self.scopes[&p.scope]
                .class
                .as_ref()
                .map(|(q, _)| vec![format!("{}:base:{q}.{}", self.e.language, parts[1])])
                .unwrap_or_default();
        }
        match self.lookup(p.scope, head) {
            Some(Binding::Value(Some(q))) if parts.len() == 2 => {
                vec![self.member_key("member", &format!("{q}.{}", parts[1]))]
            }
            Some(Binding::Type(q)) => {
                let qualified = format!("{q}.{}", parts[1..].join("."));
                if self.e.language == "cpp" {
                    vec![self.key(&qualified)]
                } else {
                    vec![format!("{}:static:{qualified}", self.e.language)]
                }
            }
            Some(Binding::Module(module)) => vec![self.member_key(
                if parts.len() == 2 { "symbol" } else { "static" },
                &format!("!{module}.{}", parts[1..].join(".")),
            )],
            Some(_) => vec![],
            None if p.label.contains("::") && self.e.language == "cpp" => {
                let mut keys = vec![self.key(target)];
                if self.includes.len() == 1 {
                    keys.push(header_key("static", &self.includes[0], target));
                }
                keys
            }
            None if target.contains('.')
                && matches!(
                    self.e.language,
                    "cpp" | "java" | "csharp" | "kotlin" | "swift"
                ) =>
            {
                // Qualified package/namespace syntax is explicit, unlike an unknown object.
                // A two-part unknown receiver could be a variable; require a type spelling.
                if parts.len() >= 3 || head.chars().next().is_some_and(char::is_uppercase) {
                    let q = if parts.len() == 2 {
                        format!("{}.{target}", self.scopes[&p.scope].namespace)
                    } else {
                        target.clone()
                    };
                    vec![if self.e.language == "cpp" {
                        self.key(&q)
                    } else {
                        format!("{}:static:{q}", self.e.language)
                    }]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }
    pub(super) fn finish(&mut self) {
        self.swift_callable_escapes();
        if matches!(self.e.language, "c" | "cpp") {
            let own = format!("@{}.", self.e.facts.path);
            let mut links = vec![];
            for node in &mut self.e.facts.nodes {
                let Some(q) = node.metadata["qualified_symbol"].as_str() else {
                    continue;
                };
                let symbol = q.strip_prefix(&own).unwrap_or(q).to_owned();
                let mut aliases: Vec<String> = node.metadata["binding_aliases"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                if is_header(&self.e.facts.path)
                    && node.binding_key.is_some()
                    && matches!(
                        node.kind.as_str(),
                        "class" | "struct" | "union" | "enum" | "type" | "function" | "method"
                    )
                {
                    aliases.push(header_key("symbol", &self.e.facts.path, &symbol));
                    if node.kind == "method" {
                        aliases.push(header_key(
                            if node.metadata["static"] == true {
                                "static"
                            } else {
                                "member"
                            },
                            &self.e.facts.path,
                            &symbol,
                        ));
                    }
                }
                if node.metadata["header_definition"] == true {
                    for header in &self.includes {
                        aliases.push(header_key("implementation", header, &symbol));
                        links.push(crate::model::Reference {
                            id: format!("declared_by:{}:{header}", node.id),
                            source: node.id.clone(),
                            label: symbol.clone(),
                            relation: "declared_by".into(),
                            file: self.e.facts.path.clone(),
                            line: node.line.unwrap_or(1),
                            candidate_keys: vec![header_key("declaration", header, &symbol)],
                            reason:
                                "definition must match a declaration in this exact quoted header"
                                    .into(),
                        });
                    }
                }
                if !aliases.is_empty() {
                    aliases.sort();
                    aliases.dedup();
                    node.metadata["binding_aliases"] = json!(aliases);
                }
            }
            self.e.facts.references.extend(links);
        }
        let pending = std::mem::take(&mut self.pending);
        let mut seen_types = HashSet::new();
        for p in pending {
            if p.context.is_some()
                && !seen_types.insert((
                    self.e.scopes[p.scope].owner.clone(),
                    p.node.start_byte(),
                    p.node.end_byte(),
                    p.context,
                ))
            {
                continue;
            }
            let callable = self
                .swift_callables
                .calls
                .get(&(p.scope, p.node.start_byte()))
                .filter(|_| p.relation == "calls");
            let proven = callable.and_then(|call| self.swift_callable_target(call));
            let keys = if callable.is_some() {
                proven
                    .map(|value| vec![value.key.clone()])
                    .unwrap_or_default()
            } else {
                self.resolve(&p)
            };
            let callable_evidence = callable.zip(proven).map(|(call, value)| {
                let local = &self.swift_callables.locals[&call.binding];
                json!({
                    "declaration_start_byte": local.declaration.0,
                    "declaration_end_byte": local.declaration.1,
                    "assignment_start_byte": call.assignment.0,
                    "assignment_end_byte": call.assignment.1,
                    "target_id": self.e.facts.nodes[value.node].id,
                })
            });
            let relation = if p.relation == "inherits"
                && matches!(self.e.language, "csharp" | "swift" | "kotlin")
                && !self.scopes[&p.scope].abstract_members
                && self
                    .e
                    .facts
                    .nodes
                    .iter()
                    .filter(|n| n.binding_key.as_ref().is_some_and(|key| keys.contains(key)))
                    .count()
                    == 1
                && self.e.facts.nodes.iter().any(|n| {
                    n.kind == "interface"
                        && n.binding_key.as_ref().is_some_and(|key| keys.contains(key))
                }) {
                "implements"
            } else {
                p.relation
            };
            let reason = p.context.map_or_else(|| "target is external, ambiguous, virtual, shadowed, or requires unsupported type/build information".to_owned(), |context| format!("{context}: type target is external, generic, shadowed, or ambiguous"));
            self.e.reference(
                p.node,
                p.scope,
                p.label.clone(),
                relation,
                keys.clone(),
                &reason,
            );
            if let Some(mut evidence) = callable_evidence {
                let owner = &self.e.scopes[p.scope].owner;
                evidence["reference_id"] = json!(self.e.facts.references.last().unwrap().id);
                if let Some(node) = self.e.facts.nodes.iter_mut().find(|n| &n.id == owner) {
                    if !node.metadata["swift_callable_values"].is_array() {
                        node.metadata["swift_callable_values"] = json!([]);
                    }
                    node.metadata["swift_callable_values"]
                        .as_array_mut()
                        .unwrap()
                        .push(evidence);
                }
            }
            if let Some(context) = p.context {
                let owner = &self.e.scopes[p.scope].owner;
                let id = self.e.facts.references.last().unwrap().id.clone();
                if let Some(node) = self.e.facts.nodes.iter_mut().find(|n| &n.id == owner) {
                    if !node.metadata["type_references"].is_array() {
                        node.metadata["type_references"] = json!([]);
                    }
                    node.metadata["type_references"].as_array_mut().unwrap().push(json!({"reference_id":id,"label":p.label,"context":context,"start_byte":p.node.start_byte(),"end_byte":p.node.end_byte(),"line":p.node.start_position().row+1,"candidate_keys":keys}));
                }
            }
        }
    }
}
