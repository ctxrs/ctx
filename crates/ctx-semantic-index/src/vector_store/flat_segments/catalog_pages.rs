use super::*;

// A fixed, source-keyed fanout bounds the root and lets unchanged catalog pages
// keep their immutable identity. Source-local vector compaction is unchanged.
pub(super) const CATALOG_PAGE_PREFIX: &str = "flat-catalog-";
const CATALOG_BUCKETS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CatalogPageDescriptor {
    bucket: u8,
    file_bytes: u64,
    sha256: String,
}

impl CatalogPageDescriptor {
    pub(super) fn file_name(&self) -> String {
        format!("{CATALOG_PAGE_PREFIX}{}.json", self.sha256)
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogPage {
    source_snapshots: Vec<SourceSnapshot>,
    segments: Vec<SegmentDescriptor>,
}

fn catalog_bucket(source: &str) -> u8 {
    Sha256::digest(source.as_bytes())[0]
}

pub(super) fn is_catalog_page_name(name: &str) -> bool {
    name.strip_prefix(CATALOG_PAGE_PREFIX)
        .and_then(|name| name.strip_suffix(".json"))
        .is_some_and(|digest| decode_sha256(digest).is_some())
}

// Schema 4's inline representation must retain its original digest on read.
// Schema 5 authenticates immutable page descriptors instead of embedding them.
pub(super) fn manifest_storage_bytes(manifest: &Manifest) -> FlatResult<Vec<u8>> {
    let mut stored = manifest.clone();
    if stored.schema_version == MANIFEST_SCHEMA_VERSION {
        stored.source_snapshots.clear();
        stored.segments.clear();
    }
    Ok(serde_json::to_vec(&stored)?)
}

pub(super) fn read_bounded_metadata(path: &Path, limit: u64) -> FlatResult<Vec<u8>> {
    let metadata = symlink_metadata_file(path)?;
    if metadata.len() == 0 || metadata.len() > limit {
        return Err(FlatStoreError::Corrupt(format!(
            "flat metadata {} has unsafe size {}",
            path.display(),
            metadata.len()
        )));
    }
    let file = File::open(path).map_err(|source| io_error("open flat metadata", path, source))?;
    let mut bytes = Vec::with_capacity(usize_from_u64(metadata.len(), "metadata size")?);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("read flat metadata", path, source))?;
    if bytes.len() as u64 != metadata.len() {
        return Err(FlatStoreError::Corrupt(
            "flat metadata size changed while reading".to_owned(),
        ));
    }
    Ok(bytes)
}

pub(super) fn load_catalog_pages(root: &Path, envelope: &mut ManifestEnvelope) -> FlatResult<()> {
    let manifest = &mut envelope.manifest;
    if !manifest.segments.is_empty()
        || !manifest.source_snapshots.is_empty()
        || manifest.catalog_pages.is_empty()
        || manifest.catalog_pages.len() > CATALOG_BUCKETS
        || encode_hex(Sha256::digest(serde_json::to_vec(manifest)?).as_slice())
            != envelope.manifest_sha256
    {
        return Err(FlatStoreError::Corrupt(
            "invalid paged manifest root".to_owned(),
        ));
    }
    let mut previous_bucket = None;
    for descriptor in &manifest.catalog_pages {
        if previous_bucket.is_some_and(|previous| previous >= descriptor.bucket)
            || descriptor.file_bytes == 0
            || descriptor.file_bytes > MAX_MANIFEST_BYTES
            || decode_sha256(&descriptor.sha256).is_none()
        {
            return Err(FlatStoreError::Corrupt(
                "invalid catalog page descriptor".to_owned(),
            ));
        }
        let path = segments_directory(root).join(descriptor.file_name());
        let bytes = read_bounded_metadata(&path, MAX_MANIFEST_BYTES)?;
        if bytes.len() as u64 != descriptor.file_bytes
            || encode_hex(Sha256::digest(&bytes).as_slice()) != descriptor.sha256
        {
            return Err(FlatStoreError::Corrupt(
                "catalog page checksum or size mismatch".to_owned(),
            ));
        }
        let page: CatalogPage = serde_json::from_slice(&bytes).map_err(|error| {
            FlatStoreError::Corrupt(format!("invalid catalog page JSON: {error}"))
        })?;
        if (page.source_snapshots.is_empty() && page.segments.is_empty())
            || page.source_snapshots.iter().any(|snapshot| {
                catalog_bucket(&snapshot.source_identity_digest) != descriptor.bucket
            })
            || page
                .segments
                .iter()
                .any(|segment| catalog_bucket(&segment.source_identity_digest) != descriptor.bucket)
        {
            return Err(FlatStoreError::Corrupt(
                "catalog page source routing is invalid".to_owned(),
            ));
        }
        manifest.source_snapshots.extend(page.source_snapshots);
        manifest.segments.extend(page.segments);
        previous_bucket = Some(descriptor.bucket);
    }
    manifest.source_snapshots.sort_by(|left, right| {
        left.source_identity_digest
            .cmp(&right.source_identity_digest)
    });
    manifest.segments.sort_by_key(|segment| segment.generation);
    Ok(())
}

pub(super) fn prepare_manifest(mut manifest: Manifest) -> FlatResult<PreparedManifest> {
    let previous_pages = std::mem::take(&mut manifest.catalog_pages);
    let mut buckets = BTreeMap::<u8, CatalogPage>::new();
    for snapshot in &manifest.source_snapshots {
        buckets
            .entry(catalog_bucket(&snapshot.source_identity_digest))
            .or_default()
            .source_snapshots
            .push(snapshot.clone());
    }
    for segment in &manifest.segments {
        buckets
            .entry(catalog_bucket(&segment.source_identity_digest))
            .or_default()
            .segments
            .push(segment.clone());
    }
    let mut pages = Vec::new();
    for (bucket, page) in buckets {
        let bytes = serde_json::to_vec(&page)?;
        if bytes.len() as u64 > manifest_byte_limit() {
            return Err(FlatStoreError::InvalidInput(
                "catalog page exceeds the safe size limit".to_owned(),
            ));
        }
        let descriptor = CatalogPageDescriptor {
            bucket,
            file_bytes: bytes.len() as u64,
            sha256: encode_hex(Sha256::digest(&bytes).as_slice()),
        };
        if !previous_pages.contains(&descriptor) {
            pages.push((descriptor.clone(), bytes));
        }
        manifest.catalog_pages.push(descriptor);
    }
    manifest.schema_version = MANIFEST_SCHEMA_VERSION;
    let digest = encode_hex(Sha256::digest(manifest_storage_bytes(&manifest)?).as_slice());
    let mut envelope = ManifestEnvelope {
        format: STORE_FORMAT.to_owned(),
        envelope_version: MANIFEST_ENVELOPE_VERSION,
        manifest,
        manifest_sha256: digest.clone(),
    };
    let snapshots = std::mem::take(&mut envelope.manifest.source_snapshots);
    let segments = std::mem::take(&mut envelope.manifest.segments);
    let bytes = serde_json::to_vec(&envelope)?;
    envelope.manifest.source_snapshots = snapshots;
    envelope.manifest.segments = segments;
    if bytes.len() as u64 > manifest_byte_limit() {
        return Err(FlatStoreError::InvalidInput(
            "manifest exceeds the safe size limit".to_owned(),
        ));
    }
    Ok(PreparedManifest {
        envelope,
        generation_hash: digest,
        bytes,
        pages,
    })
}

pub(super) fn publish_manifest(root: &Path, manifest: Manifest) -> FlatResult<SelectedManifest> {
    publish_prepared_manifest(root, prepare_manifest(manifest)?)
}

fn write_immutable_metadata(directory: &Path, name: &str, bytes: &[u8]) -> FlatResult<()> {
    let temporary = unique_temporary_path(directory, "catalog");
    let mut file = create_new_file(&temporary)?;
    file.write_all(bytes)
        .map_err(|source| io_error("write flat metadata", &temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync flat metadata", &temporary, source))?;
    drop(file);
    commit_unique_file(&temporary, &directory.join(name))
}

pub(super) fn publish_catalog_pages(
    root: &Path,
    pages: &[(CatalogPageDescriptor, Vec<u8>)],
) -> FlatResult<()> {
    let directory = segments_directory(root);
    for (descriptor, bytes) in pages {
        write_immutable_metadata(&directory, &descriptor.file_name(), bytes)?;
    }
    sync_directory(&directory)
}

pub(super) fn publish_prepared_manifest(
    root: &Path,
    prepared: PreparedManifest,
) -> FlatResult<SelectedManifest> {
    let PreparedManifest {
        envelope,
        generation_hash: digest,
        bytes,
        pages,
    } = prepared;
    // No root can expose pages which have not reached durable storage.
    publish_catalog_pages(root, &pages)?;
    #[cfg(test)]
    crash_at_test_publication_point(1);
    let directory = manifests_directory(root);
    let name = manifest_name(envelope.manifest.generation, &digest);
    write_immutable_metadata(&directory, &name, &bytes)?;
    sync_directory(&directory)?;
    #[cfg(test)]
    crash_at_test_publication_point(2);
    Ok(SelectedManifest {
        envelope,
        generation_hash: digest,
        path: directory.join(name),
    })
}

#[cfg(test)]
thread_local! {
    pub(super) static TEST_PUBLICATION_CRASH: std::cell::Cell<u8> = const {
        std::cell::Cell::new(0)
    };
    static TEST_MANIFEST_BYTE_LIMIT: std::cell::Cell<u64> = const {
        std::cell::Cell::new(MAX_MANIFEST_BYTES)
    };
}

#[cfg(test)]
fn crash_at_test_publication_point(point: u8) {
    if TEST_PUBLICATION_CRASH.with(std::cell::Cell::get) == point {
        std::process::exit(73);
    }
}

fn manifest_byte_limit() -> u64 {
    #[cfg(test)]
    {
        TEST_MANIFEST_BYTE_LIMIT.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        MAX_MANIFEST_BYTES
    }
}

#[cfg(test)]
impl FlatSegmentStore {
    pub(crate) fn with_test_manifest_byte_limit<T>(limit: u64, run: impl FnOnce() -> T) -> T {
        assert!(limit > 0 && limit <= MAX_MANIFEST_BYTES);
        struct Reset(u64);
        impl Drop for Reset {
            fn drop(&mut self) {
                TEST_MANIFEST_BYTE_LIMIT.with(|limit| limit.set(self.0));
            }
        }
        let _reset = Reset(TEST_MANIFEST_BYTE_LIMIT.with(|value| value.replace(limit)));
        run()
    }
}
