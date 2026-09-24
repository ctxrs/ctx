use super::*;

impl Javascript<'_> {
    pub(super) fn finish(mut self) -> FileFacts {
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
    pub(super) fn pattern(&mut self, node: Syntax<'_>, scope: usize, write: bool) {
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
    pub(super) fn write_this_property(&mut self, scope: usize) -> bool {
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
    pub(super) fn dotted(&self, node: Syntax<'_>) -> Option<Vec<String>> {
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
    pub(super) fn call(
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
    pub(super) fn single_factory_return<'a>(
        &self,
        node: Syntax<'a>,
    ) -> Option<(Syntax<'a>, Syntax<'a>)> {
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
}
