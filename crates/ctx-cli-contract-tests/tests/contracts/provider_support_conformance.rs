#[path = "support/native_providers/daemon.rs"]
#[allow(dead_code)]
mod provider_daemon;
#[path = "support/mod.rs"]
mod support;

use std::{collections::BTreeSet, path::Path};

use provider_daemon::*;
use serde_json::Value;
use support::*;

#[derive(Clone, Copy, Debug)]
struct ProviderCase {
    matrix_id: &'static str,
    provider: &'static str,
    user_text: &'static str,
    assistant_text: &'static str,
}

const ADAPTER_CASES: &[ProviderCase] = &[
    ProviderCase {
        matrix_id: "codex",
        provider: "codex",
        user_text: "codexuseroracle",
        assistant_text: "codexassistantoracle",
    },
    ProviderCase {
        matrix_id: "deepseek_harness",
        provider: "deepseek_harness",
        user_text: "deepseekharnessparentoracle7f31",
        assistant_text: "expected missing-file check failed safely",
    },
    ProviderCase {
        matrix_id: "grok_build",
        provider: "grok_build",
        user_text: "add a subtract(left, right) export",
        assistant_text: "Implemented subtraction support",
    },
    ProviderCase {
        matrix_id: "pi",
        provider: "pi",
        user_text: "piuseroracle",
        assistant_text: "piassistantoracle",
    },
    ProviderCase {
        matrix_id: "claude_code",
        provider: "claude",
        user_text: "claudeuseroracle",
        assistant_text: "native import ok",
    },
    ProviderCase {
        matrix_id: "open_code",
        provider: "opencode",
        user_text: "opencodeuseroracle",
        assistant_text: "OpenCode assistant response",
    },
    ProviderCase {
        matrix_id: "kilo",
        provider: "kilo",
        user_text: "kilouseroracle",
        assistant_text: "Kilo assistant response",
    },
    ProviderCase {
        matrix_id: "mimocode",
        provider: "mimocode",
        user_text: "mimocodeuseroracle",
        assistant_text: "MiMo Code assistant response",
    },
    ProviderCase {
        matrix_id: "kiro_cli",
        provider: "kiro_cli",
        user_text: "kirouseroracle",
        assistant_text: "Kiro CLI response",
    },
    ProviderCase {
        matrix_id: "crush",
        provider: "crush",
        user_text: "crush sqlite search oracle request",
        assistant_text: "summary: crush native sqlite support works",
    },
    ProviderCase {
        matrix_id: "goose",
        provider: "goose",
        user_text: "goose sqlite search oracle request",
        assistant_text: "looking at goose schema v15",
    },
    ProviderCase {
        matrix_id: "lingma",
        provider: "lingma",
        user_text: "lingmauseroracle",
        assistant_text: "Lingma CLI assistant summary import ok",
    },
    ProviderCase {
        matrix_id: "qoder",
        provider: "qoder",
        user_text: "qoderuseroracle",
        assistant_text: "qoder native import ok",
    },
    ProviderCase {
        matrix_id: "warp",
        provider: "warp",
        user_text: "warp sqlite oracle prompt",
        assistant_text: "Warp sqlite oracle answer",
    },
    ProviderCase {
        matrix_id: "codebuddy",
        provider: "codebuddy",
        user_text: "codebuddyuseroracle",
        assistant_text: "CodeBuddy CLI JSONL native import ok",
    },
    ProviderCase {
        matrix_id: "openclaw",
        provider: "openclaw",
        user_text: "openclawuseroracle",
        assistant_text: "native import ok",
    },
    ProviderCase {
        matrix_id: "hermes",
        provider: "hermes",
        user_text: "hermesuseroracle",
        assistant_text: "native import ok",
    },
    ProviderCase {
        matrix_id: "nanoclaw",
        provider: "nanoclaw",
        user_text: "nanoclawuseroracle",
        assistant_text: "native import ok",
    },
    ProviderCase {
        matrix_id: "astrbot",
        provider: "astrbot",
        user_text: "astrbotuseroracle",
        assistant_text: "native import ok",
    },
    ProviderCase {
        matrix_id: "shelley",
        provider: "shelley",
        user_text: "shelleyuseroracle",
        assistant_text: "native Shelley import ok",
    },
    ProviderCase {
        matrix_id: "continue",
        provider: "continue",
        user_text: "continueuseroracle",
        assistant_text: "native Continue import ok",
    },
    ProviderCase {
        matrix_id: "openhands",
        provider: "openhands",
        user_text: "openhandsuseroracle",
        assistant_text: "openhandsassistantoracle",
    },
    ProviderCase {
        matrix_id: "antigravity_cli",
        provider: "antigravity",
        user_text: "Create a tiny README for the demo project",
        assistant_text: "I will create a concise README",
    },
    ProviderCase {
        matrix_id: "gemini_cli",
        provider: "gemini",
        user_text: "geminiuseroracle",
        assistant_text: "geminiassistantoracle",
    },
    ProviderCase {
        matrix_id: "tabnine",
        provider: "tabnine",
        user_text: "tabnine jsonl oracle prompt",
        assistant_text: "tabnine jsonl oracle answer",
    },
    ProviderCase {
        matrix_id: "cursor",
        provider: "cursor",
        user_text: "cursoruseroracle",
        assistant_text: "native import ok",
    },
    ProviderCase {
        matrix_id: "zed",
        provider: "zed",
        user_text: "zed sqlite oracle prompt",
        assistant_text: "zed sqlite oracle answer",
    },
    ProviderCase {
        matrix_id: "copilot_cli",
        provider: "copilot_cli",
        user_text: "copilotuseroracle",
        assistant_text: "copilotassistantoracle",
    },
    ProviderCase {
        matrix_id: "factory_ai_droid",
        provider: "factory_ai_droid",
        user_text: "factorydroiduseroracle",
        assistant_text: "factorydroidassistantoracle",
    },
    ProviderCase {
        matrix_id: "qwen_code",
        provider: "qwen_code",
        user_text: "qwenuseroracle",
        assistant_text: "native Qwen import ok",
    },
    ProviderCase {
        matrix_id: "kimi_code_cli",
        provider: "kimi_code_cli",
        user_text: "kimi wire real shape prompt",
        assistant_text: "kimiassistantoracle",
    },
    ProviderCase {
        matrix_id: "auggie",
        provider: "auggie",
        user_text: "auggieuseroracle",
        assistant_text: "native Auggie import ok",
    },
    ProviderCase {
        matrix_id: "junie",
        provider: "junie",
        user_text: "junieuseroracle",
        assistant_text: "Junie answered",
    },
    ProviderCase {
        matrix_id: "firebender",
        provider: "firebender",
        user_text: "firebenderuseroracle",
        assistant_text: "Firebender fixture oracle response",
    },
    ProviderCase {
        matrix_id: "forgecode",
        provider: "forgecode",
        user_text: "forgecodeuseroracle",
        assistant_text: "forgecode native import ok",
    },
    ProviderCase {
        matrix_id: "deepagents",
        provider: "deepagents",
        user_text: "deepagents fixture oracle user prompt",
        assistant_text: "deepagents fixture oracle assistant reply",
    },
    ProviderCase {
        matrix_id: "mistral_vibe",
        provider: "mistral_vibe",
        user_text: "mistralvibeuseroracle",
        assistant_text: "mistral vibe native import ok",
    },
    ProviderCase {
        matrix_id: "mux",
        provider: "mux",
        user_text: "muxuseroracle",
        assistant_text: "mux cli native import ok",
    },
    ProviderCase {
        matrix_id: "rovodev",
        provider: "rovodev",
        user_text: "rovodevuseroracle",
        assistant_text: "rovodev native import ok",
    },
    ProviderCase {
        matrix_id: "cline",
        provider: "cline",
        user_text: "Write a short parser note for Cline task JSON support",
        assistant_text: "I will create the note now",
    },
    ProviderCase {
        matrix_id: "roo_code",
        provider: "roo_code",
        user_text: "Add a Roo Code task JSON import smoke test",
        assistant_text: "I will add a focused smoke fixture",
    },
    ProviderCase {
        matrix_id: "fx",
        provider: "fx",
        user_text: "fxuseroracle",
        assistant_text: "fxassistantoracle",
    },
];

#[derive(Debug, PartialEq)]
struct ProviderSnapshot {
    session_count: usize,
    records: Vec<Value>,
    public_ids: BTreeSet<String>,
}

#[test]
fn supported_provider_defaults_conform_to_the_public_matrix() {
    let supported_ids = assert_closed_world_case_ids();
    let provider_cases = ADAPTER_CASES
        .iter()
        .copied()
        .filter(|case| supported_ids.contains(case.matrix_id))
        .collect::<Vec<_>>();
    assert_eq!(
        provider_cases.len(),
        42,
        "support conformance must not be vacuous"
    );
    let temp = tempdir();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    for case in &provider_cases {
        install_provider_default_fixture(
            &temp,
            &workspace,
            case.matrix_id,
            case.user_text,
            case.assistant_text,
        );
    }
    let _daemon = start_isolated_provider_daemon_in(&temp, &workspace);

    let sources =
        json_output(
            ctx(&temp)
                .current_dir(&workspace)
                .args(["sources", "--all", "--format=json"]),
        );
    for case in &provider_cases {
        assert_available_native_source(&sources, case);
    }

    let first = import_all(&temp, &workspace);
    let first_publication = assert_authoritative_provider_publication(&first);
    assert_clean_import(&first);
    let first_generation = first_publication["published_generation"]
        .as_str()
        .expect("first import generation")
        .to_owned();
    let first_snapshots = provider_cases
        .iter()
        .map(|case| (case.matrix_id, snapshot_provider(&temp, &workspace, case)))
        .collect::<Vec<_>>();

    let second = import_all(&temp, &workspace);
    let second_publication = assert_authoritative_provider_publication(&second);
    assert_clean_import(&second);
    assert_noop_publication(&second);
    assert_eq!(
        second_publication["published_generation"], first_generation,
        "no-op import changed the public generation: {second:#}"
    );

    for ((matrix_id, first_snapshot), case) in first_snapshots.iter().zip(&provider_cases) {
        assert_eq!(*matrix_id, case.matrix_id);
        let second_snapshot = snapshot_provider(&temp, &workspace, case);
        assert_eq!(
            &second_snapshot, first_snapshot,
            "{} records, counts, or public ids changed after no-op import",
            case.matrix_id
        );
    }
}

fn assert_closed_world_case_ids() -> BTreeSet<String> {
    let matrix_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/provider-support-matrix.json");
    let matrix: Value = serde_json::from_slice(&fs::read(&matrix_path).unwrap())
        .unwrap_or_else(|error| panic!("parse {}: {error}", matrix_path.display()));
    let matrix_rows = matrix["providers"]
        .as_array()
        .expect("provider matrix rows");
    let matrix_ids = matrix_rows
        .iter()
        .map(|row| {
            row["id"]
                .as_str()
                .expect("provider matrix row id")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    let supported_rows = matrix_rows
        .iter()
        .filter(|row| row["status"] == "supported")
        .map(|row| {
            row["id"]
                .as_str()
                .expect("supported matrix row id")
                .to_owned()
        })
        .collect::<Vec<_>>();
    let supported_ids = supported_rows.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        supported_rows.len(),
        supported_ids.len(),
        "supported provider matrix ids must be unique"
    );
    let runtime_rows = ADAPTER_CASES
        .iter()
        .map(|case| case.matrix_id)
        .collect::<Vec<_>>();
    let runtime_ids = runtime_rows
        .iter()
        .map(|id| (*id).to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        runtime_rows.len(),
        runtime_ids.len(),
        "runtime provider case ids must be unique"
    );
    assert_eq!(matrix_ids, runtime_ids);
    assert_eq!(
        matrix_ids, supported_ids,
        "the public matrix is supported-only"
    );
    assert_eq!(
        supported_ids.len(),
        42,
        "support conformance must execute 42 rows"
    );
    supported_ids
}

fn assert_available_native_source(sources: &Value, case: &ProviderCase) {
    let matching = sources["sources"]
        .as_array()
        .expect("sources JSON rows")
        .iter()
        .filter(|source| source["provider"] == case.provider)
        .collect::<Vec<_>>();
    assert!(
        matching.iter().any(|source| {
            source["status"] == "available"
                && source["import_support"] == "native"
                && source["native_import"] == true
                && source["importable"] == true
        }),
        "{} ({}) has no available native importable default source: {matching:#?}",
        case.matrix_id,
        case.provider
    );
}

fn import_all(temp: &TempDir, workspace: &Path) -> Value {
    json_output(ctx(temp).current_dir(workspace).args([
        "import",
        "--all",
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]))
}

fn assert_clean_import(report: &Value) {
    assert_eq!(report["outcome"], "success", "{report:#}");
    assert_eq!(report["failure_scope"], "none", "{report:#}");
    assert_eq!(report["failure_type"], "none", "{report:#}");
    assert_eq!(report["totals"]["failed_sources"], 0, "{report:#}");
    assert_eq!(report["totals"]["rejected_records"], 0, "{report:#}");
    assert_eq!(
        report["totals"]["sources_completed_with_rejections"], 0,
        "{report:#}"
    );
}

fn snapshot_provider(temp: &TempDir, workspace: &Path, case: &ProviderCase) -> ProviderSnapshot {
    let records = provider_core_records(&data_root(temp), case.provider);
    assert!(
        !records.is_empty(),
        "{} has no Core records",
        case.matrix_id
    );
    assert_record_role_text(&records, case, "user", case.user_text);
    assert_record_role_text(&records, case, "assistant", case.assistant_text);
    let (session_count, event_count) = provider_core_counts(&data_root(temp), case.provider);
    assert!(session_count > 0, "{} has no sessions", case.matrix_id);
    assert!(event_count > 0, "{} has no events", case.matrix_id);
    let mut public_ids = searchable_public_ids(temp, workspace, case, case.user_text);
    public_ids.extend(searchable_public_ids(
        temp,
        workspace,
        case,
        case.assistant_text,
    ));
    ProviderSnapshot {
        session_count,
        records: records
            .into_iter()
            .map(|record| serde_json::to_value(record).unwrap())
            .collect(),
        public_ids,
    }
}

fn assert_record_role_text(
    records: &[ctx_history_index::CoreRecord],
    case: &ProviderCase,
    role: &str,
    text: &str,
) {
    let folded = text.to_lowercase();
    assert!(
        records.iter().any(|record| {
            record.role.as_deref() == Some(role)
                && record
                    .content
                    .meaningful_text()
                    .to_lowercase()
                    .contains(&folded)
        }),
        "{} has no {role} record containing {text:?}: {records:#?}",
        case.matrix_id
    );
}

fn searchable_public_ids(
    temp: &TempDir,
    workspace: &Path,
    case: &ProviderCase,
    text: &str,
) -> BTreeSet<String> {
    let search = json_output(ctx(temp).current_dir(workspace).args([
        "search",
        text,
        "--provider",
        case.provider,
        "--events",
        "--refresh",
        "off",
        "--limit",
        "20",
        "--format=json",
    ]));
    assert_eq!(search["filters"]["provider"], case.provider, "{search:#}");
    let results = search["results"].as_array().expect("search results");
    assert!(
        !results.is_empty(),
        "{} {text:?} was not searchable: {search:#}",
        case.matrix_id
    );
    let folded = text.to_lowercase();
    assert!(
        results.iter().any(|result| result["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.to_lowercase().contains(&folded))),
        "{} search omitted the known text {text:?}: {search:#}",
        case.matrix_id
    );
    let matching_result = results
        .iter()
        .find(|result| {
            result["snippet"]
                .as_str()
                .is_some_and(|snippet| snippet.to_lowercase().contains(&folded))
        })
        .expect("matching search result");
    assert_show_event(temp, workspace, case, matching_result, text);
    let mut ids = BTreeSet::new();
    for result in results {
        assert_eq!(result["provider"], case.provider, "{result:#}");
        assert_provider_citations(result, case.provider);
        for key in ["ctx_event_id", "ctx_session_id"] {
            ids.insert(format!(
                "result:{key}:{}",
                result[key].as_str().expect("public result id")
            ));
        }
        for citation in result["citations"].as_array().unwrap() {
            for key in ["item_id", "ctx_event_id", "ctx_session_id"] {
                ids.insert(format!(
                    "citation:{key}:{}",
                    citation[key].as_str().expect("public citation id")
                ));
            }
        }
    }
    ids
}

fn assert_show_event(
    temp: &TempDir,
    workspace: &Path,
    case: &ProviderCase,
    result: &Value,
    text: &str,
) {
    let event_id = result["ctx_event_id"].as_str().expect("search event id");
    let session_id = result["ctx_session_id"]
        .as_str()
        .expect("search session id");
    let shown = json_output(ctx(temp).current_dir(workspace).args([
        "show",
        "event",
        event_id,
        "--format=json",
    ]));
    assert_eq!(shown["ctx_event_id"], event_id, "{shown:#}");
    assert_eq!(shown["ctx_session_id"], session_id, "{shown:#}");
    assert_eq!(shown["event"]["provider"], case.provider, "{shown:#}");
    assert!(
        shown["event"]["text"]
            .as_str()
            .is_some_and(|shown_text| shown_text.to_lowercase().contains(&text.to_lowercase())),
        "{} show event omitted the known text {text:?}: {shown:#}",
        case.matrix_id
    );
}
