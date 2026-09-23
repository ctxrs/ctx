//! Explicit local work memory. Saved answers never change graph topology.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::model::{GraphSnapshot, Node};

pub const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
const MAX_CORPUS_BYTES: usize = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 4096;
const MAX_NODES: usize = 100;
const DAY: u64 = 86400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Useful,
    #[value(name = "dead_end")]
    DeadEnd,
    Corrected,
}

#[derive(Debug, Clone, Args)]
pub struct SaveResultArgs {
    #[arg(long)]
    pub question: String,
    #[arg(
        long,
        required_unless_present = "answer_file",
        conflicts_with = "answer_file"
    )]
    pub answer: Option<String>,
    #[arg(long, required_unless_present = "answer", conflicts_with = "answer")]
    pub answer_file: Option<PathBuf>,
    #[arg(long = "type", default_value = "query")]
    pub query_type: String,
    /// Exact IDs in the supplied graph; labels are not guessed.
    #[arg(long, num_args = 0..)]
    pub nodes: Vec<String>,
    #[arg(long, value_enum)]
    pub outcome: Option<Outcome>,
    #[arg(long)]
    pub correction: Option<String>,
    #[arg(long, default_value = "graf-out/memory")]
    pub memory_dir: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct ReflectArgs {
    #[arg(long, default_value = "graf-out/memory")]
    pub memory_dir: PathBuf,
    #[arg(long, default_value = "graf-out/reflections/LESSONS.md")]
    pub out: PathBuf,
    /// Zero disables decay. Negative and nonfinite values are rejected.
    #[arg(long, default_value_t = 30.0)]
    pub half_life_days: f64,
    /// Preferred requires this many useful events with matching native index proofs.
    /// Events without that proof remain visible but do not supply corroboration.
    #[arg(long, default_value_t = 2)]
    pub min_corroboration: usize,
    /// Preserve the output when its current content is already identical.
    #[arg(long)]
    pub if_stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEvidence {
    pub node_id: String,
    pub node_hash: String,
    pub file: String,
    /// Bytes read at save time, not the digest of the file used for indexing.
    /// Equality later proves only that these bytes have not changed since save.
    pub source_hash: Option<String>,
    /// Digest carried by the original native snapshot, even when disk differed.
    /// Never filled in later from a newer snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_source_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub schema_version: u32,
    pub event_id: String,
    pub created_unix_secs: u64,
    #[serde(rename = "type")]
    pub query_type: String,
    pub question: String,
    pub answer: String,
    pub contributor: String,
    pub outcome: Option<Outcome>,
    pub correction: Option<String>,
    pub source_nodes: Vec<String>,
    pub graph_identity: Option<String>,
    pub graph_generation: Option<u64>,
    pub evidence: Vec<SourceEvidence>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReflectionResult {
    pub out: PathBuf,
    pub skipped: bool,
    pub records: usize,
}

/// Read-only annotations for an explicit memory projection. These are observations,
/// not graph topology or evidence that a saved answer is true.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningOverlay {
    pub schema_version: u32,
    pub generated_unix_secs: u64,
    /// BLAKE3 of the exact supplied GraphSnapshot serialized as JSON.
    pub snapshot_hash: Option<String>,
    pub nodes: BTreeMap<String, LearningNode>,
    pub lessons: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningNode {
    /// preferred, tentative, contested, dead_end, corrected, stale, or unmarked.
    /// Classification uses active events; stale-only observations stay visible.
    pub status: String,
    pub score: f64,
    /// Useful/negative counts exclude stale and future-dated observations.
    pub useful: usize,
    pub negative: usize,
    pub verified_useful: usize,
    /// Active useful/negative events without full source correspondence proof.
    pub unverified: usize,
    /// Verification details, including any excluded stale or future events.
    pub reason: String,
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

pub fn save_result(args: &SaveResultArgs, graph: Option<&GraphSnapshot>) -> Result<PathBuf> {
    save_result_at(args, graph, now()?)
}

/// Explicit clock seam; each invocation is a distinct event, even at the same time.
pub fn save_result_at(
    args: &SaveResultArgs,
    graph: Option<&GraphSnapshot>,
    now_unix_secs: u64,
) -> Result<PathBuf> {
    let answer = match (&args.answer, &args.answer_file) {
        (Some(text), None) => text.clone(),
        (None, Some(path)) => read_text(path, MAX_TEXT_BYTES)?.trim().to_owned(),
        _ => bail!("provide exactly one of --answer and --answer-file"),
    };
    let mut record = MemoryRecord {
        schema_version: 1,
        event_id: String::new(),
        created_unix_secs: now_unix_secs,
        query_type: args.query_type.clone(),
        question: args.question.clone(),
        answer,
        contributor: "graf".into(),
        outcome: args.outcome,
        correction: args.correction.clone(),
        source_nodes: args.nodes.clone(),
        graph_identity: graph.and_then(graph_identity),
        graph_generation: graph.map(|g| g.generation),
        evidence: Vec::new(),
    };
    validate(&record)?;
    record.source_nodes.sort();
    record.source_nodes.dedup();
    if !record.source_nodes.is_empty() {
        let graph = graph.context("--nodes requires a graph snapshot for exact ID validation")?;
        let mut source_budget = MAX_CORPUS_BYTES;
        let mut hashes = BTreeMap::new();
        for id in &record.source_nodes {
            let mut matches = graph.nodes.iter().filter(|node| node.id == *id);
            let node = matches
                .next()
                .with_context(|| format!("unknown node ID: {id}"))?;
            ensure!(matches.next().is_none(), "ambiguous node ID: {id}");
            record.evidence.push(SourceEvidence {
                node_id: id.clone(),
                node_hash: hash(&serde_json::to_vec(node)?),
                file: node.file.clone(),
                source_hash: hashes
                    .entry(node.file.clone())
                    .or_insert_with(|| source_hash(graph, &node.file, &mut source_budget))
                    .clone(),
                indexed_source_hash: indexed_source_hash(graph, &node.file).map(str::to_owned),
            });
        }
    }
    fs::create_dir_all(&args.memory_dir)?;
    // The tempfile name is OS-random and persist_noclobber never overwrites an event.
    let mut file = tempfile::Builder::new()
        .prefix(".event-")
        .rand_bytes(16)
        .tempfile_in(&args.memory_dir)?;
    record.event_id = file
        .path()
        .file_name()
        .context("missing event filename")?
        .to_string_lossy()
        .trim_start_matches('.')
        .to_owned();
    let text = format!("---\n{}---\n", serde_yaml_ng::to_string(&record)?);
    ensure!(
        text.len() <= MAX_RECORD_BYTES,
        "memory record exceeds size limit"
    );
    file.write_all(text.as_bytes())?;
    file.as_file().sync_all()?;
    let path = args
        .memory_dir
        .join(format!("query_{}_{}.md", now_unix_secs, record.event_id));
    file.persist_noclobber(&path).map_err(|e| e.error)?;
    sync_dir(&args.memory_dir)?;
    Ok(path)
}

fn validate(record: &MemoryRecord) -> Result<()> {
    ensure!(record.schema_version == 1, "unsupported memory schema");
    for (name, text, max) in [
        ("question", record.question.as_str(), 16 * 1024),
        ("answer", record.answer.as_str(), MAX_TEXT_BYTES),
        ("type", record.query_type.as_str(), 128),
    ] {
        ensure!(
            !text.trim().is_empty() && text.len() <= max,
            "{name} is empty or exceeds {max} bytes"
        );
    }
    ensure!(
        record.source_nodes.len() <= MAX_NODES,
        "at most {MAX_NODES} source nodes are allowed"
    );
    ensure!(
        record
            .source_nodes
            .iter()
            .all(|n| !n.trim().is_empty() && n.len() <= 4096),
        "invalid node ID length"
    );
    if let Some(text) = &record.correction {
        ensure!(
            !text.trim().is_empty() && text.len() <= MAX_TEXT_BYTES,
            "correction is empty or too large"
        );
        ensure!(
            record.outcome == Some(Outcome::Corrected),
            "--correction requires --outcome corrected"
        );
    }
    // Old Graphify corrected records may omit the actual correction; preserve that fact.
    Ok(())
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn graph_identity(graph: &GraphSnapshot) -> Option<String> {
    graph
        .root
        .as_ref()
        .map(|root| hash(format!("{}\0{root}", graph.kind).as_bytes()))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn indexed_source_hash<'a>(graph: &'a GraphSnapshot, file: &str) -> Option<&'a str> {
    // Reserved native metadata comes from index stamps. Exact relative paths only;
    // accepting a basename/suffix match would attach another file's proof.
    // Caller-supplied snapshots remain trusted local input, not signed attestations.
    if graph.kind != "native"
        || file.contains(['\\', ':'])
        || file.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return None;
    }
    let proof = graph.metadata.get("graf_source_digests")?;
    if proof.get("algorithm")?.as_str()? != "blake3" {
        return None;
    }
    let digest = proof.get("files")?.as_object()?.get(file)?.as_str()?;
    valid_digest(digest).then_some(digest)
}

fn source_hash(graph: &GraphSnapshot, file: &str, budget: &mut usize) -> Option<String> {
    let root = fs::canonicalize(graph.root.as_ref()?).ok()?;
    let path = Path::new(file);
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    let resolved = fs::canonicalize(&candidate).ok()?;
    if !resolved.starts_with(&root) {
        return None;
    }
    let bytes = read_bytes(&resolved, MAX_RECORD_BYTES.min(*budget)).ok()?;
    *budget -= bytes.len();
    Some(hash(&bytes))
}

fn read_bytes(path: &Path, cap: usize) -> Result<Vec<u8>> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "expected a regular file: {}",
        path.display()
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= cap as u64,
        "file exceeds {cap} bytes or is not regular: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= cap, "file exceeds {cap} bytes");
    Ok(bytes)
}

fn read_text(path: &Path, cap: usize) -> Result<String> {
    String::from_utf8(read_bytes(path, cap)?).context("memory input must be UTF-8")
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The end of the current UTC day anchors decay for stable same-day reruns.
/// This includes today's saves. Use reflect_at for a precise reproducible clock.
pub fn reflect(args: &ReflectArgs, graph: Option<&GraphSnapshot>) -> Result<ReflectionResult> {
    reflect_at(args, graph, now()? / DAY * DAY + DAY - 1)
}

pub fn reflect_at(
    args: &ReflectArgs,
    graph: Option<&GraphSnapshot>,
    now_unix_secs: u64,
) -> Result<ReflectionResult> {
    let (overlay, records) = project(args, graph, now_unix_secs)?;
    let text = overlay.lessons;
    let parent = args
        .out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(&args.out) {
        ensure!(
            metadata.file_type().is_file(),
            "reflection output must be a regular file"
        );
        let old = read_text(&args.out, MAX_CORPUS_BYTES)?;
        ensure!(
            !old.starts_with("---\n") && !old.starts_with("---\r\n"),
            "refusing to overwrite a memory/frontmatter document"
        );
        ensure!(
            old.starts_with("# Graf lessons\n"),
            "refusing to overwrite a file not generated by graf reflect"
        );
        if args.if_stale && old == text {
            return Ok(ReflectionResult {
                out: args.out.clone(),
                skipped: true,
                records,
            });
        }
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(text.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(&args.out).map_err(|e| e.error)?;
    sync_dir(parent)?;
    Ok(ReflectionResult {
        out: args.out.clone(),
        skipped: false,
        records,
    })
}

/// Explicit, read-only memory projection. Uses memory_dir, half_life_days and
/// min_corroboration; out and if_stale are ignored. No sidecars or directories
/// are written. The ordinary clock uses the same daily anchor as reflect.
pub fn learning_overlay(
    args: &ReflectArgs,
    graph: Option<&GraphSnapshot>,
) -> Result<LearningOverlay> {
    learning_overlay_at(args, graph, now()? / DAY * DAY + DAY - 1)
}

pub fn learning_overlay_at(
    args: &ReflectArgs,
    graph: Option<&GraphSnapshot>,
    now_unix_secs: u64,
) -> Result<LearningOverlay> {
    Ok(project(args, graph, now_unix_secs)?.0)
}

fn project(
    args: &ReflectArgs,
    graph: Option<&GraphSnapshot>,
    now_unix_secs: u64,
) -> Result<(LearningOverlay, usize)> {
    ensure!(
        args.half_life_days.is_finite() && args.half_life_days >= 0.0,
        "half-life-days must be finite and nonnegative"
    );
    ensure!(
        (1..=MAX_RECORDS).contains(&args.min_corroboration),
        "min-corroboration must be between 1 and {MAX_RECORDS}"
    );
    let records = load_records(&args.memory_dir)?;
    Ok((
        aggregate(&records, graph, args, now_unix_secs)?,
        records.len(),
    ))
}

/// Reads only immediate .md records. Copies of the same event never corroborate it.
pub fn load_records(directory: &Path) -> Result<Vec<MemoryRecord>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for (i, entry) in fs::read_dir(directory)?.enumerate() {
        ensure!(i < MAX_RECORDS * 4, "memory directory has too many entries");
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            paths.push(path);
            ensure!(paths.len() <= MAX_RECORDS, "too many memory documents");
        }
    }
    paths.sort();
    let mut records = BTreeMap::new();
    let mut bytes = 0;
    for path in paths {
        let text = read_text(&path, MAX_RECORD_BYTES)
            .with_context(|| format!("reading {}", path.display()))?;
        bytes += text.len();
        ensure!(
            bytes <= MAX_CORPUS_BYTES,
            "memory corpus exceeds {MAX_CORPUS_BYTES} bytes"
        );
        let Some(record) =
            parse_record(&text).with_context(|| format!("parsing {}", path.display()))?
        else {
            continue;
        };
        validate(&record)?;
        if let Some(previous) = records.insert(record.event_id.clone(), record.clone()) {
            ensure!(
                serde_json::to_vec(&previous)? == serde_json::to_vec(&record)?,
                "conflicting copies of memory event {}",
                record.event_id
            );
        }
    }
    let mut result: Vec<MemoryRecord> = records.into_values().collect();
    result.sort_by(|a, b| {
        (a.created_unix_secs, &a.event_id).cmp(&(b.created_unix_secs, &b.event_id))
    });
    Ok(result)
}

fn parse_record(text: &str) -> Result<Option<MemoryRecord>> {
    let text = text.replace("\r\n", "\n");
    let Some(rest) = text.strip_prefix("---\n") else {
        return Ok(None);
    };
    let Some((header, body)) = rest.split_once("\n---\n") else {
        bail!("unterminated memory frontmatter")
    };
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(header)?;
    let contributor = value
        .get("contributor")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if contributor == "graf" {
        let record: MemoryRecord = serde_yaml_ng::from_value(value)?;
        ensure!(
            !record.event_id.is_empty() && record.event_id.len() <= 256,
            "invalid event ID"
        );
        return Ok(Some(record));
    }
    if contributor != "graphify" {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct Legacy {
        #[serde(rename = "type")]
        query_type: String,
        date: String,
        question: String,
        outcome: Option<Outcome>,
        correction: Option<String>,
        #[serde(default)]
        source_nodes: Vec<String>,
    }
    let legacy: Legacy = serde_yaml_ng::from_value(value)?;
    let answer = body
        .split_once("\n## Answer\n")
        .context("Graphify memory has no Answer section")?
        .1;
    let answer = answer.split("\n## Outcome\n").next().unwrap_or(answer);
    let answer = answer
        .split("\n## Source Nodes\n")
        .next()
        .unwrap_or(answer)
        .trim()
        .to_owned();
    Ok(Some(MemoryRecord {
        schema_version: 1,
        event_id: format!("graphify-{}", hash(text.as_bytes())),
        created_unix_secs: parse_utc(&legacy.date)?,
        query_type: legacy.query_type,
        question: legacy.question,
        answer,
        contributor: "graphify".into(),
        outcome: legacy.outcome,
        correction: legacy.correction,
        source_nodes: legacy.source_nodes,
        graph_identity: None,
        graph_generation: None,
        evidence: Vec::new(),
    }))
}

// Graphify's writer uses UTC ISO-8601. Reject other formats instead of inventing dates.
fn parse_utc(date: &str) -> Result<u64> {
    let date = date
        .strip_suffix("+00:00")
        .or_else(|| date.strip_suffix('Z'))
        .context("Graphify memory date must be UTC ISO-8601")?;
    let (whole, fraction) = date.split_once('.').map_or((date, ""), |(a, b)| (a, b));
    ensure!(
        whole.len() == 19 && whole.is_ascii() && fraction.bytes().all(|c| c.is_ascii_digit()),
        "invalid UTC date"
    );
    ensure!(
        &whole[4..5] == "-"
            && &whole[7..8] == "-"
            && &whole[10..11] == "T"
            && &whole[13..14] == ":"
            && &whole[16..17] == ":",
        "invalid UTC date"
    );
    let year: u64 = whole[0..4].parse()?;
    let month: usize = whole[5..7].parse()?;
    let day: u64 = whole[8..10].parse()?;
    let hour: u64 = whole[11..13].parse()?;
    let minute: u64 = whole[14..16].parse()?;
    let second: u64 = whole[17..19].parse()?;
    let leap = |y: u64| y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let months = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    ensure!(
        (1970..=9999).contains(&year)
            && (1..=12).contains(&month)
            && day >= 1
            && day <= months[month - 1]
            && hour < 24
            && minute < 60
            && second < 60,
        "invalid UTC date"
    );
    let days: u64 = (1970..year)
        .map(|y| if leap(y) { 366 } else { 365 })
        .sum::<u64>()
        + months[..month - 1].iter().sum::<u64>()
        + day
        - 1;
    Ok(days * DAY + hour * 3600 + minute * 60 + second)
}

#[derive(Default)]
struct Signals {
    positive: usize,
    negative: usize,
    verified_useful: usize,
    unverified: usize,
    stale: usize,
    future: usize,
    corrected: usize,
    score: f64,
    events: Vec<String>,
    community: Option<String>,
    reasons: BTreeMap<String, usize>,
}

fn source_status(
    record: &MemoryRecord,
    id: &str,
    graph: Option<&GraphSnapshot>,
    nodes: &BTreeMap<&str, Vec<&Node>>,
    hashes: &mut BTreeMap<String, Option<String>>,
    source_budget: &mut usize,
) -> Result<(&'static str, Option<String>)> {
    let evidence = record.evidence.iter().find(|e| e.node_id == id);
    let saved_disk = evidence
        .and_then(|e| e.source_hash.as_deref())
        .filter(|s| valid_digest(s));
    let saved_index = evidence
        .and_then(|e| e.indexed_source_hash.as_deref())
        .filter(|s| valid_digest(s));
    // This contrary evidence survives missing current proof, missing graphs and
    // a later restoration of the original indexed bytes. Never rehabilitate it.
    if let (Some(disk), Some(indexed)) = (saved_disk, saved_index)
        && disk != indexed
    {
        return Ok((
            "stale (source differed from indexed bytes when saved)",
            None,
        ));
    }
    let Some(graph) = graph else {
        return Ok(("unverified (no graph supplied)", None));
    };
    let Some(matching) = nodes.get(id) else {
        return Ok(("stale (node missing)", None));
    };
    if matching.len() != 1 {
        return Ok(("stale (ambiguous node ID)", None));
    }
    let node = matching[0];
    let community = node.metadata.get("community").and_then(|v| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    });
    let Some(evidence) = evidence else {
        return Ok(("unverified (no saved source fingerprint)", community));
    };
    if record.graph_identity.is_some() && record.graph_identity != graph_identity(graph) {
        return Ok(("stale (graph identity changed)", community));
    }
    if evidence.file != node.file {
        return Ok(("stale (source path changed)", community));
    }
    if evidence.node_hash != hash(&serde_json::to_vec(node)?) {
        return Ok(("stale (node changed)", community));
    }
    let current_index = indexed_source_hash(graph, &node.file);
    if let (Some(saved), Some(current)) = (saved_index, current_index)
        && saved != current
    {
        return Ok(("stale (indexed source changed)", community));
    }
    let current_hash = hashes
        .entry(node.file.clone())
        .or_insert_with(|| source_hash(graph, &node.file, source_budget));
    let current_disk = current_hash.as_deref().filter(|s| valid_digest(s));
    if let (Some(saved), Some(current)) = (saved_disk, current_disk)
        && saved != current
    {
        return Ok(("stale (source changed)", community));
    }
    if saved_disk.is_some() && current_disk.is_none() {
        return Ok(("stale (source unavailable)", community));
    }
    if saved_disk.is_none() {
        return Ok(("unverified (source fingerprint unavailable)", community));
    }
    if record.graph_identity.is_none() {
        return Ok(("unverified (graph identity unavailable)", community));
    }
    // Only the original proof can establish save-time correspondence. Current
    // metadata must never backfill a missing field in an older event.
    if let (Some(disk), Some(indexed), Some(current_index), Some(current_disk)) =
        (saved_disk, saved_index, current_index, current_disk)
        && disk == indexed
        && indexed == current_index
        && current_index == current_disk
    {
        return Ok(("current", community));
    }
    Ok((
        "unverified (unchanged since save; snapshot/source correspondence unknown)",
        community,
    ))
}

fn literal(text: &str) -> String {
    // Indented blocks keep arbitrary Markdown/HTML in saved answers inert in lessons.
    text.lines().map(|line| format!("    {line}\n")).collect()
}

fn rounded_score(score: f64) -> f64 {
    let score = (score * 1e9).round() / 1e9;
    if score == 0.0 { 0.0 } else { score }
}

fn aggregate(
    records: &[MemoryRecord],
    graph: Option<&GraphSnapshot>,
    args: &ReflectArgs,
    now: u64,
) -> Result<LearningOverlay> {
    let mut nodes: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
    let mut labels: BTreeMap<&str, Vec<&Node>> = BTreeMap::new();
    if let Some(graph) = graph {
        for node in &graph.nodes {
            nodes.entry(&node.id).or_default().push(node);
            labels.entry(&node.label).or_default().push(node);
        }
    }
    let mut source_budget = MAX_CORPUS_BYTES;
    let mut hashes = BTreeMap::new();
    let mut signals: BTreeMap<String, Signals> = BTreeMap::new();
    let mut status_lines = String::new();
    let mut verified: BTreeMap<String, (&str, Option<String>)> = BTreeMap::new();
    for record in records {
        let mut seen = BTreeSet::new();
        for citation in &record.source_nodes {
            // Graphify historically saved labels. Resolve only a unique exact label,
            // and only for imported records; native saves always require exact IDs.
            let id = if record.contributor == "graphify" && !nodes.contains_key(citation.as_str()) {
                labels
                    .get(citation.as_str())
                    .filter(|v| v.len() == 1)
                    .map(|v| &v[0].id)
                    .unwrap_or(citation)
            } else {
                citation
            };
            if !seen.insert(id) {
                continue;
            }
            // Repeated events against the same saved source share one verification read.
            let key = serde_json::to_string(&(
                id,
                &record.graph_identity,
                record.evidence.iter().find(|e| e.node_id == *id),
            ))?;
            let (status, community) = if let Some(value) = verified.get(&key) {
                value.clone()
            } else {
                let value =
                    source_status(record, id, graph, &nodes, &mut hashes, &mut source_budget)?;
                verified.insert(key, value.clone());
                value
            };
            status_lines.push_str(&literal(&format!(
                "{} | {} | {} | cited: {}",
                record.event_id, id, status, citation
            )));
            let entry = signals.entry(id.clone()).or_default();
            entry.community = community;
            *entry.reasons.entry(status.into()).or_default() += 1;
            entry.events.push(record.event_id.clone());
            if status.starts_with("stale") {
                entry.stale += 1;
                continue;
            }
            if record.created_unix_secs > now {
                entry.future += 1;
                continue;
            }
            if record.outcome.is_none() {
                continue;
            }
            if status.starts_with("unverified") {
                entry.unverified += 1;
            }
            let positive = record.outcome == Some(Outcome::Useful);
            if positive {
                entry.positive += 1;
                if status == "current" {
                    entry.verified_useful += 1;
                }
            } else {
                entry.negative += 1;
                if record.outcome == Some(Outcome::Corrected) {
                    entry.corrected += 1;
                }
            }
            let days = now.saturating_sub(record.created_unix_secs) as f64 / DAY as f64;
            let weight = if args.half_life_days == 0.0 {
                1.0
            } else {
                2f64.powf(-days / args.half_life_days)
            };
            entry.score += if positive { weight } else { -weight };
        }
    }
    let nodes: BTreeMap<String, LearningNode> = signals
        .iter()
        .map(|(id, signal)| {
            let status = if signal.positive > 0 && signal.negative > 0 {
                "contested"
            } else if signal.verified_useful >= args.min_corroboration {
                "preferred"
            } else if signal.positive > 0 {
                "tentative"
            } else if signal.corrected > 0 {
                "corrected"
            } else if signal.negative > 0 {
                "dead_end"
            } else if signal.stale > 0 {
                "stale"
            } else {
                "unmarked"
            };
            let reasons = signal
                .reasons
                .iter()
                .map(|(reason, count)| format!("{count}x {reason}"))
                .collect::<Vec<_>>()
                .join("; ");
            let reason = format!(
                "{reasons}; excluded stale={}; excluded future={}",
                signal.stale, signal.future
            );
            (
                id.clone(),
                LearningNode {
                    status: status.into(),
                    score: rounded_score(signal.score),
                    useful: signal.positive,
                    negative: signal.negative,
                    verified_useful: signal.verified_useful,
                    unverified: signal.unverified,
                    reason,
                },
            )
        })
        .collect();
    let lessons = render_lessons(records, &nodes, &signals, &status_lines, args, now)?;
    let snapshot_hash = graph
        .map(|graph| -> Result<String> {
            let mut hasher = blake3::Hasher::new();
            serde_json::to_writer(&mut hasher, graph)?;
            Ok(hasher.finalize().to_hex().to_string())
        })
        .transpose()?;
    Ok(LearningOverlay {
        schema_version: 1,
        generated_unix_secs: now,
        snapshot_hash,
        nodes,
        lessons,
    })
}

fn render_lessons(
    records: &[MemoryRecord],
    nodes: &BTreeMap<String, LearningNode>,
    signals: &BTreeMap<String, Signals>,
    status_lines: &str,
    args: &ReflectArgs,
    now: u64,
) -> Result<String> {
    let mut out = format!(
        "# Graf lessons\n\nExplicit local observations; verify against current source. No graph writes.\n\nClock (Unix seconds): {now}; records: {}; half-life days: {}; minimum corroboration: {}.\n\n",
        records.len(),
        args.half_life_days,
        args.min_corroboration
    );
    out.push_str("Preferred requires corroborating useful events whose saved and current source bytes match their native indexed digests. Unverified events neither supply nor veto that corroboration. Current means the cited node and owning source match the observed index proof; it does not prove an answer true or the whole checkout current.\n\n");
    let mut ranked: Vec<_> = nodes.iter().collect();
    ranked.sort_by(|(id_a, a), (id_b, b)| {
        rounded_score(b.score)
            .total_cmp(&rounded_score(a.score))
            .then_with(|| id_a.cmp(id_b))
    });
    for category in ["preferred", "tentative", "contested"] {
        out.push_str(&format!("## {category}\n\n"));
        for &(id, signal) in &ranked {
            if signal.status != category {
                continue;
            }
            let score = signal.score;
            let provenance = &signals[id];
            let verification = if signal.unverified == 0 {
                "current"
            } else if provenance.reasons.contains_key("current") {
                "mixed current and unverified observations (see counts and reasons)"
            } else {
                "unverified (snapshot/source correspondence unproven)"
            };
            out.push_str(&literal(&format!(
                "{id} | useful={} | negative={} | verified_useful={} | unverified={} | score={score:.9} | community={} | verification={}\nreason: {}\nevents: {}",
                signal.useful,
                signal.negative,
                signal.verified_useful,
                signal.unverified,
                provenance.community.as_deref().unwrap_or("unknown"),
                verification,
                signal.reason,
                provenance.events.join(", ")
            )));
            out.push('\n');
        }
    }
    for (category, outcome) in [
        ("dead_end", Outcome::DeadEnd),
        ("corrected", Outcome::Corrected),
    ] {
        out.push_str(&format!("## {category}\n\n"));
        for record in records.iter().filter(|r| r.outcome == Some(outcome)) {
            out.push_str(&literal(&format!(
                "{} | at {}\nQuestion: {}\nOriginal answer: {}\nCorrection: {}",
                record.event_id,
                record.created_unix_secs,
                record.question,
                record.answer,
                record.correction.as_deref().unwrap_or("not supplied")
            )));
            out.push('\n');
        }
    }
    out.push_str("## Source verification\n\nStale evidence is excluded from scores and corroboration. Unchanged disk bytes alone do not establish that they produced the snapshot node. Only verified useful events supply preferred corroboration. Future-dated events are excluded from scores.\n\n");
    out.push_str(status_lines);
    out.push_str("\n## Event provenance\n\n");
    for record in records {
        out.push_str(&literal(&format!(
            "{} | {} | at {} | graph generation: {:?} | type: {} | outcome: {}\nQuestion: {}\nAnswer: {}",
            record.event_id,
            record.contributor,
            record.created_unix_secs,
            record.graph_generation,
            record.query_type,
            serde_json::to_string(&record.outcome)?,
            record.question,
            record.answer
        )));
        out.push('\n');
    }
    ensure!(
        out.len() <= MAX_CORPUS_BYTES,
        "reflection output exceeds size limit"
    );
    Ok(out)
}
