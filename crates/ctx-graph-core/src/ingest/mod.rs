//! Local document ingestion. Network reads and semantic extraction are explicit opt-ins.
mod convert;
pub(crate) use convert::run as run_command;
mod documents;
mod pdf;
mod remote;
mod semantic;
pub use remote::{CaptureMetadata, apply_capture_metadata, extract_url};

use crate::model::{Diagnostic, Edge, FileFacts, Node};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

pub use semantic::{
    Provider, ProviderUsage, SemanticBudget, SemanticOptions, SemanticUsage, SemanticUsageRecorder,
};
pub use semantic::{
    SemanticCacheEntry, SemanticCacheStatus, inspect_semantic_cache, remove_semantic_cache_entry,
};

/// An explicitly configured executable, never interpreted as shell source.
/// `{input}` and `{output}` are substituted within individual arguments.
/// With `output_file`, read UTF-8 from `{output}`; otherwise read stdout.
/// Credentials belong in the executable's environment, never these arguments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandAdapter {
    pub program: String,
    pub args: Vec<String>,
    pub output_file: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestOptions {
    pub max_input_bytes: u64,
    pub max_text_bytes: usize,
    pub timeout_secs: u64,
    /// Allow explicit URL add from a local/private server (never follows links).
    pub allow_private_urls: bool,
    /// Override the metadata API endpoint (for compatible gateways or local fixtures).
    pub tweet_oembed_endpoint: Option<String>,
    pub arxiv_api_endpoint: Option<String>,
    pub converters: BTreeMap<String, CommandAdapter>,
    pub semantic: Option<SemanticOptions>,
    pub transcript_cache_dir: Option<PathBuf>,
    #[serde(skip)]
    pub force_cache_refresh: bool,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            max_input_bytes: 32 * 1024 * 1024,
            max_text_bytes: 4 * 1024 * 1024,
            timeout_secs: 60,
            allow_private_urls: false,
            tweet_oembed_endpoint: None,
            arxiv_api_endpoint: None,
            converters: BTreeMap::new(),
            semantic: None,
            transcript_cache_dir: None,
            force_cache_refresh: false,
        }
    }
}

pub(crate) fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn supports(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "md" | "markdown"
            | "mdx"
            | "qmd"
            | "skill"
            | "txt"
            | "text"
            | "html"
            | "htm"
            | "rst"
            | "yaml"
            | "yml"
            | "pdf"
            | "docx"
            | "xlsx"
            | "png"
            | "jpg"
            | "jpeg"
            | "webp"
            | "gif"
            | "bmp"
            | "tif"
            | "tiff"
            | "svg"
            | "mp3"
            | "wav"
            | "m4a"
            | "ogg"
            | "flac"
            | "aac"
            | "opus"
            | "mp4"
            | "mkv"
            | "mov"
            | "webm"
            | "avi"
            | "m4v"
            | "gdoc"
            | "gsheet"
            | "gslides"
            | "url"
            | "webloc"
    )
}

pub(crate) fn validate_relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.contains(['\\', '\0', ':'])
            && !path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == ".."),
        "document path must be a normalized relative POSIX path"
    );
    Ok(())
}

pub(crate) fn validate(options: &IngestOptions) -> Result<()> {
    for endpoint in [&options.tweet_oembed_endpoint, &options.arxiv_api_endpoint]
        .into_iter()
        .flatten()
    {
        safe_url(endpoint, true)?;
    }
    ensure!(
        (1..=1024 * 1024 * 1024).contains(&options.max_input_bytes),
        "invalid input byte limit"
    );
    ensure!(
        (1..=64 * 1024 * 1024).contains(&options.max_text_bytes),
        "invalid text byte limit"
    );
    ensure!(
        (1..=3600).contains(&options.timeout_secs),
        "invalid converter timeout"
    );
    if let Some(settings) = &options.semantic {
        semantic::validate(settings)?;
    }
    Ok(())
}

/// Revision + public settings; never reads key environment values. Cache location
/// is operational and does not affect the content of extracted facts.
pub fn config_fingerprint(options: &IngestOptions) -> Result<String> {
    validate(options)?;
    let mut settings = options.clone();
    settings.transcript_cache_dir = None;
    if let Some(s) = &mut settings.semantic {
        s.cache_dir = None;
    }
    let mut bytes = serde_json::to_vec(&settings)?;
    bytes.extend_from_slice(semantic::INSTRUCTIONS.as_bytes());
    Ok(format!("ingest-v6:{}", blake3::hash(&bytes).to_hex()))
}

pub fn content_fingerprint(content: &[u8], options: &IngestOptions) -> Result<String> {
    Ok(format!(
        "{}:{}",
        config_fingerprint(options)?,
        blake3::hash(content).to_hex()
    ))
}

pub(crate) fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "input must be a regular file, not a symlink"
    );
    let file = File::open(path).context("cannot open document")?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= limit,
        "document exceeds input byte limit"
    );
    let mut data = Vec::new();
    (&file).take(limit + 1).read_to_end(&mut data)?;
    ensure!(
        data.len() as u64 <= limit,
        "document exceeds input byte limit"
    );
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len() && before.modified()? == after.modified()?,
        "document changed during read; retry indexing"
    );
    Ok(data)
}

/// Extract one local file. Links/pointers are recorded, never fetched. Failure
/// is an error so callers can retain their prior transaction/generation.
pub fn extract(
    path: &Path,
    relative: &str,
    hash: &str,
    options: &IngestOptions,
) -> Result<FileFacts> {
    validate(options)?;
    validate_relative(relative)?;
    ensure!(supports(path), "unsupported document extension");
    let bytes = read_bounded(path, options.max_input_bytes)?;
    let ext = extension(path);
    // Google pointers always require the separate explicit export action.
    if matches!(
        ext.as_str(),
        "gdoc" | "gsheet" | "gslides" | "url" | "webloc"
    ) {
        return extract_text(
            relative,
            std::str::from_utf8(&bytes).context("pointer is not UTF-8")?,
            hash,
            options,
        );
    }
    if let Some(adapter) = options.converters.get(&ext) {
        let cache = if matches!(
            ext.as_str(),
            "mp3"
                | "wav"
                | "m4a"
                | "ogg"
                | "flac"
                | "aac"
                | "opus"
                | "mp4"
                | "mkv"
                | "mov"
                | "webm"
                | "avi"
                | "m4v"
        ) {
            options
                .transcript_cache_dir
                .as_ref()
                .map(|directory| {
                    let mut key = blake3::Hasher::new();
                    key.update(b"graf-transcript-v1\0");
                    key.update(ext.as_bytes());
                    key.update(&serde_json::to_vec(adapter)?);
                    key.update(&bytes);
                    Ok::<_, anyhow::Error>(
                        directory.join(format!("{}.json", key.finalize().to_hex())),
                    )
                })
                .transpose()?
        } else {
            None
        };
        if let Some(path) = cache
            .as_ref()
            .filter(|p| !options.force_cache_refresh && p.exists())
        {
            // JSON escaping can expand UTF-8 text by up to six bytes per byte.
            let data = read_bounded(path, options.max_text_bytes as u64 * 6 + 2)?;
            let text: String = serde_json::from_slice(&data)
                .context("invalid transcript cache; explicitly remove or force refresh")?;
            ensure!(
                text.len() <= options.max_text_bytes,
                "cached transcript exceeds text byte limit"
            );
            return converted(relative, &text, hash, options, "configured_converter");
        }
        // Run against the exact bytes read, not a mutable source file or symlink.
        let temp = tempfile::Builder::new()
            .suffix(&format!(".{ext}"))
            .tempfile()?;
        std::fs::write(temp.path(), &bytes)?;
        let text = convert::run(
            adapter,
            Some(temp.path()),
            None,
            options.timeout_secs,
            options.max_text_bytes,
        )?;
        semantic::save_cache(cache.as_deref(), &text, options.max_text_bytes * 6 + 2)?;
        return converted(relative, &text, hash, options, "configured_converter");
    }
    match ext.as_str() {
        "pdf" => {
            let text = pdf::extract(&bytes, options)?;
            converted(relative, &text, hash, options, "lopdf")
        }
        "docx" | "xlsx" => {
            let content = convert::office(&bytes, &ext, options.max_text_bytes)?;
            let mut facts = if ext == "docx" {
                let mut facts = extract_text_as(relative, &content.text, hash, options, "md")?;
                facts.nodes[0].metadata["text"] = json!(content.text);
                facts.nodes[0].metadata["converter"] = json!("zip/quick-xml");
                facts.nodes[0].metadata["line_basis"] = json!("converted_markdown");
                facts
            } else {
                converted(relative, &content.text, hash, options, "zip/quick-xml")?
            };
            office_structure(&mut facts, content.elements);
            Ok(facts)
        }
        "md" | "markdown" | "mdx" | "qmd" | "skill" | "svg" | "txt" | "text" | "html" | "htm"
        | "rst" | "yaml" | "yml" => extract_text_as(
            relative,
            std::str::from_utf8(&bytes).context("document is not UTF-8")?,
            hash,
            options,
            &ext,
        ),
        _ => {
            let mut facts = base(relative, hash, "media");
            if let Some(settings) = options.semantic.as_ref().filter(|s| s.vision) {
                let mime = match ext.as_str() {
                    "png" => Some("image/png"),
                    "jpg" | "jpeg" => Some("image/jpeg"),
                    "gif" => Some("image/gif"),
                    "webp" => Some("image/webp"),
                    _ => None,
                };
                if let Some(mime) = mime {
                    semantic::enrich_image(
                        &mut facts,
                        &bytes,
                        mime,
                        settings,
                        options.force_cache_refresh,
                    )?;
                    return Ok(facts);
                }
            }
            facts.diagnostics.push(Diagnostic {file:relative.into(), line:None,
                message:format!("{ext} media recorded; configure an explicit converter (for example Tesseract OCR or a transcription adapter) to extract content")});
            Ok(facts)
        }
    }
}

fn converted(
    relative: &str,
    text: &str,
    hash: &str,
    options: &IngestOptions,
    provenance: &str,
) -> Result<FileFacts> {
    ensure!(
        text.len() <= options.max_text_bytes,
        "converted document exceeds text byte limit"
    );
    let mut facts = documents::plain(relative, text, hash)?;
    facts.nodes[0].metadata["text"] = json!(text);
    facts.nodes[0].metadata["converter"] = json!(provenance);
    facts.nodes[0].metadata["line_basis"] = json!("converted_text");
    if text.trim().is_empty() {
        facts.diagnostics.push(Diagnostic {
            file: relative.into(),
            line: None,
            message: "No extractable text; scanned PDF/images need an explicit OCR converter"
                .into(),
        });
    }
    enrich(&mut facts, text, options)?;
    Ok(facts)
}

pub fn extract_text(
    relative: &str,
    text: &str,
    hash: &str,
    options: &IngestOptions,
) -> Result<FileFacts> {
    extract_text_as(
        relative,
        text,
        hash,
        options,
        &extension(Path::new(relative)),
    )
}

fn extract_text_as(
    relative: &str,
    text: &str,
    hash: &str,
    options: &IngestOptions,
    format: &str,
) -> Result<FileFacts> {
    validate(options)?;
    validate_relative(relative)?;
    ensure!(
        text.len() <= options.max_text_bytes,
        "document exceeds text byte limit"
    );
    let mut facts = documents::parse_as(relative, text, hash, format)?;
    if matches!(format, "html" | "htm") {
        enrich(&mut facts, &documents::html_text(text), options)?;
    } else {
        enrich(&mut facts, text, options)?;
    }
    Ok(facts)
}

fn enrich(facts: &mut FileFacts, text: &str, options: &IngestOptions) -> Result<()> {
    if let Some(settings) = &options.semantic {
        semantic::enrich(facts, text, settings, options.force_cache_refresh)?;
    }
    Ok(())
}

/// Append opt-in semantic evidence to existing AST facts without replacing any
/// native nodes, edges, references or diagnostics. Failure leaves facts untouched.
pub fn enrich_facts(facts: &mut FileFacts, text: &str, options: &IngestOptions) -> Result<()> {
    validate(options)?;
    validate_relative(&facts.path)?;
    ensure!(
        text.len() <= options.max_text_bytes,
        "document exceeds text byte limit"
    );
    let Some(settings) = &options.semantic else {
        return Ok(());
    };
    if text.trim().is_empty() {
        return Ok(());
    }
    let root = facts
        .nodes
        .first()
        .context("semantic enrichment requires a source root node")?;
    let mut additions = base(&facts.path, &facts.hash, &root.kind);
    additions.nodes[0] = root.clone();
    semantic::enrich(&mut additions, text, settings, options.force_cache_refresh)?;
    ensure!(
        additions.nodes.first().is_some_and(|n| n.id == root.id),
        "semantic enrichment changed its source root"
    );
    let mut node_ids: std::collections::HashSet<_> =
        facts.nodes.iter().map(|n| n.id.as_str()).collect();
    for node in additions.nodes.iter().skip(1) {
        ensure!(
            node_ids.insert(&node.id),
            "semantic node ID collides with existing facts"
        );
    }
    let mut edge_ids: std::collections::HashSet<_> =
        facts.edges.iter().map(|e| e.id.as_str()).collect();
    for edge in &additions.edges {
        ensure!(
            edge_ids.insert(&edge.id),
            "semantic edge ID collides with existing facts"
        );
    }
    facts.nodes.extend(additions.nodes.into_iter().skip(1));
    facts.edges.extend(additions.edges);
    Ok(())
}

/// Explicit Google export through a configured gdoc/gsheet/gslides adapter.
/// The adapter receives the pointer file and returns text (or XLSX for sheets);
/// it owns authentication. The gws recipe exports every spreadsheet worksheet.
pub fn extract_google(path: &Path, relative: &str, options: &IngestOptions) -> Result<FileFacts> {
    validate(options)?;
    validate_relative(relative)?;
    let ext = extension(path);
    ensure!(
        matches!(ext.as_str(), "gdoc" | "gsheet" | "gslides"),
        "expected Google pointer document"
    );
    let bytes = read_bounded(path, options.max_input_bytes)?;
    let pointer = documents::google_pointer(std::str::from_utf8(&bytes)?)?;
    let mut adapter = options
        .converters
        .get(&ext)
        .context("explicit Google export requires a configured converter")?
        .clone();
    let mime = if ext == "gsheet" {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    } else {
        "text/plain"
    };
    let params = json!({"fileId": pointer["file_id"], "mimeType": mime}).to_string();
    adapter.args = adapter
        .args
        .iter()
        .map(|arg| arg.replace("{google_params}", &params))
        .collect();
    let temp = tempfile::Builder::new()
        .suffix(&format!(".{ext}"))
        .tempfile()?;
    std::fs::write(temp.path(), &bytes)?;
    let exported = convert::run_bytes(
        &adapter,
        Some(temp.path()),
        None,
        options.timeout_secs,
        options.max_input_bytes as usize,
    )?;
    let (text, elements) = if ext == "gsheet" && exported.starts_with(b"PK") {
        let content = convert::office(&exported, "xlsx", options.max_text_bytes)?;
        (content.text, content.elements)
    } else {
        (
            String::from_utf8(exported.clone())
                .context("Google converter must return UTF-8 or XLSX")?,
            vec![],
        )
    };
    let hash = content_fingerprint(&exported, options)?;
    let mut facts = converted(relative, &text, &hash, options, "google_export_adapter")?;
    office_structure(&mut facts, elements);
    facts.nodes[0].metadata["pointer"] = pointer;
    apply_capture_metadata(&mut facts, &CaptureMetadata::default())?;
    Ok(facts)
}

fn office_structure(facts: &mut FileFacts, elements: Vec<(String, &'static str, Option<usize>)>) {
    let mut ids: Vec<String> = vec![];
    for (label, kind, parent) in elements {
        let id = format!("office:{}:{}", facts.path, ids.len());
        let owner = parent
            .map(|i| ids[i].clone())
            .unwrap_or_else(|| facts.nodes[0].id.clone());
        facts.nodes.push(Node {
            id: id.clone(),
            label,
            kind: kind.into(),
            file: facts.path.clone(),
            line: None,
            end_line: None,
            qualified_name: None,
            binding_key: None,
            metadata: json!({"provenance":"office_structure"}),
        });
        edge(
            facts,
            &owner,
            &id,
            "contains",
            1,
            json!({"provenance":"office_structure"}),
        );
        facts.edges.last_mut().unwrap().line = None;
        ids.push(id);
    }
}

pub(crate) fn safe_url(value: &str, endpoint: bool) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).context("invalid URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "URL must use HTTP(S)"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "URL credentials are prohibited; use a key environment variable"
    );
    if endpoint {
        ensure!(
            url.query_pairs().all(|(key, _)| key == "api-version"),
            "endpoint query may only contain api-version; credentials belong in key_env"
        );
        ensure!(
            url.fragment().is_none(),
            "endpoint fragments are prohibited"
        );
    }
    Ok(url)
}

pub(crate) fn base(relative: &str, hash: &str, kind: &str) -> FileFacts {
    FileFacts {
        path: relative.into(),
        hash: hash.into(),
        module: relative.into(),
        nodes: vec![Node {
            id: format!("document:{relative}"),
            label: relative.into(),
            kind: kind.into(),
            file: relative.into(),
            line: Some(1),
            end_line: None,
            qualified_name: Some(relative.into()),
            binding_key: Some(format!("file:{relative}")),
            metadata: json!({"provenance":"local_document", "content_hash":hash}),
        }],
        edges: vec![],
        references: vec![],
        diagnostics: vec![],
    }
}

pub(crate) fn edge(
    facts: &mut FileFacts,
    source: &str,
    target: &str,
    relation: &str,
    line: u32,
    metadata: serde_json::Value,
) {
    facts.edges.push(Edge {
        id: format!(
            "document-edge:{}",
            blake3::hash(
                format!(
                    "{source}\0{target}\0{relation}\0{line}\0{}",
                    facts.edges.len()
                )
                .as_bytes()
            )
            .to_hex()
        ),
        source: source.into(),
        target: target.into(),
        relation: relation.into(),
        directed: true,
        file: Some(facts.path.clone()),
        line: Some(line),
        confidence: "observed".into(),
        metadata,
    });
}
