use super::*;

pub(super) const SOURCE_STAGE_SCHEMA_VERSION: u32 = 1;
const SOURCE_STAGE_DIRECTORY: &str = "flat_source_stage";
const SOURCE_STAGE_BASELINE_FILE: &str = "baseline.json";
const SOURCE_STAGE_FINAL_FILE: &str = "final.json";
const SOURCE_STAGE_PAGE_PREFIX: &str = "page-";
const SOURCE_STAGE_PAGE_SUFFIX: &str = ".json";
const MAX_SOURCE_STAGE_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceStageBaseline {
    schema_version: u32,
    source: FlatSourceScope,
    publication: FlatPublicationToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceStagePage {
    pub(super) schema_version: u32,
    pub(super) source: FlatSourceScope,
    pub(super) page_sequence: u64,
    pub(super) previous_page_hash: Option<String>,
    pub(super) descriptor: Option<SegmentDescriptor>,
    pub(super) active_events: u64,
    pub(super) active_chunks: u64,
    pub(super) reused_chunks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceStageFinal {
    pub(super) schema_version: u32,
    pub(super) source: FlatSourceScope,
    pub(super) baseline_publication: FlatPublicationToken,
    pub(super) final_page: Option<FlatSourceStagingToken>,
    pub(super) candidate_publication: FlatPublicationToken,
    pub(super) created_unix_millis: u64,
    pub(super) active_events: u64,
    pub(super) active_chunks: u64,
    pub(super) deleted_chunks: u64,
    pub(super) receipt: Option<FlatSourceReceipt>,
    pub(super) catalog: SegmentDescriptor,
}

pub(super) fn source_stage_directory(root: &Path) -> PathBuf {
    root.join(SOURCE_STAGE_DIRECTORY)
}

pub(super) fn ensure_source_stage_directory(root: &Path) -> FlatResult<()> {
    let directory = source_stage_directory(root);
    fs::create_dir_all(&directory)
        .map_err(|source| io_error("create Flat source stage directory", &directory, source))?;
    ensure_real_directory(&directory)
}

pub(super) fn reset_source_stage_directory(directory: &Path) -> FlatResult<()> {
    fs::create_dir_all(directory)
        .map_err(|source| io_error("create Flat source stage directory", directory, source))?;
    ensure_real_directory(directory)?;
    for entry in fs::read_dir(directory)
        .map_err(|source| io_error("read Flat source stage directory", directory, source))?
    {
        let entry =
            entry.map_err(|source| io_error("read Flat source stage entry", directory, source))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| io_error("stat Flat source stage entry", &path, source))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(FlatStoreError::Corrupt(format!(
                "Flat source stage entry {} is not a regular file",
                path.display()
            )));
        }
        fs::remove_file(&path)
            .map_err(|source| io_error("remove Flat source stage entry", &path, source))?;
    }
    sync_directory(directory)
}

pub(super) fn write_stage_baseline(
    directory: &Path,
    source: &FlatSourceScope,
    publication: &FlatPublicationToken,
) -> FlatResult<()> {
    write_stage_json(
        &directory.join(SOURCE_STAGE_BASELINE_FILE),
        &SourceStageBaseline {
            schema_version: SOURCE_STAGE_SCHEMA_VERSION,
            source: source.clone(),
            publication: publication.clone(),
        },
    )
}

pub(super) fn load_source_stage(
    directory: &Path,
    contract: &FlatModelContract,
    source: &FlatSourceScope,
    publication: &FlatPublicationToken,
    expected: &FlatSourceStagingToken,
) -> FlatResult<Vec<SourceStagePage>> {
    let baseline: SourceStageBaseline =
        read_stage_json(&directory.join(SOURCE_STAGE_BASELINE_FILE))?;
    if baseline.schema_version != SOURCE_STAGE_SCHEMA_VERSION
        || &baseline.source != source
        || &baseline.publication != publication
        || expected.source_reconciliation_id != source.source_reconciliation_id
        || expected.page_sequence == 0
        || decode_sha256(&expected.page_hash).is_none()
    {
        return Err(FlatStoreError::Corrupt(
            "Flat source stage baseline or token disagrees".to_owned(),
        ));
    }
    let mut sequence = expected.page_sequence;
    let mut hash = expected.page_hash.clone();
    let mut pages = Vec::new();
    loop {
        let page: SourceStagePage =
            read_stage_json(&directory.join(stage_page_name(sequence, &hash)))?;
        if page.schema_version != SOURCE_STAGE_SCHEMA_VERSION
            || page.source != *source
            || page.page_sequence != sequence
            || stage_page_hash(&page)? != hash
        {
            return Err(FlatStoreError::Corrupt(
                "Flat source stage page chain disagrees".to_owned(),
            ));
        }
        let previous = page.previous_page_hash.clone();
        pages.push(page);
        if sequence == 1 {
            if previous.is_some() {
                return Err(FlatStoreError::Corrupt(
                    "first Flat source stage page has a predecessor".to_owned(),
                ));
            }
            break;
        }
        sequence -= 1;
        hash = previous.ok_or_else(|| {
            FlatStoreError::Corrupt("Flat source stage page chain is truncated".to_owned())
        })?;
    }
    pages.reverse();
    for page in &pages {
        if let Some(descriptor) = &page.descriptor {
            validate_staged_segment_in_directory(directory, contract, descriptor)?;
        }
    }
    Ok(pages)
}

pub(super) fn write_stage_page(directory: &Path, page: &SourceStagePage) -> FlatResult<String> {
    let hash = stage_page_hash(page)?;
    write_stage_json(
        &directory.join(stage_page_name(page.page_sequence, &hash)),
        page,
    )?;
    Ok(hash)
}

pub(super) fn stage_page_hash(page: &SourceStagePage) -> FlatResult<String> {
    Ok(encode_hex(
        Sha256::digest(serde_json::to_vec(page)?).as_slice(),
    ))
}

fn stage_page_name(sequence: u64, hash: &str) -> String {
    format!("{SOURCE_STAGE_PAGE_PREFIX}{sequence:020}-{hash}{SOURCE_STAGE_PAGE_SUFFIX}")
}

fn write_stage_json<T: Serialize>(path: &Path, value: &T) -> FlatResult<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_SOURCE_STAGE_RECORD_BYTES {
        return Err(FlatStoreError::InvalidInput(
            "Flat source stage record has an unsafe size".to_owned(),
        ));
    }
    let directory = path.parent().ok_or_else(|| {
        FlatStoreError::Corrupt("Flat source stage path has no parent".to_owned())
    })?;
    let temporary = unique_temporary_path(directory, "source-stage");
    let mut file = create_new_file(&temporary)?;
    file.write_all(&bytes)
        .map_err(|source| io_error("write Flat source stage record", &temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync Flat source stage record", &temporary, source))?;
    drop(file);
    commit_unique_file(&temporary, path)?;
    sync_directory(directory)
}

fn read_stage_json<T>(path: &Path) -> FlatResult<T>
where
    T: for<'de> Deserialize<'de>,
{
    let metadata = symlink_metadata_file(path)?;
    if metadata.len() == 0 || metadata.len() > MAX_SOURCE_STAGE_RECORD_BYTES {
        return Err(FlatStoreError::Corrupt(format!(
            "Flat source stage record {} has unsafe size",
            path.display()
        )));
    }
    let mut file = File::open(path)
        .map_err(|source| io_error("open Flat source stage record", path, source))?;
    let mut bytes = Vec::with_capacity(usize_from_u64(metadata.len(), "stage record size")?);
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error("read Flat source stage record", path, source))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        FlatStoreError::Corrupt(format!("invalid Flat source stage JSON: {error}"))
    })
}

pub(super) fn read_source_stage_final(directory: &Path) -> FlatResult<Option<SourceStageFinal>> {
    let path = directory.join(SOURCE_STAGE_FINAL_FILE);
    match fs::symlink_metadata(&path) {
        Ok(_) => read_stage_json(&path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error("stat Flat source finalization", &path, source)),
    }
}

pub(super) fn write_source_stage_final(
    directory: &Path,
    finalized: &SourceStageFinal,
) -> FlatResult<()> {
    write_stage_json(&directory.join(SOURCE_STAGE_FINAL_FILE), finalized)
}

#[cfg(test)]
pub(super) fn corrupt_source_stage_candidate_hash(root: &Path) -> FlatResult<()> {
    let directory = source_stage_directory(root);
    let path = directory.join(SOURCE_STAGE_FINAL_FILE);
    let mut finalized = read_source_stage_final(&directory)?.ok_or_else(|| {
        FlatStoreError::InvalidInput("source finalization corruption has no candidate".to_owned())
    })?;
    finalized.candidate_publication.generation_hash = Some("0".repeat(64));
    let bytes = serde_json::to_vec(&finalized)?;
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .map_err(|source| io_error("open source finalization for corruption", &path, source))?;
    file.write_all(&bytes)
        .map_err(|source| io_error("corrupt source finalization", &path, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync corrupt source finalization", &path, source))?;
    sync_directory(&directory)
}
