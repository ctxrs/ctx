//! Optional tool context from an existing snapshot. Never denies or updates a graph.
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::store::Store;
use clap::{Args, ValueEnum};
use serde::Deserialize;
use serde_json::{Value, json};
use tree_sitter::Node;

pub const INPUT_LIMIT: u64 = 64 * 1024;
const FILE_LIMIT: usize = 512;
const GUIDANCE: &str = "A local ctx graph snapshot covers this source scope. For orientation, use ctx graph search \"symbol or topic\", ctx graph show SYMBOL, ctx graph callers SYMBOL, or ctx graph impact SYMBOL. The snapshot may be stale or incomplete; freshness was not checked. Continue reading/searching source whenever useful to verify it. Refresh only when explicitly requested.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Host {
    Claude,
    Codebuddy,
    Gemini,
}

#[derive(Debug, Args)]
pub struct HookGuardArgs {
    #[arg(long, value_enum)]
    pub platform: Host,
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Event {
    tool_name: String,
    tool_input: Value,
    cwd: Option<PathBuf>,
}

/// All malformed, unsupported and unavailable cases are successful no-ops.
/// Gemini requires an explicit allow even when reading input or the index fails.
pub fn run(
    host: Host,
    project: Option<&Path>,
    db: Option<&Path>,
    input: impl Read,
) -> Option<Value> {
    let relevant = relevant(host, project, db, input).unwrap_or(false);
    match (host, relevant) {
        (Host::Gemini, true) => Some(json!({"decision":"allow", "additionalContext":GUIDANCE})),
        (Host::Gemini, false) => Some(json!({"decision":"allow"})),
        (_, true) => Some(json!({"hookSpecificOutput":{
            "hookEventName":"PreToolUse", "additionalContext":GUIDANCE
        }})),
        (_, false) => None,
    }
}

fn relevant(
    host: Host,
    project: Option<&Path>,
    db: Option<&Path>,
    input: impl Read,
) -> Option<bool> {
    let mut bytes = Vec::new();
    input.take(INPUT_LIMIT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > INPUT_LIMIT {
        return None;
    }
    let event: Event = serde_json::from_slice(&bytes).ok()?;
    let fields = event.tool_input.as_object()?;
    let process_cwd = std::env::current_dir().ok();
    let root = project
        .or(event.cwd.as_deref())
        .or(process_cwd.as_deref())?
        .canonicalize()
        .ok()?;
    if !root.is_dir() {
        return None;
    }
    let cwd = event.cwd.as_deref().unwrap_or(&root).canonicalize().ok()?;
    if !cwd.is_dir() || !cwd.starts_with(&root) {
        return None;
    }
    let text = |key: &str| fields.get(key).and_then(Value::as_str);
    let scope = || -> Option<PathBuf> {
        match fields.get("path") {
            None => Some(cwd.clone()),
            Some(value) => scoped(&root, &cwd, value.as_str()?),
        }
    };
    let mut scopes = Vec::new();
    let mut pattern = None;
    let mut exact = false;
    match (host, event.tool_name.as_str()) {
        (Host::Claude | Host::Codebuddy, "Read") | (Host::Gemini, "read_file") => {
            scopes.push(scoped(
                &root,
                &cwd,
                text("file_path").or_else(|| text("path"))?,
            )?);
            exact = true;
        }
        (Host::Claude | Host::Codebuddy, "Glob") => {
            let value = text("pattern")?;
            // Match indexed paths only; glob expansion never walks the source tree.
            if value.len() > 1024
                || Path::new(value).is_absolute()
                || value.split(['/', '\\']).any(|s| s == "..")
            {
                return None;
            }
            pattern = Some(globset::Glob::new(value).ok()?.compile_matcher());
            scopes.push(scope()?);
        }
        (Host::Claude | Host::Codebuddy, "Grep") => {
            if text("pattern")?.is_empty() {
                return None;
            }
            if let Some(value) = fields.get("glob") {
                pattern = Some(globset::Glob::new(value.as_str()?).ok()?.compile_matcher());
            }
            scopes.push(scope()?);
        }
        (Host::Claude | Host::Codebuddy, "Bash") => {
            scopes = shell_scopes(text("command")?, &root, &cwd)?;
        }
        (Host::Gemini, "list_directory") => scopes.push(scope()?),
        _ => return None,
    }
    if scopes.is_empty() {
        return None;
    }
    let db = db
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.join(".graf/index.db"));
    let store = snapshot(&db)?;
    let conn = store.connection().unchecked_transaction().ok()?;
    let (kind, stored_root, generation): (String, String, i64) = conn
        .query_row(
            "SELECT kind,root,generation FROM metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()?;
    if kind != "native" || generation < 1 || Path::new(&stored_root) != root {
        return None;
    }
    for scope in scopes {
        let relative = scope.strip_prefix(&root).ok()?.to_str()?.replace('\\', "/");
        if relative.split('/').any(|s| s == ".graf" || s == ".git") {
            continue;
        }
        if exact || scope.is_file() {
            if !scope.is_file()
                || !source_path(&relative)
                || pattern.as_ref().is_some_and(|p| !p.is_match(&relative))
            {
                continue;
            }
            let found: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE path=?1)",
                    [&relative],
                    |r| r.get(0),
                )
                .ok()?;
            if found {
                return Some(true);
            }
        } else {
            let prefix = if relative.is_empty() {
                String::new()
            } else {
                format!("{relative}/")
            };
            // The primary-key range is bounded even for a very large repository.
            let mut stmt = conn
                .prepare("SELECT path FROM files WHERE path>=?1 AND path<?2 ORDER BY path LIMIT ?3")
                .ok()?;
            let upper = format!("{prefix}\u{10ffff}");
            let mut rows = stmt
                .query(rusqlite::params![prefix, upper, FILE_LIMIT as i64])
                .ok()?;
            while let Some(row) = rows.next().ok()? {
                let file: String = row.get(0).ok()?;
                if source_path(&file)
                    && pattern
                        .as_ref()
                        .is_none_or(|p| p.is_match(file.strip_prefix(&prefix).unwrap_or(&file)))
                {
                    return Some(true);
                }
            }
        }
    }
    Some(false)
}

fn source_path(path: &str) -> bool {
    !path.split('/').any(|s| s == ".graf" || s == ".git")
        && (crate::languages::supports(path) || path.ends_with(".py"))
}

fn scoped(root: &Path, cwd: &Path, value: &str) -> Option<PathBuf> {
    if value.is_empty() || value.contains('\0') {
        return None;
    }
    let path = Path::new(value);
    // Foreign drive forms and rooted paths must never become relative on Unix.
    if !cfg!(windows) && (value.contains('\\') || value.as_bytes().get(1) == Some(&b':')) {
        return None;
    }
    let resolved = cwd.join(path).canonicalize().ok()?;
    resolved.starts_with(root).then_some(resolved)
}

fn snapshot(path: &Path) -> Option<Store> {
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    // Reuse Graf validation and normal SQLite synchronization, including committed
    // WAL records. Install hook limits before all validation and generation reads.
    // Opening read-only never migrates or obtains a writer lock.
    Store::open_read_only_with(path, |conn| {
        conn.busy_timeout(Duration::ZERO)?;
        let mut budget = 100;
        let started = Instant::now();
        conn.progress_handler(
            1000,
            Some(move || {
                budget -= 1;
                budget == 0 || started.elapsed() > Duration::from_millis(100)
            }),
        )?;
        Ok(())
    })
    .ok()
}

fn static_word(node: Node<'_>, source: &str) -> Option<String> {
    let text = node.utf8_text(source.as_bytes()).ok()?;
    match node.kind() {
        "command_name" => static_word(node.named_child(0)?, source),
        "word" if !text.contains(['\\', '$', '`', '*', '?', '[', '~']) => Some(text.to_owned()),
        "raw_string" => Some(text.strip_prefix('\'')?.strip_suffix('\'')?.to_owned()),
        "string" if !text.contains(['\\', '$', '`']) => {
            Some(text.strip_prefix('"')?.strip_suffix('"')?.to_owned())
        }
        _ => None,
    }
}

fn shell_scopes(source: &str, root: &Path, cwd: &Path) -> Option<Vec<PathBuf>> {
    if source.len() > 16 * 1024 {
        return None;
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    if tree.root_node().has_error() {
        return None;
    }
    let mut pending = vec![tree.root_node()];
    let mut result = Vec::new();
    let mut visited = 0;
    while let Some(node) = pending.pop() {
        visited += 1;
        if visited > 2048 {
            return None;
        }
        // Definitions, quoted prose and heredoc data are not executed commands.
        if matches!(
            node.kind(),
            "function_definition" | "heredoc_body" | "raw_string" | "comment"
        ) {
            continue;
        }
        if node.kind() == "command"
            && let Some(name) = static_word(node.child_by_field_name("name")?, source)
        {
            // Do not pretend later commands still run in the original scope.
            if matches!(
                name.as_str(),
                "cd" | "pushd" | "popd" | "eval" | "source" | "."
            ) {
                return None;
            }
            let mut cursor = node.walk();
            let args: Option<Vec<_>> = node
                .children_by_field_name("argument", &mut cursor)
                .map(|n| static_word(n, source))
                .collect();
            if let Some(args) = args
                && let Some(scopes) = search_scopes(&name, &args, root, cwd, 0)
            {
                result.extend(scopes);
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Some(result)
}

fn search_scopes(
    name: &str,
    args: &[String],
    root: &Path,
    cwd: &Path,
    depth: usize,
) -> Option<Vec<PathBuf>> {
    if depth > 16 {
        return None;
    }
    let name = name.rsplit('/').next()?.trim_end_matches(".exe");
    if matches!(name, "command" | "exec" | "env") {
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            if arg == "--" {
                index += 1;
                break;
            }
            if name == "env"
                && (matches!(arg.as_str(), "-i" | "--ignore-environment") || arg.contains('='))
            {
                index += 1;
            } else if arg.starts_with('-') {
                return None;
            } else {
                break;
            }
        }
        return search_scopes(args.get(index)?, &args[index + 1..], root, cwd, depth + 1);
    }
    if name == "git" {
        let mut i = 0;
        let mut base = cwd.to_path_buf();
        while args.get(i).is_some_and(|a| a == "-C") {
            base = scoped(root, &base, args.get(i + 1)?)?;
            i += 2;
        }
        if args.get(i)? != "grep" {
            return None;
        }
        return search_scopes("grep", &args[i + 1..], root, &base, depth + 1);
    }
    if !matches!(
        name,
        "grep" | "egrep" | "fgrep" | "zgrep" | "rg" | "ripgrep" | "ack" | "ag" | "fd" | "find"
    ) {
        return None;
    }
    let mut paths = Vec::new();
    let mut has_pattern = name == "find";
    let mut options = true;
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        i += 1;
        if options && arg == "--" {
            options = false;
            continue;
        }
        if options && arg.starts_with('-') {
            if name == "find" {
                match arg.as_str() {
                    "-name" | "-iname" | "-path" | "-type" | "-maxdepth" | "-mindepth" => {
                        args.get(i)?;
                        i += 1;
                    }
                    "-print" | "-print0" => {}
                    _ => return None,
                }
            } else if matches!(
                arg.as_str(),
                "-e" | "--regexp"
                    | "-g"
                    | "--glob"
                    | "-t"
                    | "--type"
                    | "-A"
                    | "-B"
                    | "-C"
                    | "-m"
                    | "--max-count"
            ) {
                args.get(i)?;
                if matches!(arg.as_str(), "-e" | "--regexp") {
                    has_pattern = true;
                }
                i += 1;
            } else if arg == "--files" && matches!(name, "rg" | "ripgrep") {
                has_pattern = true;
            } else if matches!(
                arg.as_str(),
                "--hidden"
                    | "--no-ignore"
                    | "--line-number"
                    | "--ignore-case"
                    | "--fixed-strings"
                    | "--recursive"
            ) || (arg.starts_with('-')
                && !arg.starts_with("--")
                && arg[1..].chars().all(|c| "rinlqsvwoxHFRE0".contains(c)))
            {
            } else {
                return None;
            }
        } else if !has_pattern {
            has_pattern = true;
        } else {
            paths.push(scoped(root, cwd, arg)?);
        }
    }
    if !has_pattern {
        return None;
    }
    if paths.is_empty() {
        paths.push(cwd.to_path_buf());
    }
    Some(paths)
}
