use super::*;

impl<'a> Ruby<'a> {
    pub(super) fn new(e: Extractor<'a>) -> Self {
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
    pub(super) fn extract(mut self, root: Syntax<'_>) -> FileFacts {
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
    pub(super) fn constant(&self, n: Syntax<'_>, namespace: &str) -> Option<String> {
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
    pub(super) fn constants(&self, n: Syntax<'_>, namespace: &str) -> Vec<String> {
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
    pub(super) fn is_singleton(&self, scope: usize) -> bool {
        let mut current = Some(scope);
        while let Some(i) = current {
            if self.singleton.contains(&i) {
                return true;
            }
            current = self.e.scopes[i].parent;
        }
        false
    }
    pub(super) fn direct_declaration(n: Syntax<'_>) -> bool {
        n.parent().is_some_and(|p| {
            p.kind() == "program"
                || (p.kind() == "body_statement"
                    && p.parent().is_some_and(|owner| {
                        matches!(owner.kind(), "class" | "module")
                            && field(owner, "body").is_some_and(|body| body.id() == p.id())
                    }))
        })
    }
    pub(super) fn self_call(&mut self, n: Syntax<'_>, scope: usize) {
        self.self_calls.insert(format!(
            "call:{}:{}-{}",
            self.e.scopes[scope].owner,
            n.start_byte(),
            n.end_byte()
        ));
    }
    pub(super) fn set_visibility(
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
    pub(super) fn attribute_name(&self, n: Syntax<'_>) -> Option<String> {
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
    pub(super) fn attributes(
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
    pub(super) fn method(
        &mut self,
        n: Syntax<'_>,
        scope: usize,
        namespace: &str,
        owner: Option<&str>,
    ) {
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
}
