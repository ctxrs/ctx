use std::{
    fs, io,
    path::{Path, PathBuf},
};

use ctx_history_index::{
    CompiledSearchFilter, EventRecord, EventSearchFilters, LexicalExecution, LexicalMode,
    VerifiedIndex,
};

#[derive(Debug)]
pub(crate) struct HydratedEventSearchCandidate {
    pub(crate) event: EventRecord,
}

pub(crate) fn complete_lexical_events(
    index: &VerifiedIndex,
    natural_text: &str,
    filters: EventSearchFilters,
    limit: usize,
) -> Vec<HydratedEventSearchCandidate> {
    let filter = CompiledSearchFilter::compile(filters).unwrap();
    let alternatives = [natural_text];
    let observed = index
        .execute_lexical(LexicalExecution::new(
            LexicalMode::Search(&alternatives),
            &filter,
            limit,
        ))
        .unwrap();
    assert!(
        observed.batch.complete,
        "lexical execution did not complete: {:?}",
        observed.batch.exhaustion
    );
    observed
        .batch
        .candidates
        .into_iter()
        .map(|candidate| {
            let event = index
                .event_by_id(candidate.event.event_id)
                .unwrap()
                .expect("selected lexical event must hydrate");
            assert_eq!(
                event.event_id.digest(),
                candidate.event.event_identity_digest,
                "selected lexical event must preserve its exact identity"
            );
            HydratedEventSearchCandidate { event }
        })
        .collect()
}

pub(crate) fn tempdir() -> io::Result<tempfile::TempDir> {
    let temp_root = fs::canonicalize(std::env::temp_dir())?;
    tempfile::Builder::new()
        .prefix("ctx-history-capture-")
        .tempdir_in(temp_root)
}

pub(crate) fn capture_manifest_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest.is_absolute() {
        return manifest;
    }

    if let Ok(current_dir) = std::env::current_dir() {
        if let Some(path) = manifest_dir_from(&current_dir, &manifest) {
            return path;
        }
    }

    if let Ok(current_exe) = std::env::current_exe() {
        for ancestor in current_exe.ancestors() {
            if let Some(path) = manifest_dir_from(ancestor, &manifest) {
                return path;
            }
        }
    }

    manifest
}

pub(crate) fn capture_repo_root() -> PathBuf {
    let manifest = capture_manifest_dir();
    manifest
        .ancestors()
        .find(|candidate| {
            candidate.join("Cargo.toml").is_file()
                && candidate
                    .join("docs/provider-support-matrix.json")
                    .is_file()
        })
        .unwrap_or_else(|| panic!("locate ctx repository above {}", manifest.display()))
        .to_path_buf()
}

pub(crate) fn provider_support_matrix() -> serde_json::Value {
    let path = capture_repo_root().join("docs/provider-support-matrix.json");
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn manifest_dir_from(base: &Path, manifest: &Path) -> Option<PathBuf> {
    let candidate = base.join(manifest);
    if candidate.join("Cargo.toml").is_file() {
        return fs::canonicalize(&candidate).ok().or(Some(candidate));
    }
    None
}
