use super::*;

impl<'s, 't> Extended<'s, 't> {
    pub(super) fn owner_metadata(&self, mut scope: usize, field: &str) -> Option<String> {
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
    pub(super) fn annotate(&mut self, scope: usize, values: serde_json::Value) {
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
    pub(super) fn local_shadow(&self, mut scope: usize, name: &str) -> bool {
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
    pub(super) fn navigation_hint(
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
    pub(super) fn pascal_call(&mut self, n: Syntax<'t>, scope: usize, target: Syntax<'t>) {
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
    pub(super) fn objc_message(&mut self, n: Syntax<'t>, scope: usize) {
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
