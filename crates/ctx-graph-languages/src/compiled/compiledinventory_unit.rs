use super::*;

impl CompiledInventory {
    pub(super) fn unit(&self, node: &crate::model::Node) -> String {
        if let Some(module) = node.metadata["swift_module"].as_str() {
            return format!("swift:{module}");
        }
        self.units
            .get(&node.file)
            .cloned()
            .unwrap_or_else(|| format!("file:{}", node.file))
    }
    pub(super) fn canonical(&self, from: &crate::model::Node, key: &str) -> Option<String> {
        let normalized = context_key(&from.file, key);
        let group = self
            .types
            .get(&(self.unit(from), normalized.clone()))
            .or_else(|| {
                // Export keys already encode exact Swift module identity and visibility.
                normalized.starts_with("swift:").then_some(())?;
                if !normalized.contains(":export:") {
                    return None;
                }
                self.types.get(&(String::new(), normalized))
            })?;
        if group.len() == 1
            || group
                .iter()
                .all(|id| self.nodes[id].metadata["partial"] == true)
        {
            group.first().cloned()
        } else {
            None
        }
    }
    pub(super) fn group(&self, id: &str) -> Vec<String> {
        let node = &self.nodes[id];
        let Some(key) = node.binding_key.as_ref() else {
            return vec![id.into()];
        };
        self.types
            .get(&(self.unit(node), context_key(&node.file, key)))
            .cloned()
            .unwrap_or_else(|| vec![id.into()])
    }
    pub(super) fn member(
        &self,
        id: &str,
        name: &str,
        static_call: bool,
        seen: &mut HashSet<String>,
    ) -> Option<String> {
        if seen.len() >= 64 || !seen.insert(id.into()) {
            return None;
        }
        let group = self.group(id);
        let own: Vec<_> = self
            .nodes
            .values()
            .filter(|n| {
                n.label == name && self.parents.get(&n.id).is_some_and(|p| group.contains(p))
            })
            .collect();
        if !own.is_empty() {
            return (own.len() == 1
                && matches!(own[0].kind.as_str(), "method" | "function" | "declaration")
                && (!static_call || own[0].metadata["static"] == true)
                && own[0].metadata["member_accessible"] == true
                && own[0].metadata["declaration_certain"] == true
                && self.inherited_name_absent(id, name, &mut HashSet::new()))
            .then(|| own[0].id.clone());
        }
        if static_call {
            let companions: Vec<_> = self
                .nodes
                .values()
                .filter(|n| {
                    n.metadata["companion"] == true
                        && self.parents.get(&n.id).is_some_and(|p| group.contains(p))
                })
                .collect();
            if companions.len() == 1 {
                return self.member(&companions[0].id, name, true, seen);
            }
        }
        let mut found = HashSet::new();
        if !self.hierarchy_known(id, &mut HashSet::new()) {
            return None;
        }
        for part in group {
            let bases = self.bases.get(&part)?.as_ref()?;
            if bases.len() > 1 {
                return None;
            }
            for base in bases {
                if let Some(member) = self.member(base, name, static_call, &mut seen.clone()) {
                    found.insert(member);
                } else if self.bases.get(base).is_some_and(Option::is_none) {
                    return None;
                }
            }
        }
        (found.len() == 1).then(|| found.into_iter().next().unwrap())
    }
    // A nearest declaration does not prove that an ancestor's overload is
    // inapplicable. Without signature resolution, repeated inherited names
    // remain ambiguous. Implemented interface contracts are not class overloads.
    pub(super) fn inherited_name_absent(
        &self,
        id: &str,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> bool {
        if seen.len() >= 64 || !seen.insert(id.into()) {
            return false;
        }
        self.group(id).iter().all(|part| {
            self.bases
                .get(part)
                .and_then(Option::as_ref)
                .is_some_and(|bases| {
                    bases.iter().all(|base| {
                        if self.nodes[id].kind != "interface"
                            && self.nodes[base].kind == "interface"
                        {
                            return true;
                        }
                        let group = self.group(base);
                        !self.nodes.values().any(|n| {
                            n.label == name
                                && self.parents.get(&n.id).is_some_and(|p| group.contains(p))
                        }) && self.inherited_name_absent(base, name, &mut seen.clone())
                    })
                })
        })
    }
    // Declaration navigation can retain separate known branches even when a
    // call cannot select one. An overload or uncertain branch poisons this
    // lookup; repeated names only shadow proven parameterless declarations.
    pub(super) fn declared_members(
        &self,
        id: &str,
        name: &str,
        static_call: bool,
        seen: &mut HashSet<String>,
    ) -> Option<Vec<String>> {
        if seen.len() >= 64 || !seen.insert(id.into()) {
            return None;
        }
        let group = self.group(id);
        let mut inherited = vec![];
        for part in &group {
            for base in self.bases.get(part)?.as_ref()? {
                inherited.extend(self.declared_members(
                    base,
                    name,
                    static_call,
                    &mut seen.clone(),
                )?);
            }
        }
        inherited.sort();
        inherited.dedup();
        let own: Vec<_> = self
            .nodes
            .values()
            .filter(|n| {
                n.label == name && self.parents.get(&n.id).is_some_and(|p| group.contains(p))
            })
            .collect();
        if own.is_empty() {
            return Some(inherited);
        }
        if own.len() != 1
            || !matches!(own[0].kind.as_str(), "method" | "function" | "declaration")
            || (static_call && own[0].metadata["static"] != true)
            || own[0].metadata["member_accessible"] != true
            || own[0].metadata["declaration_certain"] != true
        {
            return None;
        }
        if !inherited.is_empty()
            && (own[0].metadata["parameterless"] != true
                || inherited.iter().any(|id| {
                    self.nodes[id].metadata["parameterless"] != true
                        || self.nodes[id].metadata["static"] != own[0].metadata["static"]
                }))
        {
            return None;
        }
        Some(vec![own[0].id.clone()])
    }
    pub(super) fn hierarchy_known(&self, id: &str, seen: &mut HashSet<String>) -> bool {
        if seen.len() >= 64 || !seen.insert(id.into()) {
            return false;
        }
        self.group(id).iter().all(|part| {
            self.bases
                .get(part)
                .and_then(Option::as_ref)
                .is_some_and(|bases| {
                    bases
                        .iter()
                        .all(|base| self.hierarchy_known(base, &mut seen.clone()))
                })
        })
    }
    pub(super) fn navigation_key(&self, id: &str) -> String {
        let n = &self.nodes[id];
        let unit = self.unit(n);
        let owner = self.parents.get(id).and_then(|p| self.nodes.get(p));
        let identity = if matches!(n.kind.as_str(), "method" | "function" | "declaration") {
            owner
                .and_then(|n| n.binding_key.clone())
                .map(|key| format!("{}.{}", context_key(&n.file, &key), n.label))
        } else {
            n.binding_key.as_ref().map(|key| context_key(&n.file, key))
        };
        format!(
            "compiled:declaration:{}:{unit}:{}",
            unit.len(),
            identity.unwrap_or_else(|| format!(
                "{}:{}",
                n.file,
                n.qualified_name.as_deref().unwrap_or(&n.label)
            ))
        )
    }
}
