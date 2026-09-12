use std::{env, fs::File, process::Command};

use ctx_history_index_generation::GenerationReadRoot;

use super::*;

const CHILD_MODE: &str = "CTX_TEST_READER_RESOURCES_MODE";
const CHILD_ROOT: &str = "CTX_TEST_READER_RESOURCES_ROOT";
const CHILD_GENERATION: &str = "CTX_TEST_READER_RESOURCES_GENERATION";
const LOW_LIMIT: libc::rlim_t = 256;

#[test]
fn fresh_reader_raises_file_limit_before_opening_active_or_retained_history() {
    if let Ok(mode) = env::var(CHILD_MODE) {
        check_reader_child(&mode);
        return;
    }
    let temporary = tempdir().unwrap();
    let data_root = temporary.path().canonicalize().unwrap().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let search = data_root.join("search");
    ctx_history_platform::platform_security::ensure_private_directory(&search).unwrap();
    let lexical = search.join("lexical");
    let source = source("reader-file-limit.jsonl");
    let mut generations = Vec::new();
    for revision in [1, 2] {
        let mut writer = GenerationWriter::open(&lexical, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer
            .add_core_record(document(&source, 1, "searchable evidence"))
            .unwrap();
        writer
            .certify_source(certificate(&source, revision, 1))
            .unwrap();
        generations.push(writer.commit(|_| true).unwrap().generation_id);
    }
    let parent_limits = file_limits();
    assert!(parent_limits.1 > LOW_LIMIT);
    let open_error = tantivy::directory::error::OpenReadError::wrap_io_error(
        std::io::Error::from_raw_os_error(libc::EMFILE),
        "segment.term".into(),
    );
    let index_error = IndexError::from(tantivy::TantivyError::from(open_error));
    assert!(index_error
        .to_string()
        .contains("process open-file limit reached"));
    for mode in [
        "active",
        "retained",
        "index_root",
        "data_root",
        "fixed_active",
        "fixed_root",
    ] {
        let status = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::reader_resources::fresh_reader_raises_file_limit_before_opening_active_or_retained_history",
                "--nocapture",
            ])
            .env(CHILD_MODE, mode)
            .env(CHILD_ROOT, &data_root)
            .env(CHILD_GENERATION, &generations[usize::from(mode == "active")])
            .status()
            .unwrap();
        assert!(
            status.success(),
            "reader resource child {mode} failed: {status}"
        );
    }
    assert_eq!(
        file_limits(),
        parent_limits,
        "child must not change parent limits"
    );
}

fn check_reader_child(mode: &str) {
    let data_root = PathBuf::from(env::var_os(CHILD_ROOT).unwrap());
    let lexical = data_root.join("search").join("lexical");
    let generation = env::var(CHILD_GENERATION).unwrap();
    let inherited = file_limits();
    let fixed_limit = mode.starts_with("fixed_");
    let lowered = libc::rlimit {
        rlim_cur: LOW_LIMIT,
        rlim_max: if fixed_limit { LOW_LIMIT } else { inherited.1 },
    };
    assert_eq!(
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const lowered) },
        0
    );
    let mut files = Vec::new();
    loop {
        match File::open("/dev/null") {
            Ok(file) => files.push(file),
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
                break;
            }
        }
    }
    // The child has never opened a writer. Reader entry points must prepare
    // their own descriptor headroom, including before opening a lease root.
    match mode {
        "active" | "retained" => {
            let reader = if mode == "active" {
                VerifiedIndex::open_pinned(&lexical)
            } else {
                VerifiedIndex::open_pinned_generation(&lexical, &generation)
            }
            .unwrap();
            assert_eq!(reader.generation_id(), generation);
            assert_eq!(reader.count_term("evidence").unwrap(), 1);
        }
        "index_root" => {
            GenerationReadRoot::open_index_root(&lexical).unwrap();
        }
        "data_root" => {
            GenerationReadRoot::open_data_root(&data_root).unwrap();
        }
        "fixed_active" | "fixed_root" => {
            let error = if mode == "fixed_active" {
                VerifiedIndex::open_pinned(&lexical)
                    .err()
                    .unwrap()
                    .to_string()
            } else {
                GenerationReadRoot::open_index_root(&lexical)
                    .unwrap_err()
                    .to_string()
            };
            assert!(error.contains("process open-file limit reached"), "{error}");
            assert!(
                error.contains("soft limit: 256, hard limit: 256"),
                "{error}"
            );
            assert!(
                error.contains("increase the permitted open-file limit"),
                "{error}"
            );
            assert_eq!(file_limits(), (LOW_LIMIT, LOW_LIMIT));
            drop(files);
            return;
        }
        _ => panic!("unexpected child mode"),
    }
    assert_eq!(file_limits(), (inherited.1.min(4_096), inherited.1));
    drop(files);
}

fn file_limits() -> (libc::rlim_t, libc::rlim_t) {
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limits) },
        0
    );
    (limits.rlim_cur, limits.rlim_max)
}
