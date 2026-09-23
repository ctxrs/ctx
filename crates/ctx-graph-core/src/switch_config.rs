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
    ensure!(exe.is_absolute(), "Graf executable path must be absolute");
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
                    !servers.contains_key("graf") || old == Some("graf"),
                    "graf server already exists"
                );
                if let Some(old) = old {
                    servers.remove(old);
                }
                let mut entry = json!({ "command": exe, "args": ["--db", db, "serve"] });
                if *key == "servers" {
                    entry["type"] = json!("stdio");
                }
                servers.insert("graf".into(), entry);
            }
            Self::Toml(doc) => {
                let inline = doc["mcp_servers"].is_inline_table();
                let servers = doc["mcp_servers"].as_table_like_mut().unwrap();
                ensure!(
                    !servers.contains_key("graf") || old == Some("graf"),
                    "graf server already exists"
                );
                if let Some(old) = old {
                    servers.remove(old);
                }
                let mut entry = Table::new();
                entry["command"] = toml_edit::value(exe);
                entry["args"] = toml_edit::value(
                    ["--db", db, "serve"]
                        .into_iter()
                        .collect::<toml_edit::Array>(),
                );
                // Inline mcp_servers tables can contain only values.
                let item = if inline {
                    Item::Value(entry.into_inline_table().into())
                } else {
                    Item::Table(entry)
                };
                servers.insert("graf", item);
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
mod tests {
    use super::*;

    fn project() -> &'static Path {
        Path::new("/work/demo")
    }
    fn entry(args: &[&str]) -> Value {
        json!({"command": "python3", "args": args})
    }
    fn config(value: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"mcpServers": value})).unwrap()
    }
    fn plan(path: &str, bytes: Option<&[u8]>, server: Option<&str>) -> Result<Vec<u8>> {
        rewrite(
            Path::new(path),
            bytes,
            project(),
            server,
            Path::new("/tools/graf"),
            Path::new("/work/demo/.graf/graph.db"),
        )
    }

    #[test]
    fn json_keeps_metadata_and_other_servers_and_replaces_whole_entry() {
        let source = json!({
            "metadata": {"nested": [null, true, 18446744073709551615u64, {"x":"✓"}]},
            "mcpServers": {
                "old": {"command":"python", "args":["-m","graphify.serve"], "cwd":"${workspaceFolder}", "env":{"OLD":"value"}, "transport":"stdio"},
                "other": {"url":"https://example.invalid/mcp", "custom": [1, 2]}
            }
        });
        let bytes = serde_json::to_vec(&source).unwrap();
        let result = plan(".mcp.json", Some(&bytes), None).unwrap();
        let value: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(value["metadata"], source["metadata"]);
        assert_eq!(value["mcpServers"]["other"], source["mcpServers"]["other"]);
        assert!(value["mcpServers"].get("old").is_none());
        assert_eq!(
            value["mcpServers"]["graf"],
            json!({"command":"/tools/graf", "args":["--db","/work/demo/.graf/graph.db","serve"]})
        );
    }

    #[test]
    fn absent_config_and_vscode_schema() {
        let created = plan(".mcp.json", None, None).unwrap();
        let value: Value = serde_json::from_slice(&created).unwrap();
        assert!(value["mcpServers"]["graf"].is_object());
        let original = serde_json::to_vec(
            &json!({"inputs":[{"id":"x"}], "servers":{"old":entry(&["-m","graphify.serve"])}}),
        )
        .unwrap();
        let result = plan(".vscode/mcp.json", Some(&original), None).unwrap();
        let value: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(value["inputs"], json!([{"id":"x"}]));
        assert_eq!(value["servers"]["graf"]["type"], "stdio");
        assert!(plan(".mcp.json", None, Some("old")).is_err());
    }

    #[test]
    fn malformed_duplicate_and_oversized_configs_fail() {
        for bytes in [
            br#"{"mcpServers":{},"mcpServers":{}}"#.as_slice(),
            br#"{"mcpServers":{},"metadata":{"x":1,"x":2}}"#,
            br#"{"mcpServers":{},"metadata":[{"x":1,"\u0078":2}]}"#,
            br#"{"mcpServers":{},}"#,
            br#"{"mcpServers":{},"servers":{}}"#,
            br#"{"mcpServers":[]}"#,
            b"{}",
            b"[]",
            b"{",
            b"\xff",
        ] {
            assert!(
                candidates(Path::new("config.json"), bytes, project()).is_err(),
                "{bytes:?}"
            );
        }
        assert!(
            candidates(
                Path::new("config.toml"),
                b"[mcp_servers]\n[mcp_servers]",
                project()
            )
            .is_err()
        );
        assert!(
            candidates(
                Path::new("config.json"),
                &vec![b' '; MAX_BYTES + 1],
                project()
            )
            .is_err()
        );
    }

    #[test]
    fn graph_arguments_cwd_and_workspace_paths() {
        for (command, args, cwd, expected) in [
            (
                "python",
                vec!["-m", "graphify.serve"],
                None,
                "/work/demo/graphify-out/graph.json",
            ),
            (
                "/venv/bin/python3.12",
                vec![
                    "-u",
                    "-m",
                    "graphify.serve",
                    "--graph",
                    "out/graph.json",
                    "--transport",
                    "stdio",
                ],
                None,
                "/work/demo/out/graph.json",
            ),
            (
                "uv",
                vec!["run", "python", "-m", "graphify.serve", "graph.json"],
                Some("subdir"),
                "/work/demo/subdir/graph.json",
            ),
            (
                "/tools/uv",
                vec![
                    "run",
                    "--with",
                    "graphify",
                    "-m",
                    "graphify.serve",
                    "--graph=${workspaceFolder}/data/g.json",
                ],
                None,
                "/work/demo/data/g.json",
            ),
            (
                "python3",
                vec![
                    "-m",
                    "graphify.serve",
                    "--transport=stdio",
                    "${workspace.path}/graph.json",
                ],
                Some("${workspaceFolder}/subdir"),
                "/work/demo/graph.json",
            ),
            (
                "python3",
                vec!["-m", "graphify.serve", "../graph.json"],
                Some("${workspace.path}/subdir"),
                "/work/demo/subdir/../graph.json",
            ),
        ] {
            let mut server = json!({"command":command,"args":args});
            if let Some(cwd) = cwd {
                server["cwd"] = json!(cwd);
            }
            let found = candidates(
                Path::new("config.json"),
                &config(json!({"custom-name":server})),
                project(),
            )
            .unwrap();
            assert_eq!(
                found,
                vec![Candidate {
                    name: "custom-name".into(),
                    graph: expected.into()
                }]
            );
        }
    }

    #[test]
    fn rejects_http_disabled_wrappers_and_ambiguous_invocations() {
        let base = entry(&["-m", "graphify.serve"]);
        for (key, value) in [
            ("url", json!("https://example.invalid")),
            ("url", Value::Null),
            ("type", json!("http")),
            ("transport", json!("sse")),
            ("disabled", json!(true)),
            ("enabled", json!(false)),
            ("disabled", json!("false")),
            ("command", json!("bash")),
            ("command", json!("node")),
            ("command", json!("python-wrapper")),
            ("args", json!(["-c", "-m graphify.serve"])),
            ("args", json!(["-m", "other", "-m", "graphify.serve"])),
            (
                "args",
                json!(["-m", "graphify.serve", "--transport", "http"]),
            ),
            (
                "args",
                json!(["-m", "graphify.serve", "--graph", "a.json", "b.json"]),
            ),
            ("args", json!(["-m", "graphify.serve", "--unknown"])),
            ("args", json!(["-m", "graphify.serve", "--graph"])),
            ("args", json!(["-m", "graphify.serve", "--graph="])),
        ] {
            let mut value_entry = base.clone();
            value_entry[key] = value;
            let bytes = config(json!({"graphify":value_entry}));
            assert!(
                candidates(Path::new("config.json"), &bytes, project())
                    .unwrap()
                    .is_empty(),
                "{key}"
            );
            assert!(plan(".mcp.json", Some(&bytes), Some("graphify")).is_err());
        }
        for args in [
            vec!["run", "bash", "-c", "python -m graphify.serve"],
            vec![
                "run",
                "--project",
                "/other",
                "python",
                "-m",
                "graphify.serve",
            ],
        ] {
            assert!(invocation_graph("uv", &args).is_none());
        }
    }

    #[test]
    fn excludes_other_projects_and_unexpanded_variables() {
        for graph in [
            "/work/other/graph.json",
            "../other/graph.json",
            "/work/demo-other/graph.json",
            "${HOME}/graph.json",
            "${workspaceFolder:other}/graph.json",
            "${workspaceFolder}//other/graph.json",
            "~/graph.json",
            "$HOME/graph.json",
            "https://example.invalid/graph.json",
        ] {
            let bytes = config(json!({"old":entry(&["-m","graphify.serve",graph])}));
            assert!(
                candidates(Path::new("config.json"), &bytes, project())
                    .unwrap()
                    .is_empty(),
                "{graph}"
            );
            assert!(plan(".mcp.json", Some(&bytes), None).is_err());
        }
        let bytes = config(
            json!({"old":{"command":"python","args":["-m","graphify.serve"],"cwd":"/work/other"}}),
        );
        assert!(
            candidates(Path::new("config.json"), &bytes, project())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn implicit_graph_with_environment_override_is_not_guessed() {
        let mut server = entry(&["-m", "graphify.serve"]);
        server["env"] = json!({"GRAPHIFY_OUT": "/work/other"});
        assert!(graph_path(&server, project()).is_none());
        server["args"] = json!(["-m", "graphify.serve", "graphify-out/graph.json"]);
        assert_eq!(
            graph_path(&server, project()),
            Some(project().join("graphify-out/graph.json"))
        );
        let bytes = br#"[mcp_servers.old]
command = "python"
args = ["-m", "graphify.serve"]
[mcp_servers.old.env]
GRAPHIFY_OUT = "/work/other"
"#;
        assert!(
            candidates(Path::new("config.toml"), bytes, project())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn selection_and_graf_collision() {
        let bytes = config(
            json!({"one":entry(&["-m","graphify.serve"]),"two":entry(&["-m","graphify.serve","second.json"])}),
        );
        assert!(plan(".mcp.json", Some(&bytes), None).is_err());
        assert!(plan(".mcp.json", Some(&bytes), Some("missing")).is_err());
        let result = plan(".mcp.json", Some(&bytes), Some("two")).unwrap();
        let value: Value = serde_json::from_slice(&result).unwrap();
        assert!(value["mcpServers"]["one"].is_object());
        let bytes =
            config(json!({"old":entry(&["-m","graphify.serve"]),"graf":{"command":"graf"}}));
        assert!(
            plan(".mcp.json", Some(&bytes), Some("old"))
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
    }

    #[test]
    fn toml_preserves_unrelated_values_and_comments() {
        let bytes = br#"# preferences
model = "example" # keep this comment

[mcp_servers.old]
command = "python3"
args = ["-m", "graphify.serve", "--graph", "${workspaceFolder}/graph.json"]
cwd = "${workspaceFolder}"

[mcp_servers.old.env]
OLD = "value"

# other server
[mcp_servers.other]
command = "other" # retain inline comment
args = ["a"]

[unrelated]
when = 2026-09-17T12:30:00Z # date stays a date
"#;
        let found = candidates(Path::new("config.toml"), bytes, project()).unwrap();
        assert_eq!(found[0].graph, Path::new("/work/demo/graph.json"));
        let result = plan("config.toml", Some(bytes), None).unwrap();
        let output = String::from_utf8(result).unwrap();
        assert!(output.contains("# preferences\nmodel = \"example\" # keep this comment"));
        assert!(output.contains(
            "# other server\n[mcp_servers.other]\ncommand = \"other\" # retain inline comment"
        ));
        assert!(output.contains("when = 2026-09-17T12:30:00Z # date stays a date"));
        assert!(!output.contains("OLD"));
        let doc: DocumentMut = output.parse().unwrap();
        assert!(doc["mcp_servers"].get("old").is_none());
        assert_eq!(
            doc["mcp_servers"]["graf"]["command"].as_str(),
            Some("/tools/graf")
        );
        assert_eq!(
            doc["mcp_servers"]["graf"]["args"].as_array().unwrap().len(),
            3
        );
    }

    #[test]
    fn toml_inline_servers_and_collision() {
        let bytes = br#"mcp_servers = { old = { command = "python", args = ["-m", "graphify.serve"] }, other = { command = "other" } } # keep
"#;
        let result = plan("config.toml", Some(bytes), None).unwrap();
        let output = String::from_utf8(result).unwrap();
        assert!(output.contains("# keep"));
        let doc: DocumentMut = output.parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["other"]["command"].as_str(),
            Some("other")
        );
        assert_eq!(
            doc["mcp_servers"]["graf"]["command"].as_str(),
            Some("/tools/graf")
        );
        let collision = br#"[mcp_servers.old]
command = "python"
args = ["-m", "graphify.serve"]
[mcp_servers.graf]
command = "graf"
"#;
        assert!(plan("config.toml", Some(collision), None).is_err());
    }
}
