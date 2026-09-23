//! Declarative project metadata. Parsing never evaluates configuration or opens referenced paths.
use super::common::{children, diagnostic, line, relative_path, tree};
use crate::model::{Edge, FileFacts, Node, Reference};
use anyhow::{Context, Result, bail, ensure};
use quick_xml::{Reader, events::Event};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::Path,
};
use tree_sitter::Node as Syntax;

const CONFIG_MAX_BYTES: usize = 2_097_152;

const JSON_NAMES: &[&str] = &[
    "package.json",
    "tsconfig.json",
    "jsconfig.json",
    "composer.json",
    "deno.json",
    "deno.jsonc",
    "bower.json",
    "manifest.json",
    "app.json",
    "now.json",
    "vercel.json",
    "angular.json",
    "nest-cli.json",
    "biome.json",
    "biome.jsonc",
    "renovate.json",
    ".babelrc",
    ".babelrc.json",
    ".eslintrc.json",
    ".prettierrc.json",
    ".prettierrc",
    "babel.config.json",
];
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}
fn json_config(path: &str) -> bool {
    let n = basename(path).to_ascii_lowercase();
    JSON_NAMES.contains(&n.as_str())
        || [
            ".eslintrc.json",
            ".prettierrc.json",
            ".babelrc.json",
            "tsconfig.json",
            "jsconfig.json",
        ]
        .iter()
        .any(|s| n.ends_with(s))
        || ((n.starts_with("tsconfig.") || n.starts_with("jsconfig.")) && n.ends_with(".json"))
}
/// Named manifests take priority; ordinary JSON/YAML documents are not code.
pub fn supports(path: &str) -> bool {
    matches!(
        basename(path),
        "Cargo.toml" | "pyproject.toml" | "go.mod" | "pom.xml" | "apm.yml" | "apm.yaml"
    ) || json_config(path)
        || mcp_config(path)
        || matches!(
            path.rsplit('.').next(),
            Some("sql" | "tf" | "tfvars" | "hcl" | "sln" | "slnx" | "csproj" | "fsproj" | "vbproj")
        )
}
/// Content probe for arbitrarily named JSON configs; ordinary documents stay ingestible.
pub fn recognizes(path: &str, source: &str) -> bool {
    if supports(path) {
        return true;
    }
    if !matches!(path.rsplit('.').next(), Some("json" | "jsonc")) || source.len() > 1_048_576 {
        return false;
    }
    let Ok(clean) = jsonc(source) else {
        return false;
    };
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(&clean) else {
        return false;
    };
    [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
        "bundleDependencies",
        "bundledDependencies",
        "extends",
        "$ref",
        "$schema",
        "compilerOptions",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
}
/// Extract only supplied content, using project-relative identities.
pub fn parse(path: &str, source: &str, hash: &str) -> Result<Option<FileFacts>> {
    if !recognizes(path, source) {
        return Ok(None);
    }
    if path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        bail!("source path must be a normalized relative POSIX path");
    }
    let mut f = Facts::new(path, hash);
    let limit = if json_config(path)
        || mcp_config(path)
        || matches!(path.rsplit('.').next(), Some("json" | "jsonc"))
    {
        1_048_576
    } else {
        CONFIG_MAX_BYTES
    };
    if source.len() > limit {
        diagnostic(
            &mut f.0,
            None,
            "Configuration exceeds the indexing size limit",
        );
        return Ok(Some(f.0));
    }
    let result = match basename(path) {
        "Cargo.toml" | "pyproject.toml" => toml_manifest(&mut f, source),
        "go.mod" => go_manifest(&mut f, source),
        "pom.xml" => xml_config(&mut f, source, "maven"),
        "apm.yml" | "apm.yaml" => apm_manifest(&mut f, source),
        _ if mcp_config(path) => mcp(&mut f, source),
        _ if json_config(path) || matches!(path.rsplit('.').next(), Some("json" | "jsonc")) => {
            json_manifest(&mut f, source)
        }
        _ => match path.rsplit('.').next().unwrap_or("") {
            "sql" => sql(&mut f, source),
            "tf" | "tfvars" | "hcl" => hcl(&mut f, source),
            "sln" => {
                sln(&mut f, source);
                Ok(())
            }
            _ => xml_config(&mut f, source, "dotnet"),
        },
    };
    if result.is_err() {
        // Parser errors can quote source values. Retain no partial graph or raw error text.
        f.0.nodes.clear();
        f.0.edges.clear();
        f.0.references.clear();
        diagnostic(
            &mut f.0,
            None,
            "Malformed or unsupported configuration syntax",
        );
    }
    Ok(Some(f.0))
}
struct Facts(FileFacts);
impl Facts {
    fn new(path: &str, hash: &str) -> Self {
        Self(FileFacts {
            path: path.into(),
            hash: hash.into(),
            module: path.into(),
            nodes: vec![],
            edges: vec![],
            references: vec![],
            diagnostics: vec![],
        })
    }
    fn node(
        &mut self,
        label: &str,
        kind: &str,
        key: Option<String>,
        line: u32,
        metadata: Value,
    ) -> String {
        let id = format!("config:{}:{}", self.0.path, self.0.nodes.len());
        self.0.nodes.push(Node {
            id: id.clone(),
            label: label.into(),
            kind: kind.into(),
            file: self.0.path.clone(),
            line: Some(line),
            end_line: Some(line),
            qualified_name: Some(label.into()),
            binding_key: key,
            metadata,
        });
        id
    }
    fn root(&mut self, language: &str, metadata: Value) -> String {
        let path = self.0.path.clone();
        let mut data = metadata;
        data["language"] = json!(language);
        self.node(
            basename(&path),
            "module",
            Some(format!("config:file:{path}")),
            1,
            data,
        )
    }
    fn edge(&mut self, from: &str, to: &str, relation: &str, line: u32) {
        if from == to
            || self
                .0
                .edges
                .iter()
                .any(|e| e.source == from && e.target == to && e.relation == relation)
        {
            return;
        }
        self.0.edges.push(Edge {
            id: format!("config-edge:{}:{}", self.0.path, self.0.edges.len()),
            source: from.into(),
            target: to.into(),
            relation: relation.into(),
            directed: true,
            file: Some(self.0.path.clone()),
            line: Some(line),
            confidence: "static".into(),
            metadata: Value::Null,
        });
    }
    fn reference(&mut self, from: &str, label: &str, relation: &str, keys: Vec<String>, line: u32) {
        if self.0.references.iter().any(|r| {
            r.source == from
                && r.label == label
                && r.relation == relation
                && r.candidate_keys == keys
        }) {
            return;
        }
        self.0.references.push(Reference {
            id: format!("config-ref:{}:{}", self.0.path, self.0.references.len()),
            source: from.into(),
            label: label.into(),
            relation: relation.into(),
            file: self.0.path.clone(),
            line,
            candidate_keys: keys,
            reason: "Declared target is external, unavailable, or ambiguous".into(),
        });
    }
    fn package(
        &mut self,
        root: &str,
        ecosystem: &str,
        name: &str,
        version: Option<&str>,
    ) -> String {
        let id = self.node(
            name,
            "package",
            Some(package_key(ecosystem, name)),
            1,
            json!({"ecosystem":ecosystem,"version":version}),
        );
        self.edge(root, &id, "contains", 1);
        id
    }
    fn dependency(&mut self, from: &str, ecosystem: &str, name: &str, line: u32) {
        let key = package_key(ecosystem, name);
        if name.is_empty()
            || self
                .0
                .nodes
                .iter()
                .any(|n| n.id == from && n.binding_key.as_ref() == Some(&key))
        {
            return;
        }
        self.reference(from, name, "depends_on", vec![key], line);
    }
}
fn package_key(ecosystem: &str, name: &str) -> String {
    let name = if ecosystem == "python" {
        name.to_ascii_lowercase().replace(['_', '.'], "-")
    } else {
        name.to_owned()
    };
    format!("package:{ecosystem}:{name}")
}
fn local_target(path: &str, target: &str) -> Option<String> {
    let target = target.replace('\\', "/");
    if target.starts_with('/')
        || target.contains(':')
        || target.contains('$')
        || target.contains('*')
    {
        return None;
    }
    relative_path(directory(path), &target)
}
fn path_keys(path: &str, target: &str) -> Vec<String> {
    local_target(path, target)
        .map(|p| vec![format!("config:file:{p}")])
        .unwrap_or_default()
}
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => vec![],
    }
}
fn toml_value(item: &toml_edit::Item) -> Value {
    if let Some(t) = item.as_table_like() {
        return Value::Object(t.iter().map(|(k, v)| (k.into(), toml_value(v))).collect());
    }
    if let Some(a) = item.as_array_of_tables() {
        return Value::Array(
            a.iter()
                .map(|t| toml_value(&toml_edit::Item::Table(t.clone())))
                .collect(),
        );
    }
    fn value(v: &toml_edit::Value) -> Value {
        match v {
            toml_edit::Value::String(s) => json!(s.value()),
            toml_edit::Value::Boolean(b) => json!(b.value()),
            toml_edit::Value::Integer(n) => json!(n.value()),
            toml_edit::Value::Float(n) => json!(n.value()),
            toml_edit::Value::Array(a) => Value::Array(a.iter().map(value).collect()),
            toml_edit::Value::InlineTable(t) => {
                Value::Object(t.iter().map(|(k, v)| (k.into(), value(v))).collect())
            }
            _ => Value::Null,
        }
    }
    item.as_value().map(value).unwrap_or(Value::Null)
}
/// Package navigation from exact indexed manifests, independent of Rust source files.
/// Include this fingerprint in Cargo.toml stamps and apply to freshly parsed facts.
#[derive(Default)]
pub struct CargoPackageContext {
    fingerprint: String,
    packages: BTreeMap<String, CargoPackage>,
    members: BTreeMap<String, Vec<String>>,
}
struct CargoPackage {
    name: String,
    id: String,
    workspace: Option<String>,
    dependencies: Vec<CargoPackageDependency>,
}
struct CargoPackageDependency {
    alias: String,
    label: String,
    target: Option<String>,
    optional: bool,
}
struct CargoWorkspace {
    members: Vec<globset::GlobMatcher>,
    exclude: Vec<globset::GlobMatcher>,
}
impl CargoWorkspace {
    fn includes(&self, workspace: &str, manifest: &str) -> bool {
        workspace == manifest
            || (self.members.iter().any(|g| g.is_match(directory(manifest)))
                && !self.exclude.iter().any(|g| g.is_match(directory(manifest))))
    }
}
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
fn cargo_manifest_path(directory: &str) -> String {
    if directory.is_empty() {
        "Cargo.toml".into()
    } else {
        format!("{directory}/Cargo.toml")
    }
}
fn cargo_package_key(manifest: &str, name: &str) -> String {
    format!("cargo:manifest:{manifest}:{name}")
}
fn cargo_directory(manifest: &str, path: &str) -> Option<String> {
    if path.is_empty()
        || path.starts_with(['/', '~'])
        || path.contains(['\\', ':', '$', '%', '*', '?', '[', ']'])
    {
        return None;
    }
    relative_path(directory(manifest), path)
}
fn cargo_patterns(manifest: &str, value: &Value) -> Option<Vec<globset::GlobMatcher>> {
    if value.is_null() {
        return Some(vec![]);
    }
    value
        .as_array()?
        .iter()
        .map(|pattern| {
            let pattern = pattern.as_str()?;
            if pattern.is_empty()
                || pattern.starts_with(['/', '~'])
                || pattern.contains(['\\', ':', '$', '%', '{', '}'])
            {
                return None;
            }
            let mut wildcard = false;
            for component in pattern.split('/') {
                if component == ".." && wildcard {
                    return None;
                }
                wildcard |= component.contains(['*', '?', '[']);
            }
            let pattern = relative_path(directory(manifest), pattern)?;
            globset::GlobBuilder::new(&pattern)
                .literal_separator(true)
                .backslash_escape(false)
                .build()
                .ok()
                .map(|g| g.compile_matcher())
        })
        .collect()
}

fn toml_manifest(f: &mut Facts, source: &str) -> Result<()> {
    let doc: toml_edit::DocumentMut = source.parse()?;
    let data = toml_value(doc.as_item());
    if basename(&f.0.path) == "Cargo.toml" {
        let root = f.root("cargo", json!({"workspace":data["workspace"], "package":data["package"], "lib":data["lib"], "bin":data["bin"], "dependencies":data["dependencies"], "target":data["target"]}));
        let Some(name) = data["package"]["name"].as_str() else {
            return Ok(());
        };
        let owner = f.package(&root, "cargo", name, data["package"]["version"].as_str());
        let mut tables = vec![&data["dependencies"]];
        if let Some(targets) = data["target"].as_object() {
            tables.extend(targets.values().map(|v| &v["dependencies"]));
        }
        for table in tables {
            if let Some(deps) = table.as_object() {
                for (alias, spec) in deps {
                    let name = spec["package"].as_str().unwrap_or(alias);
                    if spec["workspace"].as_bool() == Some(true) {
                        f.reference(
                            &owner,
                            alias,
                            "depends_on",
                            vec![format!(
                                "cargo:workspace-dependency:{}:{alias}",
                                directory(&f.0.path)
                            )],
                            1,
                        );
                    } else if let Some(path) = spec["path"].as_str() {
                        let keys = local_target(&f.0.path, path)
                            .map(|p| vec![cargo_package_key(&cargo_manifest_path(&p), name)])
                            .unwrap_or_default();
                        f.reference(&owner, name, "depends_on", keys, 1);
                    } else {
                        f.dependency(&owner, "cargo", name, 1);
                    }
                }
            }
        }
    } else {
        let root = f.root("python-manifest", json!({}));
        let p = &data["project"];
        let poetry = &data["tool"]["poetry"];
        let Some(name) = p["name"].as_str().or_else(|| poetry["name"].as_str()) else {
            return Ok(());
        };
        let owner = f.package(
            &root,
            "python",
            name,
            p["version"].as_str().or_else(|| poetry["version"].as_str()),
        );
        for spec in strings(&p["dependencies"]) {
            let name = spec
                .trim()
                .split(|c: char| c.is_whitespace() || "<>=!~;[(".contains(c))
                .next()
                .unwrap_or("");
            f.dependency(&owner, "python", name, 1);
        }
        if let Some(deps) = poetry["dependencies"].as_object() {
            for name in deps.keys().filter(|n| !n.eq_ignore_ascii_case("python")) {
                f.dependency(&owner, "python", name, 1);
            }
        }
    }
    Ok(())
}
fn apm_manifest(f: &mut Facts, source: &str) -> Result<()> {
    let data: Value = serde_yaml_ng::from_str(source)?;
    let root = f.root("apm", json!({}));
    let Some(name) = data["name"].as_str() else {
        return Ok(());
    };
    let owner = f.package(&root, "apm", name, data["version"].as_str());
    match &data["dependencies"] {
        Value::Object(d) => {
            for name in d.keys() {
                f.dependency(&owner, "apm", name, 1);
            }
        }
        Value::Array(d) => {
            for item in d {
                if let Some(name) = item.as_str().or_else(|| {
                    item.as_object()
                        .and_then(|o| o.keys().next().map(String::as_str))
                }) {
                    f.dependency(&owner, "apm", name, 1);
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn go_manifest(f: &mut Facts, source: &str) -> Result<()> {
    let lines: Vec<_> = source
        .lines()
        .map(|s| s.split("//").next().unwrap_or("").trim())
        .collect();
    let name = lines
        .iter()
        .find_map(|s| s.strip_prefix("module ").map(str::trim))
        .unwrap_or("")
        .trim_matches('"');
    let root = f.root("go-manifest", json!({"module":name}));
    if name.is_empty() {
        return Ok(());
    }
    let owner = f.package(&root, "go", name, None);
    let mut block = false;
    for (i, s) in lines.iter().enumerate() {
        if let Some(tail) = s.strip_prefix("require") {
            let tail = tail.trim();
            block = tail.starts_with('(');
            if !block && let Some(dep) = tail.split_whitespace().next() {
                f.dependency(&owner, "go", dep.trim_matches('"'), i as u32 + 1);
            }
        } else if *s == ")" {
            block = false;
        } else if block && let Some(dep) = s.split_whitespace().next() {
            f.dependency(&owner, "go", dep.trim_matches('"'), i as u32 + 1);
        }
    }
    Ok(())
}
// JSONC preprocessing preserves offsets; serde_json owns structural validation.
fn jsonc(source: &str) -> Result<String> {
    let b = source.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                } else if b[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if b.get(i..i + 2) == Some(b"//") {
            while i < b.len() && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if b.get(i..i + 2) == Some(b"/*") {
            out[i] = b' ';
            out[i + 1] = b' ';
            i += 2;
            while i + 1 < b.len() && &b[i..i + 2] != b"*/" {
                if b[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            if i + 1 >= b.len() {
                bail!("unterminated comment");
            }
            out[i] = b' ';
            out[i + 1] = b' ';
            i += 2;
        } else {
            i += 1;
        }
    }
    i = 0;
    while i < out.len() {
        if out[i] == b'"' {
            i += 1;
            while i < out.len() {
                if out[i] == b'\\' {
                    i += 2;
                } else if out[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else {
            if out[i] == b','
                && out[i + 1..]
                    .iter()
                    .find(|c| !c.is_ascii_whitespace())
                    .is_some_and(|c| matches!(c, b'}' | b']'))
            {
                out[i] = b' ';
            }
            i += 1;
        }
    }
    Ok(String::from_utf8(out)?)
}
fn json_manifest(f: &mut Facts, source: &str) -> Result<()> {
    let data: Value = serde_json::from_str(&jsonc(source)?)?;
    if !data.is_object() {
        bail!("configuration must be an object");
    }
    let root = f.root("json-config", json!({"compilerOptions":data["compilerOptions"],"imports":data["imports"],"exports":data["exports"],"workspaces":data["workspaces"],"name":data["name"]}));
    let ecosystem = if basename(&f.0.path) == "composer.json" {
        "composer"
    } else {
        "npm"
    };
    let owner = data["name"]
        .as_str()
        .map(|name| f.package(&root, ecosystem, name, data["version"].as_str()))
        .unwrap_or_else(|| root.clone());
    let mut pending = vec![(&data, root.clone(), String::new(), 0)];
    let mut count = 0;
    while let Some((value, parent, pointer, depth)) = pending.pop() {
        if depth > 6 {
            continue;
        }
        let Some(object) = value.as_object() else {
            continue;
        };
        for (name, value) in object {
            if count >= 500 {
                diagnostic(&mut f.0, None, "Configuration key limit reached");
                return Ok(());
            }
            count += 1;
            let pointer = format!("{pointer}/{}", name.replace('~', "~0").replace('/', "~1"));
            let key = f.node(
                name,
                "config_key",
                Some(format!("config:key:{}#{pointer}", f.0.path)),
                1,
                json!({"pointer":pointer}),
            );
            f.edge(&parent, &key, "contains", 1);
            if matches!(
                name.as_str(),
                "dependencies"
                    | "devDependencies"
                    | "peerDependencies"
                    | "optionalDependencies"
                    | "bundleDependencies"
                    | "bundledDependencies"
                    | "require"
                    | "require-dev"
            ) {
                if let Some(deps) = value.as_object() {
                    let npm_group = matches!(
                        name.as_str(),
                        "dependencies"
                            | "devDependencies"
                            | "peerDependencies"
                            | "optionalDependencies"
                    );
                    for (name, specifier) in deps {
                        f.dependency(&owner, ecosystem, name, 1);
                        // A manifest declaration is useful evidence even when the
                        // package source is not part of this index. Keep it owned
                        // by this manifest, distinct from an installed package.
                        if depth == 0
                            && basename(&f.0.path) == "package.json"
                            && npm_group
                            && specifier.is_string()
                        {
                            let binding = format!("npm:dependency:{}:{name}", f.0.path);
                            if !f
                                .0
                                .nodes
                                .iter()
                                .any(|n| n.binding_key.as_deref() == Some(&binding))
                            {
                                f.node(
                                    name,
                                    "dependency",
                                    Some(binding),
                                    1,
                                    json!({"ecosystem":"npm","declared":true}),
                                );
                                let dependency = format!(
                                    "npm-dependency:{}:{}:{name}",
                                    f.0.path.len(),
                                    f.0.path
                                );
                                f.0.nodes.last_mut().unwrap().id = dependency.clone();
                                f.edge(&owner, &dependency, "depends_on", 1);
                            }
                        }
                    }
                }
                for name in strings(value) {
                    f.dependency(&owner, ecosystem, &name, 1);
                }
            }
            if matches!(name.as_str(), "extends" | "$ref" | "$schema") {
                for target in strings(value) {
                    let keys = if target.starts_with('#') {
                        vec![format!("config:key:{}{target}", f.0.path)]
                    } else if target.starts_with('.') {
                        let (p, frag) = target.split_once('#').unwrap_or((&target, ""));
                        local_target(&f.0.path, p)
                            .map(|p| {
                                vec![if frag.is_empty() {
                                    format!("config:file:{p}")
                                } else {
                                    format!("config:key:{p}#{frag}")
                                }]
                            })
                            .unwrap_or_default()
                    } else {
                        vec![]
                    };
                    f.reference(
                        &key,
                        &target,
                        if name == "extends" {
                            "extends"
                        } else {
                            "references"
                        },
                        keys,
                        1,
                    );
                }
            }
            if value.is_object() {
                pending.push((value, key, pointer, depth + 1));
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct Xml {
    name: String,
    attrs: BTreeMap<String, String>,
    text: String,
    parent: Option<usize>,
    line: u32,
}
fn xml(source: &str) -> Result<Vec<Xml>> {
    let mut reader = Reader::from_str(source);
    let mut nodes: Vec<Xml> = vec![];
    let mut stack = vec![];
    let mut offset = 0;
    let mut current_line = 1;
    loop {
        let start = reader.buffer_position() as usize;
        current_line += source[offset..start]
            .bytes()
            .filter(|b| *b == b'\n')
            .count() as u32;
        offset = start;
        match reader.read_event()? {
            Event::DocType(_) => bail!("DTD is unsupported"),
            Event::Start(ref e) | Event::Empty(ref e) => {
                if stack.len() >= 128 || nodes.len() >= 20000 {
                    bail!("XML size limit");
                }
                let name = String::from_utf8(e.local_name().as_ref().to_vec())?;
                let mut attrs = BTreeMap::new();
                for a in e.attributes() {
                    let a = a?;
                    attrs.insert(
                        String::from_utf8(a.key.local_name().as_ref().to_vec())?,
                        a.decoded_and_normalized_value(
                            quick_xml::XmlVersion::Implicit1_0,
                            reader.decoder(),
                        )?
                        .into_owned(),
                    );
                }
                let index = nodes.len();
                nodes.push(Xml {
                    name,
                    attrs,
                    text: String::new(),
                    parent: stack.last().copied(),
                    line: current_line,
                });
                // Empty events consume their closing delimiter in this same event.
                if !source[start..reader.buffer_position() as usize]
                    .trim_end()
                    .ends_with("/>")
                {
                    stack.push(index);
                }
            }
            Event::End(_) => {
                if stack.pop().is_none() {
                    bail!("unmatched XML end");
                }
            }
            Event::Text(t) => {
                if let Some(&i) = stack.last() {
                    nodes[i]
                        .text
                        .push_str(&quick_xml::escape::unescape(&t.decode()?)?);
                }
            }
            Event::CData(t) => {
                if let Some(&i) = stack.last() {
                    nodes[i].text.push_str(&t.decode()?);
                }
            }
            Event::GeneralRef(reference) => {
                let Some(&index) = stack.last() else {
                    bail!("reference outside XML element");
                };
                if let Some(c) = reference.resolve_char_ref()? {
                    nodes[index].text.push(c);
                } else if let Some(text) =
                    quick_xml::escape::resolve_predefined_entity(&reference.decode()?)
                {
                    nodes[index].text.push_str(text);
                } else {
                    bail!("undeclared XML entity");
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() || nodes.iter().filter(|n| n.parent.is_none()).count() != 1 {
        bail!("invalid XML document");
    }
    Ok(nodes)
}
fn attr<'a>(node: &'a Xml, key: &str) -> Option<&'a str> {
    node.attrs
        .get(key)
        .or_else(|| node.attrs.get(&key.to_ascii_lowercase()))
        .map(String::as_str)
}
fn child_text<'a>(nodes: &'a [Xml], parent: usize, name: &str) -> Option<&'a str> {
    nodes
        .iter()
        .find(|n| n.parent == Some(parent) && n.name == name)
        .map(|n| n.text.trim())
        .filter(|s| !s.is_empty())
}
fn xml_config(f: &mut Facts, source: &str, ecosystem: &str) -> Result<()> {
    let nodes = xml(source)?;
    let root = f.root(ecosystem, json!({}));
    if ecosystem == "maven" {
        let Some(artifact) = child_text(&nodes, 0, "artifactId") else {
            return Ok(());
        };
        let parent = nodes
            .iter()
            .position(|n| n.parent == Some(0) && n.name == "parent");
        let group = child_text(&nodes, 0, "groupId")
            .or_else(|| parent.and_then(|p| child_text(&nodes, p, "groupId")));
        let name = group
            .map(|g| format!("{g}:{artifact}"))
            .unwrap_or_else(|| artifact.into());
        let owner = f.package(&root, "maven", &name, child_text(&nodes, 0, "version"));
        for (i, n) in nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.name == "dependency")
        {
            if let Some(a) = child_text(&nodes, i, "artifactId") {
                let name = child_text(&nodes, i, "groupId")
                    .map(|g| format!("{g}:{a}"))
                    .unwrap_or_else(|| a.into());
                if !name.contains("${") {
                    f.dependency(&owner, "maven", &name, n.line);
                }
            }
        }
        return Ok(());
    }
    if f.0.path.ends_with(".slnx") {
        let mut owners = HashMap::from([(0, root.clone())]);
        let mut projects = HashMap::new();
        for (i, n) in nodes.iter().enumerate() {
            let parent = n
                .parent
                .and_then(|p| owners.get(&p))
                .unwrap_or(&root)
                .clone();
            if n.name == "Folder" {
                if let Some(name) = attr(n, "Name") {
                    let id = f.node(name, "solution_folder", None, n.line, json!({}));
                    f.edge(&parent, &id, "contains", n.line);
                    owners.insert(i, id);
                }
            } else if n.name == "Project"
                && let Some(path) = attr(n, "Path")
            {
                let label = basename(path)
                    .rsplit_once('.')
                    .map_or(basename(path), |(s, _)| s);
                let id = f.node(
                    label,
                    "project_reference",
                    None,
                    n.line,
                    json!({"path":local_target(&f.0.path,path)}),
                );
                f.edge(&parent, &id, "contains", n.line);
                f.reference(&id, path, "references", path_keys(&f.0.path, path), n.line);
                projects.insert(path.replace('\\', "/"), id.clone());
                owners.insert(i, id);
            }
        }
        for n in &nodes {
            if n.name == "BuildDependency"
                && let (Some(owner), Some(path)) =
                    (n.parent.and_then(|p| owners.get(&p)), attr(n, "Project"))
            {
                if let Some(target) = projects.get(&path.replace('\\', "/")) {
                    f.edge(owner, target, "depends_on", n.line);
                } else {
                    f.reference(
                        owner,
                        path,
                        "depends_on",
                        path_keys(&f.0.path, path),
                        n.line,
                    );
                }
            }
        }
        return Ok(());
    }
    for (i, n) in nodes.iter().enumerate() {
        if matches!(n.name.as_str(), "TargetFramework" | "TargetFrameworks") {
            for tfm in n
                .text
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty() && !s.contains('$'))
            {
                let id = f.node(tfm, "framework", None, n.line, json!({}));
                f.edge(&root, &id, "references", n.line);
            }
        }
        if n.name == "PackageReference"
            && let Some(name) = attr(n, "Include").or_else(|| attr(n, "Update"))
        {
            let version = attr(n, "Version").or_else(|| child_text(&nodes, i, "Version"));
            let id = f.node(
                name,
                "package_reference",
                None,
                n.line,
                json!({"ecosystem":"nuget","version":version,"condition":attr(n,"Condition")}),
            );
            f.edge(&root, &id, "imports", n.line);
            f.reference(
                &id,
                name,
                "references",
                vec![package_key("nuget", name)],
                n.line,
            );
        }
        if n.name == "ProjectReference"
            && let Some(path) = attr(n, "Include")
        {
            f.reference(&root, path, "imports", path_keys(&f.0.path, path), n.line);
        }
    }
    if let Some(sdk) = nodes.first().and_then(|n| attr(n, "Sdk")) {
        let id = f.node(sdk, "sdk", None, 1, json!({}));
        f.edge(&root, &id, "references", 1);
    }
    Ok(())
}
fn sln(f: &mut Facts, source: &str) {
    let root = f.root("solution", json!({}));
    let mut guids = HashMap::new();
    let mut current = None;
    let mut deps = false;
    let mut nested = false;
    let mut links = vec![];
    for (i, line) in source.lines().enumerate() {
        let s = line.trim();
        let ln = i as u32 + 1;
        if s.starts_with("Project(") {
            let quoted: Vec<_> = s.split('"').skip(1).step_by(2).collect();
            if quoted.len() < 4 {
                continue;
            }
            let (name, path, guid) = (quoted[1], quoted[2], quoted[3].to_ascii_lowercase());
            let folder = quoted[0].eq_ignore_ascii_case("{66A26720-8FB5-11D2-AA7E-00C04F688DDE}")
                || name == path;
            let id = f.node(
                name,
                if folder {
                    "solution_folder"
                } else {
                    "project_reference"
                },
                None,
                ln,
                json!({"path":if folder { None } else { local_target(&f.0.path,path) }}),
            );
            f.edge(&root, &id, "contains", ln);
            if !folder {
                f.reference(&id, path, "references", path_keys(&f.0.path, path), ln);
            }
            guids.insert(guid.clone(), id);
            current = Some(guid);
        } else if s == "EndProject" {
            current = None;
            deps = false;
        } else if s.contains("ProjectSection(ProjectDependencies)") {
            deps = true;
        } else if s == "EndProjectSection" {
            deps = false;
        } else if s.contains("GlobalSection(NestedProjects)") {
            nested = true;
        } else if s == "EndGlobalSection" {
            nested = false;
        } else if let Some((left, right)) = s.split_once('=') {
            if deps {
                if let Some(owner) = &current {
                    links.push((
                        owner.clone(),
                        left.trim().to_ascii_lowercase(),
                        "depends_on",
                        ln,
                    ));
                }
            } else if nested {
                links.push((
                    right.trim().to_ascii_lowercase(),
                    left.trim().to_ascii_lowercase(),
                    "contains",
                    ln,
                ));
            }
        }
    }
    for (from, to, relation, ln) in links {
        if let (Some(from), Some(to)) = (guids.get(&from), guids.get(&to)) {
            if relation == "contains" {
                f.0.edges
                    .retain(|e| !(e.source == root && e.target == *to && e.relation == "contains"));
            }
            f.edge(from, to, relation, ln);
        }
    }
}
/// Static local Terraform topology over the caller's exact indexed inventory.
/// Discover again after membership/content changes and include the fingerprint in
/// every `.tf`/`.tfvars` file stamp before applying this context to fresh facts.
#[derive(Default)]
pub struct TerraformContext {
    fingerprint: String,
    files: BTreeSet<String>,
    directories: BTreeMap<String, Vec<String>>,
    definitions: BTreeMap<String, usize>,
    outputs: BTreeSet<String>,
    modules: BTreeMap<String, Option<String>>,
}
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
fn indexed_config_source(root: &Path, relative: &str) -> Result<(String, Option<String>)> {
    let mut path = root.to_path_buf();
    for component in relative.split('/') {
        path.push(component);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(("missing".into(), None));
            }
            Err(e) => return Err(e).context("cannot inspect indexed configuration"),
        };
        if meta.file_type().is_symlink() {
            return Ok(("symlink".into(), None));
        }
    }
    if !std::fs::symlink_metadata(&path)?.is_file() {
        return Ok(("not-file".into(), None));
    }
    let (hash, bytes) = crate::index::read_source(&path, CONFIG_MAX_BYTES as u64)?;
    Ok((hash, bytes.and_then(|bytes| String::from_utf8(bytes).ok())))
}
fn terraform_target(path: &str, source: &str) -> Option<String> {
    if !(source.starts_with("./") || source.starts_with("../")) || source.contains('\\') {
        return None;
    }
    relative_path(directory(path), source)
}
fn hcl_string(source: &str, mut node: Syntax<'_>) -> Option<String> {
    while matches!(node.kind(), "expression" | "literal_value") && node.named_child_count() == 1 {
        node = node.named_child(0)?;
    }
    if node.kind() != "string_lit" {
        return None;
    }
    // HCL adds eight-digit Unicode escapes to JSON's quoted-string syntax.
    let raw = source[node.byte_range()]
        .strip_prefix('"')?
        .strip_suffix('"')?;
    let mut chars = raw.chars();
    let mut text = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            text.push(c);
            continue;
        }
        text.push(match chars.next()? {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '"' => '"',
            '\\' => '\\',
            escape @ ('u' | 'U') => {
                let mut scalar = 0;
                for _ in 0..if escape == 'u' { 4 } else { 8 } {
                    scalar = scalar * 16 + chars.next()?.to_digit(16)?;
                }
                char::from_u32(scalar)?
            }
            _ => return None,
        });
    }
    Some(text.replace("$${", "${").replace("%%{", "%{"))
}

fn hcl_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['_', '-'], "");
    [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "accesskey",
        "privatekey",
        "credential",
        "connectionstring",
        "auth",
        "passphrase",
    ]
    .iter()
    .any(|word| key.contains(word))
}

// Recognizable credentials in otherwise ordinary fields, including module sources.
// This is intentionally not an entropy-based classifier for arbitrary strings.
fn hcl_credential(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("private key-----")
        || lower.contains("bearer ")
        || lower.contains("basic ")
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .any(|word| {
                (word.len() >= 20
                    && ["ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-", "sk-"]
                        .iter()
                        .any(|prefix| word.starts_with(prefix)))
                    || (word.len() == 20
                        && (word.starts_with("akia") || word.starts_with("asia"))
                        && word.bytes().all(|b| b.is_ascii_alphanumeric()))
            })
    {
        return true;
    }
    if text.split("://").skip(1).any(|rest| {
        rest.split(['/', '?', '#'])
            .next()
            .is_some_and(|authority| authority.contains('@'))
    }) {
        return true;
    }
    // Find the separator before trimming whitespace so quoted keys and ordinary
    // KEY = value assignments cannot lose their key/separator association.
    lower.match_indices(['=', ':']).any(|(offset, _)| {
        lower[..offset]
            .trim_end()
            .rsplit(|c: char| {
                c.is_whitespace() || ['&', '?', ';', ',', '{', '}', '[', ']', '=', ':'].contains(&c)
            })
            .next()
            .is_some_and(|key| hcl_sensitive_key(key.trim_matches(['"', '\'', '%'])))
    })
}

fn hcl_unresolved(kind: &str) -> Value {
    json!({"$hcl":"unresolved", "kind":kind})
}

fn hcl_value(source: &str, mut node: Syntax<'_>, depth: usize) -> Value {
    if depth > 32 {
        return hcl_unresolved("depth_limit");
    }
    while matches!(
        node.kind(),
        "expression" | "literal_value" | "collection_value"
    ) {
        let parts: Vec<_> = children(node)
            .into_iter()
            .filter(|n| n.kind() != "comment")
            .collect();
        let [child] = parts.as_slice() else { break };
        node = *child;
    }
    let raw = &source[node.byte_range()];
    match node.kind() {
        "bool_lit" => json!(raw == "true"),
        "null_lit" => Value::Null,
        "numeric_lit" => {
            // Do not silently round an integer beyond JSON's supported integer range.
            if raw.bytes().all(|b| b.is_ascii_digit()) {
                raw.parse::<u64>()
                    .map(|n| json!(n))
                    .unwrap_or_else(|_| hcl_unresolved("numeric_range"))
            } else {
                serde_json::from_str::<Value>(raw)
                    .ok()
                    .filter(Value::is_number)
                    .unwrap_or_else(|| hcl_unresolved("numeric_range"))
            }
        }
        "operation" if raw.starts_with('-') => {
            let number = raw[1..].trim();
            let signed = format!("-{number}");
            if number.bytes().all(|b| b.is_ascii_digit()) {
                signed
                    .parse::<i64>()
                    .map(|n| json!(n))
                    .unwrap_or_else(|_| hcl_unresolved("numeric_range"))
            } else {
                serde_json::from_str::<Value>(&signed)
                    .ok()
                    .filter(Value::is_number)
                    .unwrap_or_else(|| hcl_unresolved("operation"))
            }
        }
        "string_lit" => hcl_string(source, node)
            .map(|text| {
                if hcl_credential(&text) {
                    json!("[redacted]")
                } else {
                    json!(text)
                }
            })
            .unwrap_or_else(|| hcl_unresolved("string_escape")),
        "tuple" => Value::Array(
            children(node)
                .into_iter()
                .filter(|n| n.kind() == "expression")
                .map(|n| hcl_value(source, n, depth + 1))
                .collect(),
        ),
        "object" => {
            let mut values = serde_json::Map::new();
            for element in children(node)
                .into_iter()
                .filter(|n| n.kind() == "object_elem")
            {
                let (Some(key), Some(value)) = (
                    element.child_by_field_name("key"),
                    element.child_by_field_name("val"),
                ) else {
                    return hcl_unresolved("object_key");
                };
                // Only a bare identifier or quoted literal is a static object key.
                let key_text = &source[key.byte_range()];
                let key = if let Some(key) = hcl_string(source, key) {
                    key
                } else if key.named_child_count() == 1
                    && key.named_child(0).is_some_and(|n| {
                        n.kind() == "variable_expr" && &source[n.byte_range()] == key_text
                    })
                {
                    key_text.to_owned()
                } else {
                    return hcl_unresolved("object_key");
                };
                if hcl_credential(&key) {
                    return json!("[redacted]");
                }
                let value = if hcl_sensitive_key(&key) {
                    json!("[redacted]")
                } else {
                    hcl_value(source, value, depth + 1)
                };
                if values.insert(key, value).is_some() {
                    return hcl_unresolved("duplicate_key");
                }
            }
            Value::Object(values)
        }
        // No source-text fallback: templates, function arguments and computed keys
        // can contain secrets. References are collected independently by hcl_refs.
        kind => hcl_unresolved(kind),
    }
}

fn hcl_attributes(source: &str, body: Syntax<'_>, sensitive: bool) -> Value {
    let mut values = serde_json::Map::new();
    for attr in children(body)
        .into_iter()
        .filter(|n| n.kind() == "attribute")
    {
        let parts: Vec<_> = children(attr)
            .into_iter()
            .filter(|n| n.kind() != "comment")
            .collect();
        let [key, value] = parts.as_slice() else {
            continue;
        };
        let key = &source[key.byte_range()];
        let value = if hcl_sensitive_key(key) || (sensitive && matches!(key, "default" | "value")) {
            json!("[redacted]")
        } else {
            hcl_value(source, *value, 0)
        };
        if values.contains_key(key) {
            values.insert(key.into(), hcl_unresolved("duplicate_attribute"));
        } else {
            values.insert(key.into(), value);
        }
    }
    Value::Object(values)
}

fn hcl(f: &mut Facts, source: &str) -> Result<()> {
    let Some(tree) = tree(tree_sitter_hcl::LANGUAGE.into(), source, &mut f.0)? else {
        return Ok(());
    };
    let root = f.root("terraform", json!({"directory":directory(&f.0.path)}));
    // Variable-value files do not declare resources or module directories.
    if f.0.path.ends_with(".tfvars") {
        return Ok(());
    }
    let text = |n: Syntax<'_>| &source[n.byte_range()];
    let body = children(tree.root_node())
        .into_iter()
        .find(|n| n.kind() == "body")
        .unwrap_or(tree.root_node());
    for block in children(body).into_iter().filter(|n| n.kind() == "block") {
        let parts: Vec<_> = children(block)
            .into_iter()
            .filter(|n| n.kind() != "comment")
            .take_while(|n| !matches!(n.kind(), "block_start" | "body" | "block_end"))
            .map(|n| hcl_string(source, n).unwrap_or_else(|| text(n).trim_matches('"').to_string()))
            .collect();
        let Some(kind) = parts.first() else {
            continue;
        };
        let body = children(block).into_iter().find(|n| n.kind() == "body");
        if kind == "locals" {
            if let Some(body) = body {
                for a in children(body)
                    .into_iter()
                    .filter(|n| n.kind() == "attribute")
                {
                    if let Some(name) = children(a).first() {
                        let name = format!("local.{}", text(*name));
                        let id = hcl_node(f, &root, &name, "local", line(a));
                        hcl_refs(f, source, a, &id, "references", &[]);
                    }
                }
            }
            continue;
        }
        let name = match (kind.as_str(), parts.len()) {
            ("resource", n) if n >= 3 => format!("{}.{}", parts[1], parts[2]),
            ("data", n) if n >= 3 => format!("data.{}.{}", parts[1], parts[2]),
            ("variable", n) if n >= 2 => format!("var.{}", parts[1]),
            ("module" | "output" | "provider", n) if n >= 2 => format!("{kind}.{}", parts[1]),
            _ => continue,
        };
        let id = hcl_node(f, &root, &name, kind, line(block));
        if let Some(body) = body {
            let sensitive = matches!(kind.as_str(), "variable" | "output")
                && (parts.get(1).is_some_and(|name| hcl_sensitive_key(name))
                    || children(body).into_iter().any(|attr| {
                        let parts: Vec<_> = children(attr)
                            .into_iter()
                            .filter(|n| n.kind() != "comment")
                            .collect();
                        attr.kind() == "attribute"
                            && parts.len() == 2
                            && text(parts[0]) == "sensitive"
                            && hcl_value(source, parts[1], 0) != Value::Bool(false)
                    }));
            let attributes = hcl_attributes(source, body, sensitive);
            if let Some(n) = f.0.nodes.iter_mut().find(|n| n.id == id) {
                n.metadata["attributes"] = attributes;
            }
            if kind == "module" && f.0.path.ends_with(".tf") {
                let sources: Vec<_> = children(body)
                    .into_iter()
                    .filter(|n| {
                        n.kind() == "attribute"
                            && children(*n).first().is_some_and(|n| text(*n) == "source")
                    })
                    .collect();
                if let [attr] = sources.as_slice() {
                    let attr = *attr;
                    let parts: Vec<_> = children(attr)
                        .into_iter()
                        .filter(|n| n.kind() != "comment")
                        .collect();
                    if parts.len() >= 2
                        && text(parts[0]) == "source"
                        && let Some(module_source) = hcl_string(source, parts[1])
                    {
                        let redacted = hcl_credential(&module_source);
                        if let Some(n) = f.0.nodes.iter_mut().find(|n| n.id == id) {
                            n.metadata["module_source"] = json!(if redacted {
                                "[redacted]"
                            } else {
                                &module_source
                            });
                            n.metadata["module_source_line"] = json!(line(attr));
                        }
                        if !redacted
                            && (module_source.starts_with("./") || module_source.starts_with("../"))
                        {
                            let keys = terraform_target(&f.0.path, &module_source)
                                .map(|p| vec![format!("terraform:directory:{p}")])
                                .unwrap_or_default();
                            f.reference(&id, &module_source, "module_source", keys, line(attr));
                        }
                    }
                }
            }
            hcl_refs(f, source, body, &id, "references", &[]);
        }
    }
    Ok(())
}
fn hcl_node(f: &mut Facts, root: &str, name: &str, kind: &str, ln: u32) -> String {
    let id = f.node(
        name,
        kind,
        Some(format!("terraform:{}:{name}", directory(&f.0.path))),
        ln,
        json!({"language":"terraform","directory":directory(&f.0.path)}),
    );
    f.edge(root, &id, "contains", ln);
    id
}
fn hcl_refs(
    f: &mut Facts,
    source: &str,
    n: Syntax<'_>,
    owner: &str,
    relation: &str,
    shadowed: &[String],
) {
    let text = |n: Syntax<'_>| &source[n.byte_range()];
    let cs = children(n);
    let relation =
        if n.kind() == "attribute" && cs.first().is_some_and(|n| text(*n) == "depends_on") {
            "depends_on"
        } else {
            relation
        };
    let mut shadowed = shadowed.to_vec();
    // A for-expression's iterator identifiers are lexical locals, not resources.
    if n.kind().starts_with("for_") {
        for c in &cs {
            if c.kind() == "for_intro" {
                shadowed.extend(
                    children(*c)
                        .into_iter()
                        .filter(|n| n.kind() == "identifier")
                        .map(|n| text(n).to_owned()),
                );
            }
        }
    }
    for (i, c) in cs.iter().enumerate() {
        if c.kind() == "variable_expr" {
            let head = text(*c);
            if ["count", "each", "self", "path", "terraform"].contains(&head)
                || shadowed.iter().any(|s| s == head)
            {
                continue;
            }
            let attrs: Vec<_> = cs[i + 1..]
                .iter()
                .take_while(|n| matches!(n.kind(), "get_attr" | "index" | "splat"))
                .filter(|n| n.kind() == "get_attr")
                .filter_map(|n| children(*n).first().map(|n| text(*n).to_owned()))
                .collect();
            // Only a contiguous module.call.output traversal identifies an output.
            // Indexed/splat module collections require evaluation and stay on the call node.
            if head == "module"
                && cs.get(i + 1).is_some_and(|n| n.kind() == "get_attr")
                && cs.get(i + 2).is_some_and(|n| n.kind() == "get_attr")
                && attrs.len() >= 2
            {
                let label = format!("module.{}.{}", attrs[0], attrs[1]);
                f.reference(
                    owner,
                    &label,
                    relation,
                    vec![format!(
                        "terraform:module-output:{}:{}:{}",
                        directory(&f.0.path),
                        attrs[0],
                        attrs[1]
                    )],
                    line(*c),
                );
            }
            let address = if head == "data" && attrs.len() >= 2 {
                Some(format!("data.{}.{}", attrs[0], attrs[1]))
            } else if head != "data" && !attrs.is_empty() {
                Some(format!("{head}.{}", attrs[0]))
            } else {
                None
            };
            if let Some(address) = address {
                f.reference(
                    owner,
                    &address,
                    relation,
                    vec![format!("terraform:{}:{address}", directory(&f.0.path))],
                    line(*c),
                );
            }
        }
        hcl_refs(f, source, *c, owner, relation, &shadowed);
    }
}

#[derive(Clone)]
struct SqlToken<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    kind: u8,
}
impl SqlToken<'_> {
    fn is(&self, word: &str) -> bool {
        self.kind == b'w' && self.text.eq_ignore_ascii_case(word)
    }
}
// A small lexer isolates statements and routine headers that the SQL grammar does
// not accept (notably T-SQL). Strings and comments never become recovery input.
fn sql_tokens(source: &str) -> Vec<SqlToken<'_>> {
    let b = source.as_bytes();
    let mut i = 0;
    let mut out = vec![];
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b.get(i..i + 2) == Some(b"--") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b.get(i..i + 2) == Some(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b.get(i..i + 2) == Some(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b.get(i..i + 2) == Some(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let start = i;
        let kind;
        if matches!(b[i], b'\'' | b'"' | b'`' | b'[') {
            let open = b[i];
            let close = if open == b'[' { b']' } else { open };
            kind = if open == b'\'' { b's' } else { b'i' };
            i += 1;
            while i < b.len() {
                if b[i] == close {
                    i += 1;
                    if b.get(i) == Some(&close) {
                        i += 1;
                    } else {
                        break;
                    }
                } else if b[i] == b'\\' && open == b'\'' {
                    i = (i + 2).min(b.len());
                } else {
                    i += 1;
                }
            }
        } else if b[i] == b'$'
            && b.get(i + 1)
                .is_some_and(|c| *c == b'$' || c.is_ascii_alphabetic() || *c == b'_')
        {
            let mut tag_end = i + 1;
            while tag_end < b.len() && (b[tag_end].is_ascii_alphanumeric() || b[tag_end] == b'_') {
                tag_end += 1;
            }
            if b.get(tag_end) == Some(&b'$') {
                let tag = &source[i..tag_end + 1];
                i = source[tag_end + 1..]
                    .find(tag)
                    .map(|end| tag_end + 1 + end + tag.len())
                    .unwrap_or(b.len());
                kind = b'd';
            } else {
                i += 1;
                kind = b'p';
            }
        } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 128 {
            i += 1;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'$') || b[i] >= 128)
            {
                i += 1;
            }
            kind = b'w';
        } else {
            i += 1;
            kind = b'p';
        }
        out.push(SqlToken {
            text: &source[start..i],
            start,
            end: i,
            kind,
        });
    }
    out
}
fn sql_identifier(t: &SqlToken<'_>) -> Option<(String, String)> {
    if t.kind == b'w' {
        return Some((t.text.into(), t.text.to_ascii_lowercase()));
    }
    if t.kind != b'i' || t.text.len() < 2 {
        return None;
    }
    let raw = &t.text[1..t.text.len() - 1];
    let name = match t.text.as_bytes()[0] {
        b'[' => raw.replace("]]", "]"),
        b'`' => raw.replace("``", "`"),
        _ => raw.replace("\"\"", "\""),
    };
    Some((name.clone(), name))
}
fn sql_name(tokens: &[SqlToken<'_>], start: usize) -> Option<(String, String, usize)> {
    let mut i = start;
    let mut labels = vec![];
    let mut keys = vec![];
    loop {
        let (label, key) = sql_identifier(tokens.get(i)?)?;
        labels.push(label);
        keys.push(key);
        i += 1;
        if tokens.get(i).is_some_and(|t| t.text == ".") {
            i += 1;
        } else {
            break;
        }
    }
    Some((labels.join("."), serde_json::to_string(&keys).ok()?, i))
}
fn sql_relation_key(key: &str) -> String {
    format!("sql:relation:{key}")
}
fn sql(f: &mut Facts, source: &str) -> Result<()> {
    let root = f.root("sql", json!({}));
    let tokens = sql_tokens(source);
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_sequel::LANGUAGE.into())?;
    let mut start = 0;
    let mut routine_owner: Option<String> = None;
    while start < tokens.len() {
        let end = tokens[start..]
            .iter()
            .position(|t| t.text == ";")
            .map(|n| start + n)
            .unwrap_or(tokens.len());
        let ts = &tokens[start..end];
        start = end + 1;
        if ts.is_empty() {
            continue;
        }
        let offset = ts[0].start;
        let source_end = ts.last().unwrap().end;
        let fragment = &source[offset..source_end];
        let ln = source[..offset].bytes().filter(|b| *b == b'\n').count() as u32 + 1;
        let mut owner = routine_owner.clone().unwrap_or_else(|| root.clone());
        let mut header = 0;
        while ts.get(header).is_some_and(|t| t.is("BEGIN") || t.is("GO")) {
            header += 1;
        }
        let create = ts.get(header).is_some_and(|t| t.is("CREATE"));
        let alter = ts.get(header).is_some_and(|t| t.is("ALTER"));
        if create || alter {
            let mut i = header + 1;
            while ts.get(i).is_some_and(|t| {
                [
                    "OR",
                    "REPLACE",
                    "ALTER",
                    "TEMP",
                    "TEMPORARY",
                    "UNLOGGED",
                    "UNIQUE",
                    "MATERIALIZED",
                ]
                .iter()
                .any(|w| t.is(w))
            }) {
                i += 1;
            }
            let kind = ts
                .get(i)
                .and_then(|t| {
                    [
                        "TABLE",
                        "VIEW",
                        "FUNCTION",
                        "PROCEDURE",
                        "PROC",
                        "INDEX",
                        "TRIGGER",
                    ]
                    .iter()
                    .find(|w| t.is(w))
                })
                .copied();
            if let Some(kind) = kind {
                i += 1;
                while ts.get(i).is_some_and(|t| {
                    ["IF", "NOT", "EXISTS", "CONCURRENTLY", "ONLY"]
                        .iter()
                        .any(|w| t.is(w))
                }) {
                    i += 1;
                }
                if !ts.get(i).is_some_and(|t| t.is("ON"))
                    && let Some((label, key, _)) = sql_name(ts, i)
                {
                    let routine = matches!(kind, "FUNCTION" | "PROCEDURE" | "PROC");
                    let binding = if matches!(kind, "TABLE" | "VIEW") {
                        sql_relation_key(&key)
                    } else {
                        format!("sql:{}:{key}", kind.to_ascii_lowercase())
                    };
                    let existing = if alter {
                        f.0.nodes
                            .iter()
                            .find(|n| n.binding_key.as_ref() == Some(&binding))
                            .map(|n| n.id.clone())
                    } else {
                        None
                    };
                    owner = existing.unwrap_or_else(|| {
                        let id = f.node(
                            &label,
                            if alter {
                                "table_reference"
                            } else if kind == "PROC" {
                                "procedure"
                            } else {
                                match kind {
                                    "TABLE" => "table",
                                    "VIEW" => "view",
                                    "FUNCTION" => "function",
                                    "PROCEDURE" => "procedure",
                                    "INDEX" => "index",
                                    _ => "trigger",
                                }
                            },
                            if alter { None } else { Some(binding.clone()) },
                            ln,
                            json!({"language":"sql"}),
                        );
                        f.edge(&root, &id, "contains", ln);
                        if alter {
                            f.reference(&id, &label, "alters", vec![binding], ln);
                        }
                        id
                    });
                    routine_owner = if routine { Some(owner.clone()) } else { None };
                    if matches!(kind, "INDEX" | "TRIGGER")
                        && let Some(on) = ts.iter().position(|t| t.is("ON"))
                        && let Some((label, key, _)) = sql_name(ts, on + 1)
                    {
                        f.reference(
                            &owner,
                            &label,
                            if kind == "INDEX" {
                                "indexes"
                            } else {
                                "references"
                            },
                            vec![sql_relation_key(&key)],
                            ln,
                        );
                    }
                }
            }
        }
        let tree = parser
            .parse(fragment, None)
            .ok_or_else(|| anyhow::anyhow!("SQL parser failed"))?;
        sql_reads(
            f,
            fragment,
            tree.root_node(),
            &owner,
            ln - 1,
            &HashSet::new(),
            0,
        );
        // REFERENCES has the same lexical form in table and ALTER constraints,
        // including dialects that recover as grammar errors. Quoted strings do not match.
        for (i, t) in ts.iter().enumerate() {
            if t.is("REFERENCES")
                && let Some((label, key, _)) = sql_name(ts, i + 1)
            {
                f.reference(
                    &owner,
                    &label,
                    "references",
                    vec![sql_relation_key(&key)],
                    ln,
                );
            }
            if t.kind == b'd' && routine_owner.is_some() {
                let tag_end = t.text[1..].find('$').map(|p| p + 2).unwrap_or(2);
                if t.text.len() >= tag_end * 2 {
                    let body = &t.text[tag_end..t.text.len() - tag_end];
                    if let Some(tree) = parser.parse(body, None) {
                        sql_reads(
                            f,
                            body,
                            tree.root_node(),
                            &owner,
                            source[..t.start + tag_end]
                                .bytes()
                                .filter(|b| *b == b'\n')
                                .count() as u32,
                            &HashSet::new(),
                            0,
                        );
                    }
                }
            }
        }
        if !create && ts.first().is_some_and(|t| t.is("END")) {
            routine_owner = None;
        }
    }
    Ok(())
}
fn sql_reads(
    f: &mut Facts,
    source: &str,
    node: Syntax<'_>,
    owner: &str,
    line_offset: u32,
    inherited: &HashSet<String>,
    depth: usize,
) {
    if depth > 128 {
        return;
    }
    let cs = children(node);
    let mut ctes = inherited.clone();
    for c in &cs {
        if c.kind() == "cte"
            && let Some(name) = children(*c).into_iter().find(|n| n.kind() == "identifier")
            && let Some((_, key, _)) = sql_name(&sql_tokens(&source[name.byte_range()]), 0)
        {
            ctes.insert(key);
        }
    }
    if matches!(node.kind(), "relation" | "from" | "join" | "cross_join") {
        for c in &cs {
            if c.kind() == "object_reference"
                && let Some((label, key, _)) = sql_name(&sql_tokens(&source[c.byte_range()]), 0)
                && !ctes.contains(&key)
            {
                f.reference(
                    owner,
                    &label,
                    "reads_from",
                    vec![sql_relation_key(&key)],
                    line(*c) + line_offset,
                );
            }
        }
    }
    for c in cs {
        if !matches!(c.kind(), "comment" | "literal" | "function_body") {
            sql_reads(f, source, c, owner, line_offset, &ctes, depth + 1);
        }
    }
}

fn mcp_config(path: &str) -> bool {
    matches!(
        basename(path),
        ".mcp.json" | "claude_desktop_config.json" | "mcp.json" | "mcp_servers.json"
    )
}
fn mcp(f: &mut Facts, source: &str) -> Result<()> {
    let data: Value = serde_json::from_str(source)?;
    let servers = data["mcpServers"]
        .as_object()
        .or_else(|| data["mcp"]["servers"].as_object())
        .ok_or_else(|| anyhow::anyhow!("missing server map"))?;
    let root = f.root("mcp-config", json!({}));
    let mut shared = HashMap::new();
    for (name, spec) in servers.iter().filter(|(_, v)| v.is_object()).take(200) {
        let name: String = name.chars().filter(|c| !c.is_control()).take(200).collect();
        if name.is_empty() {
            continue;
        }
        let owner = f.node(
            &name,
            "mcp_server",
            Some(format!("mcp:server:{}:{name}", f.0.path)),
            1,
            json!({}),
        );
        f.edge(&root, &owner, "contains", 1);
        let mut concepts = vec![];
        if let Some(cmd) = spec["command"].as_str() {
            let cmd = cmd.rsplit(['/', '\\']).next().unwrap_or("");
            if safe_name(cmd) {
                concepts.push(("mcp_command", cmd.to_owned(), "references"));
            }
        }
        let mut skip_value = false;
        for arg in strings(&spec["args"]) {
            if skip_value {
                skip_value = false;
                continue;
            }
            if arg.starts_with('-') {
                skip_value =
                    !matches!(arg.as_str(), "-y" | "--yes" | "--no-cache") && !arg.contains('=');
                continue;
            }
            let bare = if let Some(stripped) = arg.strip_prefix('@') {
                stripped
                    .split_once('@')
                    .map(|(a, _)| format!("@{a}"))
                    .unwrap_or_else(|| arg.clone())
            } else {
                arg.split('@').next().unwrap_or("").into()
            };
            let package = if let Some(stripped) = bare.strip_prefix('@') {
                stripped
                    .split_once('/')
                    .is_some_and(|(scope, name)| safe_name(scope) && safe_name(name))
            } else {
                safe_name(&bare)
                    && (bare.starts_with("mcp-")
                        || bare.ends_with("-mcp")
                        || bare.contains("-mcp-"))
            };
            if package {
                concepts.push(("mcp_package", bare, "references"));
                break;
            }
        }
        if let Some(env) = spec["env"].as_object() {
            for name in env.keys() {
                if !name.is_empty()
                    && name.len() <= 200
                    && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                {
                    concepts.push(("env_var", name.clone(), "requires_env"));
                }
            }
        }
        for (kind, name, relation) in concepts {
            let key = format!("mcp:{kind}:{name}");
            let id = shared
                .entry(key.clone())
                .or_insert_with(|| f.node(&name, kind, Some(key), 1, json!({})))
                .clone();
            f.edge(&owner, &id, relation, 1);
        }
    }
    if servers.len() > 200 {
        diagnostic(&mut f.0, None, "MCP server limit reached");
    }
    Ok(())
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
