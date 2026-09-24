use super::*;

impl TerraformContext {
    pub fn discover(root: &Path, paths: &[String]) -> Result<Self> {
        let root = root
            .canonicalize()
            .context("cannot locate Terraform project root")?;
        ensure!(root.is_dir(), "Terraform project root must be a directory");
        let mut context = Self::default();
        let mut hash = blake3::Hasher::new();
        hash.update(b"terraform-context-3");
        let mut modules = vec![];
        let paths: BTreeSet<_> = paths
            .iter()
            .filter(|p| p.ends_with(".tf") || p.ends_with(".tfvars"))
            .collect();
        for path in paths {
            ensure!(
                !path.contains(['\\', ':'])
                    && path.split('/').all(|p| !matches!(p, "" | "." | "..")),
                "Terraform inventory paths must be normalized repository-relative paths"
            );
            let (content_hash, source) = indexed_config_source(&root, path)?;
            for part in [path.as_str(), content_hash.as_str()] {
                hash.update(&(part.len() as u64).to_le_bytes());
                hash.update(part.as_bytes());
            }
            let Some(source) = source else {
                continue;
            };
            let Some(facts) = parse(path, &source, &content_hash)? else {
                continue;
            };
            if facts.nodes.is_empty() || !facts.diagnostics.is_empty() {
                continue;
            }
            context.files.insert(path.clone());
            if !path.ends_with(".tf") {
                continue;
            }
            context
                .directories
                .entry(directory(path).into())
                .or_default()
                .push(path.clone());
            for node in &facts.nodes {
                let Some(key) = node
                    .binding_key
                    .as_ref()
                    .filter(|key| key.starts_with("terraform:"))
                else {
                    continue;
                };
                *context.definitions.entry(key.clone()).or_default() += 1;
                if node.kind == "output" {
                    context.outputs.insert(key.clone());
                }
                if node.kind == "module" {
                    let target = node.metadata["module_source"]
                        .as_str()
                        .and_then(|source| terraform_target(path, source));
                    modules.push((key.clone(), target));
                }
            }
        }
        for (key, target) in modules {
            let target = target.filter(|directory| {
                context.definitions.get(&key) == Some(&1)
                    && context.directories.contains_key(directory)
            });
            context.modules.insert(key, target);
        }
        context.fingerprint = hash.finalize().to_hex().to_string();
        Ok(context)
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Add directory containment and bind only evidenced local module outputs.
    /// All added references belong to the calling file or the anchor's owner file.
    pub fn apply(&self, facts: &mut FileFacts) {
        if !self.files.contains(&facts.path) || !facts.path.ends_with(".tf") {
            return;
        }
        let dir = directory(&facts.path);
        let Some(members) = self.directories.get(dir) else {
            return;
        };
        let root_key = format!("config:file:{}", facts.path);
        if !facts
            .nodes
            .iter()
            .any(|n| n.binding_key.as_ref() == Some(&root_key))
        {
            return;
        }
        facts
            .nodes
            .retain(|n| n.metadata["terraform_directory_anchor"] != true);
        facts
            .references
            .retain(|r| !r.id.starts_with("terraform-context:"));
        let module_sources: HashMap<_, _> = facts
            .nodes
            .iter()
            .filter(|n| n.kind == "module")
            .filter_map(|n| Some((n.id.as_str(), n.binding_key.as_deref()?)))
            .collect();
        let output_prefix = format!("terraform:module-output:{dir}:");
        for reference in &mut facts.references {
            if reference.relation == "module_source" {
                reference.candidate_keys = module_sources
                    .get(reference.source.as_str())
                    .and_then(|key| self.modules.get(*key))
                    .and_then(Option::as_ref)
                    .map(|target| vec![format!("terraform:directory:{target}")])
                    .unwrap_or_default();
            } else if let Some(output) = reference
                .candidate_keys
                .first()
                .and_then(|key| key.strip_prefix(&output_prefix))
            {
                let target = output.split_once(':').and_then(|(call, output)| {
                    let module = format!("terraform:{dir}:module.{call}");
                    let target = self.modules.get(&module)?.as_ref()?;
                    let key = format!("terraform:{target}:output.{output}");
                    (self.outputs.contains(&key) && self.definitions.get(&key) == Some(&1))
                        .then_some(key)
                });
                reference.candidate_keys = target.into_iter().collect();
            } else {
                reference.candidate_keys.retain(|key| {
                    !key.starts_with("terraform:") || self.definitions.contains_key(key)
                });
            }
        }
        if members.first() != Some(&facts.path) {
            return;
        }
        let anchor = format!("terraform:directory:{dir}");
        let label = format!(
            "Terraform module: {}",
            if dir.is_empty() { "." } else { dir }
        );
        facts.nodes.push(Node {
            id: anchor.clone(), label: label.clone(), kind: "module".into(), file: facts.path.clone(),
            line: Some(1), end_line: Some(1), qualified_name: Some(label), binding_key: Some(anchor.clone()),
            metadata: json!({"language":"terraform", "directory":dir, "terraform_directory_anchor":true}),
        });
        for member in members {
            facts.references.push(Reference {
                id: format!("terraform-context:{}:contains:{member}", facts.path),
                source: anchor.clone(),
                label: member.clone(),
                relation: "contains".into(),
                file: facts.path.clone(),
                line: 1,
                candidate_keys: vec![format!("config:file:{member}")],
                reason: "Indexed Terraform member is unavailable".into(),
            });
        }
    }
}
