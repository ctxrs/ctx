use super::*;

impl ProjectContext {
    pub fn discover_with_swift_modules(
        root: &Path,
        paths: &[String],
        modules: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let mut inventory = Inventory::new(root, paths);
        let mut result = Self {
            swift: SwiftContext::discover(&inventory, modules)?,
            ..Self::default()
        };
        let ordered: BTreeSet<_> = paths.iter().cloned().collect();
        for path in ordered.iter().filter(|p| p.ends_with(".go")) {
            let directory = path.rsplit_once('/').map_or(".", |(p, _)| p);
            let source_path = root.join(path);
            let meta = std::fs::symlink_metadata(&source_path)?;
            if !meta.is_file() || meta.len() > crate::parser::MAX_SOURCE_BYTES as u64 {
                continue;
            }
            let (_, bytes) =
                crate::index::read_source(&source_path, crate::parser::MAX_SOURCE_BYTES as u64)?;
            let Some(bytes) = bytes else {
                continue;
            };
            let Ok(source) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&tree_sitter_go::LANGUAGE.into())?;
            let tree = parser
                .parse(source, None)
                .context("cannot parse Go package identity")?;
            let mut cursor = tree.root_node().walk();
            let package = tree
                .root_node()
                .named_children(&mut cursor)
                .find(|n| n.kind() == "package_clause")
                .and_then(|n| n.named_child(0))
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(str::to_owned);
            let Some(package) = package else {
                continue;
            };
            // Reuse this AST read to establish package-level owner uniqueness.
            // Do not descend into functions: their local types are different owners.
            let mut type_counts = BTreeMap::new();
            let mut cursor = tree.root_node().walk();
            for declaration in tree
                .root_node()
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "type_declaration")
            {
                let mut cursor = declaration.walk();
                for ty in declaration
                    .named_children(&mut cursor)
                    .filter(|n| matches!(n.kind(), "type_spec" | "type_alias"))
                {
                    if let Some(name) = ty
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    {
                        *type_counts.entry(name.to_owned()).or_insert(0usize) += 1;
                    }
                }
            }
            let identity = (directory.to_owned(), package.clone());
            if let Some(existing) = result.go.get_mut(&identity) {
                existing.production |= !path.ends_with("_test.go");
                for (name, count) in type_counts {
                    *existing.type_counts.entry(name).or_default() += count;
                }
                continue;
            }
            let mut cursor = Path::new(if directory == "." { "" } else { directory });
            let mut import_path = None;
            let mut manifest_bytes = Vec::new();
            loop {
                let manifest = root.join(cursor).join("go.mod");
                match std::fs::symlink_metadata(&manifest) {
                    Ok(meta) => {
                        ensure!(
                            meta.is_file() && !meta.file_type().is_symlink(),
                            "go.mod must be a regular file"
                        );
                        ensure!(meta.len() <= 1024 * 1024, "go.mod exceeds 1 MiB");
                        manifest_bytes = crate::index::read_source(&manifest, 1024 * 1024)?
                            .1
                            .context("go.mod exceeds 1 MiB")?;
                        let source =
                            std::str::from_utf8(&manifest_bytes).context("go.mod is not UTF-8")?;
                        if let Some(module) = source.lines().find_map(|line| {
                            let line = line.split("//").next().unwrap_or("").trim();
                            line.strip_prefix("module")
                                .filter(|s| s.starts_with(char::is_whitespace))
                                .map(|s| s.trim().trim_matches('"').to_owned())
                        }) {
                            ensure!(
                                !module.is_empty() && !module.contains(char::is_whitespace),
                                "invalid Go module name"
                            );
                            let suffix = Path::new(if directory == "." { "" } else { directory })
                                .strip_prefix(cursor)?;
                            let suffix = suffix
                                .to_str()
                                .context("Go package path must be UTF-8")?
                                .replace('\\', "/");
                            import_path = Some(if suffix.is_empty() {
                                module
                            } else {
                                format!("{module}/{suffix}")
                            });
                        }
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e).context("cannot inspect go.mod"),
                }
                let Some(parent) = cursor.parent() else {
                    break;
                };
                cursor = parent;
            }
            let mut hash = blake3::Hasher::new();
            hash.update(path.as_bytes());
            hash.update(&[0]);
            hash.update(&manifest_bytes);
            result.go.insert(
                identity,
                GoPackage {
                    owner: path.clone(),
                    import_path,
                    production: !path.ends_with("_test.go"),
                    fingerprint: hash.finalize().to_hex().to_string(),
                    type_counts,
                },
            );
        }
        result.javascript = JavascriptContext::discover(&mut inventory)?;
        result.rust = RustContext::discover(&mut inventory)?;
        result.templates = TemplateContext::discover(&mut inventory)?;
        result.terraform = crate::languages::configs::TerraformContext::discover(root, paths)?;
        result.cargo_packages =
            crate::languages::configs::CargoPackageContext::discover(root, paths)?;
        let (files, units) = result.compiled_inventory(&inventory)?;
        result.compiled = crate::languages::compiled::CompiledContext::new(&files, &units);
        result.extended = crate::languages::extended::ExtendedContext::discover(root, paths)?;
        Ok(result)
    }

    pub(super) fn compiled_inventory(
        &mut self,
        inventory: &Inventory<'_>,
    ) -> Result<(Vec<FileFacts>, BTreeMap<String, String>)> {
        let mut files = vec![];
        let mut units = BTreeMap::new();
        // The selected index root is an analysis unit, not compiler build proof.
        // Known split markers disable this fallback for the entire family. No
        // target lists, manifests, Gradle scripts or compiler commands are evaluated.
        let jvm_split = inventory.files.iter().any(|path| {
            let name = path.rsplit('/').next().unwrap();
            matches!(name, "settings.gradle" | "settings.gradle.kts")
                || (!directory(path).is_empty()
                    && matches!(
                        name,
                        "pom.xml" | "build.gradle" | "build.gradle.kts" | "module-info.java"
                    ))
        });
        let cpp_split = inventory.files.iter().any(|path| {
            !directory(path).is_empty() && path.rsplit('/').next() == Some("CMakeLists.txt")
        });
        for path in inventory
            .files
            .iter()
            .filter(|path| crate::languages::compiled::supports(path))
        {
            let metadata = std::fs::symlink_metadata(inventory.root.join(path))?;
            if !metadata.is_file() {
                continue;
            }
            if metadata.len() > crate::parser::MAX_SOURCE_BYTES as u64 {
                self.compiled_source_hashes
                    .insert(path.clone(), "oversized:4MiB".into());
                continue;
            }
            let Some(bytes) = inventory.read_bytes(path, crate::parser::MAX_SOURCE_BYTES as u64)?
            else {
                continue;
            };
            let hash = blake3::hash(&bytes).to_hex().to_string();
            self.templates.validate_source(path, &hash)?;
            self.swift.validate_source(path, &hash)?;
            self.compiled_source_hashes.insert(path.clone(), hash);
            let Ok(source) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let Some(mut facts) = crate::languages::parse(path, source, "context")? else {
                continue;
            };
            self.swift.apply(&mut facts);
            if path.ends_with(".cs") {
                self.templates.apply(&mut facts);
            }
            let language = facts
                .nodes
                .first()
                .and_then(|n| n.metadata["language"].as_str());
            let unit = match language {
                Some("csharp") => self.templates.unit(path),
                Some("java" | "kotlin") if !jvm_split => Some("index-root:jvm".into()),
                Some("cpp") if !cpp_split => Some("index-root:cpp".into()),
                _ => None,
            };
            if let Some(unit) = unit {
                units.insert(path.clone(), unit);
            }
            files.push(facts);
        }
        Ok((files, units))
    }

    /// Compare the raw read_source hash before stamping or skipping unchanged files.
    /// These byte hashes protect one discovery snapshot; they are not cache stamps.
    pub fn validate_source(&self, path: &str, content_hash: &str) -> Result<()> {
        self.javascript.validate_source(path, content_hash)?;
        ensure!(
            self.rust
                .source_hashes
                .get(path)
                .is_none_or(|expected| expected == content_hash),
            "source changed during Rust context discovery; retry indexing: {path}"
        );
        ensure!(
            self.compiled_source_hashes
                .get(path)
                .is_none_or(|expected| expected == content_hash),
            "source changed during compiled context discovery; retry indexing: {path}"
        );
        self.templates.validate_source(path, content_hash)?;
        self.swift.validate_source(path, content_hash)?;
        self.extended.validate_source(path, content_hash)?;
        Ok(())
    }

    pub(crate) fn take_cached_facts(
        &mut self,
        path: &str,
        content_hash: &str,
    ) -> Result<Option<FileFacts>> {
        self.javascript.validate_source(path, content_hash)?;
        if let Some(facts) = self.javascript.raw_facts.remove(path) {
            return Ok(Some(facts));
        }
        ensure!(
            self.rust
                .source_hashes
                .get(path)
                .is_none_or(|expected| expected == content_hash),
            "source changed during Rust context discovery; retry indexing: {path}"
        );
        Ok(self.rust.facts.remove(path))
    }

    pub fn fingerprint(&self, path: &str) -> String {
        if crate::languages::extended::applies(path) {
            return if crate::languages::compiled::supports(path) {
                digest([self.extended.fingerprint(), self.compiled.fingerprint()])
            } else {
                self.extended.fingerprint().into()
            };
        }
        if path.rsplit('/').next() == Some("Cargo.toml") {
            return self.cargo_packages.fingerprint().into();
        }
        if path.ends_with(".tf") || path.ends_with(".tfvars") {
            return self.terraform.fingerprint().into();
        }
        if path.ends_with(".swift") {
            return digest([self.swift.fingerprint.as_str(), self.compiled.fingerprint()]);
        }
        if path.ends_with(".cs") {
            return digest([
                self.templates.scope_fingerprint.as_str(),
                self.compiled.fingerprint(),
            ]);
        }
        if matches!(path.rsplit('.').next(), Some("xaml" | "razor" | "cshtml")) {
            return self.templates.fingerprint.clone();
        }
        if crate::languages::compiled::supports(path) {
            return self.compiled.fingerprint().into();
        }
        if self.javascript.files.contains(path) {
            return self
                .javascript
                .fingerprints
                .get(path)
                .cloned()
                .unwrap_or_else(|| "javascript-output-v1-empty".into());
        }
        if path.ends_with(".rs") {
            return self
                .rust
                .fingerprints
                .get(path)
                .unwrap_or(&self.rust.fingerprint)
                .clone();
        }
        if !path.ends_with(".go") {
            return String::new();
        }
        // Module mapping and owner ambiguity can change references in unchanged files.
        let mut hash = blake3::Hasher::new();
        for ((directory, name), package) in &self.go {
            hash.update(directory.as_bytes());
            hash.update(&[0]);
            hash.update(name.as_bytes());
            hash.update(&[u8::from(package.production)]);
            hash.update(package.fingerprint.as_bytes());
            for (name, count) in &package.type_counts {
                hash.update(name.as_bytes());
                hash.update(&[0]);
                hash.update(&(*count as u64).to_le_bytes());
            }
        }
        hash.finalize().to_hex().to_string()
    }

    pub fn apply(&self, facts: &mut FileFacts) {
        if crate::languages::extended::applies(&facts.path) {
            self.extended.apply(facts);
            if crate::languages::compiled::supports(&facts.path) {
                self.compiled.apply(facts);
            }
            return;
        }
        if facts.path.rsplit('/').next() == Some("Cargo.toml") {
            self.cargo_packages.apply(facts);
            return;
        }
        if facts.path.ends_with(".tf") || facts.path.ends_with(".tfvars") {
            self.terraform.apply(facts);
            return;
        }
        if facts.path.ends_with(".swift") {
            self.swift.apply(facts);
            self.compiled.apply(facts);
            return;
        }
        if matches!(
            facts.path.rsplit('.').next(),
            Some("cs" | "xaml" | "razor" | "cshtml")
        ) {
            self.templates.apply(facts);
            if facts.path.ends_with(".cs") {
                self.compiled.apply(facts);
            }
            return;
        }
        if crate::languages::compiled::supports(&facts.path) {
            self.compiled.apply(facts);
        }
        if self.javascript.files.contains(&facts.path) {
            self.javascript.apply(facts);
            return;
        }
        if facts.path.ends_with(".rs") {
            self.rust.apply(facts);
            return;
        }
        if !facts.path.ends_with(".go") {
            return;
        }
        let Some(module) = facts.nodes.iter().find(|n| {
            n.metadata
                .get("package_name")
                .and_then(|p| p.as_str())
                .is_some()
        }) else {
            return;
        };
        let package_name = module.metadata["package_name"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let imports: Vec<(String, Option<String>)> = module
            .metadata
            .get("imports")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                Some((
                    entry.get("path")?.as_str()?.to_owned(),
                    entry
                        .get("alias")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                ))
            })
            .collect();
        for reference in &mut facts.references {
            for key in &mut reference.candidate_keys {
                let direct = key
                    .strip_prefix("go:import:")
                    .and_then(|s| s.rsplit_once(':'));
                let target = if let Some((import_path, symbol)) = direct {
                    self.import_target(import_path)
                        .map(|(directory, name)| (directory, name, symbol.to_owned()))
                } else if let Some((receiver, symbol)) = key
                    .strip_prefix("go:selector:")
                    .and_then(|s| s.rsplit_once(':'))
                {
                    let matches: Vec<_> = imports
                        .iter()
                        .filter(|(_, alias)| alias.is_none())
                        .filter_map(|(path, _)| self.import_target(path))
                        .filter(|(_, name)| name == receiver)
                        .collect();
                    if matches.len() == 1 {
                        Some((
                            matches[0].0.clone(),
                            matches[0].1.clone(),
                            symbol.to_owned(),
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some((directory, name, symbol)) = target
                    && symbol
                        .split('.')
                        .all(|part| part.chars().next().is_some_and(char::is_uppercase))
                {
                    *key = format!("go:{directory}:{name}:{symbol}");
                }
            }
        }
        let directory = facts.path.rsplit_once('/').map_or(".", |(p, _)| p);
        let package = self.go.get(&(directory.to_owned(), package_name.clone()));
        let prefix = format!("go:{directory}:{package_name}:");
        for node in &mut facts.nodes {
            if let Some(aliases) = node
                .metadata
                .get_mut("binding_aliases")
                .and_then(Value::as_array_mut)
            {
                aliases.retain(|alias| {
                    let Some((owner, _)) = alias
                        .as_str()
                        .and_then(|key| key.strip_prefix(&prefix))
                        .and_then(|key| key.split_once("#declared."))
                    else {
                        return true;
                    };
                    // A unique member spelling cannot disambiguate its owning type.
                    package.and_then(|p| p.type_counts.get(owner)) == Some(&1)
                });
            }
        }
        if let Some(package) = package.filter(|p| p.owner == facts.path && p.production)
            && let Some(import_path) = &package.import_path
        {
            facts.nodes.push(Node {
                    id: format!("go:package:{directory}:{package_name}"), label: package_name, kind:"package".into(),
                    file:facts.path.clone(), line:None, end_line:None, qualified_name:Some(import_path.clone()),
                    binding_key:Some(format!("go:import-module:{import_path}")),
                    metadata:json!({"language":"go", "package_directory":directory, "source":"go.mod"}),
                });
        }
    }

    pub(super) fn import_target(&self, import_path: &str) -> Option<(String, String)> {
        let matches: Vec<_> = self
            .go
            .iter()
            .filter(|(_, p)| p.production && p.import_path.as_deref() == Some(import_path))
            .collect();
        if matches.len() == 1 {
            Some(matches[0].0.clone())
        } else {
            None
        }
    }
}
