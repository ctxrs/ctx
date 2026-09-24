use super::*;

impl Extractor<'_> {
    // Separate from public bindings: only same-frame, straight-line value
    // reads can use assignment history. Nothing here becomes an export alias.
    pub(super) fn collect_callable_flow(
        &mut self,
        definition: Syntax<'_>,
        body: Syntax<'_>,
        scope: usize,
    ) {
        let mut flow = CallableFlow::default();
        let mut plain = HashSet::new();
        if let Some(parameters) = definition.child_by_field_name("parameters") {
            flow.blocked.extend(
                parameter_names(parameters)
                    .into_iter()
                    .map(|n| identifier(self.text(n))),
            );
        }
        flow.opaque = definition.child(0).is_some_and(|n| n.kind() == "async");
        let mut cursor = body.walk();
        let statements: Vec<_> = body.named_children(&mut cursor).collect();
        for statement in &statements {
            if statement.kind() == "expression_statement"
                && let Some(assignment) = statement.named_child(0)
                && assignment.kind() == "assignment"
                && assignment.child_by_field_name("type").is_none()
                && let Some(left) = assignment.child_by_field_name("left")
                && left.kind() == "identifier"
                && let Some(right) = assignment.child_by_field_name("right")
                && right.kind() != "assignment"
            {
                plain.insert(assignment.id());
                flow.writes.push(CallableWrite {
                    start: assignment.start_byte(),
                    end: assignment.end_byte(),
                    name: identifier(self.text(left)),
                    value: (right.kind() == "identifier").then(|| identifier(self.text(right))),
                });
            }
        }
        if flow.writes.is_empty() {
            return;
        }
        for statement in statements {
            if !matches!(
                statement.kind(),
                "expression_statement" | "return_statement"
            ) {
                continue;
            }
            let mut pending = vec![statement];
            while let Some(node) = pending.pop() {
                if matches!(
                    node.kind(),
                    "lambda"
                        | "list_comprehension"
                        | "set_comprehension"
                        | "dictionary_comprehension"
                        | "generator_expression"
                        | "conditional_expression"
                        | "boolean_operator"
                ) {
                    continue;
                }
                if node.kind() == "call" {
                    flow.calls.insert(node.start_byte());
                }
                let mut cursor = node.walk();
                pending.extend(node.named_children(&mut cursor));
            }
        }
        // Reads in nested scopes are captures even without a nonlocal write:
        // an escaping closure may run after a later assignment. Value escapes
        // (arguments, returns, containers, member access) likewise veto history.
        let mut pending = vec![(body, false)];
        while let Some((node, nested)) = pending.pop() {
            let nested = nested
                || matches!(
                    node.kind(),
                    "function_definition"
                        | "class_definition"
                        | "lambda"
                        | "list_comprehension"
                        | "set_comprehension"
                        | "dictionary_comprehension"
                        | "generator_expression"
                );
            if matches!(node.kind(), "yield" | "await" | "exec_statement") {
                flow.opaque = true;
            }
            if node.kind() == "call"
                && let Some(function) = node.child_by_field_name("function")
                && let Some(parts) = self.call_parts(function)
                && parts.len() == 1
                && matches!(
                    identifier(&parts[0]).as_str(),
                    "eval" | "exec" | "globals" | "locals"
                )
            {
                flow.opaque = true;
            }
            if node.kind() == "identifier"
                && let Some(parent) = node.parent()
            {
                let field = |name| {
                    parent
                        .child_by_field_name(name)
                        .is_some_and(|n| n.id() == node.id())
                };
                let local_read = !nested
                    && ((parent.kind() == "call"
                        && field("function")
                        && flow.calls.contains(&parent.start_byte()))
                        || (plain.contains(&parent.id()) && (field("left") || field("right"))));
                if !(local_read || (parent.kind() == "attribute" && field("attribute"))) {
                    flow.blocked.insert(identifier(self.text(node)));
                }
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor).map(|n| (n, nested)));
        }
        // A copied function value can escape or be mutated through either
        // name. Propagate local-write vetoes through copies in both directions,
        // conservatively across rebindings too. Definition names alone are not
        // seeds: declaring a same-frame function is not an escape of its copies.
        let mut copies: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut pending = Vec::new();
        for write in &flow.writes {
            if flow.blocked.contains(&write.name) {
                pending.push(write.name.as_str());
            }
            if let Some(value) = write.value.as_deref() {
                copies.entry(&write.name).or_default().push(value);
                copies.entry(value).or_default().push(&write.name);
            }
        }
        let mut visited = HashSet::new();
        while let Some(name) = pending.pop() {
            if visited.insert(name) {
                flow.blocked.insert(name.to_owned());
                if let Some(neighbors) = copies.get(name) {
                    pending.extend(neighbors.iter().copied());
                }
            }
        }
        flow.writes.sort_by_key(|write| write.end);
        self.callable_flows.insert(scope, flow);
    }

    pub(super) fn callable_keys(&self) -> HashMap<usize, String> {
        let mut keys = HashMap::new();
        // Star-dependent definitions are published later by PythonContext.
        // A local value must not bypass that existing ambiguity proof.
        if self.callable_flows.is_empty() || !self.stars.is_empty() {
            return keys;
        }
        let mut counts = HashMap::new();
        for key in self
            .facts
            .nodes
            .iter()
            .filter_map(|n| n.binding_key.as_deref())
        {
            *counts.entry(key).or_insert(0_usize) += 1;
        }
        let functions: HashSet<_> = self
            .facts
            .nodes
            .iter()
            .filter(|n| {
                n.kind == "function"
                    && n.binding_key
                        .as_deref()
                        .is_some_and(|k| counts.get(k) == Some(&1))
            })
            .map(|n| n.id.as_str())
            .collect();
        let mut calls: HashMap<usize, Vec<usize>> = HashMap::new();
        for (index, call) in self.calls.iter().enumerate() {
            calls.entry(call.scope).or_default().push(index);
        }
        for (scope, flow) in &self.callable_flows {
            if flow.opaque || self.scopes[*scope].uncertain {
                continue;
            }
            let Some(calls) = calls.get_mut(scope) else {
                continue;
            };
            calls.sort_by_key(|index| self.calls[*index].start);
            let mut values: HashMap<&str, String> = HashMap::new();
            let mut writes = flow.writes.iter().peekable();
            for index in calls {
                let call = &self.calls[*index];
                while writes.peek().is_some_and(|write| write.end <= call.start) {
                    let write = writes.next().unwrap();
                    let key = write.value.as_deref().and_then(|name| {
                        values.get(name).cloned().or_else(|| {
                            self.callable_definition(*scope, name, write.start, &functions)
                        })
                    });
                    values.remove(write.name.as_str());
                    if !flow.blocked.contains(&write.name)
                        && let Some(key) = key
                    {
                        values.insert(&write.name, key);
                    }
                }
                if flow.calls.contains(&call.start) && call.parts.len() == 1 {
                    let name = identifier(&call.parts[0]);
                    if !(self.class_context(*scope) && private_name(&name))
                        && let Some(key) = values.get(name.as_str())
                    {
                        keys.insert(*index, key.clone());
                    }
                }
            }
        }
        keys
    }

    pub(super) fn callable_definition(
        &self,
        scope: usize,
        name: &str,
        position: usize,
        functions: &HashSet<&str>,
    ) -> Option<String> {
        if self.class_context(scope) && (private_name(name) || name == "__class__") {
            return None;
        }
        let mut current = Some(scope);
        while let Some(index) = current {
            let owner = &self.scopes[index];
            if owner.kind != ScopeKind::Class {
                if owner.uncertain {
                    return None;
                }
                if let Some(binding) = owner.bindings.get(name) {
                    // No captured/future function guess across execution frames.
                    return match binding {
                        Binding::Definition { id, key, start }
                            if (index == scope || owner.kind == ScopeKind::Module)
                                && *start <= position
                                && functions.contains(id.as_str()) =>
                        {
                            Some(key.clone())
                        }
                        _ => None,
                    };
                }
            }
            current = owner.parent;
        }
        None
    }

    pub(super) fn relative_module(&self, name: &str) -> Option<String> {
        let dots = name.bytes().take_while(|c| *c == b'.').count();
        if dots == 0 {
            return Some(name.into());
        }
        let mut package: Vec<_> = self
            .facts
            .module
            .split('.')
            .filter(|p| !p.is_empty())
            .collect();
        if !self.facts.path.ends_with("/__init__.py") && self.facts.path != "__init__.py" {
            package.pop();
        }
        if dots > package.len() {
            return None;
        }
        package.truncate(package.len() - dots + 1);
        if !name[dots..].is_empty() {
            package.push(&name[dots..]);
        }
        Some(package.join("."))
    }

    pub(super) fn import(&mut self, node: Syntax<'_>, scope: usize, conditional: bool) {
        let from = node.kind() == "import_from_statement";
        let module = node
            .child_by_field_name("module_name")
            .and_then(|n| self.relative_module(&identifier(self.text(n))));
        let module_label = node
            .child_by_field_name("module_name")
            .and_then(|n| self.relative_module(self.text(n)));
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "wildcard_import" {
                let keys = if scope == 0
                    && !conditional
                    && let Some(module) = &module
                {
                    self.stars
                        .push((module.clone(), node.end_byte(), line(node)));
                    vec![format!("module:{module}")]
                } else {
                    self.scopes[scope].uncertain = true;
                    vec![]
                };
                self.import_reference(
                    child,
                    scope,
                    "*".into(),
                    keys,
                    "star module is unavailable or ambiguous",
                );
            }
        }
        let mut cursor = node.walk();
        for item in node.children_by_field_name("name", &mut cursor) {
            let name_node = item.child_by_field_name("name").unwrap_or(item);
            let raw_name = self.text(name_node).to_owned();
            let name = identifier(&raw_name);
            let alias = item
                .child_by_field_name("alias")
                .map(|n| identifier(self.text(n)));
            let local = alias.clone().unwrap_or_else(|| {
                if from {
                    name.clone()
                } else {
                    name.split('.').next().unwrap().into()
                }
            });
            let (mut binding, mut keys) = if from {
                match &module {
                    Some(module) => {
                        let key = format!("python:{module}:{name}");
                        (
                            Binding::Symbol {
                                key: key.clone(),
                                start: node.end_byte(),
                            },
                            vec![key, format!("module:{module}.{name}")],
                        )
                    }
                    None => (Binding::Unknown, vec![]),
                }
            } else {
                (
                    Binding::Module {
                        module: name.clone(),
                        prefix: alias.unwrap_or_else(|| name.clone()),
                        start: node.end_byte(),
                    },
                    vec![format!("module:{name}")],
                )
            };
            if self.class_context(scope)
                && (name.split('.').any(private_name)
                    || module
                        .as_ref()
                        .is_some_and(|m| m.split('.').any(private_name)))
            {
                binding = Binding::Unknown;
                keys.clear();
            }
            self.bind(
                scope,
                local,
                if conditional {
                    Binding::Unknown
                } else {
                    binding
                },
            );
            let label = module_label
                .as_ref()
                .map_or_else(|| raw_name.clone(), |m| format!("{m}.{raw_name}"));
            self.import_reference(
                item,
                scope,
                label,
                keys,
                "import target is unavailable or ambiguous",
            );
        }
    }

    pub(super) fn import_reference(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        label: String,
        keys: Vec<String>,
        reason: &str,
    ) {
        self.facts.references.push(Reference {
            id: format!(
                "import:{}:{}-{}",
                self.scopes[scope].owner,
                node.start_byte(),
                node.end_byte()
            ),
            source: self.scopes[scope].owner.clone(),
            label,
            relation: "imports".into(),
            file: self.facts.path.clone(),
            line: line(node),
            candidate_keys: keys,
            reason: reason.into(),
        });
    }

    pub(super) fn dotted(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        dotted_text(node, self.source)
    }

    pub(super) fn call_parts(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        if node.kind() == "attribute"
            && let Some(object) = node.child_by_field_name("object")
            && object.kind() == "call"
            && object
                .child_by_field_name("function")
                .is_some_and(|n| n.kind() == "identifier" && self.text(n) == "super")
            && object
                .child_by_field_name("arguments")
                .is_some_and(|n| n.named_child_count() == 0)
        {
            return Some(vec![
                "super()".into(),
                self.text(node.child_by_field_name("attribute")?).into(),
            ]);
        }
        self.dotted(node)
    }

    pub(super) fn receiver(&self, scope: usize, name: &str) -> Option<(usize, usize)> {
        let mut current = Some(scope);
        while let Some(index) = current {
            let scope = &self.scopes[index];
            if scope.kind != ScopeKind::Class {
                if scope.uncertain {
                    return None;
                }
                if let Some(binding) = scope.bindings.get(name) {
                    return match binding {
                        Binding::Receiver { class, method } => Some((*class, *method)),
                        _ => None,
                    };
                }
            }
            current = scope.parent;
        }
        None
    }

    pub(super) fn receiver_key(
        &self,
        class: usize,
        method: usize,
        member: &str,
        super_call: bool,
    ) -> Option<String> {
        // The enclosing method must still be a verified descriptor. Rebinding
        // the method or a decorator builtin invalidates its implicit receiver.
        self.facts
            .nodes
            .iter()
            .find(|n| n.id == self.scopes[method].owner)?
            .binding_key
            .as_ref()?;
        let class = self
            .facts
            .nodes
            .iter()
            .find(|n| n.id == self.scopes[class].owner)?
            .binding_key
            .as_deref()?;
        Some(format!(
            "{}:{}.{}",
            if super_call {
                "python-super"
            } else {
                "python-receiver"
            },
            class.strip_prefix("python:")?,
            member
        ))
    }

    pub(super) fn call_key(&self, call: &PendingCall) -> Option<String> {
        let parts: Vec<_> = call.parts.iter().map(|part| identifier(part)).collect();
        let name = parts.first()?;
        if self.class_context(call.scope)
            && (name == "__class__" || parts.iter().any(|part| private_name(part)))
        {
            return None;
        }
        if name == "super()" && parts.len() == 2 {
            // Zero-argument super uses the *current* frame's first argument;
            // closures and anonymous scopes do not inherit that frame.
            let scope = &self.scopes[call.scope];
            if scope.uncertain || !self.stars.is_empty() {
                return None;
            }
            let (class, method) = scope.bindings.values().find_map(|b| match b {
                Binding::Receiver { class, method } => Some((*class, *method)),
                _ => None,
            })?;
            let mut current = Some(call.scope);
            while let Some(index) = current {
                let scope = &self.scopes[index];
                if scope.kind != ScopeKind::Class
                    && (scope.uncertain || scope.bindings.contains_key("super"))
                {
                    return None;
                }
                current = scope.parent;
            }
            return self.receiver_key(class, method, &parts[1], true);
        }
        let mut index = Some(call.scope);
        let mut deferred = false;
        while let Some(current) = index {
            let scope = &self.scopes[current];
            // Class namespaces are not lexical closures, including for nested classes.
            if current == call.scope || scope.kind != ScopeKind::Class {
                if scope.uncertain {
                    return None;
                }
                if let Some(binding) = scope.bindings.get(name) {
                    return match binding {
                        Binding::Receiver { class, method } if parts.len() == 2 => {
                            self.receiver_key(*class, *method, &parts[1], false)
                        }
                        Binding::Definition { key, start, .. } | Binding::Symbol { key, start }
                            if parts.len() == 1 && (deferred || *start <= call.start) =>
                        {
                            Some(key.clone())
                        }
                        Binding::Definition { key, id, start }
                            if parts.len() > 1
                                && (deferred || *start <= call.start)
                                && self
                                    .facts
                                    .nodes
                                    .iter()
                                    .any(|n| n.id == *id && n.kind == "class") =>
                        {
                            Some(format!(
                                "python-member:{}.{}",
                                key.strip_prefix("python:")?,
                                parts[1..].join(".")
                            ))
                        }
                        Binding::Symbol { key, start }
                            if parts.len() > 1 && (deferred || *start <= call.start) =>
                        {
                            Some(format!(
                                "python-member:{}.{}",
                                key.strip_prefix("python:")?,
                                parts[1..].join(".")
                            ))
                        }
                        Binding::Module {
                            module,
                            prefix,
                            start,
                        } if deferred || *start <= call.start => {
                            let dotted = parts.join(".");
                            let suffix = dotted.strip_prefix(&format!("{prefix}."))?;
                            Some(PythonContext::module_member(module, suffix))
                        }
                        _ => None,
                    }
                    .map(|key| {
                        // Keep the consuming module's binding name until context
                        // checks its stars. The provider key alone loses collisions
                        // with explicit imports, including module aliases and bases.
                        if scope.kind == ScopeKind::Module && !self.stars.is_empty() {
                            format!("python-local:{}:{name}:{key}", self.facts.module)
                        } else {
                            key
                        }
                    });
                }
            }
            if scope.kind == ScopeKind::Module && !self.stars.is_empty() {
                return (deferred || self.stars.iter().all(|(_, start, _)| *start <= call.start))
                    .then(|| PythonContext::module_member(&self.facts.module, &parts.join(".")));
            }
            deferred |= scope.kind == ScopeKind::Function;
            index = scope.parent;
        }
        None
    }

    pub(super) fn class_context(&self, scope: usize) -> bool {
        let mut index = Some(scope);
        while let Some(current) = index {
            if self.scopes[current].kind == ScopeKind::Class {
                return true;
            }
            index = self.scopes[current].parent;
        }
        false
    }

    pub(super) fn exports(&mut self) {
        // Only final, unambiguous module bindings are public import evidence.
        // __all__ governs star imports, not an explicit named import; do not
        // execute it or infer names from mutations of that value.
        let mut exports = BTreeMap::new();
        let mut blocked = BTreeSet::new();
        let mut definitions = BTreeMap::new();
        for (name, binding) in &self.scopes[0].bindings {
            let public = !name.starts_with('_')
                || self
                    .all
                    .as_array()
                    .is_some_and(|names| names.iter().any(|n| n.as_str() == Some(name)));
            let (key, start) = match binding {
                Binding::Symbol { key, start } if public => (key.clone(), *start),
                Binding::Module {
                    module,
                    prefix,
                    start,
                } if public && prefix == name => (format!("module:{module}"), *start),
                Binding::Definition { key, .. } => {
                    definitions.insert(name.clone(), key.clone());
                    continue;
                }
                _ => {
                    blocked.insert(name.clone());
                    continue;
                }
            };
            if self.scopes[0].uncertain {
                continue;
            }
            let line = self.source[..start].lines().count() as u32;
            exports.insert(name.clone(), json!({"target": key, "line": line}));
            let module = key
                .strip_prefix("module:")
                .or_else(|| {
                    key.strip_prefix("python:")
                        .and_then(|s| s.split_once(':').map(|(module, _)| module))
                })
                .unwrap();
            self.facts.references.push(Reference {
                id: format!("reexport:{}:{name}", self.scopes[0].owner),
                source: self.scopes[0].owner.clone(),
                label: name.clone(),
                relation: "re_exports".into(),
                file: self.facts.path.clone(),
                line,
                candidate_keys: vec![format!("module:{module}")],
                reason: "explicit public import target is unavailable or ambiguous".into(),
            });
        }
        for (target, start, line) in &self.stars {
            self.facts.references.push(Reference {
                id: format!("reexport-star:{}:{start}", self.scopes[0].owner),
                source: self.scopes[0].owner.clone(),
                label: "*".into(),
                relation: "re_exports".into(),
                file: self.facts.path.clone(),
                line: *line,
                candidate_keys: vec![format!("module:{target}")],
                reason: "static star module is unavailable or ambiguous".into(),
            });
        }
        let module = &mut self.facts.nodes[0];
        if module.metadata.is_null() {
            module.metadata = json!({});
        }
        module.metadata["python_exports"] = json!(exports);
        module.metadata["python_blocked_exports"] = json!(blocked);
        module.metadata["python_uncertain"] = json!(self.scopes[0].uncertain);
        module.metadata["python_all"] = self.all.clone();
        module.metadata["python_definitions"] = json!(definitions);
        module.metadata["python_stars"] =
            json!(self.stars.iter().map(|(m, _, _)| m).collect::<Vec<_>>());
    }
}
