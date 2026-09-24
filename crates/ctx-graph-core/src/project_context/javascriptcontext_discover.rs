use super::*;

impl JavascriptContext {
    pub(super) fn discover(inventory: &mut Inventory<'_>) -> Result<Self> {
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
    pub(super) fn validate_source(&self, path: &str, hash: &str) -> Result<()> {
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
    pub(super) fn validate_configs(&self, inventory: &Inventory<'_>) -> Result<()> {
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
    pub(super) fn outcome_fingerprint(facts: FileFacts) -> Result<String> {
        outcome_fingerprint("javascript-output-v1", facts)
    }
    pub(super) fn commonjs(&self, path: &str) -> bool {
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
    pub(super) fn exports(&mut self) {
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
    pub(super) fn callee_providers(
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
    pub(super) fn file(&self, importer: &str, target: &str) -> Option<String> {
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
    pub(super) fn package_target(
        &self,
        importer: &str,
        specifier: &str,
        require: bool,
    ) -> Option<String> {
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
    pub(super) fn declared_dependency(&self, importer: &str, specifier: &str) -> Option<String> {
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
    pub(super) fn resolve(&self, importer: &str, specifier: &str, require: bool) -> Option<String> {
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
    pub(super) fn apply(&self, facts: &mut FileFacts) {
        self.apply_paths(facts);
        self.apply_imported_callees(facts);
        self.apply_factory_returns(facts);
    }
}
