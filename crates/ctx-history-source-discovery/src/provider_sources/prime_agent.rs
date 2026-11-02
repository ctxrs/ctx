//! Bounded Prime v3 header admission. A ledger or cache is not a session.
//! Only flat session directories, the fixed RLM parent/child layout, and the
//! finite current T3 instance/thread layout are automatic probe authorities.

use std::{
    io::{BufRead, BufReader, Read},
    path::{Component, Path},
};

use ctx_history_source_io::{
    open_provider_source_path, provider_metadata_is_link_like, provider_safe_path_segment,
    OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceDirectory, SourceIoError,
};
use serde_json::Value;

use super::probes::BoundedProbe;

pub(super) const FORMAT: &str = "prime_agent_session_jsonl";
const MAX_ENTRIES: usize = 10_000;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_TOTAL_HEADER_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
enum Layout {
    Sessions,
    Artifacts,
    Children,
    T3,
    Instance,
}

struct Budget {
    entries: usize,
    bytes: usize,
}

pub(super) fn probe(path: &Path) -> BoundedProbe {
    probe_for_role(path, None)
}

pub(super) fn probe_for_role(path: &Path, role: Option<&[u8]>) -> BoundedProbe {
    if !path_without_links(path) {
        return BoundedProbe::IoError;
    }
    let mut budget = Budget {
        entries: MAX_ENTRIES,
        bytes: MAX_TOTAL_HEADER_BYTES,
    };
    let opened = match open_provider_source_path(path) {
        Ok(opened) => opened,
        Err(_) => return BoundedProbe::IoError,
    };
    let outcome = match opened {
        OpenedProviderSourcePath::File(file) => {
            if session_filename(path) {
                header(&file, &mut budget)
            } else {
                BoundedProbe::NotFound
            }
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let layout = if role == Some(b"selected-sessions") {
                Layout::Sessions
            } else if role == Some(b"t3-store") || path.ends_with(".t3/userdata/prime-agent") {
                Layout::T3
            } else if role == Some(b"rlm-artifacts")
                || path
                    .file_name()
                    .is_some_and(|name| name == "session-artifacts")
            {
                Layout::Artifacts
            } else {
                Layout::Sessions
            };
            // Explicit/named roots authorize flat transcripts even when their
            // basename resembles a native tree. Automatic roles stay exact.
            let flat = if role.is_none() && !matches!(layout, Layout::Sessions) {
                directory_probe(&directory, Layout::Sessions, &mut budget)
            } else {
                BoundedProbe::NotFound
            };
            if flat == BoundedProbe::Found {
                flat
            } else {
                combine(flat, directory_probe(&directory, layout, &mut budget))
            }
        }
    };
    if path_without_links(path) {
        outcome
    } else {
        BoundedProbe::IoError
    }
}

// The shared source authority deliberately permits relocated directory roots.
// Prime automatic selectors and probes have the stricter no-link contract.
pub(super) fn path_without_links(path: &Path) -> bool {
    if !path.is_absolute()
        || !super::selectors::encoded_path_within_limit(path)
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return false;
    }
    for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if provider_metadata_is_link_like(&metadata) => return false,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        }
    }
    true
}

fn session_filename(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "jsonl")
        && !matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("semantic-edges.jsonl" | "rlm-subagents.jsonl" | "rlm-ledger.jsonl")
        )
}

fn directory_probe(
    directory: &ProviderSourceDirectory,
    layout: Layout,
    budget: &mut Budget,
) -> BoundedProbe {
    let names = match directory.entries_snapshot(budget.entries.saturating_add(1)) {
        Ok(names) if names.len() <= budget.entries => names,
        Ok(_) => return BoundedProbe::BudgetExhausted,
        Err(SourceIoError::InvalidProviderTranscriptPath { reason, .. })
            if reason.contains("budget") =>
        {
            return BoundedProbe::BudgetExhausted;
        }
        Err(_) => return BoundedProbe::IoError,
    };
    budget.entries -= names.len();
    let mut outcome = BoundedProbe::NotFound;
    for name in names {
        // Never even open non-JSONL files at transcript depth. This includes
        // caches, binary artifacts, and links with unrelated names.
        if matches!(layout, Layout::Sessions) && !session_filename(Path::new(&name)) {
            continue;
        }
        if matches!(layout, Layout::Children)
            && !name
                .to_str()
                .is_some_and(|name| name.starts_with("sub-") && name.len() > 4)
        {
            continue;
        }
        let child = match directory.open_child(&name) {
            Ok(child) => child,
            Err(_) => {
                outcome = combine(outcome, BoundedProbe::IoError);
                continue;
            }
        };
        let next = match (layout, child) {
            (Layout::Sessions, OpenedProviderSourcePath::File(file)) => header(&file, budget),
            (Layout::Artifacts, OpenedProviderSourcePath::Directory(child)) => {
                directory_probe(&child, Layout::Children, budget)
            }
            (Layout::Children, OpenedProviderSourcePath::Directory(child)) => {
                directory_probe(&child, Layout::Sessions, budget)
            }
            (Layout::T3, OpenedProviderSourcePath::Directory(child)) => {
                directory_probe(&child, Layout::Instance, budget)
            }
            (Layout::Instance, OpenedProviderSourcePath::Directory(child)) => {
                let next = if name == "session-artifacts" {
                    Layout::Artifacts
                } else {
                    Layout::Sessions
                };
                directory_probe(&child, next, budget)
            }
            _ => BoundedProbe::NotFound,
        };
        outcome = combine(outcome, next);
        if outcome == BoundedProbe::Found {
            break;
        }
        if budget.entries == 0 || budget.bytes == 0 {
            outcome = combine(outcome, BoundedProbe::BudgetExhausted);
            break;
        }
    }
    if directory.revalidate().is_err() || directory.authority_root().revalidate().is_err() {
        BoundedProbe::IoError
    } else {
        outcome
    }
}

fn header(file: &OpenedProviderSourceFile, budget: &mut Budget) -> BoundedProbe {
    let maximum = (MAX_HEADER_BYTES + 1).min(budget.bytes);
    if maximum == 0 {
        return BoundedProbe::BudgetExhausted;
    }
    // Bound the prefix, not the entire transcript: a multi-megabyte session
    // still has a small first-line header. bounded_reader caps whole-file size.
    let Ok(reader) = file.file().try_clone() else {
        return BoundedProbe::IoError;
    };
    let mut line = Vec::new();
    let mut reader = BufReader::new(reader.take(maximum as u64));
    let read = reader.read_until(b'\n', &mut line);
    // Charge buffered read-ahead too, not just the parsed header length.
    let consumed = maximum - reader.into_inner().limit() as usize;
    budget.bytes = budget.bytes.saturating_sub(consumed);
    if read.is_err() || file.revalidate().is_err() {
        return BoundedProbe::IoError;
    }
    if line.len() > MAX_HEADER_BYTES || (line.len() == maximum && !line.ends_with(b"\n")) {
        return BoundedProbe::BudgetExhausted;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&line) else {
        return BoundedProbe::NotFound;
    };
    let valid = value.get("type").and_then(Value::as_str) == Some("session")
        && value.get("version").and_then(Value::as_u64) == Some(3)
        && value
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| provider_safe_path_segment(id) && !id.contains(':'))
        && value
            .get("timestamp")
            .and_then(Value::as_str)
            .is_some_and(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).is_ok())
        && value
            .get("cwd")
            .and_then(Value::as_str)
            .is_some_and(|cwd| !cwd.is_empty())
        && value
            .get("parentSession")
            .is_none_or(|parent| parent.is_null() || parent.is_string())
        && value.get("rlmDepth").is_none_or(|depth| {
            depth.is_null() || depth.as_u64().is_some_and(|depth| depth < u64::MAX)
        });
    if valid {
        BoundedProbe::Found
    } else {
        BoundedProbe::NotFound
    }
}

fn combine(left: BoundedProbe, right: BoundedProbe) -> BoundedProbe {
    use BoundedProbe::*;
    match (left, right) {
        (Found, _) | (_, Found) => Found,
        (BudgetExhausted, _) | (_, BudgetExhausted) => BudgetExhausted,
        (IoError, _) | (_, IoError) => IoError,
        _ => NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_budget_counts_read_ahead_and_is_shared_across_files() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        // The short non-session header must still charge any buffered body.
        std::fs::write(&path, format!("{{}}\n{}", "x".repeat(1024))).unwrap();
        let mut budget = Budget {
            entries: 4,
            bytes: 128,
        };
        let OpenedProviderSourcePath::File(file) = open_provider_source_path(&path).unwrap() else {
            panic!("file");
        };
        assert_eq!(header(&file, &mut budget), BoundedProbe::NotFound);
        assert_eq!(budget.bytes, 0);
        assert_eq!(header(&file, &mut budget), BoundedProbe::BudgetExhausted);
    }

    #[test]
    fn entry_budget_is_shared_across_t3_instances() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("instance-one/thread")).unwrap();
        std::fs::create_dir_all(temp.path().join("instance-two/thread")).unwrap();
        let OpenedProviderSourcePath::Directory(directory) =
            open_provider_source_path(temp.path()).unwrap()
        else {
            panic!("directory");
        };
        let mut budget = Budget {
            entries: 3,
            bytes: MAX_TOTAL_HEADER_BYTES,
        };
        assert_eq!(
            directory_probe(&directory, Layout::T3, &mut budget),
            BoundedProbe::BudgetExhausted
        );
        assert_eq!(budget.entries, 0);
    }
}
