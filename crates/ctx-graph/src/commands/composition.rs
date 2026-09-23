use super::*;

pub(crate) fn compose(entries: &[Registration], destination: &Path) -> Result<ImportedGraph> {
    let sources: Vec<_> = entries.iter().map(|entry| entry.path.clone()).collect();
    protect_sidecars(destination, &sources)?;
    let mut names = BTreeSet::new();
    let mut snapshots = Vec::new();
    for entry in entries {
        ensure!(
            !entry.name.trim().is_empty() && names.insert(&entry.name),
            "duplicate or empty project name"
        );
        regular(&entry.path).with_context(|| {
            format!("source {} is unavailable; aggregate unchanged", entry.name)
        })?;
        if destination.try_exists()? {
            ensure!(
                !same_file(destination, &entry.path)?,
                "aggregate destination must not be one of its sources"
            );
        }
        snapshots.push((
            entry.name.clone(),
            load_source(&entry.path, entry.kind)
                .with_context(|| format!("cannot load {}; aggregate unchanged", entry.name))?,
        ));
    }
    if snapshots.is_empty() {
        return Ok(ImportedGraph {
            nodes: vec![],
            edges: vec![],
            metadata: json!({"projects":[]}),
        });
    }
    snapshot::merge(snapshots)
}

// New SQLite outputs are built entirely in a private sibling staging directory.
// Closing the last connection checkpoints the WAL before the standalone DB copy
// is published without clobbering an existing destination.
pub(crate) fn create_database(path: &Path, graph: ImportedGraph) -> Result<Stats> {
    ensure!(
        fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "destination already exists; choose a new database path"
    );
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let stage = tempfile::tempdir_in(parent)?;
    crate::switch_files::protect(stage.path())?;
    let staged_db = stage.path().join("graph.db");
    let stats = {
        let mut store = Store::create(&staged_db)?;
        store.import_graph(graph)?
    };
    let mut temp = tempfile::NamedTempFile::new_in(stage.path())?;
    crate::switch_files::protect(temp.path())?;
    std::io::copy(&mut File::open(&staged_db)?, temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    crate::switch_files::replace(temp, path, false)?;
    Ok(stats)
}

pub(crate) fn merge(args: &MergeArgs, db: Option<&Path>, json_output: bool) -> Result<()> {
    ensure!(db.is_none(), "merge requires --output; omit --db");
    ensure!(
        args.project.len() + args.snapshot.len() >= 2,
        "merge requires at least two explicitly named sources"
    );
    let mut entries = Vec::new();
    for (kind, sources) in [
        (SourceKind::Database, &args.project),
        (SourceKind::Snapshot, &args.snapshot),
    ] {
        for source in sources {
            entries.push(registered_source(&source.name, &source.path, kind)?);
        }
    }
    let mut graph = compose(&entries, &args.output)?;
    let package_links = if args.link_packages {
        link_packages(&mut graph)
    } else {
        0
    };
    let reference_links = if args.link_references {
        graf::composition::link_references(&mut graph)?
    } else {
        0
    };
    let stats = create_database(&args.output, graph)?;
    print(
        &json!({"output":args.output,"projects":entries,"package_links":package_links,"reference_links":reference_links,"stats":stats}),
        json_output,
    )
}

pub(crate) fn global_path(db: Option<&Path>) -> Result<PathBuf> {
    if let Some(db) = db {
        return Ok(db.to_owned());
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("cannot find home directory; pass --db")?;
    Ok(PathBuf::from(home).join(".graf/global.db"))
}

pub(crate) fn registry(store: &Store) -> Result<Registry> {
    let metadata = store.graph_metadata()?;
    let value = metadata
        .get("graf_registry")
        .context("database is not a Graf global registry; choose a different --db")?;
    let registry: Registry =
        serde_json::from_value(value.clone()).context("invalid Graf registry metadata")?;
    ensure!(registry.version == 1, "unsupported registry version");
    let mut names = BTreeSet::new();
    for entry in &registry.entries {
        ensure!(
            !entry.name.trim().is_empty() && names.insert(&entry.name) && entry.path.is_absolute(),
            "invalid registry entry"
        );
    }
    Ok(registry)
}

// Canonical binding keys are extractor evidence, unlike labels or basenames.
// Keep each source node and its version metadata; this is a relationship only.
pub(crate) fn link_packages(graph: &mut ImportedGraph) -> usize {
    let mut packages = BTreeMap::<&str, Vec<_>>::new();
    for node in &graph.nodes {
        if node.kind != "package" {
            continue;
        }
        let Some(key) = node.binding_key.as_deref() else {
            continue;
        };
        let Some((ecosystem, name)) = key.strip_prefix("package:").and_then(|s| s.split_once(':'))
        else {
            continue;
        };
        if ecosystem.is_empty() || name.is_empty() || key.chars().any(char::is_whitespace) {
            continue;
        }
        packages.entry(key).or_default().push(node);
    }
    let before = graph.edges.len();
    for (key, mut nodes) in packages {
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        for (i, a) in nodes.iter().enumerate() {
            for b in &nodes[i + 1..] {
                let (Some(ap), Some(bp)) = (
                    a.metadata["project"].as_str(),
                    b.metadata["project"].as_str(),
                ) else {
                    continue;
                };
                if ap == bp {
                    continue;
                }
                graph.edges.push(Edge {
                    id: format!("graf:same_package:{}", json!([key, a.id, b.id])),
                    source:a.id.clone(),target:b.id.clone(),relation:"same_package".into(),directed:false,
                    file:None,line:None,confidence:"INFERRED".into(),
                    metadata:json!({"method":"exact_package_binding_key","package_key":key,
                        "sources":[
                            {"project":ap,"original_id":a.metadata["original_id"],"version":a.metadata["original_metadata"]["version"]},
                            {"project":bp,"original_id":b.metadata["original_id"],"version":b.metadata["original_metadata"]["version"]}
                        ]}),
                });
            }
        }
    }
    graph.edges.len() - before
}

pub(crate) const REGISTRY_FINGERPRINT: &str = "graf_registry_fingerprint";

pub(crate) fn aggregate_fingerprint(graph: &ImportedGraph) -> Result<String> {
    let mut nodes: Vec<_> = graph.nodes.iter().collect();
    let mut edges: Vec<_> = graph.edges.iter().collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    edges.sort_by(|a, b| a.id.cmp(&b.id));
    let mut metadata = graph.metadata.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.remove(REGISTRY_FINGERPRINT);
    }
    let mut canonical = json!({"nodes":nodes,"edges":edges,"metadata":metadata});
    canonical.sort_all_objects();
    Ok(format!(
        "blake3:{}",
        blake3::hash(&serde_json::to_vec(&canonical)?).to_hex()
    ))
}

pub(crate) fn global(args: &GlobalArgs, db: Option<&Path>, json_output: bool) -> Result<()> {
    let path = global_path(db)?;
    if matches!(args.command, GlobalCommand::Path) {
        return print(&json!({"database":path}), json_output);
    }
    let read_only = matches!(args.command, GlobalCommand::List | GlobalCommand::Query(_));
    let mut store = if path.try_exists()? {
        regular(&path)?;
        Some(if read_only {
            Store::open_read_only(&path)?
        } else {
            Store::open(&path)?
        })
    } else {
        None
    };
    let mut registry = if let Some(store) = &store {
        registry(store)?
    } else {
        Registry {
            version: 1,
            entries: vec![],
        }
    };
    match &args.command {
        GlobalCommand::List => {
            return print(
                &json!({"database":path,"entries":registry.entries,"generation":store.as_ref().map(Store::stats).transpose()?.map(|s|s.generation)}),
                json_output,
            );
        }
        GlobalCommand::Query(args) => {
            ensure!(!args.text.trim().is_empty(), "query text must not be empty");
            let store = store
                .as_ref()
                .context("no global aggregate exists; run ctx graph global add first")?;
            return print(
                &store.query(
                    &args.text,
                    &QueryOptions {
                        depth: args.depth,
                        limit: args.limit as usize,
                        direction: args.direction.into(),
                        relation: args.relation.clone(),
                    },
                )?,
                json_output,
            );
        }
        GlobalCommand::Add {
            name,
            path,
            snapshot,
        } => {
            let entry = registered_source(
                name,
                path,
                if *snapshot {
                    SourceKind::Snapshot
                } else {
                    SourceKind::Database
                },
            )?;
            registry.entries.retain(|entry| entry.name != *name);
            registry.entries.push(entry);
        }
        GlobalCommand::Remove { name } => {
            let before = registry.entries.len();
            registry.entries.retain(|entry| entry.name != *name);
            ensure!(
                registry.entries.len() != before,
                "project name is not registered: {name}"
            );
        }
        GlobalCommand::Refresh => ensure!(
            store.is_some(),
            "no global registry exists; run ctx graph global add first"
        ),
        GlobalCommand::Path => unreachable!(),
    }
    registry.entries.sort_by(|a, b| a.name.cmp(&b.name));
    // Keep the original Store handle open across input reads: its baseline
    // generation makes refresh_import reject a concurrent registry update.
    let mut graph = compose(&registry.entries, &path)?;
    let package_links = link_packages(&mut graph);
    let reference_links = graf::composition::link_references(&mut graph)?;
    graph.metadata["graf_registry"] = serde_json::to_value(&registry)?;
    let fingerprint = aggregate_fingerprint(&graph)?;
    if let Some(store) = &store
        && store.graph_metadata()?[REGISTRY_FINGERPRINT].as_str() == Some(&fingerprint)
    {
        return print(
            &json!({"database":path,"entries":registry.entries,"stats":store.stats()?,
                "unchanged":true,"package_links":package_links,"reference_links":reference_links}),
            json_output,
        );
    }
    graph.metadata[REGISTRY_FINGERPRINT] = json!(fingerprint);
    let stats = if let Some(store) = &mut store {
        store.refresh_import(graph)?
    } else {
        create_database(&path, graph)?
    };
    print(
        &json!({"database":path,"entries":registry.entries,"stats":stats,"unchanged":false,"package_links":package_links,"reference_links":reference_links}),
        json_output,
    )
}
