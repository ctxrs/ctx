use super::*;

impl CargoPackageContext {
    pub fn discover(root: &Path, paths: &[String]) -> Result<Self> {
        let root = root
            .canonicalize()
            .context("cannot locate Cargo project root")?;
        ensure!(root.is_dir(), "Cargo project root must be a directory");
        let mut result = Self::default();
        let mut hash = blake3::Hasher::new();
        hash.update(b"cargo-package-context-2");
        let mut manifests = BTreeMap::new();
        let mut workspaces = BTreeMap::new();
        let paths: BTreeSet<_> = paths
            .iter()
            .filter(|p| basename(p) == "Cargo.toml")
            .collect();
        for path in paths {
            ensure!(
                !path.contains(['\\', ':'])
                    && path.split('/').all(|p| !matches!(p, "" | "." | "..")),
                "Cargo inventory paths must be normalized repository-relative paths"
            );
            let (content_hash, source) = indexed_config_source(&root, path)?;
            for part in [path.as_str(), content_hash.as_str()] {
                hash.update(&(part.len() as u64).to_le_bytes());
                hash.update(part.as_bytes());
            }
            // An unreadable ancestor cannot prove membership in an outer workspace.
            manifests.insert(path.clone(), json!({"workspace":false}));
            let Some(source) = source else {
                continue;
            };
            let Some(facts) = parse(path, &source, &content_hash)? else {
                continue;
            };
            let Some(root) = facts.nodes.first().filter(|_| facts.diagnostics.is_empty()) else {
                continue;
            };
            let data = root.metadata.clone();
            if data["workspace"].is_object()
                && let (Some(members), Some(exclude)) = (
                    cargo_patterns(path, &data["workspace"]["members"]),
                    cargo_patterns(path, &data["workspace"]["exclude"]),
                )
            {
                workspaces.insert(path.clone(), CargoWorkspace { members, exclude });
            }
            if let Some(package) = facts
                .nodes
                .iter()
                .find(|n| n.kind == "package" && n.metadata["ecosystem"] == "cargo")
                && !package.label.is_empty()
                && !package.label.contains(['/', '\\', ':'])
            {
                result.packages.insert(
                    path.clone(),
                    CargoPackage {
                        name: package.label.clone(),
                        id: package.id.clone(),
                        workspace: None,
                        dependencies: vec![],
                    },
                );
            }
            manifests.insert(path.clone(), data);
        }
        for (path, package) in &mut result.packages {
            let data = &manifests[path];
            let workspace = if data["package"].get("workspace").is_some() {
                data["package"]["workspace"]
                    .as_str()
                    .and_then(|p| cargo_directory(path, p))
                    .map(|dir| cargo_manifest_path(&dir))
            } else {
                // A nested workspace, including an invalid one, is a boundary.
                manifests
                    .iter()
                    .filter(|(candidate, data)| {
                        !data["workspace"].is_null()
                            && (directory(candidate).is_empty()
                                || directory(path) == directory(candidate)
                                || directory(path)
                                    .starts_with(&format!("{}/", directory(candidate))))
                    })
                    .max_by_key(|(candidate, _)| directory(candidate).len())
                    .map(|(path, _)| path.clone())
            };
            package.workspace = workspace.filter(|workspace| {
                workspaces
                    .get(workspace)
                    .is_some_and(|w| w.includes(workspace, path))
            });
            if let Some(workspace) = &package.workspace {
                result
                    .members
                    .entry(workspace.clone())
                    .or_default()
                    .push(path.clone());
            }
        }
        let mut names = BTreeMap::new();
        for package in result.packages.values() {
            if let Some(workspace) = &package.workspace {
                *names
                    .entry((workspace.clone(), package.name.clone()))
                    .or_insert(0usize) += 1;
            }
        }
        let mut dependencies = BTreeMap::new();
        for (path, package) in &result.packages {
            let data = &manifests[path];
            let mut items = vec![];
            let mut tables = vec![(&data["dependencies"], false)];
            if let Some(targets) = data["target"].as_object() {
                tables.extend(targets.values().map(|v| (&v["dependencies"], true)));
            }
            for (table, conditional) in tables {
                let Some(table) = table.as_object() else {
                    continue;
                };
                for (alias, declaration) in table {
                    let inherited = declaration.get("workspace");
                    let missing = Value::Null;
                    let (base, spec) = if inherited == Some(&Value::Bool(true)) {
                        package
                            .workspace
                            .as_ref()
                            .and_then(|workspace| {
                                Some((
                                    workspace.as_str(),
                                    manifests.get(workspace)?["workspace"]["dependencies"]
                                        .get(alias)?,
                                ))
                            })
                            .unwrap_or((path.as_str(), &missing))
                    } else {
                        (path.as_str(), declaration)
                    };
                    let label = spec["package"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(alias)
                        .to_owned();
                    let optional =
                        |v: &Value| v.get("optional").map_or(Some(false), Value::as_bool);
                    // Preserve declared topology without evaluating optional activation.
                    // Malformed flags still cannot establish a dependency target.
                    let optional = optional(declaration)
                        .zip(optional(spec))
                        .map(|(local, inherited)| local || inherited);
                    let valid_inheritance = inherited.is_none()
                        || (inherited == Some(&Value::Bool(true))
                            && !["path", "package", "git", "registry", "version"]
                                .iter()
                                .any(|key| declaration.get(*key).is_some()));
                    let target = (!conditional
                        && valid_inheritance
                        && optional.is_some()
                        && spec.get("git").is_none()
                        && spec.get("registry").is_none()
                        && spec
                            .get("package")
                            .is_none_or(|v| v.as_str().is_some_and(|s| !s.is_empty())))
                    .then(|| {
                        spec["path"]
                            .as_str()
                            .and_then(|p| cargo_directory(base, p))
                            .map(|p| cargo_manifest_path(&p))
                    })
                    .flatten()
                    .filter(|target| {
                        let Some(workspace) = package.workspace.as_ref() else {
                            return false;
                        };
                        target != path
                            && result.packages.get(target).is_some_and(|p| {
                                p.name == label && p.workspace.as_ref() == Some(workspace)
                            })
                            && names.get(&(workspace.clone(), label.clone())) == Some(&1)
                            && names.get(&(workspace.clone(), package.name.clone())) == Some(&1)
                    });
                    items.push(CargoPackageDependency {
                        alias: alias.clone(),
                        label,
                        target,
                        optional: optional.unwrap_or(false),
                    });
                }
            }
            dependencies.insert(path.clone(), items);
        }
        for (path, dependencies) in dependencies {
            result.packages.get_mut(&path).unwrap().dependencies = dependencies;
        }
        result.fingerprint = hash.finalize().to_hex().to_string();
        Ok(result)
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Bind package facts only; Rust source import identities remain independent.
    pub fn apply(&self, facts: &mut FileFacts) {
        if basename(&facts.path) != "Cargo.toml" {
            return;
        }
        facts.edges.retain(|e| !e.id.starts_with("cargo-context:"));
        facts
            .references
            .retain(|r| !r.id.starts_with("cargo-context:"));
        if let Some(package) = self.packages.get(&facts.path)
            && let Some(node) = facts
                .nodes
                .iter_mut()
                .find(|n| n.id == package.id && n.label == package.name)
        {
            node.binding_key = Some(cargo_package_key(&facts.path, &package.name));
            node.metadata["workspace_manifest"] = json!(package.workspace);
            facts
                .references
                .retain(|r| r.source != package.id || r.relation != "depends_on");
            for (index, dependency) in package.dependencies.iter().enumerate() {
                let target = dependency
                    .target
                    .as_ref()
                    .and_then(|path| self.packages.get(path).map(|package| (path, package)));
                facts.references.push(Reference {
                    id: format!("cargo-context:{}:dependency:{index}", facts.path), source: package.id.clone(),
                    label: dependency.label.clone(), relation: "depends_on".into(), file: facts.path.clone(), line: 1,
                    candidate_keys: target.map(|(path, package)| vec![cargo_package_key(path, &package.name)]).unwrap_or_default(),
                    reason: "Cargo dependency is external, invalid, target-specific, or outside the indexed workspace".into(),
                });
                if let Some((_, target)) = target {
                    let mut metadata =
                        json!({"context":"cargo_dependency", "alias":dependency.alias});
                    if dependency.optional {
                        metadata["optional"] = json!(true);
                        metadata["activation"] = json!("not_evaluated");
                    }
                    facts.edges.push(Edge {
                        id: format!("cargo-context:{}:crate:{index}", facts.path),
                        source: package.id.clone(),
                        target: target.id.clone(),
                        relation: "crate_depends_on".into(),
                        directed: true,
                        file: Some(facts.path.clone()),
                        line: Some(1),
                        confidence: "static".into(),
                        metadata,
                    });
                }
            }
        }
        let root_key = format!("config:file:{}", facts.path);
        if let Some(members) = self.members.get(&facts.path)
            && let Some(root) = facts
                .nodes
                .iter()
                .find(|n| n.binding_key.as_ref() == Some(&root_key))
        {
            for member in members.iter().filter(|member| *member != &facts.path) {
                let package = &self.packages[member];
                facts.references.push(Reference {
                    id: format!("cargo-context:{}:member:{member}", facts.path),
                    source: root.id.clone(),
                    label: package.name.clone(),
                    relation: "contains".into(),
                    file: facts.path.clone(),
                    line: 1,
                    candidate_keys: vec![cargo_package_key(member, &package.name)],
                    reason: "Indexed Cargo workspace member is unavailable".into(),
                });
            }
        }
    }
}
