//! Declarative project metadata. Parsing never evaluates configuration or opens referenced paths.
use anyhow::{Context, Result, bail, ensure};
use ctx_graph_types::syntax::{children, diagnostic, line, relative_path, tree};
use ctx_graph_types::{Edge, FileFacts, Node, Reference};
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

// JSONC preprocessing preserves offsets; serde_json owns structural validation.

#[derive(Default)]
struct Xml {
    name: String,
    attrs: BTreeMap<String, String>,
    text: String,
    parent: Option<usize>,
    line: u32,
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

// Recognizable credentials in otherwise ordinary fields, including module sources.
// This is intentionally not an entropy-based classifier for arbitrary strings.

#[derive(Clone)]
struct SqlToken<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    kind: u8,
}

// A small lexer isolates statements and routine headers that the SQL grammar does
// not accept (notably T-SQL). Strings and comments never become recovery input.

mod cargopackagecontext_discover;
mod cargoworkspace_includes;
mod facts_new;
mod sqltoken_is;
mod terraformcontext_discover;

mod basename;
use basename::*;
mod sln;
use sln::*;
mod sql;
use sql::*;
