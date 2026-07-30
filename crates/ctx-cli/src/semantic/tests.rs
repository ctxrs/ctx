use super::*;
use sha2::{Digest, Sha256};

fn test_embedding(first: f32, second: f32) -> Vec<f32> {
    let mut embedding = vec![0.0; SEMANTIC_DIMENSIONS];
    let norm = first.mul_add(first, second * second).sqrt();
    if norm > 0.0 {
        embedding[0] = first / norm;
        embedding[1] = second / norm;
    }
    embedding
}

fn test_chunk(event_id: Uuid, seq: u64, source_hash: &str) -> SemanticChunkDocument {
    test_chunk_at(event_id, seq, source_hash, 0, 1)
}

fn test_daemon_run_args() -> DaemonRunArgs {
    DaemonRunArgs {
        foreground: false,
        once: true,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: Some(1),
        max_seconds: Some(1),
        force: false,
        start_mode: Some(DaemonStartModeArg::Manual),
        trigger_command: None,
        format: crate::output::JsonOutputFormat::Json,
    }
}

fn write_semantic_enabled_config(data_root: &Path) -> Result<()> {
    fs::create_dir_all(data_root)?;
    let path = data_root.join(CONFIG_FILE);
    fs::write(
        path,
        "[daemon]\nenabled = true\n\n[search]\nsemantic = true\n",
    )?;
    Ok(())
}

fn daemon_history_completed_test_job() -> Value {
    json!({
        "status": "completed",
        "source_count": 1,
        "last_run_at_ms": utc_now().timestamp_millis(),
    })
}

fn daemon_semantic_indexed_test_job(_data_root: &Path) -> Value {
    daemon_semantic_job_json(
        "budget_exhausted",
        None,
        utc_now().timestamp_millis(),
        Some(1),
        None,
    )
}

fn install_test_daemon_jobs(
    calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    history_refresh: Option<Value>,
    semantic_index: Option<Value>,
) -> DaemonTestJobHookGuard {
    install_daemon_test_job_hooks(DaemonTestJobHooks {
        calls,
        history_refresh,
        relational_projection: None,
        semantic_index,
        relational_blocker: None,
    })
}

fn test_chunk_at(
    event_id: Uuid,
    seq: u64,
    source_hash: &str,
    chunk_index: usize,
    _chunk_count: usize,
) -> SemanticChunkDocument {
    let source_text_hash = if source_hash.len() == 64
        && source_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        source_hash.to_owned()
    } else {
        format!("{:x}", Sha256::digest(source_hash.as_bytes()))
    };
    SemanticChunkDocument {
        event_id,
        seq,
        chunk_index,
        source_text_hash,
        text: String::new(),
        start_char: chunk_index.saturating_mul(10),
        end_char: chunk_index.saturating_mul(10).saturating_add(12),
    }
}

#[cfg(ctx_semantic_fastembed)]
fn write_test_semantic_cache(root: &Path) -> Result<()> {
    write_test_semantic_cache_variant(root, SemanticOrtModelVariant::CpuFp32)
}

#[cfg(ctx_semantic_fastembed)]
fn write_test_semantic_cache_variant(root: &Path, variant: SemanticOrtModelVariant) -> Result<()> {
    let snapshot = root
        .join(SEMANTIC_HF_MODEL_CACHE_DIR)
        .join("snapshots")
        .join(SEMANTIC_MODEL_REVISION);
    fs::create_dir_all(&snapshot)?;
    for file in variant.required_files() {
        let path = snapshot.join(file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::File::create(path)?.set_len(file.size)?;
    }
    Ok(())
}

mod lifecycle;
mod locking;
mod search_daemon;
mod vector_store;
mod workflow;
