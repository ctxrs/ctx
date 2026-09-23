//! Repository configuration that gives context-free syntax facts a project identity.
use crate::model::{FileFacts, Node};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::Path,
};

#[derive(Default)]
pub(crate) struct ProjectContext {
    go: BTreeMap<(String, String), GoPackage>,
    javascript: JavascriptContext,
    rust: RustContext,
    templates: TemplateContext,
    swift: SwiftContext,
    terraform: crate::languages::configs::TerraformContext,
    cargo_packages: crate::languages::configs::CargoPackageContext,
    compiled: crate::languages::compiled::CompiledContext,
    compiled_source_hashes: BTreeMap<String, String>,
    extended: crate::languages::extended::ExtendedContext,
}

struct GoPackage {
    owner: String,
    import_path: Option<String>,
    fingerprint: String,
    production: bool,
    type_counts: BTreeMap<String, usize>,
}

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

    fn compiled_inventory(
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

    fn import_target(&self, import_path: &str) -> Option<(String, String)> {
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

/// File and exact-symbol aliases let document links reuse the indexed resolver.
pub(crate) fn add_document_aliases(facts: &mut FileFacts) {
    let file_root = facts
        .nodes
        .iter()
        .position(|n| matches!(n.kind.as_str(), "module" | "file" | "document"));
    for (index, node) in facts.nodes.iter_mut().enumerate() {
        let mut aliases = vec![];
        if file_root == Some(index) {
            aliases.push(format!("file:{}", facts.path));
        }
        if matches!(
            node.kind.as_str(),
            "function" | "class" | "method" | "interface" | "struct" | "enum" | "type"
        ) {
            aliases.push(format!("symbol:{}", node.label));
            if let Some(qualified) = &node.qualified_name {
                aliases.push(format!("symbol:{qualified}"));
                aliases.push(format!("symbol:{}::{qualified}", facts.path));
            }
        }
        if aliases.is_empty() {
            continue;
        }
        if node.metadata.is_null() {
            node.metadata = json!({});
        }
        if let Some(metadata) = node.metadata.as_object_mut() {
            if let Some(existing) = metadata.get("binding_aliases").and_then(|v| v.as_array()) {
                aliases.extend(
                    existing
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned)),
                );
            }
            aliases.sort();
            aliases.dedup();
            metadata.insert("binding_aliases".into(), json!(aliases));
        }
    }
}

fn javascript_source(path: &str) -> bool {
    matches!(
        path.rsplit('.').next(),
        Some(
            "js" | "jsx"
                | "mjs"
                | "cjs"
                | "ts"
                | "tsx"
                | "mts"
                | "cts"
                | "vue"
                | "svelte"
                | "astro"
        )
    )
}
fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}
fn within(path: &str, directory: &str) -> bool {
    directory.is_empty()
        || path
            .strip_prefix(directory)
            .is_some_and(|p| p.starts_with('/'))
}
fn join(base: &str, relative: &str) -> Option<String> {
    if relative.starts_with('/')
        || relative.contains('\\')
        || relative.as_bytes().get(1) == Some(&b':')
    {
        return None;
    }
    let mut parts: Vec<_> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}
fn ancestors(path: &str) -> Vec<String> {
    let mut result = vec![];
    let mut dir = directory(path);
    loop {
        result.push(dir.into());
        if dir.is_empty() {
            break;
        }
        dir = directory(dir);
    }
    result
}
fn stem(path: &str) -> &str {
    path.rsplit_once('.')
        .filter(|(stem, suffix)| !stem.is_empty() && !stem.ends_with('/') && !suffix.contains('/'))
        .map_or(path, |(stem, _)| stem)
}
fn digest<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let mut hash = blake3::Hasher::new();
    for item in items {
        hash.update(&(item.len() as u64).to_le_bytes());
        hash.update(item.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}
struct Inventory<'a> {
    root: &'a Path,
    files: BTreeSet<String>,
    configs: BTreeMap<String, Option<String>>,
}
impl<'a> Inventory<'a> {
    fn new(root: &'a Path, paths: &[String]) -> Self {
        Self {
            root,
            files: paths.iter().cloned().collect(),
            configs: BTreeMap::new(),
        }
    }
    fn read_bytes(&self, relative: &str, limit: u64) -> Result<Option<Vec<u8>>> {
        ensure!(
            join("", relative).as_deref() == Some(relative),
            "configuration path must stay inside the repository"
        );
        let mut path = self.root.to_path_buf();
        for component in relative.split('/') {
            path.push(component);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) => ensure!(
                    !meta.file_type().is_symlink(),
                    "configuration paths must not traverse symlinks"
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e).context("cannot inspect project configuration"),
            }
        }
        let bytes = crate::index::read_source(&path, limit)?
            .1
            .context("project configuration exceeds its size limit")?;
        Ok(Some(bytes))
    }
    fn read(&self, relative: &str, limit: u64) -> Result<Option<String>> {
        self.read_bytes(relative, limit)?
            .map(String::from_utf8)
            .transpose()
            .context("project configuration is not UTF-8")
    }
    fn config(&mut self, relative: &str) -> Result<Option<String>> {
        if let Some(value) = self.configs.get(relative) {
            return Ok(value.clone());
        }
        let value = self.read(relative, 1024 * 1024)?;
        self.configs.insert(relative.into(), value.clone());
        Ok(value)
    }
    fn config_fingerprint(&self, extension: &str) -> String {
        let mut items = vec![extension];
        for (path, value) in &self.configs {
            items.push(path);
            items.push(value.as_deref().unwrap_or("<missing>"));
        }
        digest(items)
    }
}

#[derive(Default)]
struct SwiftContext {
    owners: BTreeMap<String, String>,
    imports: Vec<(String, String)>,
    target_imports: BTreeMap<String, Vec<(String, String)>>,
    source_hashes: BTreeMap<String, String>,
    fingerprint: String,
}
impl SwiftContext {
    fn discover(inventory: &Inventory<'_>, modules: &BTreeMap<String, String>) -> Result<Self> {
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
    fn discover_packages(inventory: &Inventory<'_>) -> Result<Self> {
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
    fn validate_source(&self, path: &str, hash: &str) -> Result<()> {
        ensure!(
            self.source_hashes
                .get(path)
                .is_none_or(|expected| expected == hash),
            "source changed during Swift manifest discovery; retry indexing: {path}"
        );
        Ok(())
    }
    fn apply(&self, facts: &mut FileFacts) {
        if let Some(module) = self.owners.get(&facts.path) {
            let imports = self.target_imports.get(module).unwrap_or(&self.imports);
            crate::languages::compiled::apply_swift_context(facts, module, imports);
        }
    }
}

struct SwiftTarget {
    name: String,
    id: String,
    root: String,
    sources: Option<Vec<String>>,
    exclude: Vec<String>,
    dependencies: Vec<String>,
    test: bool,
}
impl SwiftTarget {
    fn contains(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap();
        within(path, &self.root)
            && name != "Package.swift"
            && !name.starts_with("Package@swift-")
            && !path.split('/').any(|part| part.starts_with('.'))
            && self
                .sources
                .as_ref()
                .is_none_or(|sources| sources.iter().any(|s| path == s || within(path, s)))
            && !self.exclude.iter().any(|s| path == s || within(path, s))
    }
}

// This is a closed literal subset of PackageDescription, not a Swift evaluator.
// Every membership-affecting argument is consumed; unknown syntax refuses the package.
fn swift_package(source: &str, package: &str) -> Option<Vec<SwiftTarget>> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    if tree.root_node().has_error() {
        return None;
    }
    let items = swift_children(tree.root_node());
    let [import, declaration] = items.as_slice() else {
        return None;
    };
    if import.kind() != "import_declaration"
        || swift_tokens(*import, source)? != "importPackageDescription"
        || declaration.kind() != "property_declaration"
    {
        return None;
    }
    let children = swift_children(*declaration);
    let [binding, pattern, value] = children.as_slice() else {
        return None;
    };
    if binding.kind() != "value_binding_pattern"
        || swift_tokens(*binding, source)? != "let"
        || pattern.kind() != "pattern"
        || swift_tokens(*pattern, source)? != "package"
    {
        return None;
    }
    let (callee, args) = swift_call(*value, source)?;
    if callee != "Package" {
        return None;
    }
    let mut args = swift_labels(args)?;
    swift_string(args.remove("name")?, source)?;
    let targets = swift_array(args.remove("targets")?)?;
    for (key, value) in args {
        if !matches!(
            key.as_str(),
            "products"
                | "dependencies"
                | "platforms"
                | "defaultLocalization"
                | "swiftLanguageVersions"
                | "swiftLanguageModes"
                | "cLanguageStandard"
                | "cxxLanguageStandard"
        ) || !swift_literal(value, source, 0)
        {
            return None;
        }
    }
    let mut result = vec![];
    for target in targets {
        let (kind, args) = swift_call(target, source)?;
        if !matches!(
            kind.as_str(),
            ".target" | ".executableTarget" | ".testTarget"
        ) {
            return None;
        }
        let mut args = swift_labels(args)?;
        let name = swift_string(args.remove("name")?, source)?.to_owned();
        if !swift_identifier(&name) {
            return None;
        }
        let test = kind == ".testTarget";
        let root = match args.remove("path") {
            Some(value) => swift_path(package, swift_string(value, source)?)?,
            None => join(
                package,
                &format!("{}/{name}", if test { "Tests" } else { "Sources" }),
            )?,
        };
        let sources = match args.remove("sources") {
            Some(value) => Some(swift_paths(value, source, &root)?),
            None => None,
        };
        let exclude = match args.remove("exclude") {
            Some(value) => swift_paths(value, source, &root)?,
            None => vec![],
        };
        let mut dependencies = vec![];
        if let Some(value) = args.remove("dependencies") {
            for dependency in swift_array(value)? {
                if let Some(name) = swift_string(dependency, source) {
                    dependencies.push(name.to_owned());
                } else {
                    let (kind, args) = swift_call(dependency, source)?;
                    let mut args = swift_labels(args)?;
                    let name = swift_string(args.remove("name")?, source)?.to_owned();
                    match kind.as_str() {
                        ".target" | ".byName" => dependencies.push(name),
                        ".product" => {
                            swift_string(args.remove("package")?, source)?;
                        }
                        _ => return None,
                    }
                    // Conditions and module aliases are deliberately unsupported.
                    if !args.is_empty() {
                        return None;
                    }
                }
            }
        }
        if !args.is_empty() {
            return None;
        }
        // Duplicate names and overlapping target roots are ambiguous even if an
        // explicit source list happens to make today's indexed subset disjoint.
        if result.iter().any(|t: &SwiftTarget| {
            t.name == name || t.root == root || within(&root, &t.root) || within(&t.root, &root)
        }) {
            return None;
        }
        result.push(SwiftTarget {
            id: format!("swiftpm-{}", digest([package, name.as_str()])),
            name,
            root,
            sources,
            exclude,
            dependencies,
            test,
        });
    }
    Some(result)
}

fn swift_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
fn swift_path(base: &str, relative: &str) -> Option<String> {
    if relative.contains(['\\', ':'])
        || relative.starts_with('/')
        || relative.split('/').any(|p| p == "..")
    {
        return None;
    }
    join(base, relative)
}
fn swift_paths(node: tree_sitter::Node<'_>, source: &str, root: &str) -> Option<Vec<String>> {
    swift_array(node)?
        .into_iter()
        .map(|n| swift_path(root, swift_string(n, source)?))
        .collect()
}
fn swift_children(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|n| !matches!(n.kind(), "comment" | "multiline_comment"))
        .collect()
}
fn swift_tokens(node: tree_sitter::Node<'_>, source: &str) -> Option<String> {
    // Compare grammatical spellings without interpreting comments as source text.
    let mut pending = vec![node];
    let mut text = String::new();
    while let Some(node) = pending.pop() {
        if matches!(node.kind(), "comment" | "multiline_comment") {
            continue;
        }
        if node.child_count() == 0 {
            text.push_str(node.utf8_text(source.as_bytes()).ok()?);
        } else {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            pending.extend(children.into_iter().rev());
        }
    }
    Some(text)
}
fn swift_string<'a>(node: tree_sitter::Node<'_>, source: &'a str) -> Option<&'a str> {
    if node.kind() != "line_string_literal"
        || swift_children(node)
            .iter()
            .any(|n| n.kind() != "line_str_text")
    {
        return None;
    }
    node.utf8_text(source.as_bytes())
        .ok()?
        .strip_prefix('"')?
        .strip_suffix('"')
}
fn swift_array(node: tree_sitter::Node<'_>) -> Option<Vec<tree_sitter::Node<'_>>> {
    if node.kind() != "array_literal" {
        return None;
    }
    let mut cursor = node.walk();
    // Field iteration includes unnamed expressions such as nil, which must not disappear.
    let elements: Vec<_> = node
        .children_by_field_name("element", &mut cursor)
        .collect();
    swift_children(node)
        .iter()
        .all(|n| elements.contains(n))
        .then_some(elements)
}
type SwiftArguments<'a> = Vec<(Option<String>, tree_sitter::Node<'a>)>;
fn swift_call<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
) -> Option<(String, SwiftArguments<'a>)> {
    if node.kind() != "call_expression" {
        return None;
    }
    let children = swift_children(node);
    let [callee, suffix] = children.as_slice() else {
        return None;
    };
    if suffix.kind() != "call_suffix" {
        return None;
    }
    let children = swift_children(*suffix);
    let [arguments] = children.as_slice() else {
        return None;
    };
    if arguments.kind() != "value_arguments" {
        return None;
    }
    let mut result = vec![];
    for argument in swift_children(*arguments) {
        if argument.kind() != "value_argument" {
            return None;
        }
        let value = argument.child_by_field_name("value")?;
        let name = match argument.child_by_field_name("name") {
            Some(node) => Some(swift_tokens(node, source)?),
            None => None,
        };
        if argument
            .child_by_field_name("reference_specifier")
            .is_some()
            || swift_children(argument)
                .iter()
                .any(|n| *n != value && Some(*n) != argument.child_by_field_name("name"))
        {
            return None;
        }
        result.push((name, value));
    }
    Some((swift_tokens(*callee, source)?, result))
}
fn swift_labels<'a>(args: SwiftArguments<'a>) -> Option<BTreeMap<String, tree_sitter::Node<'a>>> {
    let mut result = BTreeMap::new();
    for (name, value) in args {
        if result.insert(name?, value).is_some() {
            return None;
        }
    }
    Some(result)
}
fn swift_literal(node: tree_sitter::Node<'_>, source: &str, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match node.kind() {
        "line_string_literal" => swift_string(node, source).is_some(),
        "integer_literal" | "boolean_literal" | "nil" => true,
        "array_literal" => swift_array(node).is_some_and(|items| {
            items
                .into_iter()
                .all(|n| swift_literal(n, source, depth + 1))
        }),
        "prefix_expression" => swift_tokens(node, source)
            .and_then(|s| s.strip_prefix('.').map(swift_identifier))
            .unwrap_or(false),
        "call_expression" => swift_call(node, source).is_some_and(|(name, args)| {
            matches!(
                name.as_str(),
                ".library"
                    | ".executable"
                    | ".package"
                    | ".exact"
                    | ".upToNextMajor"
                    | ".upToNextMinor"
                    | ".branch"
                    | ".revision"
                    | ".macOS"
                    | ".iOS"
                    | ".tvOS"
                    | ".watchOS"
                    | ".visionOS"
                    | ".macCatalyst"
                    | ".driverKit"
            ) && args
                .into_iter()
                .all(|(_, n)| swift_literal(n, source, depth + 1))
        }),
        _ => false,
    }
}

#[derive(Default)]
struct TemplateContext {
    // None is a boundary with ambiguous or unsupported project configuration.
    projects: BTreeMap<String, Option<CsharpProject>>,
    loose: Vec<crate::languages::templates::ProjectType>,
    fingerprint: String,
    scope_fingerprint: String,
    source_hashes: BTreeMap<String, String>,
}
struct CsharpProject {
    manifest: String,
    namespace: Option<String>,
    types: Vec<crate::languages::templates::ProjectType>,
}
impl TemplateContext {
    fn discover(inventory: &mut Inventory<'_>) -> Result<Self> {
        let mut result = Self::default();
        let mut hash = blake3::Hasher::new();
        let mut input = |value: &[u8]| {
            hash.update(&(value.len() as u64).to_le_bytes());
            hash.update(value);
        };
        input(b"xaml-project-context-2");
        let projects: Vec<_> = inventory
            .files
            .iter()
            .filter(|p| p.ends_with(".csproj"))
            .cloned()
            .collect();
        for path in projects {
            let source = inventory.config(&path)?;
            if let Some(source) = &source {
                result.source_hashes.insert(
                    path.clone(),
                    blake3::hash(source.as_bytes()).to_hex().to_string(),
                );
            }
            input(path.as_bytes());
            input(source.as_deref().unwrap_or("<missing>").as_bytes());
            let dir = directory(&path).to_owned();
            let project = source
                .as_deref()
                .and_then(csharp_namespace)
                .map(|namespace| CsharpProject {
                    manifest: path.clone(),
                    namespace,
                    types: vec![],
                });
            result
                .projects
                .entry(dir)
                .and_modify(|p| *p = None)
                .or_insert(project);
        }
        for path in inventory
            .files
            .iter()
            .filter(|p| p.ends_with(".cs") || p.ends_with(".xaml"))
        {
            input(path.as_bytes());
            if std::fs::symlink_metadata(inventory.root.join(path))?.len()
                > crate::parser::MAX_SOURCE_BYTES as u64
            {
                input(b"<oversized>");
                result
                    .source_hashes
                    .insert(path.clone(), "oversized:4MiB".into());
                continue;
            }
            let bytes = inventory.read_bytes(path, crate::parser::MAX_SOURCE_BYTES as u64)?;
            if let Some(bytes) = &bytes {
                result
                    .source_hashes
                    .insert(path.clone(), blake3::hash(bytes).to_hex().to_string());
            }
            input(bytes.as_deref().unwrap_or(b"<missing>"));
            let source = bytes.as_deref().and_then(|b| std::str::from_utf8(b).ok());
            let owner = result
                .projects
                .keys()
                .filter(|dir| within(path, dir))
                .max_by_key(|dir| dir.len())
                .cloned();
            if path.ends_with(".cs")
                && let Some(source) = source
            {
                let types = crate::languages::templates::project_types(path, source)?;
                if let Some(owner) = owner {
                    if let Some(Some(project)) = result.projects.get_mut(&owner) {
                        project.types.extend(types);
                    }
                } else {
                    // Loose sources are a separate root inventory: no declaration
                    // beneath any valid, invalid or ambiguous project leaks into it.
                    result.loose.extend(types);
                }
            }
        }
        result.fingerprint = hash.finalize().to_hex().to_string();
        let proof: Vec<_> = result
            .projects
            .iter()
            .map(|(directory, project)| {
                json!([
                    directory,
                    project.as_ref().map(|p| (&p.manifest, &p.namespace))
                ])
            })
            .collect();
        result.scope_fingerprint =
            digest(["csharp-project-scope-1", &serde_json::to_string(&proof)?]);
        Ok(result)
    }
    fn validate_source(&self, path: &str, content_hash: &str) -> Result<()> {
        ensure!(
            self.source_hashes
                .get(path)
                .is_none_or(|expected| expected == content_hash),
            "source changed during template context discovery; retry indexing: {path}"
        );
        Ok(())
    }
    fn unit(&self, path: &str) -> Option<String> {
        if self.projects.is_empty() {
            return Some("index-root:csharp".into());
        }
        let (_, project) = self
            .projects
            .iter()
            .filter(|(dir, _)| within(path, dir))
            .max_by_key(|(dir, _)| dir.len())?;
        let project = project.as_ref()?;
        Some(format!(
            "csharp-project:{}",
            digest([project.manifest.as_str()])
        ))
    }
    fn apply(&self, facts: &mut FileFacts) {
        let owner = self
            .projects
            .iter()
            .filter(|(dir, _)| within(&facts.path, dir))
            .max_by_key(|(dir, _)| dir.len());
        let razor = matches!(facts.path.rsplit('.').next(), Some("razor" | "cshtml"));
        let (root, namespace, types) = match owner {
            Some((root, Some(project))) => (
                root.as_str(),
                project.namespace.as_deref(),
                project.types.as_slice(),
            ),
            // An empty inventory deliberately clears Razor's raw syntax fallback.
            Some((root, None)) if razor => (root.as_str(), None, [].as_slice()),
            None if razor || facts.path.ends_with(".cs") => ("", None, self.loose.as_slice()),
            _ => return,
        };
        crate::languages::templates::apply_project(
            facts,
            &crate::languages::templates::TemplateProject {
                root,
                namespace,
                types,
            },
        );
    }
}

// Only unconditional literal RootNamespace is a namespace hint. Neither project
// imports, conditions nor MSBuild expressions are evaluated.
fn csharp_namespace(source: &str) -> Option<Option<String>> {
    use quick_xml::{Reader, events::Event};
    let mut reader = Reader::from_str(source);
    let mut stack: Vec<(String, bool)> = vec![];
    let mut roots = 0;
    let mut namespaces = vec![];
    let mut invalid_namespace = false;
    loop {
        match reader.read_event().ok()? {
            event @ (Event::Start(_) | Event::Empty(_)) => {
                let empty = matches!(&event, Event::Empty(_));
                let element = match &event {
                    Event::Start(e) | Event::Empty(e) => e,
                    _ => unreachable!(),
                };
                if stack.len() >= 128 {
                    return None;
                }
                let name = String::from_utf8(element.local_name().as_ref().to_vec()).ok()?;
                if stack.is_empty() {
                    roots += 1;
                    if roots != 1 || name != "Project" {
                        return None;
                    }
                }
                let mut conditional = stack.last().is_some_and(|(_, conditional)| *conditional);
                for attribute in element.attributes() {
                    if attribute.ok()?.key.local_name().as_ref() == b"Condition" {
                        conditional = true;
                    }
                }
                if name == "RootNamespace" {
                    let text = if empty {
                        String::new()
                    } else {
                        reader
                            .read_text(element.name())
                            .ok()?
                            .decode()
                            .ok()?
                            .into_owned()
                    };
                    let value = text.trim();
                    let literal = !value.is_empty()
                        && value.split('.').all(|part| {
                            let mut chars = part.chars();
                            chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
                                && chars.all(|c| c == '_' || c.is_alphanumeric())
                        });
                    if conditional || !literal || stack.len() != 2 || stack[1].0 != "PropertyGroup"
                    {
                        invalid_namespace = true;
                    } else {
                        namespaces.push(value.to_owned());
                    }
                } else if !empty {
                    stack.push((name, conditional));
                }
            }
            Event::End(_) => {
                stack.pop()?;
            }
            Event::DocType(_) => return None,
            Event::Eof => break,
            _ => {}
        }
    }
    if roots != 1 || !stack.is_empty() || invalid_namespace || namespaces.len() > 1 {
        return None;
    }
    Some(namespaces.pop())
}

#[derive(Clone, Default)]
struct TypescriptConfig {
    base_url: Option<String>,
    invalid_base_url: bool,
    paths: BTreeMap<String, Vec<String>>,
    paths_origin: String,
}
fn typescript_config(
    path: &str,
    inventory: &mut Inventory<'_>,
    stack: &mut Vec<String>,
    observations: &mut BTreeMap<String, Option<String>>,
) -> Result<TypescriptConfig> {
    ensure!(
        stack.len() < 32 && !stack.iter().any(|p| p == path),
        "cyclic or excessively nested TypeScript extends"
    );
    let Some(source) = javascript_config(inventory, path, observations)? else {
        return Ok(TypescriptConfig::default());
    };
    // Empty and comment-only tsconfig/jsconfig files are common fixtures and
    // carry no mapping information. Treat them like `{}` instead of aborting
    // an otherwise valid repository-wide index.
    let value = jsonc_parser::parse_to_serde_value(&source, &Default::default())?
        .unwrap_or_else(|| serde_json::json!({}));
    ensure!(
        value.is_object(),
        "TypeScript configuration must be an object"
    );
    stack.push(path.into());
    let mut result = TypescriptConfig::default();
    let bases: Vec<_> = value
        .get("extends")
        .into_iter()
        .flat_map(|v| {
            if let Some(s) = v.as_str() {
                vec![s]
            } else {
                v.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect()
            }
        })
        .collect();
    for base in bases {
        // Package-name extends needs installed dependency resolution; never probe node_modules.
        if !base.starts_with('.') {
            continue;
        }
        let Some(mut target) = join(directory(path), base) else {
            continue;
        };
        if !target.ends_with(".json") {
            target.push_str(".json");
        }
        let inherited = typescript_config(&target, inventory, stack, observations)?;
        if inherited.base_url.is_some() || inherited.invalid_base_url {
            result.base_url = inherited.base_url;
            result.invalid_base_url = inherited.invalid_base_url;
        }
        if !inherited.paths.is_empty() {
            result.paths = inherited.paths;
            result.paths_origin = inherited.paths_origin;
        }
    }
    if let Some(options) = value.get("compilerOptions") {
        if let Some(base) = options.get("baseUrl").and_then(Value::as_str) {
            result.base_url = join(directory(path), base);
            result.invalid_base_url = result.base_url.is_none();
        }
        if let Some(paths) = options.get("paths").and_then(Value::as_object) {
            result.paths.clear();
            result.paths_origin = directory(path).into();
            for (pattern, values) in paths {
                if pattern.matches('*').count() > 1 {
                    continue;
                }
                let targets = values
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|v| v.matches('*').count() <= 1)
                    .map(str::to_owned)
                    .collect();
                result.paths.insert(pattern.clone(), targets);
            }
        }
    }
    stack.pop();
    Ok(result)
}
fn javascript_config(
    inventory: &mut Inventory<'_>,
    path: &str,
    observations: &mut BTreeMap<String, Option<String>>,
) -> Result<Option<String>> {
    let source = inventory.config(path)?;
    observations.insert(
        path.into(),
        source
            .as_ref()
            .map(|s| blake3::hash(s.as_bytes()).to_hex().to_string()),
    );
    Ok(source)
}
fn capture<'a>(pattern: &str, value: &'a str) -> Option<&'a str> {
    if let Some((prefix, suffix)) = pattern.split_once('*') {
        value.strip_prefix(prefix)?.strip_suffix(suffix)
    } else {
        (pattern == value).then_some("")
    }
}
fn workspace_member(patterns: &[String], directory: &str) -> bool {
    patterns.iter().any(|pattern| {
        globset::Glob::new(pattern)
            .ok()
            .is_some_and(|g| g.compile_matcher().is_match(directory))
    })
}
#[derive(Default)]
struct JavascriptContext {
    files: BTreeSet<String>,
    configs: BTreeMap<String, TypescriptConfig>,
    packages: BTreeMap<String, Value>,
    workspaces: BTreeMap<String, Vec<String>>,
    raw_facts: BTreeMap<String, FileFacts>,
    source_hashes: BTreeMap<String, String>,
    config_hashes: BTreeMap<String, Option<String>>,
    fingerprints: BTreeMap<String, String>,
    // Some(key): one proven declaration; None: type-only or conflicting route.
    // Absent entries retain ordinary function/class resolution.
    imported_callees: BTreeMap<String, Option<String>>,
    // Exact immutable value declaration -> exact returned callable body key.
    factory_results: BTreeMap<String, String>,
    star_aliases: BTreeMap<String, Vec<String>>,
    esm_files: BTreeSet<String>,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum JavascriptCallee {
    Declaration { node: String, key: String },
    Ordinary(String),
    Unresolved,
}
impl JavascriptContext {
    fn discover(inventory: &mut Inventory<'_>) -> Result<Self> {
        let mut result = Self::default();
        for path in inventory
            .files
            .iter()
            .filter(|p| javascript_source(p) || Path::new(p).extension().is_none())
        {
            let (hash, bytes) = crate::index::read_source(
                &inventory.root.join(path),
                crate::parser::MAX_SOURCE_BYTES as u64,
            )?;
            // Negative shebang probes and diagnostic sources also participate in
            // the raw-byte guard, before the main loop's unchanged shortcut.
            result.source_hashes.insert(path.clone(), hash.clone());
            let source = bytes.as_deref().and_then(|b| std::str::from_utf8(b).ok());
            if !javascript_source(path)
                && source.and_then(crate::languages::scripted::shebang_language)
                    != Some("javascript")
            {
                continue;
            }
            result.files.insert(path.clone());
            if let Some(source) = source
                && let Some(mut facts) = crate::languages::parse(path, source, &hash)?
            {
                if facts
                    .nodes
                    .first()
                    .is_some_and(|n| n.metadata["module_syntax"] == "esm")
                {
                    result.esm_files.insert(path.clone());
                }
                facts.references.shrink_to_fit();
                result.raw_facts.insert(path.clone(), facts);
            }
        }
        let directories: BTreeSet<_> = result.files.iter().flat_map(|p| ancestors(p)).collect();
        for dir in directories {
            let package_path = join(&dir, "package.json").unwrap();
            if let Some(source) =
                javascript_config(inventory, &package_path, &mut result.config_hashes)?
            {
                let package: Value =
                    serde_json::from_str(&source).context("invalid package.json")?;
                let members = package
                    .get("workspaces")
                    .and_then(|v| {
                        v.as_array()
                            .or_else(|| v.get("packages").and_then(Value::as_array))
                    })
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if !members.is_empty() {
                    result.workspaces.insert(dir.clone(), members);
                }
                result.packages.insert(dir.clone(), package);
            }
            for name in ["tsconfig.json", "jsconfig.json"] {
                let path = join(&dir, name).unwrap();
                if javascript_config(inventory, &path, &mut result.config_hashes)?.is_some() {
                    result.configs.insert(
                        dir.clone(),
                        typescript_config(
                            &path,
                            inventory,
                            &mut vec![],
                            &mut result.config_hashes,
                        )?,
                    );
                    break;
                }
            }
        }
        result.exports();
        for (path, raw) in &result.raw_facts {
            let mut applied = raw.clone();
            result.apply(&mut applied);
            result
                .fingerprints
                .insert(path.clone(), Self::outcome_fingerprint(applied)?);
        }
        result.validate_configs(inventory)?;
        Ok(result)
    }
    fn validate_source(&self, path: &str, hash: &str) -> Result<()> {
        ensure!(
            self.source_hashes
                .get(path)
                .is_none_or(|expected| expected == hash)
                && self
                    .config_hashes
                    .get(path)
                    .is_none_or(|expected| expected.as_deref() == Some(hash)),
            "source changed during JavaScript context discovery; retry indexing: {path}"
        );
        Ok(())
    }
    fn validate_configs(&self, inventory: &Inventory<'_>) -> Result<()> {
        for (path, expected) in &self.config_hashes {
            let current = inventory
                .read_bytes(path, 1024 * 1024)?
                .map(|bytes| blake3::hash(&bytes).to_hex().to_string());
            ensure!(
                &current == expected,
                "configuration changed during JavaScript context discovery; retry indexing: {path}"
            );
        }
        Ok(())
    }
    fn outcome_fingerprint(facts: FileFacts) -> Result<String> {
        outcome_fingerprint("javascript-output-v1", facts)
    }
    fn commonjs(&self, path: &str) -> bool {
        if path.ends_with(".mjs") || path.ends_with(".mts") {
            return false;
        }
        if path.ends_with(".cjs") || path.ends_with(".cts") {
            return true;
        }
        if self.esm_files.contains(path) && (path.ends_with(".js") || path.ends_with(".jsx")) {
            return false;
        }
        self.packages
            .iter()
            .filter(|(dir, _)| within(path, dir))
            .max_by_key(|(dir, _)| dir.len())
            .is_none_or(|(_, p)| p.get("type").and_then(Value::as_str) != Some("module"))
    }
    fn exports(&mut self) {
        let mut direct: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
        let mut edges = vec![];
        for (path, facts) in &self.raw_facts {
            let Some(root) = facts.nodes.first() else {
                continue;
            };
            let module = if matches!(path.rsplit('.').next(), Some("vue" | "svelte" | "astro")) {
                path.as_str()
            } else {
                stem(path)
            };
            let symbols = direct.entry(path.clone()).or_default();
            for node in &facts.nodes {
                let aliases = node
                    .metadata
                    .get("binding_aliases")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str);
                for key in node.binding_key.as_deref().into_iter().chain(aliases) {
                    let named = key
                        .strip_prefix(&format!("javascript:{module}:"))
                        .map(|s| format!("esm:{s}"))
                        .or_else(|| {
                            self.commonjs(path)
                                .then(|| {
                                    key.strip_prefix(&format!("javascript:cjs:{module}:"))
                                        .map(|s| format!("cjs:{s}"))
                                })
                                .flatten()
                        });
                    if let Some(name) = named {
                        let (kind, symbol) = name.split_once(':').unwrap();
                        let canonical = if kind == "cjs" {
                            format!("javascript:cjs-file:{path}:{symbol}")
                        } else {
                            format!("javascript:file:{path}:{symbol}")
                        };
                        symbols.entry(name).or_default().insert(canonical);
                    }
                }
            }
            for (field, kind) in [("star_reexports", "esm"), ("commonjs_reexports", "cjs")] {
                if kind == "cjs" && !self.commonjs(path) {
                    continue;
                }
                for specifier in root
                    .metadata
                    .get(field)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if let Some(target) = self.resolve(path, specifier, kind == "cjs") {
                        edges.push((path.clone(), kind.to_owned(), target));
                    }
                }
            }
        }
        let mut exports = direct.clone();
        let mut reverse: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (from, kind, target) in &edges {
            reverse
                .entry(target.clone())
                .or_default()
                .push((from.clone(), kind.clone()));
        }
        let mut pending: VecDeque<_> = exports.keys().cloned().collect();
        let mut queued: BTreeSet<_> = exports.keys().cloned().collect();
        while let Some(target) = pending.pop_front() {
            queued.remove(&target);
            let names = exports.get(&target).cloned().unwrap_or_default();
            for (from, kind) in reverse.get(&target).into_iter().flatten() {
                let mut changed = false;
                for (name, origins) in &names {
                    if !name.starts_with(&format!("{kind}:"))
                        || (kind == "esm"
                            && (name == "esm:default" || name.starts_with("esm:default#")))
                        || direct.get(from).is_some_and(|d| d.contains_key(name))
                    {
                        continue;
                    }
                    let current = exports
                        .entry(from.clone())
                        .or_default()
                        .entry(name.clone())
                        .or_default();
                    for origin in origins {
                        if current.len() < 2 {
                            changed |= current.insert(origin.clone());
                        }
                    }
                }
                if changed && queued.insert(from.clone()) {
                    pending.push_back(from.clone());
                }
            }
        }
        self.callee_providers(&exports, &edges);
        for (module, names) in &exports {
            for (name, origins) in names {
                if origins.len() != 1 || direct.get(module).is_some_and(|d| d.contains_key(name)) {
                    continue;
                }
                let (kind, name) = name.split_once(':').unwrap();
                let alias = if kind == "cjs" {
                    format!("javascript:cjs-file:{module}:{name}")
                } else {
                    format!("javascript:file:{module}:{name}")
                };
                self.star_aliases
                    .entry(origins.first().unwrap().clone())
                    .or_default()
                    .push(alias);
            }
        }
    }
    fn callee_providers(
        &mut self,
        exports: &BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
        star_edges: &[(String, String, String)],
    ) {
        const SUFFIX: &str = "#declared_callee";
        let key = |path: &str, kind: &str, name: &str| {
            format!(
                "javascript:{}:{path}:{name}",
                if kind == "cjs" { "cjs-file" } else { "file" }
            )
        };
        let mut owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut providers = BTreeMap::new();
        let mut factory_returns = BTreeMap::new();
        let mut initializers = Vec::new();
        for raw in self.raw_facts.values() {
            let mut facts = raw.clone();
            // Called before star aliases are installed: these are direct owners,
            // not discovery-only names competing with their terminal targets.
            self.apply_paths(&mut facts);
            let local_prefix = format!("javascript:local:{}:", facts.path);
            let mut local_factory_keys = BTreeSet::new();
            for node in &facts.nodes {
                if node.metadata["declared_callee_binding"] != true {
                    continue;
                }
                if let Some(key) = node
                    .binding_key
                    .as_deref()
                    .filter(|k| k.starts_with(&local_prefix))
                {
                    local_factory_keys.insert(key);
                }
                if let Some(initializer) = node.metadata["factory_initializer"].as_str()
                    && let Some(reference) = facts
                        .references
                        .iter()
                        .find(|r| r.id == initializer && r.relation == "calls")
                {
                    local_factory_keys.extend(
                        reference
                            .candidate_keys
                            .iter()
                            .map(String::as_str)
                            .filter(|k| k.starts_with(&local_prefix)),
                    );
                }
            }
            for node in &facts.nodes {
                let keys: BTreeSet<_> = node
                    .binding_key
                    .iter()
                    .map(String::as_str)
                    .chain(
                        node.metadata["binding_aliases"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str),
                    )
                    .filter(|k| {
                        k.starts_with(&format!("javascript:file:{}:", facts.path))
                            || k.starts_with(&format!("javascript:cjs-file:{}:", facts.path))
                            || local_factory_keys.contains(k)
                    })
                    .map(str::to_owned)
                    .collect();
                if keys.is_empty() {
                    continue;
                }
                let declaration = (node.kind == "constant"
                    && node.metadata["declared_callee_binding"] == true
                    && node
                        .binding_key
                        .as_deref()
                        .is_some_and(|k| k.ends_with(SUFFIX)))
                .then(|| keys.iter().find(|k| k.ends_with(SUFFIX)).cloned())
                .flatten();
                if node.kind == "function"
                    && let (Some(target), Some(key)) = (
                        node.metadata["factory_return"]["target"].as_str(),
                        node.metadata["factory_return"]["key"].as_str(),
                    )
                    && facts.nodes.iter().any(|body| {
                        body.id == target
                            && body.kind == "function"
                            && body.metadata["binding_aliases"]
                                .as_array()
                                .is_some_and(|aliases| {
                                    aliases.iter().any(|alias| alias.as_str() == Some(key))
                                })
                    })
                {
                    factory_returns.insert(node.id.clone(), key.to_owned());
                }
                if let Some(key) = &declaration
                    && let Some(initializer) = node.metadata["factory_initializer"].as_str()
                    && let Some(reference) = facts
                        .references
                        .iter()
                        .find(|r| r.id == initializer && r.relation == "calls")
                {
                    initializers.push((key.clone(), reference.candidate_keys.clone()));
                }
                let references: Vec<_> = if node.kind == "alias" {
                    facts
                        .references
                        .iter()
                        .filter(|r| r.source == node.id && r.relation == "aliases")
                        .collect()
                } else {
                    vec![]
                };
                let targets = if references.len() == 1 {
                    references[0].candidate_keys.clone()
                } else {
                    vec![]
                };
                providers.insert(node.id.clone(), (node.kind.clone(), declaration, targets));
                for key in keys {
                    owners.entry(key).or_default().insert(node.id.clone());
                }
            }
        }
        let mut values: BTreeMap<String, BTreeSet<JavascriptCallee>> = BTreeMap::new();
        let mut routes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut direct_values = BTreeSet::new();
        for (published, ids) in owners {
            if let Some(base) = published.strip_suffix(SUFFIX) {
                let origin = if ids.len() == 1 {
                    let node = ids.first().unwrap();
                    providers[node]
                        .1
                        .as_ref()
                        .map(|key| JavascriptCallee::Declaration {
                            node: node.clone(),
                            key: key.clone(),
                        })
                        .unwrap_or(JavascriptCallee::Unresolved)
                } else {
                    JavascriptCallee::Unresolved
                };
                values
                    .entry(base.into())
                    .or_default()
                    .insert(origin.clone());
                direct_values.insert(base.to_owned());
                values.entry(published).or_default().insert(origin);
                continue;
            }
            values.entry(published.clone()).or_default();
            for id in ids {
                let (kind, _, targets) = &providers[&id];
                if matches!(kind.as_str(), "interface" | "type") {
                    continue;
                }
                direct_values.insert(published.clone());
                if kind == "alias" && !targets.is_empty() {
                    routes
                        .entry(published.clone())
                        .or_default()
                        .extend(targets.iter().cloned());
                } else {
                    values
                        .entry(published.clone())
                        .or_default()
                        .insert(if kind == "alias" {
                            JavascriptCallee::Unresolved
                        } else {
                            JavascriptCallee::Ordinary(id)
                        });
                }
            }
        }
        drop(providers);
        // Join value exports by their actual published symbol. In particular an
        // interface must not shadow a star-exported value, whereas a direct
        // function must shadow it, even though the reserved suffix differs.
        for (from, kind, target) in star_edges {
            for name in exports
                .get(target)
                .into_iter()
                .flat_map(|names| names.keys())
            {
                let Some(symbol) = name.strip_prefix(&format!("{kind}:")) else {
                    continue;
                };
                if kind == "esm" && (symbol == "default" || symbol.starts_with("default#")) {
                    continue;
                }
                let symbol = symbol.strip_suffix(SUFFIX).unwrap_or(symbol);
                let from_key = key(from, kind, symbol);
                if direct_values.contains(&from_key) {
                    continue;
                }
                values.entry(from_key.clone()).or_default();
                routes
                    .entry(from_key)
                    .or_default()
                    .insert(key(target, kind, symbol));
            }
        }
        let mut reverse: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (from, targets) in &routes {
            for target in targets {
                values.entry(target.clone()).or_default();
                reverse
                    .entry(target.clone())
                    .or_default()
                    .insert(from.clone());
            }
        }
        // A missing/type-only endpoint is negative evidence, not permission to
        // select another candidate. Cycles are solved by the same bounded union
        // as star exports; repeated routes to one declaration remain unique.
        for (name, origins) in &mut values {
            if origins.is_empty() && !routes.contains_key(name) {
                origins.insert(JavascriptCallee::Unresolved);
            }
        }
        let mut pending: VecDeque<_> = values.keys().cloned().collect();
        let mut queued: BTreeSet<_> = values.keys().cloned().collect();
        while let Some(target) = pending.pop_front() {
            queued.remove(&target);
            let origins = values[&target].clone();
            for from in reverse.get(&target).into_iter().flatten() {
                let current = values.entry(from.clone()).or_default();
                let mut changed = false;
                for origin in &origins {
                    if current.len() < 2 {
                        changed |= current.insert(origin.clone());
                    }
                }
                if changed && queued.insert(from.clone()) {
                    pending.push_back(from.clone());
                }
            }
        }
        self.factory_results.clear();
        for (declaration, keys) in initializers {
            let mut factory = None;
            let complete = !keys.is_empty()
                && keys.iter().all(|key| {
                    let Some(origins) = values.get(key).filter(|origins| origins.len() == 1) else {
                        return false;
                    };
                    let Some(JavascriptCallee::Ordinary(id)) = origins.first() else {
                        return false;
                    };
                    if !factory_returns.contains_key(id) || factory.is_some_and(|prior| prior != id)
                    {
                        return false;
                    }
                    factory = Some(id);
                    true
                });
            if complete && let Some(factory) = factory {
                self.factory_results
                    .insert(declaration, factory_returns[factory].clone());
            }
        }
        self.imported_callees = values
            .into_iter()
            .filter_map(|(name, origins)| {
                if origins.len() == 1 {
                    match origins.into_iter().next().unwrap() {
                        JavascriptCallee::Declaration { key, .. } => {
                            return Some((name, Some(key)));
                        }
                        JavascriptCallee::Ordinary(_) => return None,
                        JavascriptCallee::Unresolved => (),
                    }
                }
                Some((name, None))
            })
            .collect();
    }
    fn file(&self, importer: &str, target: &str) -> Option<String> {
        let mut candidates = vec![target.to_owned()];
        let typescript = matches!(
            importer.rsplit('.').next(),
            Some("ts" | "tsx" | "mts" | "cts")
        );
        if typescript && let Some(prefix) = target.strip_suffix(".js") {
            candidates.splice(0..0, [format!("{prefix}.ts"), format!("{prefix}.tsx")]);
        } else if typescript && let Some(prefix) = target.strip_suffix(".mjs") {
            candidates.insert(0, format!("{prefix}.mts"));
        } else if typescript && let Some(prefix) = target.strip_suffix(".cjs") {
            candidates.insert(0, format!("{prefix}.cts"));
        }
        if !javascript_source(target) {
            for extension in ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"] {
                candidates.push(format!("{target}.{extension}"));
            }
            for extension in ["ts", "tsx", "js", "jsx", "mts", "mjs", "cts", "cjs"] {
                candidates.push(format!("{target}/index.{extension}"));
            }
        }
        candidates.into_iter().find(|p| self.files.contains(p))
    }
    fn package_target(&self, importer: &str, specifier: &str, require: bool) -> Option<String> {
        let (name, subpath) = if specifier.starts_with('@') {
            let (scope, rest) = specifier.split_once('/')?;
            let (package, subpath) = rest.split_once('/').unwrap_or((rest, ""));
            (format!("{scope}/{package}"), subpath)
        } else {
            let (name, subpath) = specifier.split_once('/').unwrap_or((specifier, ""));
            (name.into(), subpath)
        };
        let importer_package = self
            .packages
            .iter()
            .filter(|(d, _)| within(importer, d))
            .max_by_key(|(d, _)| d.len());
        let mut directories = BTreeSet::new();
        if let Some((dir, package)) = importer_package {
            if package.get("name").and_then(Value::as_str) == Some(&name)
                && package.get("exports").is_some()
            {
                directories.insert(dir.clone());
            }
            for group in ["dependencies", "devDependencies", "optionalDependencies"] {
                if let Some(value) = package
                    .get(group)
                    .and_then(|d| d.get(&name))
                    .and_then(Value::as_str)
                    && let Some(relative) = value
                        .strip_prefix("file:")
                        .or_else(|| value.strip_prefix("link:"))
                    && let Some(target) = join(dir, relative)
                    && self.packages.contains_key(&target)
                {
                    directories.insert(target);
                }
            }
        }
        if let Some((workspace, members)) = self
            .workspaces
            .iter()
            .filter(|(d, _)| within(importer, d))
            .max_by_key(|(d, _)| d.len())
        {
            for (dir, package) in &self.packages {
                let relative = if workspace.is_empty() {
                    Some(dir.as_str())
                } else {
                    dir.strip_prefix(&format!("{workspace}/"))
                };
                if relative.is_some_and(|p| workspace_member(members, p))
                    && package.get("name").and_then(Value::as_str) == Some(&name)
                {
                    directories.insert(dir.clone());
                }
            }
        }
        if directories.len() != 1 {
            return None;
        }
        let dir = directories.into_iter().next()?;
        let package = &self.packages[&dir];
        let target = if let Some(exports) = package.get("exports") {
            export_target(exports, subpath, require)?
        } else if subpath.is_empty() {
            package
                .get("main")
                .and_then(Value::as_str)
                .unwrap_or("index.js")
                .into()
        } else {
            subpath.into()
        };
        let target = join(&dir, &target)?;
        if !within(&target, &dir) {
            return None;
        }
        self.file(importer, &target)
    }
    fn declared_dependency(&self, importer: &str, specifier: &str) -> Option<String> {
        if specifier.starts_with(['.', '/', '#']) || specifier.contains([':', '\\']) {
            return None;
        }
        let mut parts = specifier.split('/');
        let first = parts.next()?;
        let name = if first.starts_with('@') {
            format!("{first}/{}", parts.next()?)
        } else {
            first.to_owned()
        };
        if name.is_empty()
            || specifier
                .split('/')
                .any(|p| p.is_empty() || matches!(p, "." | ".."))
        {
            return None;
        }
        let (directory, package) = self
            .packages
            .iter()
            .filter(|(d, _)| within(importer, d))
            .max_by_key(|(d, _)| d.len())?;
        let declared = [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ]
        .iter()
        .any(|group| {
            package
                .get(group)
                .and_then(|v| v.get(&name))
                .is_some_and(Value::is_string)
        });
        declared.then(|| {
            format!(
                "npm:dependency:{}:{name}",
                join(directory, "package.json").unwrap()
            )
        })
    }
    fn resolve(&self, importer: &str, specifier: &str, require: bool) -> Option<String> {
        if specifier.starts_with('.') {
            return self.file(importer, &join(directory(importer), specifier)?);
        }
        if let Some((_, config)) = self
            .configs
            .iter()
            .filter(|(d, _)| within(importer, d))
            .max_by_key(|(d, _)| d.len())
        {
            if config.invalid_base_url {
                return self.package_target(importer, specifier, require);
            }
            let selected = config
                .paths
                .iter()
                .filter_map(|(pattern, targets)| {
                    capture(pattern, specifier).map(|capture| (pattern, targets, capture))
                })
                .max_by_key(|(pattern, _, _)| {
                    (
                        usize::from(!pattern.contains('*')),
                        pattern.split('*').next().unwrap_or("").len(),
                    )
                });
            if let Some((_, targets, capture)) = selected {
                let base = config.base_url.as_deref().unwrap_or(&config.paths_origin);
                for target in targets {
                    if let Some(path) = join(base, &target.replace('*', capture))
                        && let Some(module) = self.file(importer, &path)
                    {
                        return Some(module);
                    }
                }
            }
            if let Some(base) = &config.base_url
                && let Some(path) = join(base, specifier)
                && let Some(module) = self.file(importer, &path)
            {
                return Some(module);
            }
        }
        self.package_target(importer, specifier, require)
    }
    fn apply(&self, facts: &mut FileFacts) {
        self.apply_paths(facts);
        self.apply_imported_callees(facts);
        self.apply_factory_returns(facts);
    }
    fn apply_factory_returns(&self, facts: &mut FileFacts) {
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
    fn apply_imported_callees(&self, facts: &mut FileFacts) {
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
    fn apply_paths(&self, facts: &mut FileFacts) {
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
fn export_target(exports: &Value, subpath: &str, require: bool) -> Option<String> {
    let key = if subpath.is_empty() {
        ".".into()
    } else {
        format!("./{subpath}")
    };
    let (target, capture) = if let Some(map) = exports
        .as_object()
        .filter(|m| m.keys().any(|k| k.starts_with('.')))
    {
        let (pattern, target, capture) = map
            .iter()
            .filter_map(|(p, v)| capture(p, &key).map(|c| (p, v, c)))
            .max_by_key(|(p, _, _)| {
                (
                    usize::from(!p.contains('*')),
                    p.split('*').next().unwrap_or("").len(),
                )
            })?;
        if pattern.matches('*').count() > 1 {
            return None;
        }
        (target, capture)
    } else if subpath.is_empty() {
        (exports, "")
    } else {
        return None;
    };
    fn condition(value: &Value, require: bool) -> Option<&str> {
        if let Some(target) = value.as_str() {
            return Some(target);
        }
        let map = value.as_object()?;
        if map
            .keys()
            .any(|k| !matches!(k.as_str(), "import" | "default" | "require" | "types"))
        {
            return None;
        }
        let import = map
            .get(if require { "require" } else { "import" })
            .and_then(|v| condition(v, require));
        let default = map.get("default").and_then(|v| condition(v, require));
        match (import, default) {
            (Some(a), Some(b)) if a != b => None,
            (Some(a), _) | (_, Some(a)) => Some(a),
            _ => None,
        }
    }
    let target = condition(target, require)?;
    target
        .starts_with("./")
        .then(|| target.replace('*', capture))
}

#[derive(Default)]
struct RustContext {
    crates: BTreeMap<String, RustCrate>,
    owners: BTreeMap<String, String>,
    modules: BTreeMap<String, Vec<RustModule>>,
    forwarding: BTreeMap<String, Vec<String>>,
    generic_owners: BTreeMap<(String, String), u64>,
    fingerprint: String,
    fingerprints: BTreeMap<String, String>,
    source_hashes: BTreeMap<String, String>,
    facts: BTreeMap<String, FileFacts>,
}
struct RustCrate {
    name: String,
    library: String,
    root: Option<String>,
    dependencies: BTreeMap<String, String>,
    modules: BTreeSet<String>,
    public_modules: BTreeSet<String>,
    unavailable_modules: BTreeSet<String>,
}
#[derive(Debug)]
struct RustModule {
    package: String,
    module: String,
    public: bool,
}
fn toml_strings(item: Option<&toml_edit::Item>) -> Vec<String> {
    item.and_then(toml_edit::Item::as_array)
        .into_iter()
        .flat_map(|a| a.iter())
        .filter_map(toml_edit::Value::as_str)
        .map(str::to_owned)
        .collect()
}
impl RustContext {
    fn discover(inventory: &mut Inventory<'_>) -> Result<Self> {
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
    fn public_uses(&mut self, inventory: &Inventory<'_>) -> Result<String> {
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
    fn visit_module(
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
    fn prefix(&self, package: &str) -> String {
        format!(
            "rust:package:{}:{}:",
            if package.is_empty() { "." } else { package },
            self.crates[package].library
        )
    }
    fn generic_impl_known(&self, node: &Node) -> bool {
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
    fn apply(&self, facts: &mut FileFacts) {
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
    fn apply_paths(&self, facts: &mut FileFacts) {
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
                    && !reference.label.starts_with("super::")
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
fn outcome_fingerprint(discriminator: &str, mut facts: FileFacts) -> Result<String> {
    facts.hash.clear();
    facts.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    facts.edges.sort_by(|a, b| a.id.cmp(&b.id));
    facts.references.sort_by(|a, b| a.id.cmp(&b.id));
    facts
        .diagnostics
        .sort_by(|a, b| (&a.file, a.line, &a.message).cmp(&(&b.file, b.line, &b.message)));
    Ok(format!(
        "{discriminator}-{}",
        blake3::hash(&serde_json::to_vec(&facts)?).to_hex()
    ))
}
fn merge_aliases(node: &mut Node, mut aliases: Vec<String>) {
    if aliases.is_empty() {
        return;
    }
    if !node.metadata.is_object() {
        node.metadata = json!({});
    }
    aliases.extend(
        node.metadata
            .get("binding_aliases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned),
    );
    aliases.sort();
    aliases.dedup();
    node.metadata["binding_aliases"] = json!(aliases);
}

#[cfg(test)]
mod source_snapshot_tests {
    use super::*;

    #[test]
    fn rust_public_uses_requires_the_module_traversal_snapshot() {
        let declared = "mod child; pub fn caller() { crate::child::grand::work(); }";
        for changed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("src/child")).unwrap();
            std::fs::write(
                root.path().join("Cargo.toml"),
                "[package]\nname='demo'\nversion='0.1.0'\n",
            )
            .unwrap();
            let first = if changed {
                "pub fn caller() {}"
            } else {
                declared
            };
            std::fs::write(root.path().join("src/lib.rs"), first).unwrap();
            std::fs::write(root.path().join("src/child/grand.rs"), "pub fn work() {}").unwrap();
            let mut paths: Vec<String> = ["Cargo.toml", "src/lib.rs", "src/child/grand.rs"]
                .map(String::from)
                .into();
            if !changed {
                std::fs::write(root.path().join("src/child.rs"), "pub mod grand;").unwrap();
                paths.push("src/child.rs".into());
            }
            let inventory = Inventory::new(root.path(), &paths);
            let mut context = RustContext {
                crates: BTreeMap::from([(
                    String::new(),
                    RustCrate {
                        name: "demo".into(),
                        library: "demo".into(),
                        root: Some("src/lib.rs".into()),
                        dependencies: BTreeMap::new(),
                        modules: BTreeSet::new(),
                        public_modules: BTreeSet::new(),
                        unavailable_modules: BTreeSet::new(),
                    },
                )]),
                owners: paths
                    .iter()
                    .filter(|path| path.ends_with(".rs"))
                    .map(|path| (path.clone(), String::new()))
                    .collect(),
                ..Default::default()
            };
            context
                .visit_module(&inventory, "", "src/lib.rs", "", true, 0)
                .unwrap();
            let traversal_hashes = context.source_hashes.clone();
            assert_eq!(
                traversal_hashes["src/lib.rs"],
                blake3::hash(first.as_bytes()).to_hex().as_str()
            );
            if changed {
                // The new declaration rejects a missing parent, but traversal
                // has already observed the earlier source without that module.
                std::fs::write(root.path().join("src/lib.rs"), declared).unwrap();
                let error = context.public_uses(&inventory).unwrap_err();
                assert!(error.to_string().contains(
                    "source changed during Rust context discovery; retry indexing: src/lib.rs"
                ));
                assert!(context.facts.is_empty());
            } else {
                context.public_uses(&inventory).unwrap();
                assert_eq!(context.facts.len(), 3);
                assert!(
                    context.facts["src/lib.rs"]
                        .references
                        .iter()
                        .any(|reference| {
                            reference.relation == "calls" && !reference.candidate_keys.is_empty()
                        })
                );
                assert!(
                    context.facts["src/child/grand.rs"]
                        .nodes
                        .iter()
                        .any(|node| { node.label == "work" && node.kind == "function" })
                );
            }
            // Neither acceptance nor rejection may replace the traversal proof.
            assert_eq!(context.source_hashes, traversal_hashes);
        }
    }

    #[test]
    fn rust_cached_tokens_include_final_impl_checks_and_preserve_source_guards() {
        let root = tempfile::tempdir().unwrap();
        let sources = [
            ("Cargo.toml", "[package]\nname='demo'\nversion='0.1.0'\n"),
            ("src/lib.rs", "mod model; mod provider;"),
            ("src/model.rs", "pub struct Register<A>(pub A);"),
            (
                "src/provider.rs",
                "use crate::model::Register; impl<A, B> Register<A, B> { pub fn work() {} } // a",
            ),
        ];
        for (path, source) in sources {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        let mut paths: Vec<String> = sources.iter().map(|(path, _)| (*path).into()).collect();
        let mut context =
            ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new())
                .unwrap();
        paths.reverse();
        let reordered =
            ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new())
                .unwrap();
        let path = "src/provider.rs";
        let source = sources[3].1;
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        let token = context.fingerprint(path);
        assert_eq!(token, reordered.fingerprint(path));
        let mut cached = context.take_cached_facts(path, &hash).unwrap().unwrap();
        assert!(
            cached
                .nodes
                .iter()
                .find(|node| node.label == "work")
                .unwrap()
                .binding_key
                .is_some()
        );
        assert_ne!(
            token,
            outcome_fingerprint("rust-output-v1", cached.clone()).unwrap()
        );
        context.apply(&mut cached);
        assert!(
            cached
                .nodes
                .iter()
                .find(|node| node.label == "work")
                .unwrap()
                .binding_key
                .is_none()
        );
        assert_eq!(
            token,
            outcome_fingerprint("rust-output-v1", cached).unwrap()
        );
        assert!(context.take_cached_facts(path, &hash).unwrap().is_none());
        assert_eq!(token, context.fingerprint(path));

        // Same-size, comment-only edits still invalidate the discovery snapshot,
        // even after cached facts have been consumed.
        std::fs::write(root.path().join(path), source.replace("// a", "// b")).unwrap();
        let (changed_hash, _) = crate::index::read_source(
            &root.path().join(path),
            crate::parser::MAX_SOURCE_BYTES as u64,
        )
        .unwrap();
        assert!(context.validate_source(path, &changed_hash).is_err());
        assert!(context.take_cached_facts(path, &changed_hash).is_err());
        let edited =
            ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new())
                .unwrap();
        assert_eq!(token, edited.fingerprint(path));
        std::fs::write(root.path().join(path), source).unwrap();
        let (restored_hash, _) = crate::index::read_source(
            &root.path().join(path),
            crate::parser::MAX_SOURCE_BYTES as u64,
        )
        .unwrap();
        assert_eq!(restored_hash, hash);
        assert!(context.validate_source(path, &restored_hash).is_ok());
        assert!(edited.validate_source(path, &restored_hash).is_err());
    }

    #[test]
    fn javascript_provider_scratch_preserves_raw_facts_and_caller_outcomes() {
        let root = tempfile::tempdir().unwrap();
        let caller = "import {Shape, ordinary} from './provider'; function use(value: Shape): Shape { Shape(); ordinary(); return value; }";
        std::fs::write(root.path().join("main.ts"), caller).unwrap();
        let paths = ["provider.ts".into(), "main.ts".into()];
        let mut previous = None;
        for private_count in [0, 16, 0] {
            let mut provider = String::from(
                "export interface Shape {}\nexport const Shape = factory();\nexport function ordinary() {}\nfunction privateScope() {\nfunction local() {} local();\n",
            );
            for i in 0..private_count {
                provider.push_str(&format!("function helper_{i}() {{}} helper_{i}();\n"));
            }
            provider.push_str("}\n");
            std::fs::write(root.path().join("provider.ts"), &provider).unwrap();
            let mut context =
                ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new())
                    .unwrap();
            let provider_hash = blake3::hash(provider.as_bytes()).to_hex().to_string();
            let raw = crate::languages::parse("provider.ts", &provider, &provider_hash)
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(&raw).unwrap(),
                serde_json::to_value(&context.javascript.raw_facts["provider.ts"]).unwrap()
            );
            let caller_hash = blake3::hash(caller.as_bytes()).to_hex().to_string();
            let mut applied = context
                .take_cached_facts("main.ts", &caller_hash)
                .unwrap()
                .unwrap();
            context.apply(&mut applied);
            let outcome = (
                context.javascript.imported_callees.clone(),
                context.fingerprint("main.ts"),
                serde_json::to_value(applied).unwrap(),
            );
            if let Some(previous) = previous {
                assert_eq!(previous, outcome);
            }
            previous = Some(outcome);
        }
    }

    #[test]
    fn javascript_imported_declarations_require_unique_complete_provider_proof() {
        let parse = |path: &str, source: &str| {
            crate::languages::parse(path, source, "raw")
                .unwrap()
                .unwrap()
        };
        let provider = parse(
            "provider.ts",
            "export interface Shape {} export const Shape = factory(); export function ordinary() {}",
        );
        let other = parse("other.ts", "export const Shape = factory();");
        let caller = parse(
            "main.ts",
            "import {Shape as Value} from './provider'; function use(v: Value): Value { Value(); return v; }",
        );
        for variation in [
            "valid",
            "missing marker",
            "false marker",
            "wrong kind",
            "wrong key",
            "competing owner",
            "ordinary candidate",
            "other declaration",
            "unproven candidate",
        ] {
            let mut raw = provider.clone();
            let constant = raw.nodes.iter().position(|n| n.kind == "constant").unwrap();
            match variation {
                "missing marker" => {
                    raw.nodes[constant]
                        .metadata
                        .as_object_mut()
                        .unwrap()
                        .remove("declared_callee_binding");
                }
                "false marker" => {
                    raw.nodes[constant].metadata["declared_callee_binding"] = false.into()
                }
                "wrong kind" => raw.nodes[constant].kind = "function".into(),
                "wrong key" => {
                    raw.nodes[constant].binding_key = Some("javascript:provider:Shape".into())
                }
                "competing owner" => {
                    let mut competitor = raw.nodes[constant].clone();
                    competitor.id.push_str(":competitor");
                    competitor.kind = "function".into();
                    competitor.metadata["declared_callee_binding"] = false.into();
                    raw.nodes.push(competitor);
                }
                _ => (),
            }
            let mut context = JavascriptContext {
                files: ["provider.ts", "other.ts", "main.ts"]
                    .map(String::from)
                    .into(),
                raw_facts: [
                    ("provider.ts".into(), raw),
                    ("other.ts".into(), other.clone()),
                ]
                .into(),
                ..Default::default()
            };
            context.exports();
            let mut facts = caller.clone();
            context.apply_paths(&mut facts);
            let call = facts
                .references
                .iter_mut()
                .find(|r| r.relation == "calls")
                .unwrap();
            match variation {
                "ordinary candidate" => call
                    .candidate_keys
                    .push("javascript:file:provider.ts:ordinary".into()),
                "other declaration" => call
                    .candidate_keys
                    .push("javascript:file:other.ts:Shape".into()),
                "unproven candidate" => call
                    .candidate_keys
                    .push("javascript:file:absent.ts:Shape".into()),
                _ => (),
            }
            let original = call.clone();
            let types: Vec<_> = facts
                .references
                .iter()
                .filter(|r| r.relation != "calls")
                .map(|r| serde_json::to_value(r).unwrap())
                .collect();
            context.apply_imported_callees(&mut facts);
            let calls: Vec<_> = facts
                .references
                .iter()
                .filter(|r| r.relation == "calls")
                .collect();
            assert_eq!(calls.len(), 1);
            assert!(calls[0].candidate_keys.is_empty(), "{variation}");
            let mut runtime = original.clone();
            runtime.candidate_keys.clear();
            assert_eq!(
                serde_json::to_value(calls[0]).unwrap(),
                serde_json::to_value(runtime).unwrap()
            );
            let siblings: Vec<_> = facts
                .references
                .iter()
                .filter(|r| r.relation == "declared_callee")
                .collect();
            assert_eq!(
                siblings.len(),
                usize::from(variation == "valid"),
                "{variation}"
            );
            if let Some(sibling) = siblings.first() {
                assert_eq!(sibling.id, format!("{}:declared_callee", original.id));
                assert_eq!(
                    (&sibling.source, &sibling.label, &sibling.file, sibling.line),
                    (
                        &original.source,
                        &original.label,
                        &original.file,
                        original.line
                    )
                );
                assert_eq!(
                    sibling.candidate_keys,
                    ["javascript:file:provider.ts:Shape#declared_callee"]
                );
                assert_eq!(
                    sibling.reason,
                    "written immutable callee binding; factory result and runtime dispatch are unresolved"
                );
            }
            assert_eq!(
                types,
                facts
                    .references
                    .iter()
                    .filter(|r| !matches!(r.relation.as_str(), "calls" | "declared_callee"))
                    .map(|r| serde_json::to_value(r).unwrap())
                    .collect::<Vec<_>>()
            );
            let once = serde_json::to_value(&facts).unwrap();
            context.apply_imported_callees(&mut facts);
            assert_eq!(once, serde_json::to_value(facts).unwrap());
        }
    }

    #[test]
    fn javascript_cache_returns_raw_facts_once_and_retains_output_tokens() {
        let root = tempfile::tempdir().unwrap();
        let sources = [
            ("lib.cjs", "exports.work = function work() {};"),
            (
                "main.cjs",
                "const lib = require('./lib.cjs'); function use() { lib.work(); }",
            ),
            (
                "typed.ts",
                "import {work} from './lib.cjs'; function use() { work(); }",
            ),
            ("broken.ts", "export function ("),
        ];
        for (path, source) in sources {
            std::fs::write(root.path().join(path), source).unwrap();
        }
        let mut paths: Vec<String> = sources.iter().map(|(p, _)| (*p).into()).collect();
        let mut context =
            ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new())
                .unwrap();
        paths.reverse();
        let reordered =
            ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new())
                .unwrap();
        for (path, source) in sources {
            let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            let raw = crate::languages::parse(path, source, &hash)
                .unwrap()
                .unwrap();
            let token = context.fingerprint(path);
            assert_eq!(token, reordered.fingerprint(path));
            let mut cached = context.take_cached_facts(path, &hash).unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(&cached).unwrap(),
                serde_json::to_value(&raw).unwrap()
            );
            let mut expected = raw;
            context.apply(&mut expected);
            context.apply(&mut cached);
            assert_eq!(
                serde_json::to_value(&cached).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            assert_eq!(
                token,
                JavascriptContext::outcome_fingerprint(cached).unwrap()
            );
            assert!(context.take_cached_facts(path, &hash).unwrap().is_none());
            assert_eq!(token, context.fingerprint(path));
            assert!(context.validate_source(path, "different bytes").is_err());
        }
    }

    #[test]
    fn javascript_context_rejects_changed_bytes_and_configuration_observations() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lib.ts");
        let old = "export function old() {}";
        let new = "export function new() {}";
        std::fs::write(&path, new).unwrap();
        std::fs::write(root.path().join("bad.js"), [255]).unwrap();
        std::fs::write(
            root.path().join("large.js"),
            vec![b' '; crate::parser::MAX_SOURCE_BYTES + 1],
        )
        .unwrap();
        std::fs::write(root.path().join("tool"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(
            root.path().join("tsconfig.json"),
            r#"{"extends":"./base.json"}"#,
        )
        .unwrap();
        std::fs::write(root.path().join("base.json"), "{}").unwrap();
        let paths = [
            "lib.ts",
            "bad.js",
            "large.js",
            "tool",
            "tsconfig.json",
            "base.json",
        ]
        .map(String::from);
        let mut context =
            ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new())
                .unwrap();
        assert!(!context.javascript.files.contains("tool"));
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, old).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        for (name, bytes) in [
            ("lib.ts", old.as_bytes()),
            ("bad.js", b"x".as_slice()),
            ("large.js", b"export {};".as_slice()),
            ("tool", b"#!/usr/bin/env node\nrun();".as_slice()),
        ] {
            if name != "lib.ts" {
                std::fs::write(root.path().join(name), bytes).unwrap();
            }
            let (hash, _) = crate::index::read_source(
                &root.path().join(name),
                crate::parser::MAX_SOURCE_BYTES as u64,
            )
            .unwrap();
            assert!(context.validate_source(name, &hash).is_err(), "{name}");
            assert!(context.take_cached_facts(name, &hash).is_err(), "{name}");
        }
        assert!(context.javascript.raw_facts.contains_key("lib.ts"));
        let inventory = Inventory::new(root.path(), &paths);
        context.javascript.validate_configs(&inventory).unwrap();
        for (name, contents) in [
            ("base.json", "{ }"),
            ("package.json", "{}"),
            ("base.json", "{}"),
        ] {
            std::fs::write(root.path().join(name), contents).unwrap();
            if name == "base.json" && contents == "{}" {
                std::fs::remove_file(root.path().join("package.json")).unwrap();
                context.javascript.validate_configs(&inventory).unwrap();
            } else {
                assert!(context.javascript.validate_configs(&inventory).is_err());
                assert!(
                    context
                        .validate_source(name, blake3::hash(contents.as_bytes()).to_hex().as_ref())
                        .is_err()
                );
            }
        }
        std::fs::remove_file(root.path().join("base.json")).unwrap();
        assert!(context.javascript.validate_configs(&inventory).is_err());
    }

    #[test]
    fn javascript_output_token_hashes_complete_facts_except_the_source_stamp() {
        let raw = crate::languages::parse(
            "main.ts",
            "import {work} from './lib'; function use() { work(); }",
            "raw",
        )
        .unwrap()
        .unwrap();
        let token = JavascriptContext::outcome_fingerprint(raw.clone()).unwrap();
        let mut reordered = raw.clone();
        reordered.hash = "final-stamp".into();
        reordered.nodes.reverse();
        reordered.edges.reverse();
        reordered.references.reverse();
        assert_eq!(
            token,
            JavascriptContext::outcome_fingerprint(reordered).unwrap()
        );
        let call = raw
            .references
            .iter()
            .position(|r| r.relation == "calls")
            .unwrap();
        for field in ["candidate", "reason", "line", "metadata", "diagnostic"] {
            let mut changed = raw.clone();
            match field {
                "candidate" => changed.references[call]
                    .candidate_keys
                    .push("another:target".into()),
                "reason" => changed.references[call].reason.push('!'),
                "line" => changed.references[call].line += 1,
                "metadata" => changed.nodes[0].metadata["proof"] = true.into(),
                _ => changed.diagnostics.push(crate::model::Diagnostic {
                    file: "main.ts".into(),
                    line: Some(1),
                    message: "unsupported".into(),
                }),
            }
            assert_ne!(
                token,
                JavascriptContext::outcome_fingerprint(changed).unwrap(),
                "{field}"
            );
        }
        let mut ordered = raw;
        ordered.references[call].candidate_keys = vec!["first".into(), "second".into()];
        let first = JavascriptContext::outcome_fingerprint(ordered.clone()).unwrap();
        ordered.references[call].candidate_keys.reverse();
        assert_ne!(
            first,
            JavascriptContext::outcome_fingerprint(ordered).unwrap()
        );
    }

    #[test]
    fn swiftpm_literal_ast_comments_and_raw_snapshot_validation() {
        let source = r#"// swift-tools-version: 6.0
import PackageDescription
let /* binding */ package = Package(
    name: "Example", platforms: [.macOS(.v13)],
    products: [.library(name: "Core", targets: ["Core"])],
    dependencies: [.package(url: "https://example.invalid/library", from: "1.0.0")],
    targets: [/* .target(name: "Fake") */ .target(/* argument */ name: "Core"),]
)
"#;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let targets =
            swift_package(source, "").unwrap_or_else(|| panic!("{}", tree.root_node().to_sexp()));
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].root, "Sources/Core");
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Package.swift"), source).unwrap();
        let paths = vec!["Package.swift".into()];
        let inventory = Inventory::new(root.path(), &paths);
        let swift = SwiftContext::discover(&inventory, &BTreeMap::new()).unwrap();
        let changed = format!("{source}// only a comment changed\n");
        let hash = |s: &str| blake3::hash(s.as_bytes()).to_hex().to_string();
        swift
            .validate_source("Package.swift", &hash(source))
            .unwrap();
        assert!(
            swift
                .validate_source("Package.swift", &hash(&changed))
                .is_err()
        );
        std::fs::write(root.path().join("Package.swift"), &changed).unwrap();
        let mut context = ProjectContext {
            swift,
            ..Default::default()
        };
        assert!(
            context
                .compiled_inventory(&inventory)
                .unwrap_err()
                .to_string()
                .contains("Swift manifest")
        );
        let refreshed = SwiftContext::discover(&inventory, &BTreeMap::new()).unwrap();
        assert_ne!(context.swift.fingerprint, refreshed.fingerprint);
        // Uninventoried manifests are never probed, even though one exists on disk.
        let inventory = Inventory::new(root.path(), &[]);
        assert!(
            SwiftContext::discover(&inventory, &BTreeMap::new())
                .unwrap()
                .source_hashes
                .is_empty()
        );
    }

    #[test]
    fn compiled_and_template_proofs_reject_changed_source_snapshots() {
        let root = tempfile::tempdir().unwrap();
        let public = "namespace api; public class Base { public void work() {} }";
        let private = "namespace api; public class Base { private void work() {} }";
        std::fs::write(root.path().join("App.csproj"), "<Project/>").unwrap();
        std::fs::write(root.path().join("Base.cs"), public).unwrap();
        let pascal = "unit Proof; interface implementation end.";
        std::fs::write(root.path().join("Proof.pas"), pascal).unwrap();
        let paths = vec!["App.csproj".into(), "Base.cs".into(), "Proof.pas".into()];
        let database = tempfile::tempdir().unwrap();
        let db = database.path().join("graph.db");
        crate::index::run_with_options(
            root.path(),
            &db,
            &crate::index::IndexOptions {
                code_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        let snapshot = || {
            serde_json::to_value(crate::store::Store::open(&db).unwrap().snapshot().unwrap())
                .unwrap()
        };
        let previous_graph = snapshot();
        let mut context =
            ProjectContext::discover_with_swift_modules(root.path(), &paths, &BTreeMap::new())
                .unwrap();
        let hash = |source: &str| blake3::hash(source.as_bytes()).to_hex().to_string();
        context.validate_source("Base.cs", &hash(public)).unwrap();
        context.validate_source("Proof.pas", &hash(pascal)).unwrap();
        assert!(
            context
                .validate_source(
                    "Proof.pas",
                    &hash("unit Changed; interface implementation end.")
                )
                .is_err()
        );
        std::fs::write(root.path().join("Base.cs"), private).unwrap();
        let (changed, _) = crate::index::read_source(
            &root.path().join("Base.cs"),
            crate::parser::MAX_SOURCE_BYTES as u64,
        )
        .unwrap();
        assert!(
            context
                .validate_source("Base.cs", &changed)
                .unwrap_err()
                .to_string()
                .contains("retry indexing")
        );
        // A later compiled read must also reject a stale earlier template inventory.
        let inventory = Inventory::new(root.path(), &paths);
        assert!(
            context
                .compiled_inventory(&inventory)
                .unwrap_err()
                .to_string()
                .contains("template context")
        );
        assert!(
            context
                .validate_source("App.csproj", &hash("<Project><PropertyGroup/></Project>"))
                .is_err()
        );
        context
            .validate_source("Uninventoried.cs", &hash(private))
            .unwrap();
        assert_eq!(
            snapshot(),
            previous_graph,
            "rejected discovery must preserve the stored graph"
        );
    }
}
