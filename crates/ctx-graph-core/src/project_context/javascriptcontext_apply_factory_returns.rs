use super::*;

impl JavascriptContext {
    pub(super) fn apply_factory_returns(&self, facts: &mut FileFacts) {
        let targets: BTreeMap<_, _> = facts
            .references
            .iter_mut()
            .filter_map(|reference| {
                if reference.relation != "declared_callee" || reference.candidate_keys.len() != 1 {
                    return None;
                }
                let target = self.factory_results.get(&reference.candidate_keys[0])?;
                let call = reference.id.strip_suffix(":declared_callee")?.to_owned();
                reference.reason =
                    "written immutable callee binding; invocation body proved by factory return"
                        .into();
                Some((call, target.clone()))
            })
            .collect();
        for reference in &mut facts.references {
            if reference.relation == "calls"
                && let Some(target) = targets.get(&reference.id)
            {
                reference.candidate_keys = vec![target.clone()];
                reference.reason = "immutable factory result; single returned callable body".into();
            }
        }
    }
    pub(super) fn apply_imported_callees(&self, facts: &mut FileFacts) {
        let mut ids: BTreeSet<_> = facts.references.iter().map(|r| r.id.clone()).collect();
        let mut siblings = vec![];
        for reference in &mut facts.references {
            if reference.relation != "calls"
                || !reference.candidate_keys.iter().any(|key| {
                    key.strip_prefix("javascript:file:")
                        .or_else(|| key.strip_prefix("javascript:cjs-file:"))
                        .and_then(|s| s.rsplit_once(':'))
                        .is_some_and(|(path, _)| path != facts.path)
                })
            {
                continue;
            }
            let decisions: Vec<_> = reference
                .candidate_keys
                .iter()
                .map(|key| self.imported_callees.get(key))
                .collect();
            if decisions.iter().all(Option::is_none) {
                continue;
            }
            let mut target = None;
            let unique = decisions.iter().all(|decision| {
                if let Some(Some(key)) = decision {
                    if target.is_some_and(|previous| previous != key) {
                        return false;
                    }
                    target = Some(key);
                    true
                } else {
                    false
                }
            });
            reference.candidate_keys.clear();
            if unique && let Some(target) = target {
                let mut sibling = reference.clone();
                sibling.id.push_str(":declared_callee");
                if ids.insert(sibling.id.clone()) {
                    sibling.relation = "declared_callee".into();
                    sibling.candidate_keys = vec![target.clone()];
                    sibling.reason = "written immutable callee binding; factory result and runtime dispatch are unresolved".into();
                    siblings.push(sibling);
                }
            }
        }
        facts.references.extend(siblings);
    }
    pub(super) fn apply_paths(&self, facts: &mut FileFacts) {
        let commonjs = self.commonjs(&facts.path);
        let native_module = if matches!(
            facts.path.rsplit('.').next(),
            Some("vue" | "svelte" | "astro")
        ) {
            facts.path.as_str()
        } else {
            stem(&facts.path)
        };
        for node in &mut facts.nodes {
            if !commonjs
                && let Some(aliases) = node
                    .metadata
                    .get_mut("binding_aliases")
                    .and_then(Value::as_array_mut)
            {
                aliases.retain(|a| !a.as_str().is_some_and(|a| a.starts_with("javascript:cjs:")));
            }
            let canonical = node
                .binding_key
                .as_deref()
                .into_iter()
                .chain(
                    node.metadata
                        .get("binding_aliases")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                )
                .filter_map(|key| {
                    if key == format!("javascript:module:{native_module}") {
                        Some(format!("javascript:file-module:{}", facts.path))
                    } else if let Some(name) =
                        key.strip_prefix(&format!("javascript:{native_module}:"))
                    {
                        Some(format!("javascript:file:{}:{name}", facts.path))
                    } else {
                        key.strip_prefix(&format!("javascript:cjs:{native_module}:"))
                            .map(|name| format!("javascript:cjs-file:{}:{name}", facts.path))
                    }
                })
                .collect();
            merge_aliases(node, canonical);
            let keys = node.binding_key.as_deref().into_iter().chain(
                node.metadata
                    .get("binding_aliases")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            );
            let aliases = keys
                .flat_map(|key| self.star_aliases.get(key).into_iter().flatten().cloned())
                .collect();
            merge_aliases(node, aliases);
        }
        for reference in &mut facts.references {
            let mut keys = vec![];
            let mut selected_relative_module = None;
            for key in &reference.candidate_keys {
                if key
                    .strip_prefix("javascript:file:")
                    .is_some_and(|rest| rest.contains(':'))
                {
                    keys.push(key.clone());
                    continue;
                }
                if !commonjs
                    && (key.starts_with("javascript:cjs")
                        || reference.reason.starts_with("CommonJS"))
                {
                    continue;
                }
                if let Some((specifier, symbol)) = key
                    .strip_prefix("javascript:cjs-import:")
                    .and_then(|p| p.rsplit_once(':'))
                {
                    keys.push(self.resolve(&facts.path, specifier, true).map_or_else(
                        || key.clone(),
                        |module| format!("javascript:cjs-file:{module}:{symbol}"),
                    ));
                } else if let Some((module, symbol)) = key
                    .strip_prefix("javascript:cjs:")
                    .and_then(|p| p.rsplit_once(':'))
                {
                    if let Some(target) = self.file(&facts.path, module)
                        && selected_relative_module
                            .as_deref()
                            .is_none_or(|selected| selected == target)
                    {
                        selected_relative_module = Some(target.clone());
                        let candidate = format!("javascript:cjs-file:{target}:{symbol}");
                        if !keys.contains(&candidate) {
                            keys.push(candidate);
                        }
                    }
                } else if let Some((specifier, symbol)) = key
                    .strip_prefix("javascript:import:")
                    .and_then(|p| p.rsplit_once(':'))
                {
                    if let Some(module) = self.resolve(&facts.path, specifier, false) {
                        keys.push(format!("javascript:file:{module}:{symbol}"));
                    } else {
                        keys.push(key.clone());
                    }
                } else if let Some(specifier) = key.strip_prefix("javascript:import-module:") {
                    if let Some(module) = self.resolve(
                        &facts.path,
                        specifier,
                        reference.reason.starts_with("CommonJS"),
                    ) {
                        keys.push(format!("javascript:file-module:{module}"));
                    } else if let Some(dependency) =
                        self.declared_dependency(&facts.path, specifier)
                    {
                        keys.push(dependency);
                    } else {
                        keys.push(key.clone());
                    }
                } else if let Some(module) = key.strip_prefix("javascript:module:") {
                    if selected_relative_module.is_none()
                        && let Some(target) = self.file(&facts.path, module)
                    {
                        selected_relative_module = Some(target.clone());
                        keys.push(format!("javascript:file-module:{target}"));
                    }
                } else if let Some((module, symbol)) = key
                    .strip_prefix("javascript:")
                    .and_then(|p| p.rsplit_once(':'))
                    .filter(|(m, _)| !m.starts_with("local:"))
                {
                    if let Some(target) = self.file(&facts.path, module)
                        && selected_relative_module
                            .as_deref()
                            .is_none_or(|selected| selected == target)
                    {
                        selected_relative_module = Some(target.clone());
                        let candidate = format!("javascript:file:{target}:{symbol}");
                        if !keys.contains(&candidate) {
                            keys.push(candidate);
                        }
                    }
                } else {
                    keys.push(key.clone());
                }
            }
            reference.candidate_keys = keys;
        }
    }
}
