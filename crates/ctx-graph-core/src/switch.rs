//! Project-scoped Graphify migration. The original graph is never modified.
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use crate::{import, model::QueryOptions, store::Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{switch_config, switch_files};

#[derive(Debug, Args)]
pub struct SwitchArgs {
    /// Source to migrate. Currently graphify.
    #[arg(value_parser = ["graphify"], required_unless_present = "undo", conflicts_with = "undo")]
    from: Option<String>,
    /// Restore the saved MCP configuration, retaining the imported database.
    #[arg(long)]
    undo: bool,
    /// Project root. Otherwise find graphify-out/graph.json in this directory or its ancestors.
    #[arg(long)]
    project: Option<PathBuf>,
    /// Snapshot to import, relative to the project root.
    #[arg(long, conflicts_with = "undo")]
    graph: Option<PathBuf>,
    /// MCP configuration to change. Required for global configurations.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Graphify server to replace when several are configured.
    #[arg(long, conflicts_with = "undo")]
    server: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    project: PathBuf,
    executable: PathBuf,
    database_hash: String,
    graph: PathBuf,
    server: Option<String>,
    config: PathBuf,
    before: Option<String>,
    after: String,
    nodes: usize,
    edges: usize,
}

#[derive(Serialize)]
pub struct Report {
    pub status: &'static str,
    pub database: PathBuf,
    pub config: PathBuf,
    pub nodes: usize,
    pub edges: usize,
}

const CONFIG_LIMIT: u64 = 8 * 1024 * 1024;
const RECEIPT: &str = "switch-graphify.json";

fn absolute(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

fn root(args: &SwitchArgs) -> Result<PathBuf> {
    if let Some(path) = &args.project {
        let root = path.canonicalize().context("cannot find project")?;
        ensure!(root.is_dir(), "project must be a directory");
        return Ok(root);
    }
    let cwd = std::env::current_dir()?.canonicalize()?;
    for ancestor in cwd.ancestors() {
        if ancestor.join(".graf").join(RECEIPT).try_exists()?
            || ancestor.join("graphify-out/graph.json").try_exists()?
        {
            return Ok(ancestor.to_owned());
        }
    }
    Ok(cwd)
}

fn regular(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            ensure!(
                m.file_type().is_file(),
                "expected a regular file: {}",
                path.display()
            );
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn read_optional(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    if !regular(path)? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "file exceeds size limit: {}",
        path.display()
    );
    Ok(Some(bytes))
}

fn private_dir(path: &Path) -> Result<()> {
    if path.try_exists()? {
        ensure!(
            fs::symlink_metadata(path)?.file_type().is_dir(),
            "migration directory must not be a symlink"
        );
    } else {
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let mut builder = builder;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)?;
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8], expected: Option<&[u8]>) -> Result<()> {
    ensure!(
        read_optional(path, CONFIG_LIMIT * 3)?.as_deref() == expected,
        "file changed since it was read; refusing to replace {}",
        path.display()
    );
    let parent = path.parent().context("file has no parent directory")?;
    // Protect the empty directory before creating a file that will hold secrets.
    // This also prevents inherited ACL readers opening the file before protection.
    let staging = tempfile::tempdir_in(parent)?;
    switch_files::protect(staging.path())?;
    let mut tmp = tempfile::NamedTempFile::new_in(staging.path())?;
    switch_files::protect(tmp.path())?;
    if expected.is_some() {
        switch_files::preserve_permissions(tmp.as_file(), path)?;
    }
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    // Recheck immediately before replacement. The project lock serializes Graf
    // migrations; editors do not participate, so callers should close the client.
    ensure!(
        read_optional(path, CONFIG_LIMIT * 3)?.as_deref() == expected,
        "file changed during migration: {}",
        path.display()
    );
    switch_files::replace(tmp, path, expected.is_some())?;
    Ok(())
}

fn config_path(path: &Path) -> Result<PathBuf> {
    // Resolve parent components once, reject a symlink at the file itself.
    regular(path)?;
    Ok(path
        .parent()
        .context("config has no parent")?
        .canonicalize()?
        .join(path.file_name().context("config has no filename")?))
}

struct Selection {
    config: PathBuf,
    before: Option<Vec<u8>>,
    server: Option<String>,
    graph: PathBuf,
}

fn select(args: &SwitchArgs, project: &Path) -> Result<Selection> {
    let paths = if let Some(path) = &args.config {
        vec![absolute(project, path)]
    } else {
        [".mcp.json", ".cursor/mcp.json", ".vscode/mcp.json"]
            .into_iter()
            .map(|name| project.join(name))
            .filter(|p| p.exists())
            .collect()
    };
    let had_config = !paths.is_empty();
    let mut found = Vec::new();
    for path in paths {
        let path = config_path(&path)?;
        ensure!(
            args.config.is_some() || path.starts_with(project),
            "project MCP config resolves outside the project; select it explicitly with --config"
        );
        let before = read_optional(&path, CONFIG_LIMIT)?;
        if let Some(bytes) = &before {
            for candidate in switch_config::candidates(&path, bytes, project)? {
                if args.server.as_ref().is_none_or(|s| s == &candidate.name) {
                    found.push((
                        path.clone(),
                        Some(bytes.clone()),
                        Some(candidate.name),
                        candidate.graph,
                    ));
                }
            }
        }
        // An explicitly selected config without Graphify can receive a new Graf entry.
        if args.config.is_some() && found.is_empty() && args.server.is_none() {
            found.push((path, before, None, project.join("graphify-out/graph.json")));
        }
    }
    ensure!(
        found.len() <= 1,
        "multiple Graphify connections found: {}; select --config and --server",
        found
            .iter()
            .map(|(p, _, s, _)| format!("{} ({})", p.display(), s.as_deref().unwrap_or("new")))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let (path, before, server, detected_graph) = match found.pop() {
        Some(selected) => selected,
        None => {
            ensure!(args.server.is_none(), "no matching Graphify server found");
            ensure!(
                !had_config,
                "no supported Graphify stdio connection found in the project MCP configs; select a configuration with --config"
            );
            let path = config_path(&project.join(".mcp.json"))?;
            let bytes = read_optional(&path, CONFIG_LIMIT)?;
            (path, bytes, None, project.join("graphify-out/graph.json"))
        }
    };
    let graph = absolute(project, args.graph.as_deref().unwrap_or(&detected_graph))
        .canonicalize()
        .context("cannot find Graphify snapshot; pass --graph PATH")?;
    ensure!(
        server.is_none() || graph.starts_with(project),
        "selected Graphify graph resolves outside this project"
    );
    if server.is_some() && args.graph.is_some() {
        ensure!(
            detected_graph.canonicalize()? == graph,
            "--graph does not match the selected Graphify connection"
        );
    }
    ensure!(graph.is_file(), "Graphify snapshot must be a file");
    Ok(Selection {
        config: path,
        before,
        server,
        graph,
    })
}

fn database_hash(db: &Path) -> Result<String> {
    ensure!(regular(db)?, "migration database is missing");
    let mut digest = blake3::Hasher::new();
    digest.update_reader(File::open(db)?)?;
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    if regular(&wal)? && fs::metadata(&wal)?.len() != 0 {
        // Imported snapshots have no writers. Include pending WAL changes so
        // an externally modified database cannot pass a main-file-only check.
        digest.update(b"WAL");
        digest.update_reader(File::open(wal)?)?;
    }
    Ok(digest.finalize().to_hex().to_string())
}

fn verify(db: &Path, exe: &Path, nodes: usize, edges: usize) -> Result<()> {
    let store = Store::open(db)?;
    let stats = store.stats()?;
    ensure!(
        stats.kind == "imported" && stats.nodes == nodes && stats.edges == edges,
        "imported database no longer matches the migration record"
    );
    store.query("graf migration verification", &QueryOptions::default())?;
    drop(store);
    verify_mcp(db, exe, nodes, edges).context("Graf MCP verification failed")
}

fn verify_mcp(db: &Path, exe: &Path, nodes: usize, edges: usize) -> Result<()> {
    let mut child = Command::new(exe)
        .args(["--db"])
        .arg(db)
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| -> Result<()> {
        let mut input = child.stdin.take().context("missing MCP stdin")?;
        let output = child.stdout.take().context("missing MCP stdout")?;
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut line = String::new();
                match reader.by_ref().take(1024 * 1024).read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut request = |value: Value, id: u64| -> Result<Value> {
            serde_json::to_writer(&mut input, &value)?;
            input.write_all(b"\n")?;
            input.flush()?;
            if id == 0 {
                return Ok(Value::Null);
            }
            for _ in 0..16 {
                let line = rx
                    .recv_timeout(Duration::from_secs(10))
                    .context("MCP response timed out")?;
                let v: Value = serde_json::from_str(&line)?;
                if v.get("id").and_then(Value::as_u64) == Some(id) {
                    ensure!(v.get("error").is_none(), "MCP returned an error");
                    return Ok(v);
                }
            }
            bail!("MCP response missing")
        };
        request(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"graf-switch","version":env!("CARGO_PKG_VERSION")}}}),
            1,
        )?;
        // Notification and request share the same stdio stream.
        request(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            0,
        )?;
        // Notifications do not receive a reply; the separate call below is the actual check.
        let stats = request(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"stats","arguments":{}}}),
            2,
        )?;
        ensure!(stats["result"]["isError"] != true, "MCP stats failed");
        let content = &stats["result"]["structuredContent"];
        ensure!(
            content["nodes"] == nodes && content["edges"] == edges,
            "MCP graph counts differ"
        );
        let query = request(
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"query","arguments":{"text":"graf migration verification","depth":0,"limit":1}}}),
            3,
        )?;
        ensure!(
            query["result"]["isError"] != true
                && query["result"]["structuredContent"]["nodes"].is_array(),
            "MCP query failed"
        );
        drop(input);
        drop(reader); // Child cleanup below closes the pipe even after a protocol failure.
        Ok(())
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

pub fn run(args: SwitchArgs) -> Result<Report> {
    let project = root(&args)?;
    let directory = project.join(".graf");
    let record_path = directory.join(RECEIPT);
    let db = directory.join("index.db");
    let exe = std::env::current_exe()?.canonicalize()?;
    // Preflight all source/config input before creating migration files.
    let existing = if directory.try_exists()? {
        private_dir(&directory)?;
        read_optional(&record_path, CONFIG_LIMIT * 3)?
    } else {
        None
    };
    if let Some(bytes) = existing {
        let mut record: Receipt =
            serde_json::from_slice(&bytes).context("invalid Graf migration record")?;
        ensure!(
            record.schema_version == 1 && record.project == project,
            "migration record does not belong to this project"
        );
        // Parent components can be redirected after the initial switch. Resolve
        // the saved destination again before applying project/global boundaries.
        record.config = config_path(&record.config)?;
        let selected_config = args
            .config
            .as_ref()
            .map(|p| config_path(&absolute(&project, p)))
            .transpose()?;
        ensure!(
            selected_config.as_ref().is_none_or(|p| p == &record.config),
            "--config differs from the migration record"
        );
        ensure!(
            record.config.starts_with(&project) || selected_config.is_some(),
            "this migration changed a global config; pass --config {}",
            record.config.display()
        );
        ensure!(
            args.server
                .as_ref()
                .is_none_or(|name| record.server.as_ref() == Some(name)),
            "--server differs from the migration record"
        );
        if let Some(graph) = &args.graph {
            let requested = absolute(&project, graph);
            ensure!(
                requested.canonicalize().unwrap_or(requested) == record.graph,
                "--graph differs from the migration record"
            );
        }
        let lock = lock(&directory)?;
        let current = read_optional(&record.config, CONFIG_LIMIT)?;
        let before = record.before.as_deref().map(str::as_bytes);
        let after = Some(record.after.as_bytes());
        ensure!(
            current.as_deref() == before || current.as_deref() == after,
            "MCP config changed after switching; refusing to overwrite your edits: {}",
            record.config.display()
        );
        let status = if args.undo {
            if current.as_deref() == after {
                if let Some(original) = before {
                    write_atomic(&record.config, original, after)?;
                } else {
                    fs::remove_file(&record.config)?;
                }
            }
            "undone"
        } else {
            ensure!(
                record.executable == exe,
                "migration uses a different Graf executable; run the original executable or undo before changing it"
            );
            ensure!(
                database_hash(&db)? == record.database_hash,
                "imported database changed after switching; refusing to activate a different snapshot"
            );
            verify(&db, &exe, record.nodes, record.edges)?;
            if current.as_deref() != after {
                write_atomic(&record.config, record.after.as_bytes(), before)?;
                "switched"
            } else {
                "already_switched"
            }
        };
        drop(lock);
        return Ok(Report {
            status,
            database: db,
            config: record.config,
            nodes: record.nodes,
            edges: record.edges,
        });
    }
    ensure!(
        !args.undo,
        "no Graphify migration record found in this project"
    );
    ensure!(
        !regular(&db)?,
        "{} already exists; migration requires a new database",
        db.display()
    );
    let Selection {
        config,
        before,
        server,
        graph: graph_path,
    } = select(&args, &project)?;
    let after = switch_config::rewrite(
        &config,
        before.as_deref(),
        &project,
        server.as_deref(),
        &exe,
        &db,
    )?;
    let graph = import::read_graphify_export(&graph_path)?;
    let nodes = graph.nodes.len();
    let edges = graph.edges.len();
    let mut record = Receipt {
        schema_version: 1,
        project,
        executable: exe.clone(),
        database_hash: String::new(),
        graph: graph_path,
        server,
        config: config.clone(),
        before: before
            .as_ref()
            .map(|b| String::from_utf8(b.clone()))
            .transpose()?,
        after: String::from_utf8(after.clone())?,
        nodes,
        edges,
    };
    ensure!(
        serde_json::to_vec_pretty(&record)?.len() as u64 + 64 <= CONFIG_LIMIT * 3,
        "migration backup exceeds size limit; configuration was not changed"
    );
    private_dir(&directory)?;
    let _lock = lock(&directory)?;
    ensure!(
        !record_path.try_exists()? && !db.try_exists()?,
        "another migration created a database; run the command again"
    );
    // Keep local backups (which may contain MCP environment secrets) out of Git.
    let ignore_path = directory.join(".gitignore");
    let ignored = read_optional(&ignore_path, CONFIG_LIMIT)?;
    if let Some(original) = &ignored {
        let mut updated = original.clone();
        updated.extend_from_slice(b"\n/switch-graphify.json\n/switch.lock\n");
        write_atomic(&ignore_path, &updated, Some(original))?;
    } else {
        write_atomic(&ignore_path, b"*\n", None)?;
    }
    let staging = tempfile::tempdir_in(&directory)?;
    switch_files::protect(staging.path())?;
    let staged_db = staging.path().join("index.db");
    let stats = Store::create(&staged_db)?.import_graph(graph)?;
    ensure!(
        stats.nodes == nodes && stats.edges == edges,
        "import counts differ"
    );
    verify(&staged_db, &exe, nodes, edges)?;
    record.database_hash = database_hash(&staged_db)?;
    // Closing the last SQLite connection checkpoints WAL before publishing the DB.
    let mut target = tempfile::NamedTempFile::new_in(staging.path())?;
    switch_files::protect(target.path())?;
    std::io::copy(&mut File::open(&staged_db)?, &mut target)?;
    target.as_file().sync_all()?;
    target.persist_noclobber(&db)?;
    // Save recovery data before switching. If replacement fails, repeating or
    // undoing the command safely reconciles the two exact config versions.
    write_atomic(&record_path, &serde_json::to_vec_pretty(&record)?, None)?;
    verify(&db, &exe, nodes, edges)?;
    write_atomic(&config, &after, before.as_deref())?;
    Ok(Report {
        status: "switched",
        database: db,
        config,
        nodes,
        edges,
    })
}

fn lock(directory: &Path) -> Result<File> {
    let path = directory.join("switch.lock");
    regular(&path)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    lock.try_lock()
        .context("another Graf migration is running")?;
    Ok(lock)
}
