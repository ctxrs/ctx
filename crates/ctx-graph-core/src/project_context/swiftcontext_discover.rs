use super::*;

impl SwiftContext {
    pub(super) fn discover(
        inventory: &Inventory<'_>,
        modules: &BTreeMap<String, String>,
    ) -> Result<Self> {
        if modules.is_empty() {
            return Self::discover_packages(inventory);
        }
        // A configured map is the complete override, including its ambiguity barriers.
        let mut result = Self::default();
        let mut roots = BTreeMap::<String, Option<(String, String)>>::new();
        let mut inputs = vec!["swift-configured-context-1".to_owned()];
        for (name, directory) in modules {
            let mut chars = name.chars();
            ensure!(
                chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
                    && chars.all(|c| c == '_' || c.is_alphanumeric()),
                "Swift module name must be an identifier"
            );
            let directory = if directory == "." {
                ""
            } else {
                directory.as_str()
            };
            ensure!(
                !directory.contains(':') && join("", directory).as_deref() == Some(directory),
                "Swift module source directory must be a normalized repository-relative path"
            );
            let id = format!("configured-{}", digest([name.as_str(), directory]));
            inputs.extend([name.clone(), directory.to_owned()]);
            roots
                .entry(directory.to_owned())
                .and_modify(|root| *root = None)
                .or_insert(Some((name.clone(), id)));
        }
        result.imports = roots.values().flatten().cloned().collect();
        let files: BTreeSet<_> = inventory
            .files
            .iter()
            .filter(|p| p.ends_with(".swift"))
            .collect();
        for path in files {
            inputs.push(path.clone());
            if let Some((_, Some((_, module)))) = roots
                .iter()
                .filter(|(root, _)| within(path, root))
                .max_by_key(|(root, _)| root.len())
            {
                result.owners.insert(path.clone(), module.clone());
            }
        }
        result.fingerprint = digest(inputs.iter().map(String::as_str));
        Ok(result)
    }
    pub(super) fn discover_packages(inventory: &Inventory<'_>) -> Result<Self> {
        let mut result = Self::default();
        let mut packages = BTreeMap::<String, Option<Vec<SwiftTarget>>>::new();
        let mut evidence = vec!["swift-literal-packages-1".to_owned()];
        for path in &inventory.files {
            if !path.ends_with(".swift") {
                continue;
            }
            evidence.push(path.clone());
            let name = path.rsplit('/').next().unwrap();
            if name != "Package.swift" && !name.starts_with("Package@swift-") {
                continue;
            }
            // Version-specific manifests are barriers: choosing one requires a toolchain.
            let package = directory(path).to_owned();
            let metadata = std::fs::symlink_metadata(inventory.root.join(path))?;
            let targets = if metadata.len() > crate::parser::MAX_SOURCE_BYTES as u64 {
                result
                    .source_hashes
                    .insert(path.clone(), "oversized:4MiB".into());
                None
            } else if let Some(bytes) =
                inventory.read_bytes(path, crate::parser::MAX_SOURCE_BYTES as u64)?
            {
                result
                    .source_hashes
                    .insert(path.clone(), blake3::hash(&bytes).to_hex().to_string());
                if name == "Package.swift" {
                    std::str::from_utf8(&bytes)
                        .ok()
                        .and_then(|source| swift_package(source, &package))
                } else {
                    None
                }
            } else {
                // Discovery already inventoried this file; silently losing it would change ownership.
                anyhow::bail!(
                    "Swift manifest disappeared during discovery; retry indexing: {path}"
                );
            };
            packages
                .entry(package)
                .and_modify(|p| *p = None)
                .or_insert(targets);
        }
        for (path, hash) in &result.source_hashes {
            evidence.extend([path.clone(), hash.clone()]);
        }
        for (package, targets) in &packages {
            let Some(targets) = targets else { continue };
            let id = |name: &str| format!("swiftpm-{}", digest([package.as_str(), name]));
            for target in targets {
                result.target_imports.insert(
                    id(&target.name),
                    target
                        .dependencies
                        .iter()
                        .filter(|name| targets.iter().any(|t| &t.name == *name && !t.test))
                        .map(|name| (name.clone(), id(name)))
                        .collect(),
                );
            }
        }
        for path in inventory.files.iter().filter(|p| p.ends_with(".swift")) {
            let Some((_, Some(targets))) = packages
                .iter()
                .filter(|(dir, _)| within(path, dir))
                .max_by_key(|(dir, _)| dir.len())
            else {
                continue;
            };
            let matches: Vec<_> = targets.iter().filter(|t| t.contains(path)).collect();
            if let [target] = matches.as_slice() {
                result.owners.insert(path.clone(), target.id.clone());
            }
        }
        result.fingerprint = digest(evidence.iter().map(String::as_str));
        Ok(result)
    }
    pub(super) fn validate_source(&self, path: &str, hash: &str) -> Result<()> {
        ensure!(
            self.source_hashes
                .get(path)
                .is_none_or(|expected| expected == hash),
            "source changed during Swift manifest discovery; retry indexing: {path}"
        );
        Ok(())
    }
    pub(super) fn apply(&self, facts: &mut FileFacts) {
        if let Some(module) = self.owners.get(&facts.path) {
            let imports = self.target_imports.get(module).unwrap_or(&self.imports);
            crate::languages::compiled::apply_swift_context(facts, module, imports);
        }
    }
}
