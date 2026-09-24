use super::*;

impl Extractor<'_> {
    pub(super) fn finish(&mut self) {
        for (scope, name, id) in &self.builtin_methods {
            let mut current = Some(*scope);
            // Star imports can replace decorator builtins. Without inventory at
            // extraction time, do not certify the decorated method's identity.
            let mut shadowed = !self.stars.is_empty();
            while let Some(index) = current {
                shadowed |=
                    self.scopes[index].uncertain || self.scopes[index].bindings.contains_key(name);
                current = self.scopes[index].parent;
            }
            if shadowed {
                for binding in self.scopes[*scope].bindings.values_mut() {
                    if matches!(binding, Binding::Definition { id: bound, .. } if bound == id) {
                        *binding = Binding::Unknown;
                    }
                }
            }
        }
        let valid: HashMap<_, _> = self
            .scopes
            .iter()
            .filter(|s| !s.uncertain)
            .flat_map(|s| s.bindings.values())
            .filter_map(|b| {
                if let Binding::Definition { id, key, .. } = b {
                    Some((id.clone(), key.clone()))
                } else {
                    None
                }
            })
            .collect();
        for node in self.facts.nodes.iter_mut().filter(|n| n.kind != "module") {
            node.binding_key = valid.get(&node.id).cloned();
        }
        // Only verified class ownership exports member aliases. A nested function
        // with the same qualified spelling must never satisfy an imported member.
        for scope in &self.scopes {
            if scope.kind != ScopeKind::Class
                || !valid.contains_key(&scope.owner)
                || scope.uncertain
                || scope.qualified.split('.').any(private_name)
            {
                continue;
            }
            let mut ancestor = scope.parent;
            let mut uncertain = false;
            while let Some(index) = ancestor {
                let parent = &self.scopes[index];
                uncertain |= parent.uncertain
                    || (parent.kind != ScopeKind::Module && !valid.contains_key(&parent.owner));
                ancestor = parent.parent;
            }
            if uncertain {
                continue;
            }
            for binding in scope.bindings.values() {
                if let Binding::Definition { id, key, .. } = binding
                    && let Some(node) = self.facts.nodes.iter_mut().find(|n| {
                        n.id == *id
                            && n.kind == "method"
                            && n.binding_key.is_some()
                            && !private_name(&n.label)
                    })
                {
                    if node.metadata.is_null() {
                        node.metadata = json!({});
                    }
                    node.metadata["binding_aliases"] = json!([format!(
                        "python-member:{}",
                        key.strip_prefix("python:").unwrap()
                    )]);
                }
            }
        }
        if !self.scopes[0].uncertain
            && let Some((package, module)) = self.facts.module.rsplit_once('.')
        {
            for binding in self.scopes[0].bindings.values() {
                if let Binding::Definition { id, .. } = binding
                    && let Some(node) = self.facts.nodes.iter_mut().find(|n| {
                        n.id == *id
                            && matches!(n.kind.as_str(), "class" | "function")
                            && n.binding_key.is_some()
                    })
                {
                    if node.metadata.is_null() {
                        node.metadata = json!({});
                    }
                    node.metadata["binding_aliases"] = json!([format!(
                        "python-member:{package}:{module}.{}",
                        node.qualified_name.as_deref().unwrap()
                    )]);
                }
            }
        }
        for evidence in &self.evidence {
            let key = self.call_key(&evidence.site);
            if evidence.context == "decorator"
                && self.decorator_noise(&evidence.site, key.as_deref())
            {
                continue;
            }
            let reference_id = format!(
                "{}:{}:{}:{}-{}",
                evidence.relation,
                evidence.context,
                evidence.owner,
                evidence.site.start,
                evidence.site.end
            );
            if let Some(node) = self.facts.nodes.iter_mut().find(|n| n.id == evidence.owner) {
                if node.metadata.is_null() {
                    node.metadata = json!({});
                }
                if node.metadata.get("python_references").is_none() {
                    node.metadata["python_references"] = json!([]);
                }
                node.metadata["python_references"].as_array_mut().unwrap().push(json!({
                    "reference_id": reference_id, "context": evidence.context,
                    "line": evidence.site.line, "text": &self.source[evidence.site.start..evidence.site.end],
                }));
            }
            self.facts.references.push(Reference {
                id: reference_id,
                source: evidence.owner.clone(),
                label: evidence.site.parts.join("."),
                relation: evidence.relation.into(),
                file: self.facts.path.clone(),
                line: evidence.site.line,
                candidate_keys: key.into_iter().collect(),
                reason: format!(
                    "{}: target is unavailable, shadowed, or uncertain",
                    evidence.context
                ),
            });
        }
        let callable_keys = self.callable_keys();
        for (index, call) in self.calls.iter().enumerate() {
            let key = callable_keys
                .get(&index)
                .cloned()
                .or_else(|| self.call_key(call));
            let reference_id = format!(
                "call:{}:{}-{}",
                self.scopes[call.scope].owner, call.start, call.end
            );
            if key.as_ref().is_some_and(|k| {
                k.starts_with("python-receiver:") || k.starts_with("python-super:")
            }) {
                let node = self
                    .facts
                    .nodes
                    .iter_mut()
                    .find(|n| n.id == self.scopes[call.scope].owner)
                    .unwrap();
                if node.metadata.is_null() {
                    node.metadata = json!({});
                }
                if node.metadata.get("python_references").is_none() {
                    node.metadata["python_references"] = json!([]);
                }
                node.metadata["python_references"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({
                        "reference_id": reference_id, "context": "receiver_declaration",
                        "line": call.line, "text": &self.source[call.start..call.end],
                    }));
            }
            let reason = if key.is_some() {
                "static target is unavailable or ambiguous"
            } else {
                "dynamic, shadowed, or uncertain Python binding"
            };
            self.facts.references.push(Reference {
                id: reference_id,
                source: self.scopes[call.scope].owner.clone(),
                label: if call.parts.is_empty() {
                    "<dynamic call>".into()
                } else {
                    call.parts.join(".")
                },
                relation: "calls".into(),
                file: self.facts.path.clone(),
                line: call.line,
                candidate_keys: key.into_iter().collect(),
                reason: reason.into(),
            });
        }
        for (class, scope) in self
            .scopes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == ScopeKind::Class)
        {
            let bases: Vec<_> = self
                .evidence
                .iter()
                .filter(|e| e.owner == scope.owner && e.relation == "inherits")
                .map(|e| {
                    self.call_key(&e.site).or_else(|| {
                        if e.site.parts != ["object"] || !self.stars.is_empty() {
                            return None;
                        }
                        let mut current = Some(e.site.scope);
                        while let Some(index) = current {
                            let parent = &self.scopes[index];
                            if parent.uncertain || parent.bindings.contains_key("object") {
                                return None;
                            }
                            current = parent.parent;
                        }
                        Some("python-builtin:object".into())
                    })
                })
                .collect();
            let members: BTreeMap<_, _> = scope
                .bindings
                .iter()
                .map(|(name, binding)| {
                    let key = match binding {
                        Binding::Definition { key, id, .. } if valid.contains_key(id) => {
                            Some(key.clone())
                        }
                        _ => None,
                    };
                    (name.clone(), key)
                })
                .collect();
            if let Some(node) = self.facts.nodes.iter_mut().find(|n| n.id == scope.owner) {
                node.metadata["python_bases"] = json!(bases);
                node.metadata["python_members"] = json!(members);
                node.metadata["python_class_uncertain"] = json!(scope.uncertain);
                node.metadata["python_receiver_writes"] = json!(
                    self.receiver_writes
                        .get(&class)
                        .cloned()
                        .unwrap_or_default()
                );
            }
        }
        self.exports();
        if !self.stars.is_empty() {
            // File-local syntax cannot prove that a star leaves these names in
            // place. Keep the candidate inventory for context, but do not publish
            // keys or aliases to Store until the whole module set proves them.
            for node in &mut self.facts.nodes {
                if node.kind == "module" {
                    continue;
                }
                if let Some(key) = node.binding_key.take() {
                    let name = node
                        .qualified_name
                        .as_deref()
                        .unwrap()
                        .split('.')
                        .next()
                        .unwrap();
                    let aliases = node
                        .metadata
                        .get("binding_aliases")
                        .cloned()
                        .unwrap_or_else(|| json!([]));
                    if node.metadata.is_null() {
                        node.metadata = json!({});
                    }
                    node.metadata["python_pending_binding"] =
                        json!({"name": name, "key": key, "aliases": aliases});
                    node.metadata
                        .as_object_mut()
                        .unwrap()
                        .remove("binding_aliases");
                }
            }
        }
    }
}
