use super::*;

impl ExtendedContext {
    pub fn discover(root: &std::path::Path, paths: &[String]) -> Result<Self> {
        anyhow::ensure!(
            std::fs::symlink_metadata(root)?.is_dir(),
            "source root must be a directory, not a symlink"
        );
        let mut facts = vec![];
        let ordered: std::collections::BTreeSet<_> = paths.iter().filter(|p| applies(p)).collect();
        for path in ordered {
            anyhow::ensure!(
                !path.starts_with('/')
                    && !path.contains(['\\', ':'])
                    && !path
                        .split('/')
                        .any(|p| p.is_empty() || p == "." || p == ".."),
                "source path must be a normalized relative POSIX path"
            );
            let mut cursor = root.to_path_buf();
            let mut eligible = true;
            for component in path.split('/') {
                cursor.push(component);
                match std::fs::symlink_metadata(&cursor) {
                    Ok(metadata) if !metadata.file_type().is_symlink() => {}
                    Ok(_) => {
                        eligible = false;
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        eligible = false;
                        break;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            if !eligible || !std::fs::symlink_metadata(&cursor)?.is_file() {
                facts.push(asset_facts(path, "unavailable"));
                continue;
            }
            let (hash, bytes) =
                ctx_graph_types::read_source(&cursor, crate::parser::MAX_SOURCE_BYTES as u64)?;
            let parsed = if let Some(bytes) = bytes {
                if matches!(path.rsplit('.').next(), Some("dfm" | "lfm")) {
                    Some(parse_pascal_form_bytes(path, &bytes, &hash)?)
                } else if let Ok(source) = std::str::from_utf8(&bytes) {
                    parse(path, source, &hash)?
                } else {
                    None
                }
            } else {
                None
            };
            facts.push(parsed.unwrap_or_else(|| asset_facts(path, &hash)));
        }
        Ok(Self::from_facts(&facts))
    }
    /// Build the same context from already parsed, bounded indexed inputs.
    pub fn from_facts(files: &[FileFacts]) -> Self {
        let mut result = Self::default();
        let mut hash = blake3::Hasher::new();
        hash.update(b"extended-context-v1\0");
        let mut ordered: Vec<_> = files.iter().filter(|f| applies(&f.path)).collect();
        ordered.sort_by(|a, b| a.path.cmp(&b.path));
        for facts in ordered {
            result
                .source_hashes
                .insert(facts.path.clone(), facts.hash.clone());
            hash.update(facts.path.as_bytes());
            hash.update(&[0]);
            hash.update(facts.hash.as_bytes());
            hash.update(&[0]);
            if !facts.diagnostics.is_empty() {
                continue;
            }
            let imports = result.imports.entry(facts.path.clone()).or_default();
            for reference in facts.references.iter().filter(|r| r.relation == "imports") {
                if reference.source.starts_with("objc:") {
                    imports.extend(
                        reference
                            .candidate_keys
                            .iter()
                            .filter_map(|k| k.strip_prefix("objc:file:").map(str::to_owned)),
                    );
                } else if reference.source.starts_with("pascal:") {
                    imports.push(reference.label.to_lowercase());
                }
            }
            for node in &facts.nodes {
                if node.metadata["objc_class"].is_string()
                    || node.metadata["objc_owner"].is_string()
                    || node.metadata["pascal_class"].is_string()
                    || node.metadata["pascal_unit"].is_string()
                {
                    result.nodes.insert(node.id.clone(), node.clone());
                }
            }
        }
        result.fingerprint = hash.finalize().to_hex().to_string();
        for node in result.nodes.values().filter(|n| project_definition(n)) {
            if let Some(key) = &node.binding_key {
                result
                    .bindings
                    .entry(key.clone())
                    .and_modify(|value| *value = None)
                    .or_insert_with(|| Some(project_key(node)));
            }
        }
        // First anchor ordinary interfaces; pair implementations with one
        // explicit imported/sibling interface, then attach category declarations.
        let classes: Vec<_> = result
            .nodes
            .values()
            .filter(|n| n.metadata["objc_class"].is_string())
            .cloned()
            .collect();
        for class in classes.iter().filter(|n| {
            n.metadata["objc_category"] != true && n.metadata["objc_role"] == "class_interface"
        }) {
            result.groups.insert(class.id.clone(), class.id.clone());
        }
        for class in classes.iter().filter(|n| {
            n.metadata["objc_category"] != true && n.metadata["objc_role"] == "class_implementation"
        }) {
            let Some(visible) = result.visible_files(&class.file) else {
                result.groups.insert(class.id.clone(), class.id.clone());
                continue;
            };
            let candidates: Vec<_> = classes
                .iter()
                .filter(|n| {
                    n.metadata["objc_role"] == "class_interface"
                        && n.metadata["objc_category"] != true
                        && n.label == class.label
                        && (visible.contains(&n.file)
                            || module_path(&n.file) == module_path(&class.file))
                })
                .collect();
            let anchor = if candidates.len() == 1 {
                candidates[0].id.clone()
            } else {
                class.id.clone()
            };
            result.groups.insert(class.id.clone(), anchor);
        }
        for class in classes
            .iter()
            .filter(|n| n.metadata["objc_category"] == true)
        {
            let Some(visible) = result.visible_files(&class.file) else {
                continue;
            };
            let stem = module_path(&class.file);
            let base_stem = stem.rsplit_once('+').map(|(base, _)| base);
            let candidates: HashSet<_> = classes
                .iter()
                .filter(|n| {
                    n.metadata["objc_category"] != true
                        && n.label == class.label
                        && (visible.contains(&n.file)
                            || base_stem.is_some_and(|s| module_path(&n.file) == s))
                })
                .filter_map(|n| result.groups.get(&n.id).cloned())
                .collect();
            if candidates.len() == 1 {
                result
                    .groups
                    .insert(class.id.clone(), candidates.into_iter().next().unwrap());
            }
        }
        result
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    /// Compare the raw bounded-reader hash, before any project decoration.
    pub fn validate_source(&self, path: &str, content_hash: &str) -> Result<()> {
        anyhow::ensure!(
            !applies(path)
                || self
                    .source_hashes
                    .get(path)
                    .is_some_and(|hash| hash == content_hash),
            "source changed during context discovery; retry indexing"
        );
        Ok(())
    }
    pub(super) fn visible_files(&self, path: &str) -> Option<HashSet<String>> {
        let mut seen = HashSet::new();
        let mut pending = vec![path.to_string()];
        while let Some(path) = pending.pop() {
            if !seen.insert(path.clone()) {
                continue;
            }
            if seen.len() > 4096 {
                // Incomplete evidence must also suppress sibling pairing.
                return None;
            }
            if let Some(imports) = self.imports.get(&path) {
                pending.extend(imports.iter().cloned());
            }
        }
        Some(seen)
    }
    pub(super) fn unique_objc_group(&self, file: &str, name: &str) -> Option<String> {
        let visible = self.visible_files(file)?;
        let groups: HashSet<_> = self
            .nodes
            .values()
            .filter(|n| n.metadata["objc_class"] == name && visible.contains(&n.file))
            .filter_map(|n| self.groups.get(&n.id).cloned())
            .collect();
        (groups.len() == 1).then(|| groups.into_iter().next().unwrap())
    }
    pub(super) fn objc_members(&self, group: &str, member: &str) -> Vec<&crate::model::Node> {
        self.nodes
            .values()
            .filter(|n| {
                n.metadata["objc_method"] == member
                    && n.metadata["objc_owner"]
                        .as_str()
                        .and_then(|owner| self.groups.get(owner))
                        .is_some_and(|g| g == group)
            })
            .collect()
    }
    pub(super) fn method_target<'a>(
        members: &[&'a crate::model::Node],
    ) -> Result<Option<&'a crate::model::Node>, ()> {
        let bodies: Vec<_> = members
            .iter()
            .copied()
            .filter(|n| n.metadata["body"] == true)
            .collect();
        let declarations: Vec<_> = members
            .iter()
            .copied()
            .filter(|n| n.metadata["body"] != true)
            .collect();
        if bodies.len() > 1 || declarations.len() > 1 {
            return Err(());
        }
        Ok(bodies.first().or(declarations.first()).copied())
    }
    pub(super) fn pascal_class(&self, file: &str, name: &str) -> Option<&crate::model::Node> {
        let name = name.to_lowercase();
        let (unit, name) = name
            .rsplit_once('.')
            .map_or((None, name.as_str()), |(u, n)| (Some(u), n));
        let local: Vec<_> = self
            .nodes
            .values()
            .filter(|n| {
                n.file == file
                    && n.metadata["pascal_class"] == name
                    && unit.is_none_or(|u| n.metadata["pascal_unit"] == u)
            })
            .collect();
        if !local.is_empty() {
            return (local.len() == 1).then_some(local[0]);
        }
        let mut candidates = vec![];
        for imported in self
            .imports
            .get(file)
            .into_iter()
            .flatten()
            .filter(|u| unit.is_none_or(|v| v == u.as_str()))
        {
            let units: Vec<_> = self
                .nodes
                .values()
                .filter(|n| n.kind == "module" && n.metadata["pascal_unit"] == imported.as_str())
                .collect();
            if units.len() != 1 {
                return None;
            }
            candidates.extend(
                self.nodes
                    .values()
                    .filter(|n| n.file == units[0].file && n.metadata["pascal_class"] == name),
            );
        }
        candidates.sort_by(|a, b| a.id.cmp(&b.id));
        candidates.dedup_by(|a, b| a.id == b.id);
        (candidates.len() == 1).then(|| candidates[0])
    }
    pub(super) fn pascal_base(
        &self,
        class: &crate::model::Node,
    ) -> Result<Option<&crate::model::Node>, ()> {
        let bases = class.metadata["pascal_bases"].as_array().ok_or(())?;
        if bases.is_empty() {
            return Ok(None);
        }
        if bases.len() != 1 {
            return Err(());
        }
        self.pascal_class(&class.file, bases[0].as_str().ok_or(())?)
            .map(Some)
            .ok_or(())
    }
    pub(super) fn pascal_member(
        &self,
        class: &crate::model::Node,
        name: &str,
        inherited: bool,
    ) -> Result<Option<&crate::model::Node>, ()> {
        let mut owner = if inherited {
            self.pascal_base(class)?
        } else {
            self.nodes.get(&class.id)
        };
        let mut seen = HashSet::new();
        while let Some(class) = owner {
            if seen.len() >= 256 || !seen.insert(class.id.clone()) {
                return Err(());
            }
            if class.metadata["pascal_shadowed_members"]
                .as_array()
                .is_some_and(|names| names.iter().any(|value| value == name))
            {
                return Err(());
            }
            let members: Vec<_> = self
                .nodes
                .values()
                .filter(|n| {
                    n.file == class.file
                        && n.metadata["pascal_method"] == name
                        && n.metadata["pascal_owner"]
                            .as_str()
                            .is_some_and(|v| v.rsplit('.').next() == Some(class.label.as_str()))
                })
                .collect();
            if !members.is_empty() {
                return Self::method_target(&members);
            }
            owner = self.pascal_base(class)?;
        }
        Ok(None)
    }
    pub fn apply(&self, facts: &mut FileFacts) {
        if !applies(&facts.path) || !facts.diagnostics.is_empty() {
            return;
        }
        for reference in &mut facts.references {
            for key in &mut reference.candidate_keys {
                if let Some(Some(replacement)) = self.bindings.get(key) {
                    *key = replacement.clone();
                }
            }
        }
        for node in &mut facts.nodes {
            if project_definition(node) && self.nodes.contains_key(&node.id) {
                node.binding_key = Some(project_key(node));
            }
        }
        let hints: Vec<_> = facts
            .nodes
            .first()
            .and_then(|n| n.metadata["member_navigation"].as_array())
            .cloned()
            .unwrap_or_default();
        for hint in hints {
            let Some(reference) = facts
                .references
                .iter_mut()
                .find(|r| Some(r.id.as_str()) == hint["reference"].as_str())
            else {
                continue;
            };
            let member = hint["member"].as_str().unwrap_or("");
            if hint["language"] == "objc" {
                let group = if hint["receiver"] == "self" {
                    hint["owner"]
                        .as_str()
                        .and_then(|id| self.groups.get(id).cloned())
                } else {
                    self.unique_objc_group(&facts.path, hint["receiver"].as_str().unwrap_or(""))
                };
                let target = group.and_then(|g| {
                    Self::method_target(&self.objc_members(&g, member))
                        .ok()
                        .flatten()
                });
                reference.candidate_keys = target.map(project_key).into_iter().collect();
            } else if hint["language"] == "pascal" {
                let target = self
                    .pascal_class(&facts.path, hint["class"].as_str().unwrap_or(""))
                    .map(|c| self.pascal_member(c, member, hint["inherited"] == true))
                    .unwrap_or(Err(()));
                match target {
                    Ok(Some(target)) => reference.candidate_keys = vec![project_key(target)],
                    Err(()) => reference.candidate_keys.clear(),
                    Ok(None) => {}
                }
            }
        }
        // Link separately retained declarations, never merge their identities.
        let own: Vec<_> = facts
            .nodes
            .iter()
            .filter(|n| self.nodes.contains_key(&n.id))
            .cloned()
            .collect();
        for node in own {
            if let Some(anchor) = self
                .groups
                .get(&node.id)
                .filter(|anchor| *anchor != &node.id)
                .and_then(|id| self.nodes.get(id))
            {
                project_reference(
                    facts,
                    &node,
                    anchor,
                    if node.metadata["objc_category"] == true {
                        "extends"
                    } else {
                        "implements"
                    },
                );
            }
            if let Some(owner) = node.metadata["objc_owner"]
                .as_str()
                .and_then(|id| self.groups.get(id))
                && node.metadata["body"] == true
            {
                let members =
                    self.objc_members(owner, node.metadata["objc_method"].as_str().unwrap_or(""));
                let declarations: Vec<_> = members
                    .into_iter()
                    .filter(|n| n.metadata["body"] != true)
                    .collect();
                if declarations.len() == 1 {
                    project_reference(facts, &node, declarations[0], "implements");
                }
            }
            if node.metadata["pascal_class"].is_string()
                && let Ok(Some(base)) = self.pascal_base(&node)
            {
                project_reference(facts, &node, base, "inherits");
            }
            if let Some(owner) = node.metadata["pascal_owner"].as_str()
                && let Some(class) = self.pascal_class(&node.file, owner)
                && class.file == node.file
            {
                project_reference(facts, class, &node, "method");
                if node.metadata["body"] == true {
                    let declarations: Vec<_> = self
                        .nodes
                        .values()
                        .filter(|n| {
                            n.file == node.file
                                && n.metadata["pascal_owner"] == owner
                                && n.metadata["pascal_method"] == node.metadata["pascal_method"]
                                && n.metadata["body"] != true
                        })
                        .collect();
                    if declarations.len() == 1 {
                        project_reference(facts, &node, declarations[0], "implements");
                    }
                }
            }
        }
        if matches!(facts.path.rsplit('.').next(), Some("dfm" | "lfm")) {
            let roots: Vec<_> = facts
                .nodes
                .iter()
                .filter(|n| {
                    n.kind == "component"
                        && n.qualified_name.as_ref().is_some_and(|q| !q.contains('.'))
                })
                .collect();
            if roots.len() != 1 {
                return;
            }
            let class_name = roots[0].metadata["class"]
                .as_str()
                .unwrap_or("")
                .to_lowercase();
            let classes: Vec<_> = self
                .nodes
                .values()
                .filter(|n| {
                    n.metadata["pascal_class"] == class_name
                        && module_path(&n.file) == module_path(&facts.path)
                })
                .collect();
            if classes.len() != 1 {
                return;
            }
            for reference in &mut facts.references {
                if reference.reason.starts_with("event property ") {
                    reference.candidate_keys = self
                        .pascal_member(classes[0], &reference.label.to_lowercase(), false)
                        .ok()
                        .flatten()
                        .map(project_key)
                        .into_iter()
                        .collect();
                }
            }
        }
    }
}
