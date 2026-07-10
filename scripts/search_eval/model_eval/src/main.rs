use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use fastembed::{
    EmbeddingModel, RerankInitOptions, RerankerModel, TextEmbedding, TextInitOptions, TextRerank,
};
use rusqlite::{params, Connection};
use serde::Serialize;

const DEFAULT_QUERIES: &[&str] = &[
    "schema 25 provider import command code rovo dev cortex code",
    "semantic search daemon background worker stale locks",
    "obliscence subagents hooks history disk",
    "incremental refresh semantic indexing cold warm daemon",
    "ctx private cli surface principles daemon status doctor",
    "fastembed all MiniLM bge small embedding model",
    "provider event conflict codex history jsonl import",
    "buildkite public cli install scripts",
    "cold emailing agent history broad smykm",
    "source identity filters semantic fallback",
    "semantic payload chunking 1200 200",
    "feature branch dogfood real corpus performance testing",
];
const E5_QUERY_PREFIX: &str = "query: ";
const E5_PASSAGE_PREFIX: &str = "passage: ";
const E5_DIMENSIONS: usize = 384;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long)]
    data_root: Option<PathBuf>,
    #[arg(long)]
    work_db: Option<PathBuf>,
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "e5-small")]
    model: ModelArg,
    #[arg(long, default_value_t = 2)]
    threads: usize,
    #[arg(long, default_value_t = 64)]
    batch_size: usize,
    #[arg(long, default_value_t = 1_600)]
    sample_limit: usize,
    #[arg(long, default_value_t = 120)]
    refs_per_query: usize,
    #[arg(long, default_value_t = 1_200)]
    text_chars: usize,
    #[arg(long, default_value_t = 15)]
    top_k: usize,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    include_snippets: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModelArg {
    E5Small,
    MiniLm,
    BgeSmall,
    SnowflakeXs,
    JinaBaseEn,
    BgeReranker,
}

impl ModelArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::E5Small => "intfloat/multilingual-e5-small",
            Self::MiniLm => "minilm",
            Self::BgeSmall => "bge-small",
            Self::SnowflakeXs => "snowflake-xs",
            Self::JinaBaseEn => "jina-base-en",
            Self::BgeReranker => "bge-reranker",
        }
    }

    fn embedding_model(self) -> Option<EmbeddingModel> {
        match self {
            Self::E5Small => Some(EmbeddingModel::MultilingualE5Small),
            Self::MiniLm => Some(EmbeddingModel::AllMiniLML6V2),
            Self::BgeSmall => Some(EmbeddingModel::BGESmallENV15),
            Self::SnowflakeXs => Some(EmbeddingModel::SnowflakeArcticEmbedXS),
            Self::JinaBaseEn => Some(EmbeddingModel::JinaEmbeddingsV2BaseEN),
            Self::BgeReranker => None,
        }
    }

    fn document_text(self, text: &str) -> String {
        match self {
            Self::E5Small => prefixed_text(E5_PASSAGE_PREFIX, text),
            _ => text.to_owned(),
        }
    }

    fn query_text(self, text: &str) -> String {
        match self {
            Self::E5Small => prefixed_text(E5_QUERY_PREFIX, text),
            _ => text.to_owned(),
        }
    }
}

fn prefixed_text(prefix: &str, text: &str) -> String {
    let text = text.trim_start();
    if text.starts_with(prefix) {
        text.to_owned()
    } else {
        format!("{prefix}{text}")
    }
}

#[derive(Debug, Clone)]
struct Doc {
    event_id: String,
    seq: i64,
    text: String,
}

#[derive(Debug, Clone)]
struct QueryRefs {
    query: String,
    refs: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Report {
    model: String,
    kind: String,
    work_db: String,
    cache_dir: String,
    docs: usize,
    queries: usize,
    dimensions: Option<usize>,
    threads: usize,
    batch_size: usize,
    text_chars: usize,
    model_init_ms: u128,
    embed_docs_ms: Option<u128>,
    query_eval_ms: Option<u128>,
    rerank_ms: Option<u128>,
    docs_per_second: Option<f64>,
    query_embed_ms_avg: Option<f64>,
    recall_at_5_avg: f64,
    recall_at_10_avg: f64,
    mrr_avg: f64,
    lexical_overlap_at_10_avg: f64,
    cache_bytes: u64,
    process_peak_rss_kb: Option<u64>,
    per_query: Vec<QueryReport>,
}

#[derive(Debug, Serialize)]
struct QueryReport {
    query: String,
    ref_count: usize,
    recall_at_5: f64,
    recall_at_10: f64,
    mrr: f64,
    lexical_overlap_at_10: f64,
    query_embed_ms: Option<u128>,
    top_event_ids: Vec<String>,
    top_scores: Vec<f32>,
    top_snippets: Option<Vec<String>>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let data_root = args.data_root.clone().unwrap_or_else(default_data_root);
    let work_db = args
        .work_db
        .clone()
        .unwrap_or_else(|| data_root.join("work.sqlite"));
    let cache_dir = args
        .cache_dir
        .clone()
        .unwrap_or_else(|| data_root.join("model-eval-cache"));
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create cache dir {}", cache_dir.display()))?;

    let conn = Connection::open_with_flags(
        &work_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open work db {}", work_db.display()))?;
    let query_refs = query_refs(&conn, args.refs_per_query)?;
    eprintln!(
        "loaded {} query reference sets with {} total refs",
        query_refs.len(),
        query_refs.iter().map(|refs| refs.refs.len()).sum::<usize>()
    );
    let docs = candidate_docs(&conn, &query_refs, args.sample_limit, args.text_chars)?;
    eprintln!("loaded {} candidate docs", docs.len());

    let report = match args.model {
        ModelArg::BgeReranker => run_reranker(&args, &cache_dir, &work_db, &docs, &query_refs)?,
        _ => run_embedding_model(&args, &cache_dir, &work_db, &docs, &query_refs)?,
    };

    let json = serde_json::to_string_pretty(&report)?;
    if let Some(path) = args.output {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create output dir {}", parent.display()))?;
        }
        fs::write(&path, json.as_bytes()).with_context(|| format!("write {}", path.display()))?;
    }
    println!("{json}");
    Ok(())
}

fn default_data_root() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ctx-semantic-dev")
}

fn run_embedding_model(
    args: &Args,
    cache_dir: &Path,
    work_db: &Path,
    docs: &[Doc],
    query_refs: &[QueryRefs],
) -> Result<Report> {
    let embedding_model = args
        .model
        .embedding_model()
        .ok_or_else(|| anyhow!("not an embedding model"))?;
    let model_started = Instant::now();
    eprintln!("initializing embedding model {}", args.model.as_str());
    let mut model = TextEmbedding::try_new(
        TextInitOptions::new(embedding_model)
            .with_show_download_progress(false)
            .with_intra_threads(args.threads)
            .with_cache_dir(cache_dir.to_path_buf()),
    )
    .with_context(|| format!("initialize {}", args.model.as_str()))?;
    let model_init_ms = model_started.elapsed().as_millis();

    let doc_texts = docs
        .iter()
        .map(|doc| args.model.document_text(&doc.text))
        .collect::<Vec<_>>();
    let embed_started = Instant::now();
    eprintln!("embedding {} docs with {}", docs.len(), args.model.as_str());
    let mut doc_embeddings = model
        .embed(doc_texts, Some(args.batch_size))
        .with_context(|| format!("embed docs with {}", args.model.as_str()))?;
    for embedding in &mut doc_embeddings {
        normalize(embedding);
    }
    let embed_docs_ms = embed_started.elapsed().as_millis();
    let dimensions = doc_embeddings.first().map(Vec::len);
    if args.model == ModelArg::E5Small && dimensions != Some(E5_DIMENSIONS) {
        return Err(anyhow!(
            "{} returned {:?} dimensions, expected {}",
            args.model.as_str(),
            dimensions,
            E5_DIMENSIONS
        ));
    }

    let doc_index = docs
        .iter()
        .enumerate()
        .map(|(index, doc)| (doc.event_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let lexical_top10 = query_refs
        .iter()
        .map(|refs| {
            refs.refs
                .iter()
                .filter_map(|id| doc_index.get(id.as_str()).copied())
                .take(10)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let query_started = Instant::now();
    eprintln!("embedding/evaluating {} queries", query_refs.len());
    let mut per_query = Vec::new();
    let mut query_embed_ms_total = 0_u128;
    for (query_index, refs) in query_refs.iter().enumerate() {
        let query_embed_started = Instant::now();
        let mut query_embedding = model
            .embed(vec![args.model.query_text(&refs.query)], Some(1))
            .with_context(|| format!("embed query with {}", args.model.as_str()))?
            .pop()
            .ok_or_else(|| anyhow!("empty query embedding"))?;
        normalize(&mut query_embedding);
        let query_embed_ms = query_embed_started.elapsed().as_millis();
        query_embed_ms_total += query_embed_ms;

        let mut scored = doc_embeddings
            .iter()
            .enumerate()
            .map(|(index, embedding)| (index, dot(&query_embedding, embedding)))
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| score_order(left.1, right.1));
        let top = scored.into_iter().take(args.top_k).collect::<Vec<_>>();
        per_query.push(query_report(
            refs,
            &top,
            docs,
            &lexical_top10[query_index],
            args.include_snippets,
            Some(query_embed_ms),
        ));
    }
    let query_eval_ms = query_started.elapsed().as_millis();

    Ok(finish_report(
        args,
        "embedding",
        work_db,
        cache_dir,
        docs.len(),
        query_refs.len(),
        dimensions,
        model_init_ms,
        Some(embed_docs_ms),
        Some(query_eval_ms),
        None,
        if embed_docs_ms > 0 {
            Some((docs.len() as f64) / (embed_docs_ms as f64 / 1000.0))
        } else {
            None
        },
        if query_refs.is_empty() {
            None
        } else {
            Some(query_embed_ms_total as f64 / query_refs.len() as f64)
        },
        per_query,
    ))
}

fn run_reranker(
    args: &Args,
    cache_dir: &Path,
    work_db: &Path,
    docs: &[Doc],
    query_refs: &[QueryRefs],
) -> Result<Report> {
    let doc_index = docs
        .iter()
        .enumerate()
        .map(|(index, doc)| (doc.event_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let lexical_refs = query_refs
        .iter()
        .map(|refs| {
            refs.refs
                .iter()
                .filter_map(|id| doc_index.get(id.as_str()).copied())
                .take(50)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let model_started = Instant::now();
    eprintln!("initializing reranker {}", args.model.as_str());
    let mut reranker = TextRerank::try_new(
        RerankInitOptions::new(RerankerModel::BGERerankerBase)
            .with_show_download_progress(false)
            .with_cache_dir(cache_dir.to_path_buf()),
    )
    .context("initialize BGE reranker")?;
    let model_init_ms = model_started.elapsed().as_millis();

    let rerank_started = Instant::now();
    eprintln!("reranking {} query candidate sets", query_refs.len());
    let mut per_query = Vec::new();
    for (query_index, refs) in query_refs.iter().enumerate() {
        let candidates = &lexical_refs[query_index];
        let candidate_docs = candidates
            .iter()
            .map(|index| docs[*index].text.clone())
            .collect::<Vec<_>>();
        let reranked = reranker
            .rerank(
                refs.query.clone(),
                candidate_docs,
                false,
                Some(args.batch_size),
            )
            .with_context(|| format!("rerank query {}", refs.query))?;
        let top = reranked
            .iter()
            .take(args.top_k)
            .filter_map(|result| {
                candidates
                    .get(result.index)
                    .map(|doc_index| (*doc_index, result.score as f32))
            })
            .collect::<Vec<_>>();
        per_query.push(query_report(
            refs,
            &top,
            docs,
            &lexical_refs[query_index]
                .iter()
                .copied()
                .take(10)
                .collect::<Vec<_>>(),
            args.include_snippets,
            None,
        ));
    }
    let rerank_ms = rerank_started.elapsed().as_millis();

    Ok(finish_report(
        args,
        "reranker",
        work_db,
        cache_dir,
        docs.len(),
        query_refs.len(),
        None,
        model_init_ms,
        None,
        None,
        Some(rerank_ms),
        None,
        None,
        per_query,
    ))
}

fn finish_report(
    args: &Args,
    kind: &str,
    work_db: &Path,
    cache_dir: &Path,
    docs: usize,
    queries: usize,
    dimensions: Option<usize>,
    model_init_ms: u128,
    embed_docs_ms: Option<u128>,
    query_eval_ms: Option<u128>,
    rerank_ms: Option<u128>,
    docs_per_second: Option<f64>,
    query_embed_ms_avg: Option<f64>,
    per_query: Vec<QueryReport>,
) -> Report {
    let recall_at_5_avg = avg(per_query.iter().map(|query| query.recall_at_5));
    let recall_at_10_avg = avg(per_query.iter().map(|query| query.recall_at_10));
    let mrr_avg = avg(per_query.iter().map(|query| query.mrr));
    let lexical_overlap_at_10_avg = avg(per_query.iter().map(|query| query.lexical_overlap_at_10));
    Report {
        model: args.model.as_str().to_owned(),
        kind: kind.to_owned(),
        work_db: work_db.display().to_string(),
        cache_dir: cache_dir.display().to_string(),
        docs,
        queries,
        dimensions,
        threads: args.threads,
        batch_size: args.batch_size,
        text_chars: args.text_chars,
        model_init_ms,
        embed_docs_ms,
        query_eval_ms,
        rerank_ms,
        docs_per_second,
        query_embed_ms_avg,
        recall_at_5_avg,
        recall_at_10_avg,
        mrr_avg,
        lexical_overlap_at_10_avg,
        cache_bytes: dir_size(cache_dir).unwrap_or(0),
        process_peak_rss_kb: process_peak_rss_kb(),
        per_query,
    }
}

fn query_report(
    refs: &QueryRefs,
    top: &[(usize, f32)],
    docs: &[Doc],
    lexical_top10: &[usize],
    include_snippets: bool,
    query_embed_ms: Option<u128>,
) -> QueryReport {
    let ref_set = refs.refs.iter().collect::<HashSet<_>>();
    let top_ids = top
        .iter()
        .map(|(index, _score)| docs[*index].event_id.clone())
        .collect::<Vec<_>>();
    let top_scores = top.iter().map(|(_index, score)| *score).collect::<Vec<_>>();
    let recall_at_5 = recall_at(&top_ids, &ref_set, 5);
    let recall_at_10 = recall_at(&top_ids, &ref_set, 10);
    let mrr = reciprocal_rank(&top_ids, &ref_set);
    let lexical_overlap_at_10 = if lexical_top10.is_empty() {
        0.0
    } else {
        let lexical_ids = lexical_top10
            .iter()
            .map(|index| docs[*index].event_id.as_str())
            .collect::<HashSet<_>>();
        let overlap = top_ids
            .iter()
            .take(10)
            .filter(|id| lexical_ids.contains(id.as_str()))
            .count();
        overlap as f64 / lexical_top10.len().min(10) as f64
    };
    let top_snippets = include_snippets.then(|| {
        top.iter()
            .map(|(index, _score)| {
                docs[*index]
                    .text
                    .chars()
                    .take(220)
                    .collect::<String>()
                    .replace('\n', " ")
            })
            .collect()
    });
    QueryReport {
        query: refs.query.clone(),
        ref_count: refs.refs.len(),
        recall_at_5,
        recall_at_10,
        mrr,
        lexical_overlap_at_10,
        query_embed_ms,
        top_event_ids: top_ids,
        top_scores,
        top_snippets,
    }
}

fn query_refs(conn: &Connection, refs_per_query: usize) -> Result<Vec<QueryRefs>> {
    DEFAULT_QUERIES
        .iter()
        .map(|query| {
            let match_expr = fts_and_query(query, 5);
            let mut stmt = conn.prepare(
                "SELECT event_search.event_id
                 FROM event_search
                 JOIN events e ON e.id = event_search.event_id
                 WHERE event_search MATCH ?1
                   AND e.deleted_at_ms IS NULL
                   AND length(trim(event_search.safe_preview_text)) > 0
                 ORDER BY bm25(event_search)
                 LIMIT ?2",
            )?;
            let mut refs = stmt
                .query_map(params![match_expr, refs_per_query as i64], |row| row.get(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            if refs.len() < refs_per_query.min(10) {
                refs = query_refs_or_fallback(conn, query, refs_per_query)?;
            }
            Ok(QueryRefs {
                query: (*query).to_owned(),
                refs,
            })
        })
        .collect()
}

fn query_refs_or_fallback(
    conn: &Connection,
    query: &str,
    refs_per_query: usize,
) -> Result<Vec<String>> {
    let match_expr = fts_or_query(query, 6);
    let mut stmt = conn.prepare(
        "SELECT event_search.event_id
         FROM event_search
         JOIN events e ON e.id = event_search.event_id
         WHERE event_search MATCH ?1
           AND e.deleted_at_ms IS NULL
           AND length(trim(event_search.safe_preview_text)) > 0
         LIMIT ?2",
    )?;
    let refs = stmt
        .query_map(params![match_expr, refs_per_query as i64], |row| row.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(refs)
}

fn candidate_docs(
    conn: &Connection,
    refs: &[QueryRefs],
    sample_limit: usize,
    text_chars: usize,
) -> Result<Vec<Doc>> {
    let mut ids = BTreeSet::new();
    for query in refs {
        for event_id in &query.refs {
            ids.insert(event_id.clone());
        }
    }

    let recent_limit = sample_limit / 2;
    let mut recent_stmt = conn.prepare(
        "SELECT event_search.event_id
         FROM event_search
         JOIN events e ON e.id = event_search.event_id
         WHERE e.deleted_at_ms IS NULL
           AND length(trim(event_search.safe_preview_text)) > 0
         ORDER BY e.seq DESC
         LIMIT ?1",
    )?;
    for id in recent_stmt.query_map(params![recent_limit as i64], |row| row.get::<_, String>(0))? {
        ids.insert(id?);
    }

    let total: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM event_search
         JOIN events e ON e.id = event_search.event_id
         WHERE e.deleted_at_ms IS NULL
           AND length(trim(event_search.safe_preview_text)) > 0",
        [],
        |row| row.get(0),
    )?;
    let step = (total / sample_limit.max(1) as i64).max(1);
    let mut sample_stmt = conn.prepare(
        "SELECT event_search.event_id
         FROM event_search
         JOIN events e ON e.id = event_search.event_id
         WHERE e.deleted_at_ms IS NULL
           AND length(trim(event_search.safe_preview_text)) > 0
           AND (e.seq % ?1) = 0
         ORDER BY e.seq
         LIMIT ?2",
    )?;
    for id in sample_stmt.query_map(params![step, sample_limit as i64], |row| {
        row.get::<_, String>(0)
    })? {
        ids.insert(id?);
    }

    let selected = ids.into_iter().collect::<HashSet<_>>();
    eprintln!("materializing {} selected candidate IDs", selected.len());
    let mut docs = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT event_search.event_id, e.seq, event_search.safe_preview_text
         FROM event_search
         JOIN events e ON e.id = event_search.event_id
         WHERE e.deleted_at_ms IS NULL
           AND length(trim(event_search.safe_preview_text)) > 0",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let event_id: String = row.get(0)?;
        if selected.contains(&event_id) {
            let seq: i64 = row.get(1)?;
            let text: String = row.get(2)?;
            docs.insert(
                seq,
                Doc {
                    event_id,
                    seq,
                    text: truncate_chars(&text, text_chars),
                },
            );
            if docs.len() >= selected.len() {
                break;
            }
        }
    }
    Ok(docs.into_values().collect())
}

fn query_tokens(value: &str) -> Vec<String> {
    let stop = [
        "the", "and", "for", "with", "from", "that", "this", "what", "does", "into", "our", "all",
        "are", "was", "were", "how", "why",
    ];
    let stop = stop.into_iter().collect::<HashSet<_>>();
    let mut tokens = value
        .split(|ch: char| !ch.is_alphanumeric())
        .filter_map(|token| {
            let token = token.trim().to_lowercase();
            if token.len() < 3 || stop.contains(token.as_str()) {
                None
            } else {
                Some(token)
            }
        })
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    tokens.retain(|token| seen.insert(token.clone()));
    tokens
}

fn fts_and_query(value: &str, max_terms: usize) -> String {
    let tokens = query_tokens(value);
    if tokens.is_empty() {
        "\"ctx\"".to_owned()
    } else {
        tokens
            .into_iter()
            .take(max_terms)
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn fts_or_query(value: &str, max_terms: usize) -> String {
    let tokens = query_tokens(value);
    if tokens.is_empty() {
        "\"ctx\"".to_owned()
    } else {
        tokens
            .into_iter()
            .take(max_terms)
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ")
    }
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn normalize(values: &mut [f32]) {
    let norm = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in values {
            *value /= norm;
        }
    }
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right.iter())
        .map(|(left, right)| left * right)
        .sum()
}

fn score_order(left: f32, right: f32) -> Ordering {
    right.partial_cmp(&left).unwrap_or(Ordering::Equal)
}

fn recall_at(top_ids: &[String], refs: &HashSet<&String>, limit: usize) -> f64 {
    if refs.is_empty() {
        return 0.0;
    }
    let hits = top_ids
        .iter()
        .take(limit)
        .filter(|id| refs.contains(id))
        .count();
    hits as f64 / refs.len().min(limit) as f64
}

fn reciprocal_rank(top_ids: &[String], refs: &HashSet<&String>) -> f64 {
    for (index, id) in top_ids.iter().enumerate() {
        if refs.contains(id) {
            return 1.0 / (index + 1) as f64;
        }
    }
    0.0
}

fn avg(values: impl Iterator<Item = f64>) -> f64 {
    let mut count = 0_usize;
    let mut total = 0.0;
    for value in values {
        count += 1;
        total += value;
    }
    if count == 0 {
        0.0
    } else {
        total / count as f64
    }
}

fn dir_size(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    if !path.exists() {
        return Ok(0);
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            total += dir_size(&entry.path())?;
        } else {
            total += meta.len();
        }
    }
    Ok(total)
}

fn process_peak_rss_kb() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_e5_model_is_the_eval_default() {
        let args = Args::try_parse_from(["ctx-model-eval"]).unwrap();

        assert_eq!(args.model, ModelArg::E5Small);
        assert_eq!(E5_DIMENSIONS, 384);
    }

    #[test]
    fn e5_inputs_use_query_and_passage_prefixes_once() {
        assert_eq!(ModelArg::E5Small.query_text("find it"), "query: find it");
        assert_eq!(
            ModelArg::E5Small.query_text("  query: find it"),
            "query: find it"
        );
        assert_eq!(
            ModelArg::E5Small.document_text("found it"),
            "passage: found it"
        );
        assert_eq!(
            ModelArg::E5Small.document_text("  passage: found it"),
            "passage: found it"
        );
    }

    #[test]
    fn comparison_models_keep_unprefixed_inputs() {
        assert_eq!(ModelArg::MiniLm.query_text("find it"), "find it");
        assert_eq!(ModelArg::BgeSmall.document_text("found it"), "found it");
    }
}
