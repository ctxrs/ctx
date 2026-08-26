//! Test-only deterministic Custom History V2 Core-generation materializer.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    env,
    ffi::OsString,
    fmt::Write as _,
    fs::{self, File},
    io::{BufRead, BufReader, Write as _},
    path::{Component, Path, PathBuf},
};

use ctx_history_capture_composition::{
    refresh_source_backed_generation, register_custom_history_source_backed_route,
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, SourceBackedProviderRegistry,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope, CaptureProvider,
    CtxHistoryJsonlCopiedFromSelector, CtxHistoryJsonlCopyProofKind,
    CtxHistoryJsonlLineageContract, CtxHistoryJsonlRecord, EventIdentityInput, NativeItemKey,
    NativeSessionKey, ProviderNativeCopyProof, ProviderNativeEventCopy,
    ProviderNativeSessionRelationship, SessionIdentityInput, SourceAnchor, SourceKey,
    StableEntityId, TypedKey, CTX_HISTORY_JSONL_SCHEMA_VERSION,
};
use ctx_history_index::{durable_atomic_replace_file, VerifiedIndex, WriterOptions};
use ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::{Builder as TempBuilder, NamedTempFile};

const USAGE: &str = "usage: custom_history_core_fixture --jsonl PATH --catalog-lineage 64-hex --data-root PATH --identity-map PATH --corpus-manifest PATH";
const CUSTOM_ROUTE_SOURCE_FORMAT: &str = "ctx_history_jsonl_v2";
const CUSTOM_SOURCE_SCHEMA_VARIANT: &str = "ctx-history-jsonl-v2-source-backed-v1";
const CUSTOM_SOURCE_IDENTITY_VERSION: u32 = 1;
const CUSTOM_SESSION_KEY_NAMESPACE: &str = "custom-history.session";
const CUSTOM_EVENT_KEY_NAMESPACE: &str = "custom-history.event";
const CUSTOM_LOGICAL_SESSION_KIND: &str = "custom-history-session";
const CUSTOM_LOGICAL_EVENT_KIND: &str = "custom-history-event";
const CUSTOM_SOURCE_BACKED_PARSER_REVISION: &str =
    "custom-history-jsonl-source-backed-v9-provider-session-identity";
const FIXTURE_ALIAS_FIELD: &str = "fixture_alias";
const FIXTURE_ALIAS_MAX_BYTES: usize = 128;
const PAGE_ITEMS: usize = 4_096;

type AppResult<T> = Result<T, String>;
type SessionKey = (String, String);
type EventKey = (String, String, u64);

#[derive(Debug, Clone)]
struct Arguments {
    jsonl: PathBuf,
    catalog_lineage: [u8; 32],
    catalog_lineage_hex: String,
    data_root: PathBuf,
    identity_map: PathBuf,
    corpus_manifest: PathBuf,
}

#[derive(Debug)]
struct PreparedPaths {
    jsonl: PathBuf,
    data_root: PathBuf,
    identity_map: PathBuf,
    corpus_manifest: PathBuf,
}

#[derive(Debug)]
struct SourceDeclaration {
    provider_key: String,
}

#[derive(Debug)]
struct SessionDeclaration {
    source_id: String,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    root_provider_session_id: Option<String>,
    relationship: Option<ProviderNativeSessionRelationship>,
    agent_scope: Option<AgentScope>,
}

#[derive(Debug)]
struct EventDeclaration {
    source_id: String,
    provider_session_id: String,
    event_index: u64,
    event_id: Option<String>,
    copied_from: Option<CtxHistoryJsonlCopiedFromSelector>,
}

#[derive(Debug)]
struct EdgeDeclaration {
    source_id: String,
    from_provider_session_id: String,
    to_provider_session_id: String,
    relationship: Option<ProviderNativeSessionRelationship>,
}

#[derive(Debug)]
struct ExpectedSession {
    declaration: SessionDeclaration,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: Option<StableEntityId>,
    materialized: bool,
}

#[derive(Debug)]
struct ExpectedEvent {
    declaration: EventDeclaration,
    event_id: StableEntityId,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: Option<StableEntityId>,
    native_event_id: TypedKey,
    relationship: Option<ProviderNativeSessionRelationship>,
    agent_scope: Option<AgentScope>,
    event_copy: Option<ProviderNativeEventCopy>,
}

#[derive(Debug)]
struct FixtureOracle {
    fixture_sha256: String,
    source: SourceKey,
    sources: BTreeMap<String, SourceDeclaration>,
    sessions: BTreeMap<SessionKey, ExpectedSession>,
    session_order: Vec<SessionKey>,
    events: BTreeMap<EventKey, ExpectedEvent>,
    event_order: Vec<EventKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MaterializationSummary {
    generation_id: String,
    source_count: usize,
    core_source_count: usize,
    session_count: usize,
    event_count: usize,
}

fn main() {
    let result = parse_arguments(env::args_os().skip(1)).and_then(run_materializer);
    match result {
        Ok(summary) => {
            println!(
                "{}",
                json!({
                    "schema_version": 2,
                    "generation_id": summary.generation_id,
                    "source_count": summary.source_count,
                    "core_source_count": summary.core_source_count,
                    "session_count": summary.session_count,
                    "event_count": summary.event_count,
                })
            );
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> AppResult<Arguments> {
    let mut arguments = arguments.into_iter();
    let mut jsonl = None;
    let mut lineage = None;
    let mut data_root = None;
    let mut identity_map = None;
    let mut corpus_manifest = None;
    while let Some(raw_flag) = arguments.next() {
        let flag = raw_flag
            .to_str()
            .ok_or_else(|| format!("argument names must be UTF-8; {USAGE}"))?;
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {flag}; {USAGE}"))?;
        let slot = match flag {
            "--jsonl" => &mut jsonl,
            "--catalog-lineage" => &mut lineage,
            "--data-root" => &mut data_root,
            "--identity-map" => &mut identity_map,
            "--corpus-manifest" => &mut corpus_manifest,
            _ => return Err(format!("unsupported argument {flag}; {USAGE}")),
        };
        if slot.replace(value).is_some() {
            return Err(format!("duplicate argument {flag}; {USAGE}"));
        }
    }
    let jsonl = PathBuf::from(jsonl.ok_or_else(|| format!("missing --jsonl; {USAGE}"))?);
    let lineage = lineage.ok_or_else(|| format!("missing --catalog-lineage; {USAGE}"))?;
    let lineage = lineage
        .into_string()
        .map_err(|_| format!("--catalog-lineage must be UTF-8; {USAGE}"))?;
    let (catalog_lineage, catalog_lineage_hex) = parse_lineage(&lineage)?;
    Ok(Arguments {
        jsonl,
        catalog_lineage,
        catalog_lineage_hex,
        data_root: PathBuf::from(data_root.ok_or_else(|| format!("missing --data-root; {USAGE}"))?),
        identity_map: PathBuf::from(
            identity_map.ok_or_else(|| format!("missing --identity-map; {USAGE}"))?,
        ),
        corpus_manifest: PathBuf::from(
            corpus_manifest.ok_or_else(|| format!("missing --corpus-manifest; {USAGE}"))?,
        ),
    })
}

fn parse_lineage(value: &str) -> AppResult<([u8; 32], String)> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("--catalog-lineage must be exactly 64 hexadecimal characters".to_owned());
    }
    let canonical = value.to_ascii_lowercase();
    let mut bytes = [0_u8; 32];
    for (index, pair) in canonical.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair)
            .map_err(|_| "--catalog-lineage contains invalid hexadecimal".to_owned())?;
        bytes[index] = u8::from_str_radix(pair, 16)
            .map_err(|_| "--catalog-lineage contains invalid hexadecimal".to_owned())?;
    }
    Ok((bytes, canonical))
}

fn run_materializer(arguments: Arguments) -> AppResult<MaterializationSummary> {
    let paths = prepare_paths(&arguments)?;
    let oracle = FixtureOracle::parse(
        &paths.jsonl,
        arguments.catalog_lineage,
        &arguments.catalog_lineage_hex,
    )?;
    prepare_output_parents(&paths)?;

    let data_parent = paths
        .data_root
        .parent()
        .ok_or_else(|| "--data-root has no parent directory".to_owned())?;
    let staging = TempBuilder::new()
        .prefix(".ctx-custom-history-core-fixture-")
        .tempdir_in(data_parent)
        .map_err(|error| format!("create generation staging directory: {error}"))?;
    let staged_data_root = staging.path().join("data-root");
    let index_root = staged_data_root.join("search").join("lexical");
    let mut registry = SourceBackedProviderRegistry::new();
    register_custom_history_source_backed_route(
        &mut registry,
        custom_provider_source(&paths.jsonl),
        arguments.catalog_lineage,
    )
    .map_err(|error| format!("register Custom History source-backed route: {error}"))?;
    let receipt = refresh_source_backed_generation(
        &index_root,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .map_err(|error| format!("materialize Custom History Core generation: {error}"))?;
    let summary = verify_generation(&index_root, &receipt, &oracle)?;
    let observed_fixture_sha256 = sha256_file(&paths.jsonl)?;
    if observed_fixture_sha256 != oracle.fixture_sha256 {
        return Err("Custom History fixture changed during materialization".to_owned());
    }

    let identity_value = identity_map_value(&summary.generation_id, &arguments, &oracle)?;
    let identity_bytes = encode_json(&identity_value)?;
    let identity_map_sha256 = sha256_bytes(&identity_bytes);
    let corpus_value = json!({
        "schema_version": 2,
        "artifact_type": "custom_history_core_fixture_corpus",
        "generation_id": summary.generation_id,
        "source_jsonl_sha256": oracle.fixture_sha256,
        "catalog_lineage": arguments.catalog_lineage_hex,
        "lineage_identifier": oracle.source.identity().as_uuid().to_string(),
        "lineage_digest": hex(&oracle.source.identity().digest()),
        "source_descriptor_digest": hex(&oracle.source.exact_descriptor_digest()),
        "source_count": summary.source_count,
        "core_source_count": summary.core_source_count,
        "session_count": summary.session_count,
        "event_count": summary.event_count,
        "identity_map_sha256": identity_map_sha256,
    });
    let corpus_bytes = encode_json(&corpus_value)?;
    let mut staged_identity = stage_atomic_file(&paths.identity_map, &identity_bytes)?;
    let mut staged_corpus = stage_atomic_file(&paths.corpus_manifest, &corpus_bytes)?;

    inspect_outputs(&paths)?;
    publish_data_root(&staged_data_root, &paths.data_root)?;
    publish_staged_file(&mut staged_identity, &paths.identity_map)?;
    publish_staged_file(&mut staged_corpus, &paths.corpus_manifest)?;
    Ok(summary)
}

fn custom_provider_source(path: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Custom,
        path: path.to_path_buf(),
        exists: true,
        source_format: CUSTOM_ROUTE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: Default::default(),
    }
}

fn prepare_paths(arguments: &Arguments) -> AppResult<PreparedPaths> {
    let jsonl =
        fs::canonicalize(&arguments.jsonl).map_err(|error| format!("resolve --jsonl: {error}"))?;
    let metadata = fs::metadata(&jsonl).map_err(|error| format!("inspect --jsonl: {error}"))?;
    if !metadata.is_file() {
        return Err("--jsonl must name a regular file".to_owned());
    }
    let paths = PreparedPaths {
        jsonl,
        data_root: normalize_absolute(&arguments.data_root)?,
        identity_map: normalize_absolute(&arguments.identity_map)?,
        corpus_manifest: normalize_absolute(&arguments.corpus_manifest)?,
    };
    for output in [
        &paths.data_root,
        &paths.identity_map,
        &paths.corpus_manifest,
    ] {
        reject_output_escape(output)?;
    }
    if paths_overlap(&paths.data_root, &paths.jsonl)
        || paths_overlap(&paths.data_root, &paths.identity_map)
        || paths_overlap(&paths.data_root, &paths.corpus_manifest)
        || paths_overlap(&paths.jsonl, &paths.identity_map)
        || paths_overlap(&paths.jsonl, &paths.corpus_manifest)
        || paths_overlap(&paths.identity_map, &paths.corpus_manifest)
    {
        return Err("input and output paths overlap".to_owned());
    }
    inspect_outputs(&paths)?;
    Ok(paths)
}

fn normalize_absolute(path: &Path) -> AppResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err("output paths must not be empty".to_owned());
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| format!("resolve current directory: {error}"))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("output escape through parent traversal".to_owned());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.file_name().is_none() {
        return Err("output paths must not name a filesystem root".to_owned());
    }
    Ok(normalized)
}

fn reject_output_escape(path: &Path) -> AppResult<()> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "output escape through symbolic link {}",
                    prefix.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect output path {}: {error}", prefix.display())),
        }
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn inspect_outputs(paths: &PreparedPaths) -> AppResult<()> {
    inspect_data_root(&paths.data_root)?;
    inspect_output_file(&paths.identity_map, "--identity-map")?;
    inspect_output_file(&paths.corpus_manifest, "--corpus-manifest")?;
    Ok(())
}

fn inspect_data_root(path: &Path) -> AppResult<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            let mut entries =
                fs::read_dir(path).map_err(|error| format!("inspect --data-root: {error}"))?;
            if entries
                .next()
                .transpose()
                .map_err(|error| error.to_string())?
                .is_some()
            {
                return Err("pre-existing nonempty --data-root is not allowed".to_owned());
            }
            Ok(())
        }
        Ok(_) => Err("--data-root exists and is not a directory".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("inspect --data-root: {error}")),
    }
}

fn inspect_output_file(path: &Path, flag: &str) -> AppResult<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() == 0 => Ok(()),
        Ok(metadata) if metadata.is_file() => {
            Err(format!("pre-existing nonempty {flag} is not allowed"))
        }
        Ok(_) => Err(format!("{flag} exists and is not a regular file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("inspect {flag}: {error}")),
    }
}

fn prepare_output_parents(paths: &PreparedPaths) -> AppResult<()> {
    for output in [
        &paths.data_root,
        &paths.identity_map,
        &paths.corpus_manifest,
    ] {
        let parent = output
            .parent()
            .ok_or_else(|| format!("output path {} has no parent", output.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create output parent {}: {error}", parent.display()))?;
        reject_output_escape(output)?;
    }
    Ok(())
}

fn stage_atomic_file(path: &Path, bytes: &[u8]) -> AppResult<NamedTempFile> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("output path {} has no parent", path.display()))?;
    let mut staged = TempBuilder::new()
        .prefix(".ctx-custom-history-core-json-")
        .tempfile_in(parent)
        .map_err(|error| format!("stage JSON output {}: {error}", path.display()))?;
    staged
        .write_all(bytes)
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|error| format!("synchronize JSON output {}: {error}", path.display()))?;
    Ok(staged)
}

fn publish_staged_file(staged: &mut NamedTempFile, target: &Path) -> AppResult<()> {
    durable_atomic_replace_file(staged.path(), target)
        .map_err(|error| format!("atomically publish {}: {error}", target.display()))
}

fn publish_data_root(staged: &Path, target: &Path) -> AppResult<()> {
    if target.exists() {
        fs::remove_dir(target)
            .map_err(|error| format!("replace empty --data-root {}: {error}", target.display()))?;
    }
    fs::rename(staged, target).map_err(|error| {
        format!(
            "atomically publish --data-root {}: {error}",
            target.display()
        )
    })
}

impl FixtureOracle {
    fn parse(path: &Path, catalog_lineage: [u8; 32], lineage_hex: &str) -> AppResult<Self> {
        let file =
            File::open(path).map_err(|error| format!("open Custom History fixture: {error}"))?;
        let mut reader = BufReader::new(file);
        let mut fixture_digest = Sha256::new();
        let mut line = Vec::new();
        let mut line_number = 0_usize;
        let mut manifest = None;
        let mut aliases = BTreeSet::new();
        let mut sources = BTreeMap::new();
        let mut sessions = BTreeMap::new();
        let mut session_order = Vec::new();
        let mut events = BTreeMap::new();
        let mut event_order = Vec::new();
        let mut edges = Vec::new();
        loop {
            line.clear();
            let count = reader
                .read_until(b'\n', &mut line)
                .map_err(|error| format!("read Custom History fixture: {error}"))?;
            if count == 0 {
                break;
            }
            line_number = line_number.saturating_add(1);
            fixture_digest.update(&line);
            if line.len() > MAX_PROVIDER_JSONL_LINE_BYTES {
                return Err(format!(
                    "fixture line {line_number} exceeds the JSONL line bound"
                ));
            }
            if !line.ends_with(b"\n") {
                return Err("Custom History fixture must end with a complete newline".to_owned());
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let record = serde_json::from_slice::<CtxHistoryJsonlRecord>(&line)
                .map_err(|error| format!("malformed fixture line {line_number}: {error}"))?;
            match record {
                CtxHistoryJsonlRecord::Manifest(value) => {
                    validate_fixture_alias(&value.metadata, &mut aliases, line_number)?;
                    if manifest.replace(value).is_some() {
                        return Err("duplicate Custom History manifest".to_owned());
                    }
                }
                CtxHistoryJsonlRecord::Source(value) => {
                    validate_fixture_alias(&value.metadata, &mut aliases, line_number)?;
                    if sources
                        .insert(
                            value.source_id.clone(),
                            SourceDeclaration {
                                provider_key: value.provider_key,
                            },
                        )
                        .is_some()
                    {
                        return Err(format!("duplicate source_id on fixture line {line_number}"));
                    }
                }
                CtxHistoryJsonlRecord::Session(value) => {
                    validate_fixture_alias(&value.metadata, &mut aliases, line_number)?;
                    let key = (value.source_id.clone(), value.provider_session_id.clone());
                    let declaration = SessionDeclaration {
                        source_id: value.source_id,
                        provider_session_id: value.provider_session_id,
                        parent_provider_session_id: value.parent_provider_session_id,
                        root_provider_session_id: value.root_provider_session_id,
                        relationship: value.session_relationship,
                        agent_scope: value.agent_scope,
                    };
                    if sessions.contains_key(&key) {
                        return Err(format!(
                            "duplicate provider_session_id on fixture line {line_number}"
                        ));
                    }
                    session_order.push(key.clone());
                    sessions.insert(key, declaration);
                }
                CtxHistoryJsonlRecord::Event(value) => {
                    validate_fixture_alias(&value.metadata, &mut aliases, line_number)?;
                    let key = (
                        value.source_id.clone(),
                        value.provider_session_id.clone(),
                        value.event_index,
                    );
                    let declaration = EventDeclaration {
                        source_id: value.source_id,
                        provider_session_id: value.provider_session_id,
                        event_index: value.event_index,
                        event_id: value.event_id,
                        copied_from: value.copied_from,
                    };
                    if events.contains_key(&key) {
                        return Err(format!(
                            "duplicate event_index on fixture line {line_number}"
                        ));
                    }
                    event_order.push(key.clone());
                    events.insert(key, declaration);
                }
                CtxHistoryJsonlRecord::Edge(value) => {
                    validate_fixture_alias(&value.metadata, &mut aliases, line_number)?;
                    edges.push(EdgeDeclaration {
                        source_id: value.source_id,
                        from_provider_session_id: value.from_provider_session_id,
                        to_provider_session_id: value.to_provider_session_id,
                        relationship: value.relationship,
                    });
                }
                CtxHistoryJsonlRecord::FileReference(value) => {
                    validate_fixture_alias(&value.metadata, &mut aliases, line_number)?;
                }
            }
        }
        let manifest = manifest.ok_or_else(|| "missing Custom History manifest".to_owned())?;
        if manifest.schema_version != CTX_HISTORY_JSONL_SCHEMA_VERSION {
            return Err("Custom History manifest has an unsupported schema version".to_owned());
        }
        if sources.is_empty() || events.is_empty() {
            return Err("Custom History fixture must contain sources and events".to_owned());
        }
        apply_edges(&mut sessions, edges)?;
        if manifest.lineage_contract.is_none() {
            for session in sessions.values_mut() {
                session.relationship = None;
            }
            if events.values().any(|event| event.copied_from.is_some()) {
                return Err(
                    "copied_from requires the provider_native_v1 lineage contract".to_owned(),
                );
            }
        }
        validate_declarations(manifest.lineage_contract, &sources, &sessions, &events)?;
        let source = derive_custom_source(catalog_lineage)
            .map_err(|error| format!("derive fixed-lineage SourceKey: {error}"))?;
        if hex(catalog_lineage.as_slice()) != lineage_hex {
            return Err("catalog lineage canonicalization disagrees with parsed bytes".to_owned());
        }
        let materialized_sessions = events
            .keys()
            .map(|key| (key.0.clone(), key.1.clone()))
            .collect::<BTreeSet<_>>();
        let mut expected_sessions = BTreeMap::new();
        for (key, declaration) in sessions {
            let provider_key = &sources
                .get(&declaration.source_id)
                .ok_or_else(|| "session references unknown source".to_owned())?
                .provider_key;
            let session_id = derive_custom_session(
                &source,
                provider_key,
                &declaration.source_id,
                &declaration.provider_session_id,
            )?;
            let parent_session_id = declaration
                .parent_provider_session_id
                .as_deref()
                .map(|value| {
                    derive_custom_session(&source, provider_key, &declaration.source_id, value)
                })
                .transpose()?;
            let root_session_id = declaration
                .root_provider_session_id
                .as_deref()
                .map(|value| {
                    derive_custom_session(&source, provider_key, &declaration.source_id, value)
                })
                .transpose()?;
            expected_sessions.insert(
                key.clone(),
                ExpectedSession {
                    declaration,
                    session_id,
                    parent_session_id,
                    root_session_id,
                    materialized: materialized_sessions.contains(&key),
                },
            );
        }
        let mut expected_events = BTreeMap::new();
        let mut stable_events = HashSet::new();
        for (key, declaration) in events {
            let session = expected_sessions
                .get(&(
                    declaration.source_id.clone(),
                    declaration.provider_session_id.clone(),
                ))
                .ok_or_else(|| "event references unknown session".to_owned())?;
            let provider_key = &sources
                .get(&declaration.source_id)
                .ok_or_else(|| "event references unknown source".to_owned())?
                .provider_key;
            let native_item_key = NativeItemKey::native_id(
                CUSTOM_EVENT_KEY_NAMESPACE,
                custom_event_typed_key(declaration.event_id.as_deref(), declaration.event_index)?,
            )
            .map_err(|error| format!("derive event native key: {error}"))?;
            let event_id = derive_event_id(EventIdentityInput {
                source: &source,
                session_id: session.session_id,
                logical_item_kind: CUSTOM_LOGICAL_EVENT_KIND,
                native_item_key: &native_item_key,
                subrecord_selector: None,
            })
            .map_err(|error| format!("derive event identity: {error}"))?;
            if !stable_events.insert(event_id) {
                return Err(
                    "fixture events derive a duplicate stable Core event identity".to_owned(),
                );
            }
            let selector = declaration.event_id.as_ref().map_or_else(
                || format!("event_index:{}", declaration.event_index),
                |value| format!("event_id:{value}"),
            );
            let native_event_id = TypedKey::composite(vec![
                TypedKey::utf8(provider_key.clone()).map_err(|error| error.to_string())?,
                TypedKey::utf8(declaration.source_id.clone()).map_err(|error| error.to_string())?,
                TypedKey::utf8(selector).map_err(|error| error.to_string())?,
            ])
            .map_err(|error| format!("derive native event selector: {error}"))?;
            let event_copy = declaration
                .copied_from
                .as_ref()
                .map(|copy| derive_copy(&source, provider_key, &declaration.source_id, copy))
                .transpose()?;
            expected_events.insert(
                key,
                ExpectedEvent {
                    declaration,
                    event_id,
                    session_id: session.session_id,
                    parent_session_id: session.parent_session_id,
                    root_session_id: session.root_session_id,
                    native_event_id,
                    relationship: session.declaration.relationship,
                    agent_scope: session.declaration.agent_scope,
                    event_copy,
                },
            );
        }
        Ok(Self {
            fixture_sha256: hex(&fixture_digest.finalize()),
            source,
            sources,
            sessions: expected_sessions,
            session_order,
            events: expected_events,
            event_order,
        })
    }
}

fn validate_fixture_alias(
    metadata: &Value,
    aliases: &mut BTreeSet<String>,
    line_number: usize,
) -> AppResult<()> {
    let Some(value) = metadata.get(FIXTURE_ALIAS_FIELD) else {
        return Ok(());
    };
    let value = value.as_str().ok_or_else(|| {
        format!("{FIXTURE_ALIAS_FIELD} on fixture line {line_number} must be a string")
    })?;
    let valid = !value.is_empty()
        && value.len() <= FIXTURE_ALIAS_MAX_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    if !valid {
        return Err(format!(
            "{FIXTURE_ALIAS_FIELD} on fixture line {line_number} is not a bounded symbolic ID"
        ));
    }
    if !aliases.insert(value.to_owned()) {
        return Err(format!("duplicate fixture alias `{value}`"));
    }
    Ok(())
}

fn apply_edges(
    sessions: &mut BTreeMap<SessionKey, SessionDeclaration>,
    edges: Vec<EdgeDeclaration>,
) -> AppResult<()> {
    for edge in edges {
        let from_key = (
            edge.source_id.clone(),
            edge.from_provider_session_id.clone(),
        );
        let to_key = (edge.source_id.clone(), edge.to_provider_session_id.clone());
        if !sessions.contains_key(&from_key) || !sessions.contains_key(&to_key) {
            return Err("edge references an unknown session".to_owned());
        }
        let Some(relationship) = edge.relationship else {
            continue;
        };
        let child = sessions
            .get_mut(&to_key)
            .ok_or_else(|| "edge child disappeared during validation".to_owned())?;
        if child
            .parent_provider_session_id
            .as_deref()
            .is_some_and(|parent| parent != edge.from_provider_session_id)
            || child
                .relationship
                .is_some_and(|existing| existing != relationship)
        {
            return Err("inconsistent edge and session claims".to_owned());
        }
        child.parent_provider_session_id = Some(edge.from_provider_session_id);
        child.relationship = Some(relationship);
    }
    Ok(())
}

fn validate_declarations(
    lineage_contract: Option<CtxHistoryJsonlLineageContract>,
    sources: &BTreeMap<String, SourceDeclaration>,
    sessions: &BTreeMap<SessionKey, SessionDeclaration>,
    events: &BTreeMap<EventKey, EventDeclaration>,
) -> AppResult<()> {
    for session in sessions.values() {
        if !sources.contains_key(&session.source_id) {
            return Err("session references an unknown source".to_owned());
        }
        if session.parent_provider_session_id.as_deref()
            == Some(session.provider_session_id.as_str())
        {
            return Err("session declares itself as its direct parent".to_owned());
        }
        if lineage_contract.is_some() {
            let valid = match session.relationship {
                None => true,
                Some(ProviderNativeSessionRelationship::Root) => {
                    session.parent_provider_session_id.is_none()
                        && session
                            .root_provider_session_id
                            .as_deref()
                            .is_none_or(|root| root == session.provider_session_id)
                }
                Some(
                    ProviderNativeSessionRelationship::Delegated
                    | ProviderNativeSessionRelationship::Forked
                    | ProviderNativeSessionRelationship::ResumedFrom
                    | ProviderNativeSessionRelationship::WorkflowChild,
                ) => session
                    .parent_provider_session_id
                    .as_deref()
                    .is_some_and(|parent| {
                        parent != session.provider_session_id
                            && session
                                .root_provider_session_id
                                .as_deref()
                                .is_none_or(|root| root != session.provider_session_id)
                    }),
            };
            if !valid {
                return Err("inconsistent session relationship claims".to_owned());
            }
        }
    }
    let mut native_event_ids = HashMap::<(String, String, String), usize>::new();
    for event in events.values() {
        if !sessions.contains_key(&(event.source_id.clone(), event.provider_session_id.clone())) {
            return Err("event references an unknown session".to_owned());
        }
        if let Some(event_id) = &event.event_id {
            *native_event_ids
                .entry((
                    event.source_id.clone(),
                    event.provider_session_id.clone(),
                    event_id.clone(),
                ))
                .or_default() += 1;
        }
    }
    for event in events.values() {
        let Some(copy) = &event.copied_from else {
            continue;
        };
        let session = sessions
            .get(&(event.source_id.clone(), event.provider_session_id.clone()))
            .ok_or_else(|| "copied event session is unavailable".to_owned())?;
        let unique_event_id = event.event_id.as_ref().is_some_and(|event_id| {
            native_event_ids.get(&(
                event.source_id.clone(),
                event.provider_session_id.clone(),
                event_id.clone(),
            )) == Some(&1)
        });
        let typed_child = matches!(
            session.relationship,
            Some(
                ProviderNativeSessionRelationship::Delegated
                    | ProviderNativeSessionRelationship::Forked
                    | ProviderNativeSessionRelationship::ResumedFrom
                    | ProviderNativeSessionRelationship::WorkflowChild
            )
        ) && copy.ancestor_provider_session_id != session.provider_session_id;
        let proof_consistent = copy.proof != CtxHistoryJsonlCopyProofKind::NativeEventIdentity
            || event.event_id.as_deref() == Some(copy.ancestor_event_id.as_str());
        if !unique_event_id || !typed_child || !proof_consistent {
            return Err("inconsistent copied_from claim".to_owned());
        }
    }
    Ok(())
}

fn derive_custom_source(lineage: [u8; 32]) -> Result<SourceKey, impl std::fmt::Display> {
    SourceKey::derive(
        CaptureProvider::Custom.as_str(),
        CUSTOM_ROUTE_SOURCE_FORMAT,
        CUSTOM_SOURCE_SCHEMA_VARIANT,
        CUSTOM_SOURCE_IDENTITY_VERSION,
        SourceAnchor::CatalogLineage(lineage),
    )
}

fn derive_custom_session(
    source: &SourceKey,
    provider_key: &str,
    source_id: &str,
    provider_session_id: &str,
) -> AppResult<StableEntityId> {
    let native_session_key = NativeSessionKey::composite(
        CUSTOM_SESSION_KEY_NAMESPACE,
        vec![
            TypedKey::utf8(provider_key).map_err(|error| error.to_string())?,
            TypedKey::utf8(source_id).map_err(|error| error.to_string())?,
            TypedKey::utf8(provider_session_id).map_err(|error| error.to_string())?,
        ],
    )
    .map_err(|error| format!("derive session native key: {error}"))?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: CUSTOM_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
    .map_err(|error| format!("derive session identity: {error}"))
}

fn custom_event_typed_key(event_id: Option<&str>, event_index: u64) -> AppResult<TypedKey> {
    let parts = match event_id.filter(|value| !value.is_empty()) {
        Some(event_id) => vec![
            TypedKey::utf8("event_id").map_err(|error| error.to_string())?,
            TypedKey::utf8(event_id).map_err(|error| error.to_string())?,
        ],
        None => vec![
            TypedKey::utf8("event_index").map_err(|error| error.to_string())?,
            TypedKey::U64(event_index),
        ],
    };
    TypedKey::composite(parts).map_err(|error| format!("derive event selector: {error}"))
}

fn derive_copy(
    source: &SourceKey,
    provider_key: &str,
    source_id: &str,
    copy: &CtxHistoryJsonlCopiedFromSelector,
) -> AppResult<ProviderNativeEventCopy> {
    let ancestor_session_id = derive_custom_session(
        source,
        provider_key,
        source_id,
        &copy.ancestor_provider_session_id,
    )?;
    let native_item_key = NativeItemKey::native_id(
        CUSTOM_EVENT_KEY_NAMESPACE,
        custom_event_typed_key(Some(&copy.ancestor_event_id), 0)?,
    )
    .map_err(|error| format!("derive copied-event native key: {error}"))?;
    let ancestor_event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: ancestor_session_id,
        logical_item_kind: CUSTOM_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(|error| format!("derive copied-event identity: {error}"))?;
    let proof = match copy.proof {
        CtxHistoryJsonlCopyProofKind::NativeEventIdentity => {
            ProviderNativeCopyProof::NativeEventIdentity
        }
        CtxHistoryJsonlCopyProofKind::NativeCopiedFromField => {
            ProviderNativeCopyProof::NativeCopiedFromField
        }
        CtxHistoryJsonlCopyProofKind::NativeCallResultIdentity => {
            ProviderNativeCopyProof::NativeCallResultIdentity
        }
    };
    Ok(ProviderNativeEventCopy {
        ancestor_session_id,
        ancestor_event_id,
        proof,
    })
}

fn verify_generation(
    index_root: &Path,
    receipt: &ctx_history_capture_composition::SourceBackedRefreshReceipt,
    oracle: &FixtureOracle,
) -> AppResult<MaterializationSummary> {
    if !receipt.failed_routes.is_empty()
        || !receipt.source_failures.is_empty()
        || !receipt.logical_source_failures.is_empty()
        || !receipt.record_rejections.is_empty()
    {
        return Err("Custom History refresh reported source or record rejection".to_owned());
    }
    let index = VerifiedIndex::open(index_root)
        .map_err(|error| format!("open exact materialized generation: {error}"))?;
    if index.manifest().sources.len() != 1
        || receipt.sources.len() != 1
        || !index.manifest().sources[0]
            .observation()
            .source()
            .exact_descriptor_eq(&oracle.source)
    {
        return Err("materialized generation has the wrong exact SourceKey".to_owned());
    }
    if index.generation_id() != receipt.commit.generation_id {
        return Err("refresh receipt and exact generation ID disagree".to_owned());
    }
    let mut cursor = None;
    let mut actual_events = HashMap::new();
    loop {
        let page = index
            .core_source_event_page(&oracle.source, cursor.as_ref(), PAGE_ITEMS)
            .map_err(|error| format!("enumerate exact Core generation: {error}"))?;
        if page.generation_id != index.generation_id() {
            return Err("Core enumeration changed logical generation".to_owned());
        }
        for item in page.items {
            let digest = item.core_record.event_id.digest();
            if actual_events.insert(digest, item.core_record).is_some() {
                return Err("exact Core generation contains duplicate event identity".to_owned());
            }
        }
        if page.terminal {
            if page.next_cursor.is_some() {
                return Err("terminal Core page retained a cursor".to_owned());
            }
            break;
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Err("nonterminal Core page omitted its cursor".to_owned());
        }
    }
    if actual_events.len() != oracle.events.len()
        || index.document_count() != oracle.events.len() as u64
        || receipt.commit.indexed_documents != oracle.events.len() as u64
    {
        return Err("materialized event count disagrees with the identity oracle".to_owned());
    }
    let mut session_claims = HashMap::new();
    for expected in oracle.events.values() {
        let actual = actual_events
            .remove(&expected.event_id.digest())
            .ok_or_else(|| "oracle event is absent from exact Core generation".to_owned())?;
        if actual.event_id != expected.event_id
            || actual.session_id != expected.session_id
            || actual.parent_session_id != expected.parent_session_id
            || actual.root_session_id != expected.root_session_id
            || actual.session_relationship != expected.relationship
            || actual.event_copy != expected.event_copy
            || actual.agent_scope != expected.agent_scope
            || actual.event_sequence != expected.declaration.event_index
            || actual.provider_session_id.as_deref()
                != Some(expected.declaration.provider_session_id.as_str())
            || actual.native_event_id.as_ref() != Some(&expected.native_event_id)
            || actual.parser_revision != CUSTOM_SOURCE_BACKED_PARSER_REVISION
            || !actual.source.exact_descriptor_eq(&oracle.source)
        {
            return Err("stored Core mapping disagrees with the pre-publication oracle".to_owned());
        }
        let claims = (
            actual.parent_session_id,
            actual.root_session_id,
            actual.session_relationship,
        );
        if session_claims
            .insert(actual.session_id, claims)
            .is_some_and(|existing| existing != claims)
        {
            return Err("stored Core session claims are inconsistent".to_owned());
        }
    }
    if !actual_events.is_empty() {
        return Err("exact Core generation contains an event outside the oracle".to_owned());
    }
    let session_count = oracle
        .sessions
        .values()
        .filter(|session| session.materialized)
        .count();
    if session_claims.len() != session_count {
        return Err("materialized session count disagrees with the identity oracle".to_owned());
    }
    Ok(MaterializationSummary {
        generation_id: index.generation_id().to_owned(),
        source_count: oracle.sources.len(),
        core_source_count: index.manifest().sources.len(),
        session_count: oracle.sessions.len(),
        event_count: oracle.events.len(),
    })
}

fn identity_map_value(
    generation_id: &str,
    arguments: &Arguments,
    oracle: &FixtureOracle,
) -> AppResult<Value> {
    let sessions = oracle
        .session_order
        .iter()
        .map(|key| {
            let session = oracle
                .sessions
                .get(key)
                .ok_or_else(|| "ordered fixture session is unavailable".to_owned())?;
            Ok::<_, String>(json!({
                "source_id": session.declaration.source_id,
                "provider_session_id": session.declaration.provider_session_id,
                "ctx_session_id": session.session_id.to_string(),
                "root_ctx_session_id": session.root_session_id.map(|identity| identity.to_string()),
            }))
        })
        .collect::<AppResult<Vec<_>>>()?;
    let events = oracle
        .event_order
        .iter()
        .map(|key| {
            let event = oracle
                .events
                .get(key)
                .ok_or_else(|| "ordered fixture event is unavailable".to_owned())?;
            Ok::<_, String>(json!({
                "source_id": event.declaration.source_id,
                "provider_session_id": event.declaration.provider_session_id,
                "event_id": event.declaration.event_id,
                "event_index": event.declaration.event_index,
                "ctx_event_id": event.event_id.to_string(),
                "ctx_session_id": event.session_id.to_string(),
            }))
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(json!({
        "schema_version": 2,
        "artifact_type": "custom_history_core_fixture_identity_map",
        "generation_id": generation_id,
        "catalog_lineage": arguments.catalog_lineage_hex,
        "source_jsonl_sha256": oracle.fixture_sha256,
        "sessions": sessions,
        "events": events,
    }))
}

fn encode_json(value: &Value) -> AppResult<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize content-free fixture artifact: {error}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sha256_file(path: &Path) -> AppResult<String> {
    let mut file =
        File::open(path).map_err(|error| format!("open fixture for hashing: {error}"))?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest).map_err(|error| format!("hash fixture: {error}"))?;
    Ok(hex(&digest.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lineage() -> String {
        "2a".repeat(32)
    }

    fn arguments(root: &Path, fixture: &Path) -> Arguments {
        let lineage = lineage();
        let (catalog_lineage, catalog_lineage_hex) = parse_lineage(&lineage).unwrap();
        Arguments {
            jsonl: fixture.to_path_buf(),
            catalog_lineage,
            catalog_lineage_hex,
            data_root: root.join("data"),
            identity_map: root.join("identity-map.json"),
            corpus_manifest: root.join("corpus-manifest.json"),
        }
    }

    fn existing_lineage_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/custom-history-jsonl/provider-native-lineage.jsonl")
    }

    #[test]
    fn representative_lineage_copy_fixture_is_exact_and_repeatable() {
        let temp = tempfile::tempdir().unwrap();
        let fixture = existing_lineage_fixture();
        let first_root = temp.path().join("first-output");
        let second_root = temp.path().join("second-output");
        let first = arguments(&first_root, &fixture);
        let second = arguments(&second_root, &fixture);

        let first_summary = run_materializer(first.clone()).unwrap();
        let second_summary = run_materializer(second.clone()).unwrap();
        assert_eq!(first_summary, second_summary);
        assert_eq!(first_summary.source_count, 1);
        assert_eq!(first_summary.core_source_count, 1);
        assert_eq!(first_summary.session_count, 2);
        assert_eq!(first_summary.event_count, 2);
        assert_eq!(first_summary.generation_id.len(), 64);
        assert_eq!(
            fs::read(&first.identity_map).unwrap(),
            fs::read(&second.identity_map).unwrap()
        );
        assert_eq!(
            fs::read(&first.corpus_manifest).unwrap(),
            fs::read(&second.corpus_manifest).unwrap()
        );

        let identity: Value =
            serde_json::from_slice(&fs::read(&first.identity_map).unwrap()).unwrap();
        let manifest: Value =
            serde_json::from_slice(&fs::read(&first.corpus_manifest).unwrap()).unwrap();
        assert_eq!(identity["schema_version"], 2);
        assert_eq!(manifest["schema_version"], 2);
        assert_eq!(
            identity["artifact_type"],
            "custom_history_core_fixture_identity_map"
        );
        assert_eq!(
            manifest["artifact_type"],
            "custom_history_core_fixture_corpus"
        );
        assert_eq!(manifest["generation_id"], first_summary.generation_id);
        assert_eq!(manifest["source_count"], 1);
        assert_eq!(manifest["core_source_count"], 1);
        assert_eq!(manifest["session_count"], 2);
        assert_eq!(manifest["event_count"], 2);
        assert_eq!(
            manifest["identity_map_sha256"],
            sha256_file(&first.identity_map).unwrap()
        );
        let events = identity["events"].as_array().unwrap();
        let copied = events
            .iter()
            .find(|event| event["event_id"] == "native-fork-event-0")
            .unwrap();
        assert!(copied["ctx_event_id"].as_str().is_some());
        let identity_text = serde_json::to_string(&identity).unwrap();
        assert!(!identity_text.contains("original event"));
        assert!(!identity_text.contains("copied event"));
        let reopened = VerifiedIndex::open(first.data_root.join("search/lexical")).unwrap();
        assert_eq!(reopened.generation_id(), first_summary.generation_id);
        let source = derive_custom_source(first.catalog_lineage)
            .map_err(|error| error.to_string())
            .unwrap();
        let page = reopened
            .core_source_event_page(&source, None, PAGE_ITEMS)
            .unwrap();
        assert!(page
            .items
            .iter()
            .any(|item| item.core_record.event_copy.is_some()));
    }

    #[test]
    fn malformed_alias_claim_overlap_and_nonempty_outputs_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let malformed = temp.path().join("malformed.jsonl");
        fs::write(&malformed, b"{\n").unwrap();
        let malformed_args = arguments(&temp.path().join("malformed-output"), &malformed);
        assert!(run_materializer(malformed_args.clone())
            .unwrap_err()
            .contains("malformed fixture"));
        assert!(!malformed_args.data_root.exists());

        let duplicate_alias = temp.path().join("duplicate-alias.jsonl");
        fs::write(
            &duplicate_alias,
            concat!(
                "{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\"}\n",
                "{\"record_type\":\"source\",\"source_id\":\"s\",\"provider_key\":\"p\",\"source_format\":\"f\",\"metadata\":{\"fixture_alias\":\"same\"}}\n",
                "{\"record_type\":\"session\",\"source_id\":\"s\",\"provider_session_id\":\"one\",\"started_at\":\"2026-08-25T00:00:00Z\",\"metadata\":{\"fixture_alias\":\"same\"}}\n",
                "{\"record_type\":\"event\",\"source_id\":\"s\",\"provider_session_id\":\"one\",\"event_index\":0,\"event_id\":\"e\",\"occurred_at\":\"2026-08-25T00:00:01Z\"}\n",
            ),
        )
        .unwrap();
        assert!(run_materializer(arguments(
            &temp.path().join("duplicate-output"),
            &duplicate_alias,
        ))
        .unwrap_err()
        .contains("duplicate fixture alias"));

        let inconsistent = temp.path().join("inconsistent.jsonl");
        fs::write(
            &inconsistent,
            concat!(
                "{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\",\"lineage_contract\":\"provider_native_v1\"}\n",
                "{\"record_type\":\"source\",\"source_id\":\"s\",\"provider_key\":\"p\",\"source_format\":\"f\"}\n",
                "{\"record_type\":\"session\",\"source_id\":\"s\",\"provider_session_id\":\"root\",\"started_at\":\"2026-08-25T00:00:00Z\"}\n",
                "{\"record_type\":\"session\",\"source_id\":\"s\",\"provider_session_id\":\"child\",\"parent_provider_session_id\":\"root\",\"session_relationship\":\"root\",\"started_at\":\"2026-08-25T00:00:01Z\"}\n",
                "{\"record_type\":\"event\",\"source_id\":\"s\",\"provider_session_id\":\"child\",\"event_index\":0,\"event_id\":\"e\",\"occurred_at\":\"2026-08-25T00:00:02Z\"}\n",
            ),
        )
        .unwrap();
        assert!(run_materializer(arguments(
            &temp.path().join("inconsistent-output"),
            &inconsistent,
        ))
        .unwrap_err()
        .contains("inconsistent session relationship"));

        let overlap_root = temp.path().join("overlap");
        fs::create_dir_all(&overlap_root).unwrap();
        let overlap_fixture = overlap_root.join("history.jsonl");
        fs::copy(existing_lineage_fixture(), &overlap_fixture).unwrap();
        let mut overlap_args = arguments(&temp.path().join("unused"), &overlap_fixture);
        overlap_args.data_root = overlap_root;
        assert!(run_materializer(overlap_args)
            .unwrap_err()
            .contains("paths overlap"));

        let nested_json_fixture = temp.path().join("nested-json-fixture.jsonl");
        fs::copy(existing_lineage_fixture(), &nested_json_fixture).unwrap();
        let mut nested_json_args =
            arguments(&temp.path().join("nested-json"), &nested_json_fixture);
        nested_json_args.corpus_manifest = nested_json_args.identity_map.join("corpus.json");
        assert!(run_materializer(nested_json_args)
            .unwrap_err()
            .contains("paths overlap"));

        let fixture = temp.path().join("valid.jsonl");
        fs::copy(existing_lineage_fixture(), &fixture).unwrap();
        let nonempty_args = arguments(&temp.path().join("nonempty-output"), &fixture);
        fs::create_dir_all(nonempty_args.identity_map.parent().unwrap()).unwrap();
        fs::write(&nonempty_args.identity_map, b"occupied").unwrap();
        assert!(run_materializer(nonempty_args)
            .unwrap_err()
            .contains("pre-existing nonempty --identity-map"));

        let nonempty_root_args = arguments(&temp.path().join("nonempty-root-output"), &fixture);
        fs::create_dir_all(&nonempty_root_args.data_root).unwrap();
        fs::write(nonempty_root_args.data_root.join("occupied"), b"occupied").unwrap();
        assert!(run_materializer(nonempty_root_args)
            .unwrap_err()
            .contains("pre-existing nonempty --data-root"));
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_output_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let fixture = temp.path().join("fixture.jsonl");
        fs::copy(existing_lineage_fixture(), &fixture).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let linked = temp.path().join("linked");
        symlink(&outside, &linked).unwrap();
        let mut escaped = arguments(&temp.path().join("output"), &fixture);
        escaped.identity_map = linked.join("identity.json");
        assert!(run_materializer(escaped)
            .unwrap_err()
            .contains("output escape"));
    }

    #[test]
    fn cli_requires_each_exact_argument_once() {
        let error = parse_arguments([
            OsString::from("--jsonl"),
            OsString::from("fixture"),
            OsString::from("--jsonl"),
            OsString::from("fixture"),
        ])
        .unwrap_err();
        assert!(error.contains("duplicate argument --jsonl"));
        assert!(parse_lineage(&"g".repeat(64)).is_err());
    }
}
