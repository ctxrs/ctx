use super::*;

impl TemplateContext {
    pub(super) fn discover(inventory: &mut Inventory<'_>) -> Result<Self> {
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
    pub(super) fn validate_source(&self, path: &str, content_hash: &str) -> Result<()> {
        ensure!(
            self.source_hashes
                .get(path)
                .is_none_or(|expected| expected == content_hash),
            "source changed during template context discovery; retry indexing: {path}"
        );
        Ok(())
    }
    pub(super) fn unit(&self, path: &str) -> Option<String> {
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
    pub(super) fn apply(&self, facts: &mut FileFacts) {
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
