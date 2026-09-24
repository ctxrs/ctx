//! Bounded MCP configuration planning. No file contents are read or written.
//!
//! Candidate paths use filesystem identity when available and retain parent
//! components; the caller must recheck filesystem identity
//! and containment, select a candidate graph, and check any explicit graph agrees before
//! applying the plan. Discovery of config files belongs to the caller.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value, json};
use toml_edit::{DocumentMut, Item, Table};

const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub graph: PathBuf,
}

/// Return only supported stdio Graphify entries whose graph is in `project`.
/// `project` must be absolute. Unsupported entries are left alone.
pub fn candidates(path: &Path, bytes: &[u8], project: &Path) -> Result<Vec<Candidate>> {
    Document::parse(path, bytes)?.candidates(project)
}

/// Replace a selected Graphify entry, requiring disambiguation with `server`.
/// `None` bytes creates a config; an existing config must contain a candidate.
/// `project`, `exe`, and `db` must be absolute. Returns the replacement bytes;
/// the caller retains its input bytes and candidate name for the migration receipt.
pub fn rewrite(
    path: &Path,
    bytes: Option<&[u8]>,
    project: &Path,
    server: Option<&str>,
    exe: &Path,
    db: &Path,
) -> Result<Vec<u8>> {
    ensure!(project.is_absolute(), "project path must be absolute");
    ensure!(exe.is_absolute(), "ctx executable path must be absolute");
    ensure!(db.is_absolute(), "database path must be absolute");
    let exe = exe.to_str().context("executable path is not UTF-8")?;
    let db = db.to_str().context("database path is not UTF-8")?;
    let mut doc = match bytes {
        Some(bytes) => Document::parse(path, bytes)?,
        None => {
            ensure!(
                server.is_none(),
                "--server requires an existing config entry"
            );
            Document::empty(path)
        }
    };
    let old_server = if bytes.is_some() {
        let available = doc.candidates(project)?;
        let selected =
            if let Some(name) = server {
                available.into_iter().find(|c| c.name == name).with_context(|| {
                format!("server {name:?} is not a supported Graphify stdio entry for this project")
            })?
            } else {
                ensure!(
                    !available.is_empty(),
                    "no supported Graphify stdio entry for this project"
                );
                ensure!(
                    available.len() == 1,
                    "multiple Graphify entries; select one with --server"
                );
                available.into_iter().next().unwrap()
            };
        Some(selected.name)
    } else {
        None
    };
    doc.replace(old_server.as_deref(), exe, db)?;
    let bytes = doc.bytes()?;
    ensure!(
        bytes.len() <= MAX_BYTES,
        "rewritten MCP config exceeds 8 MiB limit; configuration was not changed"
    );
    Ok(bytes)
}

enum Document {
    Json { root: Value, key: &'static str },
    Toml(DocumentMut),
}

impl Document {
    fn parse(path: &Path, bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_BYTES, "MCP config exceeds 8 MiB limit");
        if path.extension().is_some_and(|e| e == "toml") {
            let doc: DocumentMut = std::str::from_utf8(bytes)
                .context("MCP config is not UTF-8")?
                .parse()
                .context("invalid MCP TOML")?;
            ensure!(
                doc.get("mcp_servers")
                    .is_some_and(|s| s.as_table_like().is_some()),
                "expected an mcp_servers table"
            );
            Ok(Self::Toml(doc))
        } else {
            let StrictJson(root) = serde_json::from_slice(bytes).context("invalid MCP JSON")?;
            let object = root
                .as_object()
                .context("MCP config root must be an object")?;
            ensure!(
                object.contains_key("mcpServers") != object.contains_key("servers"),
                "expected exactly one of mcpServers or servers"
            );
            let key = if object.contains_key("mcpServers") {
                "mcpServers"
            } else {
                "servers"
            };
            ensure!(root[key].is_object(), "{key} must be an object");
            Ok(Self::Json { root, key })
        }
    }

    fn empty(path: &Path) -> Self {
        if path.extension().is_some_and(|e| e == "toml") {
            let mut doc = DocumentMut::new();
            doc["mcp_servers"] = Item::Table(Table::new());
            Self::Toml(doc)
        } else {
            let key = if path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == ".vscode")
            {
                "servers"
            } else {
                "mcpServers"
            };
            Self::Json {
                root: json!({ key: {} }),
                key,
            }
        }
    }

    fn candidates(&self, project: &Path) -> Result<Vec<Candidate>> {
        ensure!(project.is_absolute(), "project path must be absolute");
        let project = normalize(project);
        let mut found = Vec::new();
        let mut visit = |name: &str, entry: &Value| {
            if let Some(graph) = graph_path(entry, &project) {
                found.push(Candidate {
                    name: name.into(),
                    graph,
                });
            }
        };
        match self {
            Self::Json { root, key } => {
                for (name, entry) in root[*key].as_object().unwrap() {
                    visit(name, entry);
                }
            }
            Self::Toml(doc) => {
                for (name, entry) in doc["mcp_servers"].as_table_like().unwrap().iter() {
                    // Only recognition fields need a JSON view; serialize the original
                    // TOML document to preserve unrelated values and their comments.
                    if let Some(table) = entry.as_table_like() {
                        let mut fields = Map::new();
                        for key in [
                            "command",
                            "args",
                            "cwd",
                            "url",
                            "disabled",
                            "enabled",
                            "type",
                            "transport",
                            "env",
                        ] {
                            if let Some(item) = table.get(key) {
                                let value = if key == "env" {
                                    match item.as_table_like() {
                                        Some(env) if env.contains_key("GRAPHIFY_OUT") => {
                                            json!({"GRAPHIFY_OUT": true})
                                        }
                                        Some(_) => json!({}),
                                        None => Value::Null,
                                    }
                                } else if let Some(s) = item.as_str() {
                                    Value::String(s.into())
                                } else if let Some(b) = item.as_bool() {
                                    Value::Bool(b)
                                } else if let Some(a) = item.as_array() {
                                    Value::Array(
                                        a.iter()
                                            .map(|v| {
                                                v.as_str().map_or(Value::Null, |s| {
                                                    Value::String(s.into())
                                                })
                                            })
                                            .collect(),
                                    )
                                } else {
                                    Value::Null
                                };
                                fields.insert(key.into(), value);
                            }
                        }
                        visit(name, &Value::Object(fields));
                    }
                }
            }
        }
        Ok(found)
    }

    fn replace(&mut self, old: Option<&str>, exe: &str, db: &str) -> Result<()> {
        match self {
            Self::Json { root, key } => {
                let servers = root[*key].as_object_mut().unwrap();
                ensure!(
                    !servers.contains_key("ctx-graph") || old == Some("ctx-graph"),
                    "ctx-graph server already exists"
                );
                if let Some(old) = old {
                    servers.remove(old);
                }
                let mut entry = json!({ "command": exe, "args": ["graph", "--db", db, "serve"] });
                if *key == "servers" {
                    entry["type"] = json!("stdio");
                }
                servers.insert("ctx-graph".into(), entry);
            }
            Self::Toml(doc) => {
                let inline = doc["mcp_servers"].is_inline_table();
                let servers = doc["mcp_servers"].as_table_like_mut().unwrap();
                ensure!(
                    !servers.contains_key("ctx-graph") || old == Some("ctx-graph"),
                    "ctx-graph server already exists"
                );
                if let Some(old) = old {
                    servers.remove(old);
                }
                let mut entry = Table::new();
                entry["command"] = toml_edit::value(exe);
                entry["args"] = toml_edit::value(
                    ["graph", "--db", db, "serve"]
                        .into_iter()
                        .collect::<toml_edit::Array>(),
                );
                // Inline mcp_servers tables can contain only values.
                let item = if inline {
                    Item::Value(entry.into_inline_table().into())
                } else {
                    Item::Table(entry)
                };
                servers.insert("ctx-graph", item);
            }
        }
        Ok(())
    }

    fn bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Json { root, .. } => {
                let mut bytes = serde_json::to_vec_pretty(root)?;
                bytes.push(b'\n');
                Ok(bytes)
            }
            Self::Toml(doc) => Ok(doc.to_string().into_bytes()),
        }
    }
}

fn graph_path(entry: &Value, project: &Path) -> Option<PathBuf> {
    let fields = entry.as_object()?;
    if fields.contains_key("url")
        || fields
            .get("disabled")
            .is_some_and(|v| v != &Value::Bool(false))
        || fields
            .get("enabled")
            .is_some_and(|v| v != &Value::Bool(true))
        || ["type", "transport"]
            .iter()
            .any(|k| fields.get(*k).is_some_and(|v| v.as_str() != Some("stdio")))
    {
        return None;
    }
    let command = Path::new(fields.get("command")?.as_str()?)
        .file_name()?
        .to_str()?;
    let args: Vec<&str> = fields
        .get("args")?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<_>>()?;
    let graph = invocation_graph(command, &args)?;
    if graph.is_none()
        && fields.get("env").is_some_and(|env| {
            env.as_object()
                .is_none_or(|env| env.contains_key("GRAPHIFY_OUT"))
        })
    {
        return None;
    }
    let graph = graph.unwrap_or("graphify-out/graph.json");
    let cwd = match fields.get("cwd") {
        Some(value) => resolve(value.as_str()?, project, project)?,
        None => project.to_owned(),
    };
    let graph = resolve(graph, &cwd, project)?;
    // Existing aliases (including macOS /var -> /private/var) must identify the
    // same project. Keep the original path for the caller's final resolution.
    // Missing snapshots retain lexical discovery and fail during that check.
    let resolved = graph.canonicalize().unwrap_or_else(|_| graph.clone());
    normalize(&resolved).starts_with(project).then_some(graph)
}

fn python(command: &str) -> bool {
    let command = command.strip_suffix(".exe").unwrap_or(command);
    command == "python"
        || command.strip_prefix("python").is_some_and(|version| {
            !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn invocation_graph<'a>(command: &str, mut args: &'a [&str]) -> Option<Option<&'a str>> {
    if command == "uv" || command == "uv.exe" {
        if args.first()? != &"run" {
            return None;
        }
        args = &args[1..];
        while args.first().is_some_and(|a| *a == "--with") {
            if args.len() < 2 || args[1].starts_with('-') {
                return None;
            }
            args = &args[2..];
        }
        if args.first().is_some_and(|s| {
            Path::new(s)
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(python)
        }) {
            args = &args[1..];
        }
    } else if !python(command) {
        return None;
    }
    while args
        .first()
        .is_some_and(|a| matches!(*a, "-u" | "-B" | "-E" | "-I" | "-s" | "-S"))
    {
        args = &args[1..];
    }
    if !args.starts_with(&["-m", "graphify.serve"]) {
        return None;
    }
    args = &args[2..];
    let mut graph = None;
    let mut transport_seen = false;
    while let Some(arg) = args.first() {
        args = &args[1..];
        if *arg == "--graph" {
            let value = *args.first()?;
            if value.starts_with('-') || graph.replace(value).is_some() {
                return None;
            }
            args = &args[1..];
        } else if let Some(value) = arg.strip_prefix("--graph=") {
            if graph.replace(value).is_some() {
                return None;
            }
        } else if *arg == "--transport" || arg.starts_with("--transport=") {
            if transport_seen {
                return None;
            }
            transport_seen = true;
            let value = if let Some(value) = arg.strip_prefix("--transport=") {
                value
            } else {
                let value = *args.first()?;
                args = &args[1..];
                value
            };
            if value != "stdio" {
                return None;
            }
        } else if arg.starts_with('-') || graph.replace(*arg).is_some() {
            return None;
        }
    }
    Some(graph)
}

fn resolve(raw: &str, base: &Path, project: &Path) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let mut path = None;
    for variable in ["${workspaceFolder}", "${workspace.path}"] {
        if raw == variable {
            path = Some(project.to_owned());
        } else if let Some(tail) = raw.strip_prefix(variable).and_then(|s| s.strip_prefix('/')) {
            if tail.starts_with('/') {
                return None;
            }
            path = Some(project.join(tail));
        }
    }
    let path = path.unwrap_or_else(|| base.join(raw));
    let text = path.to_str()?;
    if text.contains('$')
        || text.contains('%')
        || text.contains('~')
        || text.contains('`')
        || text.contains("://")
    {
        return None;
    }
    Some(path)
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            #[cfg(windows)]
            Component::Prefix(prefix) => {
                use std::path::Prefix;
                match prefix.kind() {
                    Prefix::VerbatimDisk(drive) | Prefix::Disk(drive) => {
                        result.push(format!("{}:", drive.to_ascii_uppercase() as char));
                    }
                    Prefix::VerbatimUNC(server, share) | Prefix::UNC(server, share) => {
                        let mut unc = std::ffi::OsString::from("\\\\");
                        unc.push(server);
                        unc.push("\\");
                        unc.push(share);
                        result.push(unc);
                    }
                    _ => result.push(component.as_os_str()),
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

// Keep duplicate detection local until the importer's strict parser is shared.
struct StrictJson(Value);

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = StrictJson;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<StrictJson, E> {
                Number::from_f64(v)
                    .map(|n| StrictJson(n.into()))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<StrictJson, E> {
                Ok(StrictJson(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<StrictJson, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictJson(v)) = seq.next_element()? {
                    values.push(v);
                }
                Ok(StrictJson(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<StrictJson, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON object key"));
                    }
                    let StrictJson(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(StrictJson(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}

#[cfg(test)]
#[path = "switch_config_tests.rs"]
mod tests;
