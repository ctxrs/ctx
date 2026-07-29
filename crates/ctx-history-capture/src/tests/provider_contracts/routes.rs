use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{CaptureProvider, ProviderSupportMatrixDocument, ProviderSupportStatus};

use crate::provider_sources::provider_source_specs;

const CLI_DISPATCH_PATH: &str = "crates/ctx-cli/src/commands/import/native/dispatch.rs";
const SUPPORT_MATRIX_PATH: &str = "docs/provider-support-matrix.json";
const PROVIDER_SOURCE_ROOTS: &[&str] = &[
    "crates/ctx-history-capture/src/provider",
    "crates/ctx-history-capture/src/provider_sources",
    "crates/ctx-cli/src/commands/import",
];

const NATIVE_PATH_AUTHORITY_MUTATIONS: &[&str] = &[
    "assign_session_to_record",
    "bind_capture_source_provider_route",
    "bind_event_identity_alias",
    "compare_and_set_sync_cursor",
    "insert_event_if_absent",
    "insert_run_if_absent",
    "insert_session_if_absent",
    "reconcile_provider_event",
    "reconcile_provider_event_migrating_exact_legacy_provider_hash",
    "reconcile_provider_event_migrating_exact_legacy_provider_hash_with_native_path_accounting",
    "reconcile_provider_event_with_native_path_accounting",
    "reconcile_provider_source_locator",
    "retire_provider_source_route",
    "retire_source_generation_page",
    "stage_source_generation_page",
    "upsert_capture_source",
    "upsert_event",
    "upsert_event_with_native_path_accounting",
    "upsert_file_touched",
    "upsert_projection_neutral_session_edge",
    "upsert_run",
    "upsert_session",
    "upsert_session_edge",
    "upsert_sync_cursor",
];

#[derive(Debug)]
struct RouteBinding {
    public_source: &'static str,
    public_route: &'static str,
    nativepath_route: &'static str,
}

#[derive(Debug)]
struct ProviderRouteContract {
    semantic_id: &'static str,
    provider: CaptureProvider,
    dispatch_variant: &'static str,
    routes: &'static [RouteBinding],
}

macro_rules! binding {
    ($source:expr, $public:literal, $native:literal) => {
        RouteBinding {
            public_source: $source,
            public_route: $public,
            nativepath_route: $native,
        }
    };
}

const JSON_SOURCES: &str = "crates/ctx-history-capture/src/provider/api/json_sources.rs";
const SQLITE_SOURCES: &str = "crates/ctx-history-capture/src/provider/api/sqlite_sources.rs";
const NATIVE_STREAMS: &str = "crates/ctx-history-capture/src/provider/api/native_streams.rs";
const CODEX_HISTORY: &str = "crates/ctx-history-capture/src/provider/codex/history.rs";
const CODEX_SESSION: &str = "crates/ctx-history-capture/src/provider/codex/session.rs";

const PROVIDER_ROUTES: &[ProviderRouteContract] = &[
    ProviderRouteContract {
        semantic_id: "codex",
        provider: CaptureProvider::Codex,
        dispatch_variant: "Codex",
        routes: &[
            binding!(
                CODEX_SESSION,
                "import_codex_session_tree",
                "import_codex_native_session_root"
            ),
            binding!(
                CODEX_HISTORY,
                "import_codex_history_jsonl",
                "import_codex_native_prompt_history"
            ),
            binding!(
                CODEX_SESSION,
                "import_codex_session_jsonl",
                "import_codex_native_session_files"
            ),
        ],
    },
    ProviderRouteContract {
        semantic_id: "pi",
        provider: CaptureProvider::Pi,
        dispatch_variant: "Pi",
        routes: &[binding!(
            JSON_SOURCES,
            "import_pi_session_jsonl",
            "import_pi_nativepath_history"
        )],
    },
    ProviderRouteContract {
        semantic_id: "claude_code",
        provider: CaptureProvider::Claude,
        dispatch_variant: "Claude",
        routes: &[binding!(
            JSON_SOURCES,
            "import_claude_projects_jsonl_tree",
            "import_claude_nativepath_projects"
        )],
    },
    ProviderRouteContract {
        semantic_id: "open_code",
        provider: CaptureProvider::OpenCode,
        dispatch_variant: "OpenCode",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_opencode_sqlite",
            "import_opencode_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "kilo",
        provider: CaptureProvider::Kilo,
        dispatch_variant: "Kilo",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_kilo_sqlite",
            "import_opencode_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "mimocode",
        provider: CaptureProvider::MiMoCode,
        dispatch_variant: "MiMoCode",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_mimocode_sqlite",
            "import_opencode_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "kiro_cli",
        provider: CaptureProvider::KiroCli,
        dispatch_variant: "KiroCli",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_kiro_sqlite",
            "import_kiro_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "crush",
        provider: CaptureProvider::Crush,
        dispatch_variant: "Crush",
        routes: &[binding!(
            JSON_SOURCES,
            "import_crush_sqlite",
            "import_crush_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "goose",
        provider: CaptureProvider::Goose,
        dispatch_variant: "Goose",
        routes: &[binding!(
            JSON_SOURCES,
            "import_goose_sessions_sqlite",
            "import_goose_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "lingma",
        provider: CaptureProvider::Lingma,
        dispatch_variant: "Lingma",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_lingma_sqlite",
            "import_lingma_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "qoder",
        provider: CaptureProvider::Qoder,
        dispatch_variant: "Qoder",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_qoder_history",
            "import_qoder_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "warp",
        provider: CaptureProvider::Warp,
        dispatch_variant: "Warp",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_warp_sqlite",
            "import_warp_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "codebuddy",
        provider: CaptureProvider::CodeBuddy,
        dispatch_variant: "CodeBuddy",
        routes: &[binding!(
            JSON_SOURCES,
            "import_codebuddy_history",
            "import_codebuddy_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "trae",
        provider: CaptureProvider::Trae,
        dispatch_variant: "Trae",
        routes: &[binding!(
            JSON_SOURCES,
            "import_trae_history",
            "import_trae_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "openclaw",
        provider: CaptureProvider::OpenClaw,
        dispatch_variant: "OpenClaw",
        routes: &[binding!(
            JSON_SOURCES,
            "import_openclaw_history",
            "import_openclaw_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "hermes",
        provider: CaptureProvider::Hermes,
        dispatch_variant: "Hermes",
        routes: &[binding!(
            JSON_SOURCES,
            "import_hermes_sqlite",
            "import_hermes_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "nanoclaw",
        provider: CaptureProvider::NanoClaw,
        dispatch_variant: "NanoClaw",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_nanoclaw_project",
            "import_nanoclaw_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "shelley",
        provider: CaptureProvider::Shelley,
        dispatch_variant: "Shelley",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_shelley_sqlite",
            "import_shelley_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "continue",
        provider: CaptureProvider::Continue,
        dispatch_variant: "Continue",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_continue_cli_sessions",
            "import_continue_cli_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "openhands",
        provider: CaptureProvider::OpenHands,
        dispatch_variant: "OpenHands",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_openhands_file_events",
            "import_openhands_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "antigravity_cli",
        provider: CaptureProvider::Antigravity,
        dispatch_variant: "Antigravity",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_antigravity_cli_history",
            "import_antigravity_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "gemini_cli",
        provider: CaptureProvider::Gemini,
        dispatch_variant: "Gemini",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_gemini_cli_history",
            "import_gemini_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "tabnine",
        provider: CaptureProvider::Tabnine,
        dispatch_variant: "Tabnine",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_tabnine_cli_history",
            "import_tabnine_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "cursor",
        provider: CaptureProvider::Cursor,
        dispatch_variant: "Cursor",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_cursor_native_history",
            "import_cursor_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "windsurf",
        provider: CaptureProvider::Windsurf,
        dispatch_variant: "Windsurf",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_windsurf_cascade_hook_transcripts",
            "import_windsurf_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "zed",
        provider: CaptureProvider::Zed,
        dispatch_variant: "Zed",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_zed_threads_sqlite",
            "import_zed_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "copilot_cli",
        provider: CaptureProvider::CopilotCli,
        dispatch_variant: "CopilotCli",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_copilot_cli_session_events",
            "import_copilot_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "factory_ai_droid",
        provider: CaptureProvider::FactoryAiDroid,
        dispatch_variant: "FactoryAiDroid",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_factory_ai_droid_sessions",
            "import_factory_ai_droid_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "qwen_code",
        provider: CaptureProvider::QwenCode,
        dispatch_variant: "QwenCode",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_qwen_code_history",
            "import_qwen_code_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "kimi_code_cli",
        provider: CaptureProvider::KimiCodeCli,
        dispatch_variant: "KimiCodeCli",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_kimi_code_cli_history",
            "import_kimi_nativepath_tree"
        )],
    },
    ProviderRouteContract {
        semantic_id: "auggie",
        provider: CaptureProvider::Auggie,
        dispatch_variant: "Auggie",
        routes: &[binding!(
            JSON_SOURCES,
            "import_auggie_history",
            "import_auggie_sessions_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "junie",
        provider: CaptureProvider::Junie,
        dispatch_variant: "Junie",
        routes: &[binding!(
            JSON_SOURCES,
            "import_junie_history",
            "import_junie_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "firebender",
        provider: CaptureProvider::Firebender,
        dispatch_variant: "Firebender",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_firebender_sqlite",
            "import_firebender_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "forgecode",
        provider: CaptureProvider::ForgeCode,
        dispatch_variant: "ForgeCode",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_forgecode_sqlite",
            "import_forgecode_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "deepagents",
        provider: CaptureProvider::DeepAgents,
        dispatch_variant: "DeepAgents",
        routes: &[binding!(
            SQLITE_SOURCES,
            "import_deepagents_sqlite",
            "import_deepagents_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "mistral_vibe",
        provider: CaptureProvider::MistralVibe,
        dispatch_variant: "MistralVibe",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_mistral_vibe_history",
            "import_mistral_vibe_nativepath"
        )],
    },
    ProviderRouteContract {
        semantic_id: "mux",
        provider: CaptureProvider::Mux,
        dispatch_variant: "Mux",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_mux_history",
            "import_mux_native_path"
        )],
    },
    ProviderRouteContract {
        semantic_id: "rovodev",
        provider: CaptureProvider::RovoDev,
        dispatch_variant: "RovoDev",
        routes: &[binding!(
            NATIVE_STREAMS,
            "import_rovodev_history",
            "import_rovodev_native_path"
        )],
    },
    ProviderRouteContract {
        semantic_id: "cline",
        provider: CaptureProvider::Cline,
        dispatch_variant: "Cline",
        routes: &[binding!(
            JSON_SOURCES,
            "import_cline_task_json_history",
            "import_cline_nativepath_history"
        )],
    },
    ProviderRouteContract {
        semantic_id: "roo_code",
        provider: CaptureProvider::RooCode,
        dispatch_variant: "RooCode",
        routes: &[binding!(
            JSON_SOURCES,
            "import_roo_task_json_history",
            "import_roo_nativepath_history"
        )],
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
    text: String,
    line: usize,
}

#[derive(Debug)]
struct Function<'a> {
    name: &'a str,
    parameters: &'a [Token],
    body: &'a [Token],
}

fn workspace_root() -> PathBuf {
    if let (Ok(test_srcdir), Ok(test_workspace)) =
        (env::var("TEST_SRCDIR"), env::var("TEST_WORKSPACE"))
    {
        let root = PathBuf::from(test_srcdir).join(test_workspace);
        if root.join(SUPPORT_MATRIX_PATH).is_file() {
            return root;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_workspace_source(path: &str) -> String {
    fs::read_to_string(workspace_root().join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

fn is_ident_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

fn skip_quoted(bytes: &[u8], mut cursor: usize, quote: u8, line: &mut usize) -> usize {
    cursor += 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            byte if byte == quote => return cursor + 1,
            b'\n' => {
                *line += 1;
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    cursor
}

fn raw_string_start(bytes: &[u8], cursor: usize) -> Option<(usize, usize)> {
    let mut marker = cursor;
    if bytes.get(marker) == Some(&b'b') {
        marker += 1;
    }
    if bytes.get(marker) != Some(&b'r') {
        return None;
    }
    marker += 1;
    let hashes_start = marker;
    while bytes.get(marker) == Some(&b'#') {
        marker += 1;
    }
    (bytes.get(marker) == Some(&b'"')).then_some((marker + 1, marker - hashes_start))
}

fn skip_raw_string(bytes: &[u8], mut cursor: usize, hashes: usize, line: &mut usize) -> usize {
    while cursor < bytes.len() {
        if bytes[cursor] == b'\n' {
            *line += 1;
        }
        if bytes[cursor] == b'"'
            && bytes
                .get(cursor + 1..cursor + 1 + hashes)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            return cursor + 1 + hashes;
        }
        cursor += 1;
    }
    cursor
}

fn rust_tokens(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0;
    let mut line = 1;
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_whitespace() {
            if bytes[cursor] == b'\n' {
                line += 1;
            }
            cursor += 1;
            continue;
        }
        if bytes.get(cursor..cursor + 2) == Some(b"//") {
            while cursor < bytes.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            continue;
        }
        if bytes.get(cursor..cursor + 2) == Some(b"/*") {
            let mut depth = 1;
            cursor += 2;
            while cursor < bytes.len() && depth > 0 {
                if bytes.get(cursor..cursor + 2) == Some(b"/*") {
                    depth += 1;
                    cursor += 2;
                } else if bytes.get(cursor..cursor + 2) == Some(b"*/") {
                    depth -= 1;
                    cursor += 2;
                } else {
                    if bytes[cursor] == b'\n' {
                        line += 1;
                    }
                    cursor += 1;
                }
            }
            continue;
        }
        if let Some((body, hashes)) = raw_string_start(bytes, cursor) {
            cursor = skip_raw_string(bytes, body, hashes, &mut line);
            continue;
        }
        if bytes[cursor] == b'"' || (bytes[cursor] == b'b' && bytes.get(cursor + 1) == Some(&b'"'))
        {
            cursor = skip_quoted(
                bytes,
                cursor + usize::from(bytes[cursor] == b'b'),
                b'"',
                &mut line,
            );
            continue;
        }
        let char_literal = bytes[cursor] == b'\''
            && (bytes.get(cursor + 2) == Some(&b'\'')
                || (bytes.get(cursor + 1) == Some(&b'\\')
                    && bytes.get(cursor + 3) == Some(&b'\'')));
        if char_literal {
            cursor = skip_quoted(bytes, cursor, b'\'', &mut line);
            continue;
        }
        if is_ident_start(bytes[cursor]) {
            let start = cursor;
            cursor += 1;
            while cursor < bytes.len() && is_ident_continue(bytes[cursor]) {
                cursor += 1;
            }
            tokens.push(Token {
                text: source[start..cursor].to_owned(),
                line,
            });
            continue;
        }
        let (text, width) = match bytes.get(cursor..cursor + 2) {
            Some(b"::") => ("::", 2),
            Some(b"=>") => ("=>", 2),
            Some(b"->") => ("->", 2),
            _ => {
                let character = source[cursor..]
                    .chars()
                    .next()
                    .expect("token cursor must be in bounds");
                (
                    &source[cursor..cursor + character.len_utf8()],
                    character.len_utf8(),
                )
            }
        };
        tokens.push(Token {
            text: text.to_owned(),
            line,
        });
        cursor += width;
    }
    tokens
}

fn matching_delimiter(tokens: &[Token], open: usize, left: &str, right: &str) -> Option<usize> {
    let mut depth = 0;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        if token.text == left {
            depth += 1;
        } else if token.text == right {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn cfg_test_attribute_end(tokens: &[Token], start: usize) -> Option<usize> {
    let attribute_end = (tokens.get(start)?.text == "#")
        .then(|| matching_delimiter(tokens, start + 1, "[", "]"))??;
    let attribute = &tokens[start + 2..attribute_end];
    (attribute
        .iter()
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>()
        == ["cfg", "(", "test", ")"])
    .then_some(attribute_end + 1)
}

fn production_tokens(source: &str) -> Vec<Token> {
    let tokens = rust_tokens(source);
    let mut production = Vec::with_capacity(tokens.len());
    let mut cursor = 0;
    while cursor < tokens.len() {
        let Some(mut item_start) = cfg_test_attribute_end(&tokens, cursor) else {
            production.push(tokens[cursor].clone());
            cursor += 1;
            continue;
        };
        while item_start < tokens.len() && tokens[item_start].text == "#" {
            let Some(end) = matching_delimiter(&tokens, item_start + 1, "[", "]") else {
                break;
            };
            item_start = end + 1;
        }
        let mut boundary = item_start;
        while boundary < tokens.len()
            && tokens[boundary].text != "{"
            && tokens[boundary].text != ";"
        {
            boundary += 1;
        }
        cursor = if tokens.get(boundary).is_some_and(|token| token.text == "{") {
            matching_delimiter(&tokens, boundary, "{", "}").map_or(tokens.len(), |end| end + 1)
        } else {
            (boundary + 1).min(tokens.len())
        };
    }
    production
}

fn functions(tokens: &[Token]) -> Vec<Function<'_>> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while cursor < tokens.len() {
        if tokens[cursor].text != "fn" {
            cursor += 1;
            continue;
        }
        let Some(name) = tokens.get(cursor + 1) else {
            break;
        };
        let Some(parameters_start) = tokens[cursor + 2..]
            .iter()
            .position(|token| token.text == "(")
            .map(|offset| cursor + 2 + offset)
        else {
            cursor += 1;
            continue;
        };
        let Some(parameters_end) = matching_delimiter(tokens, parameters_start, "(", ")") else {
            cursor += 1;
            continue;
        };
        let Some(body_start) = tokens[parameters_end + 1..]
            .iter()
            .position(|token| token.text == "{" || token.text == ";")
            .map(|offset| parameters_end + 1 + offset)
        else {
            break;
        };
        if tokens[body_start].text == ";" {
            cursor = body_start + 1;
            continue;
        }
        let Some(body_end) = matching_delimiter(tokens, body_start, "{", "}") else {
            break;
        };
        found.push(Function {
            name: &name.text,
            parameters: &tokens[parameters_start + 1..parameters_end],
            body: &tokens[body_start + 1..body_end],
        });
        cursor = body_end + 1;
    }
    found
}

fn called_identifiers(tokens: &[Token]) -> BTreeSet<&str> {
    tokens
        .windows(2)
        .filter(|window| window[1].text == "(" && is_ident_start(window[0].text.as_bytes()[0]))
        .map(|window| window[0].text.as_str())
        .collect()
}

fn function_calls(source: &str, name: &str) -> BTreeSet<String> {
    let tokens = rust_tokens(source);
    let matches = functions(&tokens)
        .into_iter()
        .filter(|function| function.name == name)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one function named {name}"
    );
    called_identifiers(matches[0].body)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn dispatch_arms(source: &str) -> BTreeMap<String, BTreeSet<String>> {
    let tokens = rust_tokens(source);
    let matches = functions(&tokens)
        .into_iter()
        .filter(|function| function.name == "import_direct_source")
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one production import_direct_source function"
    );
    let function_body = matches[0].body;
    let provider_matches = function_body
        .windows(5)
        .enumerate()
        .filter(|(_, window)| {
            window[0].text == "match"
                && window[1].text == "source"
                && window[2].text == "."
                && window[3].text == "provider"
                && window[4].text == "{"
        })
        .map(|(index, _)| index + 4)
        .collect::<Vec<_>>();
    assert_eq!(
        provider_matches.len(),
        1,
        "expected exactly one `match source.provider` dispatch"
    );
    let body_start = provider_matches[0];
    let body_end = matching_delimiter(function_body, body_start, "{", "}")
        .expect("provider dispatch match must close");
    let body = &function_body[body_start + 1..body_end];
    let public_routes = PROVIDER_ROUTES
        .iter()
        .flat_map(|contract| contract.routes)
        .map(|route| route.public_route)
        .collect::<BTreeSet<_>>();
    let mut arms = BTreeMap::new();
    let mut starts = Vec::new();
    let mut brace_depth = 0;
    for index in 0..body.len().saturating_sub(3) {
        if body[index].text == "{" {
            brace_depth += 1;
        } else if body[index].text == "}" {
            brace_depth -= 1;
        } else if brace_depth == 0
            && body[index].text == "CaptureProvider"
            && body[index + 1].text == "::"
            && body[index + 3].text == "=>"
        {
            starts.push((body[index + 2].text.clone(), index + 4));
        }
    }
    for (position, (variant, body_start)) in starts.iter().enumerate() {
        let body_end = starts
            .get(position + 1)
            .map_or(body.len(), |(_, next_start)| next_start.saturating_sub(4));
        let calls = called_identifiers(&body[*body_start..body_end])
            .into_iter()
            .filter(|call| public_routes.contains(call))
            .map(str::to_owned)
            .collect();
        assert!(
            arms.insert(variant.clone(), calls).is_none(),
            "duplicate CLI dispatch arm for {variant}"
        );
    }
    arms
}

fn expected_contract_sets() -> (BTreeMap<String, String>, BTreeSet<String>, BTreeSet<String>) {
    let mut semantic_to_provider = BTreeMap::new();
    let mut providers = BTreeSet::new();
    let mut variants = BTreeSet::new();
    let mut public_routes = BTreeSet::new();
    for contract in PROVIDER_ROUTES {
        assert!(
            semantic_to_provider
                .insert(
                    contract.semantic_id.to_owned(),
                    contract.provider.as_str().to_owned(),
                )
                .is_none(),
            "duplicate semantic provider id {}",
            contract.semantic_id
        );
        assert!(
            providers.insert(contract.provider.as_str().to_owned()),
            "duplicate CaptureProvider {}",
            contract.provider.as_str()
        );
        assert!(
            variants.insert(contract.dispatch_variant.to_owned()),
            "duplicate dispatch variant {}",
            contract.dispatch_variant
        );
        for route in contract.routes {
            assert!(
                public_routes.insert(route.public_route),
                "duplicate public import route {}",
                route.public_route
            );
        }
    }
    assert_eq!(public_routes.len(), 43, "public route count changed");
    (semantic_to_provider, providers, variants)
}

fn semantic_provider_id(id: ctx_history_core::ProviderId) -> String {
    serde_json::to_value(id)
        .expect("ProviderId must serialize")
        .as_str()
        .expect("ProviderId must serialize as a string")
        .to_owned()
}

#[test]
fn provider_matrix_registry_and_cli_dispatch_are_the_exact_same_41_provider_set() {
    let (expected_pairs, expected_providers, expected_variants) = expected_contract_sets();
    assert_eq!(expected_pairs.len(), 41, "semantic provider count changed");

    let matrix: ProviderSupportMatrixDocument =
        serde_json::from_str(&read_workspace_source(SUPPORT_MATRIX_PATH))
            .expect("provider support matrix must parse");
    assert_eq!(
        matrix.providers.len(),
        41,
        "support matrix row count changed"
    );
    let matrix_pairs = matrix
        .providers
        .iter()
        .map(|entry| {
            assert_eq!(
                entry.status,
                ProviderSupportStatus::Supported,
                "{} is not supported",
                semantic_provider_id(entry.id)
            );
            assert!(
                entry.imports_existing_history,
                "{} does not import existing history",
                semantic_provider_id(entry.id)
            );
            let provider = entry.capture_provider.unwrap_or_else(|| {
                panic!("{} lacks capture_provider", semantic_provider_id(entry.id))
            });
            (semantic_provider_id(entry.id), provider.as_str().to_owned())
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        matrix_pairs.len(),
        matrix.providers.len(),
        "support matrix contains duplicate semantic ids"
    );
    assert_eq!(matrix_pairs, expected_pairs, "support matrix drifted");

    let importable_specs = provider_source_specs()
        .iter()
        .filter(|spec| spec.import_support.is_importable())
        .collect::<Vec<_>>();
    assert_eq!(
        importable_specs.len(),
        41,
        "provider source registry importable row count changed"
    );
    let registry_providers = importable_specs
        .iter()
        .map(|spec| spec.provider.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        registry_providers.len(),
        importable_specs.len(),
        "provider source registry contains duplicate CaptureProvider rows"
    );
    assert_eq!(
        registry_providers, expected_providers,
        "provider source registry drifted"
    );

    let dispatch = dispatch_arms(&read_workspace_source(CLI_DISPATCH_PATH));
    let dispatch_variants = dispatch.keys().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        dispatch_variants, expected_variants,
        "CLI provider dispatch drifted"
    );
}

#[test]
fn every_cli_route_calls_its_exact_public_nativepath_adapter() {
    let dispatch = dispatch_arms(&read_workspace_source(CLI_DISPATCH_PATH));
    for contract in PROVIDER_ROUTES {
        let expected_public_routes = contract
            .routes
            .iter()
            .map(|route| route.public_route)
            .collect::<BTreeSet<_>>();
        let actual_public_routes = dispatch
            .get(contract.dispatch_variant)
            .unwrap_or_else(|| panic!("missing CLI dispatch arm {}", contract.dispatch_variant))
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actual_public_routes, expected_public_routes,
            "{} CLI routes drifted",
            contract.dispatch_variant
        );
        for route in contract.routes {
            let source = read_workspace_source(route.public_source);
            let calls = function_calls(&source, route.public_route);
            let import_calls = calls
                .iter()
                .filter(|call| call.starts_with("import_"))
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                import_calls,
                BTreeSet::from([route.nativepath_route]),
                "{} in {} must call only its exact NativePath import adapter {}",
                route.public_route,
                route.public_source,
                route.nativepath_route
            );
        }
    }
}

#[test]
fn custom_jsonl_public_routes_call_the_exact_nativepath_adapters() {
    let source = read_workspace_source("crates/ctx-history-capture/src/provider/api.rs");
    for (public_route, nativepath_route) in [
        (
            "import_custom_history_jsonl_v1",
            "import_custom_history_nativepath",
        ),
        (
            "import_custom_history_jsonl_v1_reader",
            "import_custom_history_nativepath_reader",
        ),
        (
            "validate_custom_history_jsonl_v1",
            "validate_custom_history_nativepath",
        ),
        (
            "validate_custom_history_jsonl_v1_reader",
            "validate_custom_history_nativepath_reader",
        ),
    ] {
        let calls = function_calls(&source, public_route);
        let architecture_calls = calls
            .iter()
            .filter(|call| call.starts_with("import_") || call.starts_with("validate_"))
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            architecture_calls,
            BTreeSet::from([nativepath_route]),
            "{public_route} must call only {nativepath_route}"
        );
    }
}

fn is_test_source(path: &Path) -> bool {
    if path
        .components()
        .any(|component| component.as_os_str() == "tests")
    {
        return true;
    }
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name == "tests.rs"
        || file_name.ends_with("_tests.rs")
        || file_name.starts_with("test_support")
}

fn collect_production_rust_sources(root: &Path, sources: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", root.display()))
        .map(|entry| {
            entry
                .expect("source directory entry must be readable")
                .path()
        })
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "tests") {
                continue;
            }
            collect_production_rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && !is_test_source(&path)
        {
            sources.push(path);
        }
    }
}

fn parameter_boundary_bindings(function: &Function<'_>) -> BTreeMap<String, bool> {
    let mut bindings = BTreeMap::new();
    for segment in function.parameters.split(|token| token.text == ",") {
        if segment
            .iter()
            .any(|token| token.text == "NativePathPublicationGroup")
        {
            if let Some(colon) = segment.iter().position(|token| token.text == ":") {
                if let Some(binding) = segment[..colon]
                    .iter()
                    .rev()
                    .find(|token| is_ident_start(token.text.as_bytes()[0]))
                {
                    bindings.insert(binding.text.clone(), true);
                }
            }
        }
    }
    bindings
}

fn nearest_binding_is_boundary(scopes: &[BTreeMap<String, bool>], receiver: &str) -> bool {
    scopes
        .iter()
        .rev()
        .find_map(|scope| scope.get(receiver))
        .copied()
        .unwrap_or(false)
}

fn let_bindings(statement: &[Token]) -> (Vec<String>, bool) {
    let equals = statement.iter().position(|token| token.text == "=");
    let pattern_end = equals.unwrap_or(statement.len());
    let type_start = statement[..pattern_end]
        .iter()
        .position(|token| token.text == ":")
        .unwrap_or(pattern_end);
    let bindings = statement[..type_start]
        .iter()
        .filter(|token| {
            is_ident_start(token.text.as_bytes()[0])
                && !matches!(token.text.as_str(), "mut" | "ref")
        })
        .map(|token| token.text.clone())
        .collect::<Vec<_>>();
    let begins_publication = equals.is_some_and(|equals| {
        statement[equals + 1..]
            .iter()
            .any(|token| token.text == "begin_native_path_publication_group")
    });
    (bindings, begins_publication)
}

fn let_statement_end(tokens: &[Token], start: usize) -> Option<usize> {
    let mut delimiters = Vec::new();
    for (index, token) in tokens.iter().enumerate().skip(start) {
        match token.text.as_str() {
            "(" | "[" | "{" => delimiters.push(token.text.as_str()),
            ")" => {
                if delimiters.last() == Some(&"(") {
                    delimiters.pop();
                }
            }
            "]" => {
                if delimiters.last() == Some(&"[") {
                    delimiters.pop();
                }
            }
            "}" => {
                if delimiters.last() == Some(&"{") {
                    delimiters.pop();
                }
            }
            ";" if delimiters.is_empty() => return Some(index),
            _ => {}
        }
    }
    None
}

fn direct_store_write_violations(source: &str, path: &Path) -> Vec<String> {
    let tokens = production_tokens(source);
    let mutations = NATIVE_PATH_AUTHORITY_MUTATIONS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut violations = Vec::new();
    for function in functions(&tokens) {
        let mut scopes = vec![parameter_boundary_bindings(&function)];
        let mut pending_bindings = Vec::<(usize, usize, Vec<String>, bool)>::new();
        let mut index = 0;
        while index < function.body.len() {
            let mut pending = 0;
            while pending < pending_bindings.len() {
                if pending_bindings[pending].0 != index {
                    pending += 1;
                    continue;
                }
                let (_, scope_depth, bindings, begins_publication) =
                    pending_bindings.remove(pending);
                let simple_boundary = begins_publication && bindings.len() == 1;
                for binding in bindings {
                    scopes[scope_depth].insert(binding, simple_boundary);
                }
            }
            let token = &function.body[index];
            if token.text == "{" {
                scopes.push(BTreeMap::new());
                index += 1;
                continue;
            }
            if token.text == "}" {
                if scopes.len() > 1 {
                    scopes.pop();
                }
                index += 1;
                continue;
            }
            if token.text == "let"
                && !function
                    .body
                    .get(index.wrapping_sub(1))
                    .is_some_and(|previous| matches!(previous.text.as_str(), "if" | "while"))
            {
                let statement_end =
                    let_statement_end(function.body, index + 1).unwrap_or(function.body.len());
                let statement = &function.body[index + 1..statement_end];
                let (bindings, begins_publication) = let_bindings(statement);
                if !bindings.is_empty() {
                    pending_bindings.push((
                        statement_end + 1,
                        scopes.len() - 1,
                        bindings,
                        begins_publication,
                    ));
                }
            }
            if !mutations.contains(token.text.as_str()) {
                index += 1;
                continue;
            }
            let method_reference = index.checked_sub(1).is_some_and(|previous| {
                matches!(function.body[previous].text.as_str(), "." | "::")
            });
            if !method_reference {
                index += 1;
                continue;
            }
            let receiver = index
                .checked_sub(2)
                .and_then(|receiver| {
                    (function.body[index - 1].text == "." || function.body[index - 1].text == "::")
                        .then(|| function.body[receiver].text.as_str())
                })
                .unwrap_or("<expression>");
            if !nearest_binding_is_boundary(&scopes, receiver) {
                violations.push(format!(
                    "{}:{}: {} calls {} through `{receiver}` outside a declared NativePath publication group",
                    path.display(),
                    token.line,
                    function.name,
                    token.text
                ));
            }
            index += 1;
        }
    }
    violations
}

#[test]
fn production_provider_and_cli_writes_stay_inside_nativepath_publication_groups() {
    let root = workspace_root();
    let mut sources = Vec::new();
    for relative_root in PROVIDER_SOURCE_ROOTS {
        let previous_count = sources.len();
        collect_production_rust_sources(&root.join(relative_root), &mut sources);
        assert!(
            sources.len() > previous_count,
            "{relative_root} has no production Rust sources"
        );
    }

    let mut violations = Vec::new();
    for path in sources {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        violations.extend(direct_store_write_violations(&source, &path));
    }
    assert!(
        violations.is_empty(),
        "direct Store provider-authority writes bypass NativePath publication:\n{}",
        violations.join("\n")
    );
}

#[test]
fn structural_checks_ignore_comments_and_strings_but_reject_direct_store_writes() {
    let fake_routes = r#"
        fn import_direct_source() {
            match source.provider {
                // CaptureProvider::Codex => import_codex_session_tree()
                other => {
                    const LIE: &str =
                        "CaptureProvider::Pi => import_pi_session_jsonl()";
                }
            }
        }
    "#;
    assert!(dispatch_arms(fake_routes).is_empty());
    assert!(function_calls(
        "fn route() { /* import_pi_nativepath_history() */ }",
        "route"
    )
    .is_empty());

    let direct = "fn publish(store: &Store) { store.upsert_event(&event); }";
    assert_eq!(
        direct_store_write_violations(direct, Path::new("provider/direct.rs")).len(),
        1
    );
    let nativepath = "fn publish(group: &mut NativePathPublicationGroup<'_>) {
        group.upsert_event(&event);
    }";
    assert!(direct_store_write_violations(
        nativepath,
        Path::new("provider/example/native_path/publication.rs")
    )
    .is_empty());
    let direct_locator = "fn resolve(store: &Store) {
        store.reconcile_provider_source_locator(&observation);
    }";
    assert_eq!(
        direct_store_write_violations(direct_locator, Path::new("provider/direct_locator.rs"))
            .len(),
        1
    );
    let newly_guarded = r#"
        fn bypass(store: &Store) {
            store.upsert_run(&run);
            store.insert_run_if_absent(&run);
            store.upsert_file_touched(&file);
            store.bind_capture_source_provider_route(source_id, &route);
            store.stage_source_generation_page(&key, &retained);
            store.retire_source_generation_page(&key, generation, limit);
            store.compare_and_set_sync_cursor(current.as_ref(), &next);
        }
    "#;
    let newly_guarded_violations =
        direct_store_write_violations(newly_guarded, Path::new("provider/direct_authority.rs"));
    assert_eq!(newly_guarded_violations.len(), 7);
    for mutation in [
        "upsert_run",
        "insert_run_if_absent",
        "upsert_file_touched",
        "bind_capture_source_provider_route",
        "stage_source_generation_page",
        "retire_source_generation_page",
        "compare_and_set_sync_cursor",
    ] {
        assert!(
            newly_guarded_violations
                .iter()
                .any(|violation| violation.contains(&format!("calls {mutation} "))),
            "direct Store {mutation} call was not rejected"
        );
    }
    let nativepath_authority = r#"
        fn publish(group: &mut NativePathPublicationGroup<'_>) {
            group.upsert_run(&run);
            group.upsert_file_touched(&file);
            group.bind_capture_source_provider_route(source_id, &route);
            group.stage_source_generation_page(&key, &retained);
            group.retire_source_generation_page(&key, generation, limit);
        }
    "#;
    assert!(direct_store_write_violations(
        nativepath_authority,
        Path::new("provider/example/native_path/publication.rs")
    )
    .is_empty());
    let escaped_scope = "fn publish(store: &Store) {
        {
            let mut store = store.begin_native_path_publication_group(admission, accounting);
        }
        store.upsert_event(&event);
    }";
    assert_eq!(
        direct_store_write_violations(
            escaped_scope,
            Path::new("provider/example/native_path/publication.rs")
        )
        .len(),
        1
    );
    let production_cfg = "#[cfg(not(test))]
        fn bypass(store: &Store) { store.upsert_event(&event); }";
    assert_eq!(
        direct_store_write_violations(production_cfg, Path::new("provider/example/native_path.rs"))
            .len(),
        1
    );
    let test_only = "#[cfg(test)] mod tests {
        fn bypass(store: &Store) { store.upsert_event(&event); }
    }";
    assert!(
        direct_store_write_violations(test_only, Path::new("provider/example/native_path.rs"))
            .is_empty()
    );
}

#[path = "routes/cli_architecture.rs"]
mod cli_architecture;
