use super::*;

impl RustContext {
    pub(super) fn discover(inventory: &mut Inventory<'_>) -> Result<Self> {
        let mut result = Self::default();
        let sources: Vec<_> = inventory
            .files
            .iter()
            .filter(|p| p.ends_with(".rs"))
            .cloned()
            .collect();
        let directories: BTreeSet<_> = sources.iter().flat_map(|p| ancestors(p)).collect();
        let mut manifests = BTreeMap::new();
        for dir in directories {
            if let Some(source) = inventory.config(&join(&dir, "Cargo.toml").unwrap())? {
                manifests.insert(
                    dir,
                    source
                        .parse::<toml_edit::DocumentMut>()
                        .context("invalid Cargo.toml")?,
                );
            }
        }
        for (dir, manifest) in &manifests {
            let Some(name) = manifest
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(toml_edit::Item::as_str)
            else {
                continue;
            };
            let library = manifest
                .get("lib")
                .and_then(|l| l.get("name"))
                .and_then(toml_edit::Item::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| name.replace('-', "_"));
            let root = manifest
                .get("lib")
                .and_then(|l| l.get("path"))
                .and_then(toml_edit::Item::as_str)
                .unwrap_or("src/lib.rs");
            let autolib = manifest
                .get("package")
                .and_then(|p| p.get("autolib"))
                .and_then(toml_edit::Item::as_bool)
                != Some(false)
                || manifest.get("lib").is_some();
            let root = join(dir, root).filter(|p| autolib && inventory.files.contains(p));
            result.crates.insert(
                dir.clone(),
                RustCrate {
                    name: name.into(),
                    library,
                    root,
                    dependencies: BTreeMap::new(),
                    modules: BTreeSet::new(),
                    public_modules: BTreeSet::new(),
                    unavailable_modules: BTreeSet::new(),
                },
            );
        }
        for (dir, manifest) in &manifests {
            if !result.crates.contains_key(dir) {
                continue;
            }
            let explicit_workspace = manifest
                .get("package")
                .and_then(|p| p.get("workspace"))
                .and_then(toml_edit::Item::as_str)
                .and_then(|p| join(dir, p));
            let workspace = manifests
                .iter()
                .filter(|(base, m)| {
                    if m.get("workspace").is_none() {
                        return false;
                    }
                    if let Some(explicit) = &explicit_workspace {
                        return *base == explicit;
                    }
                    if *base == dir {
                        return true;
                    }
                    let relative = if base.is_empty() {
                        Some(dir.as_str())
                    } else {
                        dir.strip_prefix(&format!("{base}/"))
                    };
                    let members = toml_strings(m.get("workspace").and_then(|w| w.get("members")));
                    let excluded = toml_strings(m.get("workspace").and_then(|w| w.get("exclude")));
                    relative.is_some_and(|r| {
                        workspace_member(&members, r) && !workspace_member(&excluded, r)
                    })
                })
                .max_by_key(|(base, _)| base.len());
            let mut dependencies = BTreeMap::new();
            if let Some(table) = manifest
                .get("dependencies")
                .and_then(toml_edit::Item::as_table_like)
            {
                for (alias, item) in table.iter() {
                    if item.get("optional").and_then(toml_edit::Item::as_bool) == Some(true) {
                        continue;
                    }
                    let (base, dependency) =
                        if item.get("workspace").and_then(toml_edit::Item::as_bool) == Some(true) {
                            let Some((base, workspace)) = workspace else {
                                continue;
                            };
                            let Some(item) = workspace
                                .get("workspace")
                                .and_then(|w| w.get("dependencies"))
                                .and_then(|d| d.get(alias))
                            else {
                                continue;
                            };
                            (base, item)
                        } else {
                            (dir, item)
                        };
                    if dependency
                        .get("optional")
                        .and_then(toml_edit::Item::as_bool)
                        == Some(true)
                    {
                        continue;
                    }
                    let Some(path) = dependency
                        .get("path")
                        .and_then(toml_edit::Item::as_str)
                        .and_then(|p| join(base, p))
                    else {
                        continue;
                    };
                    let Some(target) = result.crates.get(&path).filter(|c| c.root.is_some()) else {
                        continue;
                    };
                    let package = dependency
                        .get("package")
                        .and_then(toml_edit::Item::as_str)
                        .unwrap_or(alias);
                    if package != target.name {
                        continue;
                    }
                    let import = if alias == target.name {
                        target.library.clone()
                    } else {
                        alias.replace('-', "_")
                    };
                    dependencies.insert(import, path);
                }
            }
            result.crates.get_mut(dir).unwrap().dependencies = dependencies;
        }
        for source in &sources {
            if let Some((owner, _)) = result
                .crates
                .iter()
                .filter(|(d, _)| within(source, d))
                .max_by_key(|(d, _)| d.len())
            {
                result.owners.insert(source.clone(), owner.clone());
            }
        }
        let roots: Vec<_> = result
            .crates
            .iter()
            .filter_map(|(dir, c)| c.root.as_ref().map(|root| (dir.clone(), root.clone())))
            .collect();
        for (package, root) in roots {
            result.visit_module(inventory, &package, &root, "", true, 0)?;
        }
        let evidence = result.public_uses(inventory)?;
        for (path, cached) in &result.facts {
            let mut applied = cached.clone();
            // Preview the same final pass used by indexing, including generic
            // owner checks. Keep the cached input and enrichment order intact.
            result.apply(&mut applied);
            result.fingerprints.insert(
                path.clone(),
                outcome_fingerprint("rust-output-v1", applied)?,
            );
        }
        let structure = format!("{:?}{:?}", result.modules, result.forwarding);
        // A repository without an explicit Rust module graph has no project
        // context for an unrelated file to invalidate. Store already rebinds
        // ordinary terminal keys from the changed/deleted file itself.
        let inventory_fingerprint = if result.modules.is_empty() {
            "rust-context-empty".to_owned()
        } else {
            inventory.config_fingerprint("rust-context-5")
        };
        result.fingerprint = digest([
            inventory_fingerprint.as_str(),
            structure.as_str(),
            evidence.as_str(),
        ]);
        Ok(result)
    }
    pub(super) fn public_uses(&mut self, inventory: &Inventory<'_>) -> Result<String> {
        let mut definitions = BTreeMap::new();
        let mut origins: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut exports: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut evidence = vec![];
        let module_paths: Vec<_> = self.modules.keys().cloned().collect();
        let mut parsed = BTreeMap::new();
        for path in module_paths {
            let (hash, bytes) = crate::index::read_source(
                &inventory.root.join(&path),
                crate::parser::MAX_SOURCE_BYTES as u64,
            )?;
            ensure!(
                self.source_hashes.get(&path) == Some(&hash),
                "source changed during Rust context discovery; retry indexing: {path}"
            );
            let Some(bytes) = bytes else { continue };
            evidence.push(format!("{path}:{hash}"));
            let Ok(source) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let Some(mut facts) = crate::languages::parse(&path, source, "context")? else {
                continue;
            };
            // Reuse the existing public-module boundary rules for each explicit alias.
            for node in &mut facts.nodes {
                if node.metadata["conditional"] != true
                    && let Some(key) = node.metadata["reexport_key"].as_str()
                {
                    node.binding_key = Some(key.into());
                }
            }
            parsed.insert(path, facts);
        }
        for (path, mut facts) in parsed {
            self.apply_paths(&mut facts);
            self.facts.insert(path, facts.clone());
            for node in facts.nodes {
                let keys: Vec<_> = node
                    .binding_key
                    .iter()
                    .cloned()
                    .chain(
                        node.metadata["binding_aliases"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned),
                    )
                    .collect();
                if node.kind == "reexport" {
                    let targets: BTreeSet<_> = facts
                        .references
                        .iter()
                        .filter(|r| r.source == node.id && r.relation == "reexports")
                        .flat_map(|r| r.candidate_keys.iter().cloned())
                        .collect();
                    if targets.len() == 1 {
                        for key in keys {
                            exports.entry(key).or_default().extend(targets.clone());
                        }
                    }
                } else {
                    for key in keys {
                        origins.entry(key).or_default().insert(node.id.clone());
                    }
                    definitions.insert(node.id.clone(), node);
                }
            }
        }
        let mut reverse: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (alias, targets) in exports {
            if targets.len() == 1 {
                reverse
                    .entry(targets.into_iter().next().unwrap())
                    .or_default()
                    .push(alias);
            }
        }
        let mut pending: VecDeque<_> = origins.keys().cloned().collect();
        while let Some(target) = pending.pop_front() {
            let values = origins[&target].clone();
            for alias in reverse.get(&target).into_iter().flatten() {
                let entry = origins.entry(alias.clone()).or_default();
                let mut changed = false;
                for id in &values {
                    if entry.len() < 2 {
                        changed |= entry.insert(id.clone());
                    }
                }
                if changed {
                    pending.push_back(alias.clone());
                }
            }
        }
        for (key, ids) in &origins {
            if ids.len() == 1 {
                let node = &definitions[ids.first().unwrap()];
                if matches!(node.kind.as_str(), "struct" | "enum" | "union")
                    && node.metadata["conditional"] != true
                    && let Some(arity) = node.metadata["generic_type_arity"].as_u64()
                    && let Some(package) = self.owners.get(&node.file)
                {
                    self.generic_owners
                        .insert((package.clone(), key.clone()), arity);
                }
            }
        }
        for (alias, targets) in &origins {
            if targets.len() != 1 {
                continue;
            }
            let id = targets.first().unwrap();
            if definitions[id].metadata["public"] == true
                && self.generic_impl_known(&definitions[id])
            {
                self.forwarding
                    .entry(id.clone())
                    .or_default()
                    .push(alias.clone());
            }
        }
        // A public inherent method or constant follows its exact concrete type, even when the
        // impl lives in another module. Trait dispatch and alias expansion are omitted.
        for method in definitions.values().filter(|n| {
            matches!(n.kind.as_str(), "method" | "constant") && n.metadata["public"] == true
        }) {
            if !self.generic_impl_known(method) {
                continue;
            }
            let Some(ty) = method.metadata["impl_type"].as_str() else {
                continue;
            };
            let Some(types) = origins.get(ty).filter(|ids| ids.len() == 1) else {
                continue;
            };
            let target = &definitions[types.first().unwrap()];
            if self.owners.get(&method.file) != self.owners.get(&target.file) {
                continue;
            }
            let aliases = self.forwarding.get(&target.id).cloned().unwrap_or_default();
            for alias in aliases {
                self.forwarding
                    .entry(method.id.clone())
                    .or_default()
                    .push(format!("{alias}::{}", method.label));
            }
        }
        for aliases in self.forwarding.values_mut() {
            aliases.sort();
            aliases.dedup();
        }
        let cached_paths: Vec<_> = self.facts.keys().cloned().collect();
        for path in cached_paths {
            if let Some(mut facts) = self.facts.remove(&path) {
                // Reexport keys are discovery inputs, not competing definitions.
                // Only their verified terminal targets publish these aliases.
                for node in &mut facts.nodes {
                    if node.kind == "reexport" {
                        node.binding_key = None;
                        node.metadata
                            .as_object_mut()
                            .unwrap()
                            .remove("binding_aliases");
                    }
                }
                self.apply_paths(&mut facts);
                self.facts.insert(path, facts);
            }
        }
        Ok(digest(evidence.iter().map(String::as_str)))
    }
    pub(super) fn visit_module(
        &mut self,
        inventory: &Inventory<'_>,
        package: &str,
        path: &str,
        module: &str,
        public: bool,
        depth: usize,
    ) -> Result<()> {
        if depth > 128 {
            return Ok(());
        }
        self.modules
            .entry(path.into())
            .or_default()
            .push(RustModule {
                package: package.into(),
                module: module.into(),
                public,
            });
        self.crates
            .get_mut(package)
            .unwrap()
            .modules
            .insert(module.into());
        if public {
            self.crates
                .get_mut(package)
                .unwrap()
                .public_modules
                .insert(module.into());
        }
        let (hash, bytes) = crate::index::read_source(
            &inventory.root.join(path),
            crate::parser::MAX_SOURCE_BYTES as u64,
        )?;
        ensure!(
            self.source_hashes
                .get(path)
                .is_none_or(|expected| expected == &hash),
            "source changed during Rust context discovery; retry indexing: {path}"
        );
        self.source_hashes.entry(path.into()).or_insert(hash);
        let Some(bytes) = bytes else {
            return Ok(());
        };
        let Ok(source) = std::str::from_utf8(&bytes) else {
            return Ok(());
        };
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
        let Some(tree) = parser.parse(source, None) else {
            return Ok(());
        };
        if tree.root_node().has_error() {
            return Ok(());
        }
        let child_dir =
            if self.crates[package].root.as_deref() == Some(path) || path.ends_with("/mod.rs") {
                directory(path).into()
            } else {
                stem(path).to_owned()
            };
        let mut pending = vec![(
            tree.root_node(),
            module.to_owned(),
            child_dir,
            public,
            depth,
        )];
        while let Some((body, parent_module, child_dir, parent_public, nesting)) = pending.pop() {
            if nesting > 128 {
                continue;
            }
            let mut skip = false;
            let mut cursor = body.walk();
            for item in body.named_children(&mut cursor) {
                if item.kind() == "attribute_item" {
                    let name = item
                        .named_child(0)
                        .and_then(|a| a.named_child(0))
                        .and_then(|n| n.utf8_text(&bytes).ok())
                        .unwrap_or("");
                    skip |= !matches!(name, "allow" | "warn" | "deny" | "doc" | "deprecated");
                    continue;
                }
                if item.kind() != "mod_item" {
                    skip = false;
                    continue;
                }
                let Some(name) = item
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(&bytes).ok())
                else {
                    continue;
                };
                let name = name.trim_start_matches("r#");
                let next_module = if parent_module.is_empty() {
                    name.into()
                } else {
                    format!("{parent_module}::{name}")
                };
                self.crates
                    .get_mut(package)
                    .unwrap()
                    .modules
                    .insert(next_module.clone());
                if skip {
                    skip = false;
                    continue;
                }
                let mut c = item.walk();
                let public = parent_public
                    && item.named_children(&mut c).any(|n| {
                        n.kind() == "visibility_modifier" && n.utf8_text(&bytes).ok() == Some("pub")
                    });
                if public {
                    self.crates
                        .get_mut(package)
                        .unwrap()
                        .public_modules
                        .insert(next_module.clone());
                }
                let next_dir = join(&child_dir, name).unwrap();
                if let Some(body) = item.child_by_field_name("body") {
                    pending.push((body, next_module, next_dir, public, nesting + 1));
                } else {
                    let candidates: Vec<_> =
                        [format!("{next_dir}.rs"), format!("{next_dir}/mod.rs")]
                            .into_iter()
                            .filter(|p| inventory.files.contains(p))
                            .collect();
                    if let [path] = candidates.as_slice() {
                        self.visit_module(
                            inventory,
                            package,
                            path,
                            &next_module,
                            public,
                            nesting + 1,
                        )?;
                    } else {
                        self.crates
                            .get_mut(package)
                            .unwrap()
                            .unavailable_modules
                            .insert(next_module);
                    }
                }
            }
        }
        Ok(())
    }
    pub(super) fn prefix(&self, package: &str) -> String {
        format!(
            "rust:package:{}:{}:",
            if package.is_empty() { "." } else { package },
            self.crates[package].library
        )
    }
    pub(super) fn generic_impl_known(&self, node: &Node) -> bool {
        let Some(arity) = node.metadata["generic_impl_arity"].as_u64() else {
            return true;
        };
        if node.metadata["conditional"] == true {
            return false;
        }
        let Some(ty) = node.metadata["generic_impl_type"].as_str() else {
            return false;
        };
        self.owners.get(&node.file).is_some_and(|package| {
            self.modules
                .get(&node.file)
                .is_some_and(|modules| modules.iter().any(|m| &m.package == package))
                && self.generic_owners.get(&(package.clone(), ty.into())) == Some(&arity)
        })
    }
    pub(super) fn apply(&self, facts: &mut FileFacts) {
        self.apply_paths(facts);
        let mut blocked = BTreeMap::new();
        for node in &mut facts.nodes {
            if !self.generic_impl_known(node) {
                node.binding_key = None;
                node.metadata
                    .as_object_mut()
                    .unwrap()
                    .remove("binding_aliases");
                if let Some(ty) = node.metadata["generic_impl_type"].as_str() {
                    blocked.insert(node.id.clone(), format!("{ty}::"));
                }
            }
        }
        // Keep ordinary function/type references in an unsupported impl; only
        // receiver calls that depended on its missing owner proof are removed.
        for reference in &mut facts.references {
            if reference.relation == "calls"
                && let Some(prefix) = blocked.get(&reference.source)
            {
                reference
                    .candidate_keys
                    .retain(|key| !key.starts_with(prefix));
            }
        }
    }
    pub(super) fn apply_paths(&self, facts: &mut FileFacts) {
        let Some(owner) = self.owners.get(&facts.path) else {
            return;
        };
        let Some(module_key) = facts
            .nodes
            .first()
            .and_then(|n| n.binding_key.as_deref())
            .and_then(|k| k.strip_prefix("rust:module:"))
        else {
            return;
        };
        let Some(native_root) = module_key.strip_suffix(&format!(":{}", facts.module)) else {
            return;
        };
        let native_prefix = format!("rust:{native_root}:");
        let native_module_prefix = format!("rust:module:{native_root}:");
        let local_prefix = if facts.module.is_empty() {
            String::new()
        } else {
            format!("{}::", facts.module)
        };
        let local_imports: BTreeSet<_> = facts
            .references
            .iter()
            .filter(|r| r.relation == "imports")
            .flat_map(|r| r.candidate_keys.iter())
            .filter(|k| k.starts_with(&native_prefix))
            .cloned()
            .collect();
        let mut dependencies = self.crates[owner].dependencies.clone();
        let library_source = self
            .modules
            .get(&facts.path)
            .is_some_and(|m| m.iter().any(|m| &m.package == owner));
        if !library_source && self.crates[owner].root.is_some() {
            dependencies.insert(self.crates[owner].library.clone(), owner.clone());
        }
        if !library_source {
            for node in &mut facts.nodes {
                if node.metadata["impl_type"].is_string() {
                    node.binding_key = None;
                    node.metadata["impl_context_unavailable"] = true.into();
                }
            }
        }
        for reference in &mut facts.references {
            if library_source {
                // A proven lexical module name still needs an actual source.
                // Keep orphan definitions navigable, but do not bind through a
                // declared child whose file choice discovery rejected.
                reference.candidate_keys.retain(|key| {
                    let suffix = key
                        .strip_prefix(&native_prefix)
                        .or_else(|| key.strip_prefix(&native_module_prefix));
                    !suffix.is_some_and(|suffix| {
                        self.crates[owner].unavailable_modules.iter().any(|module| {
                            (key.starts_with(&native_module_prefix) && suffix == module)
                                || suffix
                                    .strip_prefix(module)
                                    .is_some_and(|rest| rest.starts_with("::"))
                        })
                    })
                });
            }
            for key in &mut reference.candidate_keys {
                let external = if let Some(path) = key.strip_prefix("rust:external:") {
                    Some(path)
                } else if !reference.label.starts_with("crate::")
                    && !reference.label.starts_with("self::")
                    && !reference.label.starts_with("super::super::")
                    && !local_imports.contains(key)
                {
                    key.strip_prefix(&native_prefix)
                        .and_then(|p| p.strip_prefix(&local_prefix))
                        .filter(|p| p.contains("::"))
                } else {
                    None
                };
                let Some(external) = external else {
                    continue;
                };
                let (alias, suffix) = external.split_once("::").unwrap_or((external, ""));
                let local_name = format!("{local_prefix}{alias}");
                if !key.starts_with("rust:external:")
                    && (self.crates[owner].modules.contains(&local_name)
                        || facts.nodes.iter().any(|n| {
                            n.binding_key.as_deref()
                                == Some(&format!("{native_prefix}{local_name}"))
                        }))
                {
                    continue;
                }
                if let Some(target) = dependencies.get(alias) {
                    *key = format!("{}{suffix}", self.prefix(target));
                }
            }
        }
        if let Some(modules) = self.modules.get(&facts.path) {
            for node in &mut facts.nodes {
                let Some(key) = &node.binding_key else {
                    continue;
                };
                let module_node = key.starts_with(&native_module_prefix);
                if !module_node
                    && node.metadata.get("public").and_then(Value::as_bool) != Some(true)
                {
                    continue;
                }
                let suffix = key.strip_prefix(if module_node {
                    &native_module_prefix
                } else {
                    &native_prefix
                });
                let Some(suffix) = suffix.and_then(|s| {
                    if s == facts.module {
                        Some("")
                    } else {
                        s.strip_prefix(&local_prefix)
                    }
                }) else {
                    continue;
                };
                let aliases = modules
                    .iter()
                    .filter(|m| m.public)
                    .filter_map(|m| {
                        let name = if m.module.is_empty() {
                            suffix.into()
                        } else if suffix.is_empty() {
                            m.module.clone()
                        } else {
                            format!("{}::{suffix}", m.module)
                        };
                        let krate = &self.crates[&m.package];
                        let visible = krate
                            .modules
                            .iter()
                            .filter(|module| {
                                !module.is_empty()
                                    && (name == **module
                                        || name.starts_with(&format!("{module}::")))
                            })
                            .all(|module| krate.public_modules.contains(module));
                        visible.then(|| format!("{}{name}", self.prefix(&m.package)))
                    })
                    .collect();
                merge_aliases(node, aliases);
            }
        }
        for node in &mut facts.nodes {
            if let Some(aliases) = self.forwarding.get(&node.id) {
                merge_aliases(node, aliases.clone());
            }
        }
    }
}
