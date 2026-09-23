//! Explicit, reversible agent guidance, MCP setup, and foreground Git refresh hooks.
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::switch_files;

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Agent host; agents installs the portable Agent Skills format.
    #[arg(long, default_value = "agents")]
    pub platform: String,
    /// Project directory (defaults to the current directory).
    #[arg(long, conflicts_with = "global")]
    pub project: Option<PathBuf>,
    /// Explicitly select the user-global installation.
    #[arg(long)]
    pub global: bool,
    /// Existing Claude, Codex or Hermes configuration root; also select it in the host.
    #[arg(long, requires = "global", conflicts_with = "profile")]
    pub config_root: Option<PathBuf>,
    /// Existing VS Code user-profile directory (locate via MCP: Open User Configuration).
    #[arg(long, requires = "global", conflicts_with = "config_root")]
    pub profile: Option<PathBuf>,
    /// Configure a native stdio MCP server, where supported.
    #[arg(long)]
    pub mcp: bool,
    /// Install graph guidance and CLI usage (the default when no component is selected).
    #[arg(long)]
    pub skill: bool,
    /// Opt in to fail-open source read/search guidance (Claude, CodeBuddy, Gemini projects).
    #[arg(long)]
    pub tool_hooks: bool,
}

#[derive(Debug, Args)]
pub struct HookArgs {
    #[command(subcommand)]
    pub command: HookCommand,
}

#[derive(Debug, Subcommand)]
pub enum HookCommand {
    /// Opt in to foreground graph refresh after commits, checkouts, and merges.
    Install {
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Restore receipt-owned hooks; refuse later edits.
    Uninstall {
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Inspect hook installation without modifying files.
    Status {
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Serialize)]
pub struct SetupReport {
    pub status: String,
    pub platform: String,
    pub scope: PathBuf,
    pub files: Vec<PathBuf>,
    pub notes: Vec<String>,
}

const LIMIT: u64 = 8 * 1024 * 1024;
const BEGIN: &str = "<!-- graf:begin -->";
const GUIDANCE_VERSION: u32 = 2;
const HOSTS: &[&str] = &[
    "agents",
    "claude",
    "codex",
    "cursor",
    "gemini",
    "opencode",
    "kilo",
    "aider",
    "copilot",
    "claw",
    "droid",
    "trae",
    "trae-cn",
    "hermes",
    "kiro",
    "pi",
    "codebuddy",
    "antigravity",
    "kimi",
    "amp",
    "devin",
    "vscode",
];
const GUIDANCE: &str = r#"# ctx graph

Use `ctx graph search "symbol or topic"`, `ctx graph show SYMBOL`, `ctx graph callers SYMBOL`,
`ctx graph callees SYMBOL`, `ctx graph impact SYMBOL`, and `ctx graph path FROM TO` to navigate
the local SQLite snapshot. With MCP enabled, use the ctx graph tools for the same
reads. Use exact returned IDs for ambiguous names, `--json` for structured
results, and `--db PATH` to select a database. Check `truncated`, diagnostics,
and unresolved references. Read source files whenever useful to verify results.
Default graph reads do not check source freshness. Graph reads never rebuild,
fetch URLs or call providers; explicit memory annotations check cited local files.

## Create and refresh

Run `ctx graph index .` for static code and supported local documents; `--code-only`
limits discovery to code/configuration. `ctx graph check-update` compares local
fingerprints without models or converters. `ctx graph update` explicitly refreshes.
`ctx graph watch --interval-ms 1000` is an optional foreground update loop, not a
service. Index/update/watch reuse stored extraction settings. Keep `.graf/`
databases, source caches, and setup receipts out of Git. No watcher is required.

## Add sources and select providers

`ctx graph add FILE --project .` or `ctx graph add URL --name page.html --project .`
imports once and saves extracted facts for later updates. Repeat add to fetch
again. Ordinary queries and updates do not refetch these saved sources.
Google pointers require explicit `--google` and configured gws; OCR uses
`--ocr` with installed Tesseract, media uses `--whisper MODEL` with Whisper/FFmpeg
or explicit `--download-media` with yt-dlp. Do not treat missing converters or
failed downloads as successful extraction; inspect errors and retain provenance.

Semantic extraction is opt-in: `ctx graph provider --project . list` lists provider
choices; `ctx graph provider --project . add NAME settings.json` registers settings.
`ctx graph index . --provider NAME` selects one. Built-in HTTP providers need a full
`--endpoint URL`, a suitable `--model MODEL`, and `--key-env VARIABLE` when needed.
Use `--deep` to additionally enrich code, and `--vision` to allow image upload.
`--code-only --deep` can still send code to a model. Choose providers/endpoints
only for explicitly requested enrichment: calls may disclose source text and
incur costs. Bound work with `--max-semantic-files N` and provider call/token
settings; per-file limits are not a whole-corpus spending limit. Keys belong in
environment variables. Never invent an endpoint or silently enable a provider.
`ctx graph index . --no-semantic` disables stored semantic settings. Check failures,
inferred confidence, evidence, and coverage rather than claiming complete facts.

## Analyze, export, and combine

`ctx graph analyze`, `ctx graph communities`, and `ctx graph hubs --sort pagerank` explicitly
load the complete saved graph and can cost more than bounded navigation.
`--resolution` controls granularity; `--max-community-size` and `--min-cohesion`
are soft split targets. Inspect reported unsatisfied constraints.
`ctx graph report --output report.md` and `ctx graph export html --output graph.html`
create reports; `ctx graph export snapshot-json --output graph.json` saves interchange
data. Other export formats include graphml, cypher, mermaid, svg, canvas,
callflow-html, tree-html, wiki, and obsidian. Wiki/Obsidian need an existing
output directory and create a fresh folder. `ctx graph diagnose multigraph` reports
parallel/mixed edges and collapse risks without changing topology.
`ctx graph label --output labels.json` saves deterministic membership-based labels;
`ctx graph report --labels labels.json` applies only labels whose members still match.
`ctx graph report --check-freshness` explicitly scans native source fingerprints;
without it, coverage describes stored inputs rather than checkout freshness.
`ctx graph benchmark --query SYMBOL --iterations 20` times local bounded SQL reads;
timings are machine/cache dependent and do not compare other graph tools.

`ctx graph global add NAME PROJECT` and `ctx graph global refresh` explicitly rebuild a
stored aggregate. `ctx graph global list` and `ctx graph global query TEXT` use saved data
without opening registered sources. Missing sources abort rebuilds and retain
the previous aggregate. `ctx graph merge --project NAME=PATH --snapshot NAME=FILE
--output NEW_DB` combines named inputs without collapsing source identities.
Analysis, labels, and exports never refresh the original graph implicitly.

Save reviewed useful answers explicitly with `ctx graph save-result --question TEXT
--answer-file FILE --outcome useful --nodes ID`, using exact returned node IDs.
`ctx graph reflect --if-stale` writes local lessons; ordinary queries never save answers
automatically. Add `--memory-dir graf-out/memory` to `ctx graph show SYMBOL` or
`ctx graph report --output report.md` to read observations and check cited sources
without changing graph data, ranking or lessons; the report still writes its
requested output. Inspect stale, unverified and omitted observations.

`ctx graph provider detect --json` inspects local configuration without contacting
providers or verifying authentication. Preview `ctx graph provider template PRESET
--json`; explicitly register `ctx graph provider --project . setup NAME PRESET`.
Registration does not enable extraction; `index --provider NAME` does. Only when
GitHub inspection is requested, use `ctx graph prs --repo OWNER/REPO` or
`ctx graph prs NUMBER --repo OWNER/REPO`. These contact GitHub through authenticated
`gh`; they do not post comments/reviews, merge or change worktrees.

Optional project guidance hooks use `ctx graph install --platform claude --project .
--tool-hooks` (also `codebuddy`). For Gemini with MCP, select `--mcp --tool-hooks`
together. Hooks never deny source access. Uninstall with the same platform,
project and component selection; add `--skill` explicitly if wanted. If Gemini
MCP or hooks are already installed separately, uninstall that selection before
installing both together.

## Guidance updates

Compare the installed guidance version with a newer ctx installation's output.
Rerun `ctx graph install` with the same platform, scope, and component flags to update
receipt-owned guidance. Later edits are refused, not overwritten. Uninstall with
the same selection restores the original pre-install bytes, including after a
guidance upgrade. No source-read restrictions or background services are needed.
"#;

fn guidance() -> String {
    format!(
        "<!-- ctx graph guidance version: {GUIDANCE_VERSION}; executable: {} -->\n{GUIDANCE}",
        env!("CARGO_PKG_VERSION")
    )
}

#[derive(Clone, Copy)]
struct GuidanceStamp {
    revision: u32,
    package: (u64, u64, u64),
}

// Compare numeric release cores, not lexical text. Prerelease/build suffixes
// don't change guidance compatibility; the guidance revision remains explicit.
fn package_core(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split(['-', '+']).next()?.split('.');
    let core = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(core)
}

fn guidance_stamp(bytes: &[u8]) -> Option<GuidanceStamp> {
    let line = bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .find_map(|line| {
            let line = line.trim_end_matches('\r');
            line.strip_prefix("<!-- ctx graph guidance version: ")
                .or_else(|| line.strip_prefix("<!-- graf guidance version: "))
        })?;
    let (revision, package) = line.strip_suffix(" -->")?.split_once("; executable: ")?;
    Some(GuidanceStamp {
        revision: revision.parse().ok()?,
        package: package_core(package)?,
    })
}

fn guidance_direction(stamp: GuidanceStamp) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let current =
        package_core(env!("CARGO_PKG_VERSION")).expect("Cargo package has a semantic version");
    // A newer component always wins: never advise a downgrade when package and
    // guidance revisions point in opposite directions.
    if stamp.package > current || stamp.revision > GUIDANCE_VERSION {
        Ordering::Greater
    } else if stamp.package < current || stamp.revision < GUIDANCE_VERSION {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

fn read_guidance_stamp(path: &Path) -> Option<GuidanceStamp> {
    check_parents(path).ok()?;
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut prefix = Vec::new();
    file.take(4096).read_to_end(&mut prefix).ok()?;
    guidance_stamp(&prefix)
}

/// Read only bounded headers at known skill paths. The caller supplies an
/// already-selected project and prints notices to stderr, never protocol output.
/// Missing, unreadable, unrecognized and matching guidance is silent.
pub fn guidance_notices(project: Option<&Path>) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(project) = project {
        for host in HOSTS {
            if let Ok(path) = skill_path(host, false) {
                candidates.push((*host, "project", project.join(path)));
            }
        }
    }
    if let Ok(home) = home() {
        for host in HOSTS {
            let path = match *host {
                "claude" => std::env::var_os("CLAUDE_CONFIG_DIR")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".claude"))
                    .join("skills/graf/SKILL.md"),
                "hermes" => {
                    let local = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty());
                    let base = std::env::var_os("HERMES_HOME")
                        .filter(|v| !v.is_empty())
                        .map(PathBuf::from)
                        .or_else(|| {
                            hermes_root(&home, cfg!(windows), local.as_deref().map(Path::new)).ok()
                        });
                    let Some(base) = base else {
                        continue;
                    };
                    base.join("skills/graf/SKILL.md")
                }
                _ => {
                    let Ok(path) = skill_path(host, true) else {
                        continue;
                    };
                    home.join(path)
                }
            };
            candidates.push((*host, "global", path));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut notices = Vec::new();
    for (host, scope, path) in candidates {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(stamp) = read_guidance_stamp(&path) else {
            continue;
        };
        let (major, minor, patch) = stamp.package;
        let summary = format!(
            "ctx graph {scope} guidance for {host} was installed by ctx {major}.{minor}.{patch} (guidance revision {}); this executable is {} (revision {GUIDANCE_VERSION}).",
            stamp.revision,
            env!("CARGO_PKG_VERSION")
        );
        match guidance_direction(stamp) {
            std::cmp::Ordering::Less => notices.push(format!("{summary} Installed guidance is older. Rerun `ctx graph install` with the same platform, scope and component flags to update it; later edits will be refused.")),
            std::cmp::Ordering::Greater => notices.push(format!("{summary} Installed guidance is newer. Upgrade the ctx executable before updating guidance to avoid a downgrade.")),
            std::cmp::Ordering::Equal => {}
        }
    }
    notices
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
    /// A hook backup retains the original access permissions, including ACLs.
    permission_source: Option<PathBuf>,
    executable: bool,
    /// Previous installed bytes accepted only while a guidance upgrade is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_after: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    scope: PathBuf,
    changes: Vec<Change>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guidance_version: Option<u32>,
}

fn root(path: Option<&Path>) -> Result<PathBuf> {
    let path = path.unwrap_or(Path::new(".")).canonicalize()?;
    ensure!(path.is_dir(), "scope must be a directory");
    Ok(path)
}

fn home() -> Result<PathBuf> {
    let value = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("cannot determine home directory")?;
    root(Some(Path::new(&value)))
}

fn platform(name: &str) -> Result<&str> {
    let name = match name {
        "skills" => "agents",
        "windows" => "claude",
        "antigravity-windows" => "antigravity",
        other => other,
    };
    ensure!(
        HOSTS.contains(&name),
        "unknown platform {name:?}; use agents for a host supporting Agent Skills"
    );
    Ok(name)
}

// Native skill locations follow the host's discovery contract, not its name.
fn skill_path(host: &str, global: bool) -> Result<&'static str> {
    Ok(match (host, global) {
        ("agents" | "codex" | "amp", false) | ("agents" | "codex", true) => {
            ".agents/skills/graf/SKILL.md"
        }
        ("claude", _) => ".claude/skills/graf/SKILL.md",
        ("cursor", false) => ".cursor/rules/graf.mdc",
        ("cursor", true) => ".cursor/skills/graf/SKILL.md",
        ("gemini", _) => ".gemini/skills/graf/SKILL.md",
        ("opencode", false) => ".opencode/skills/graf/SKILL.md",
        ("opencode", true) => ".config/opencode/skills/graf/SKILL.md",
        ("kilo", _) => ".kilo/skills/graf/SKILL.md",
        ("aider", _) => ".aider/graf.md",
        ("copilot" | "vscode", false) => ".github/skills/graf/SKILL.md",
        ("copilot" | "vscode", true) => ".copilot/skills/graf/SKILL.md",
        ("claw", false) => "skills/graf/SKILL.md",
        ("claw", true) => ".openclaw/skills/graf/SKILL.md",
        ("droid", _) => ".factory/skills/graf/SKILL.md",
        ("trae", _) => ".trae/skills/graf/SKILL.md",
        ("trae-cn", false) => ".trae/skills/graf/SKILL.md",
        ("trae-cn", true) => ".trae-cn/skills/graf/SKILL.md",
        ("hermes", _) => ".hermes/skills/graf/SKILL.md",
        ("kiro", _) => ".kiro/skills/graf/SKILL.md",
        ("pi", false) => ".pi/skills/graf/SKILL.md",
        ("pi", true) => ".pi/agent/skills/graf/SKILL.md",
        ("codebuddy", _) => ".codebuddy/skills/graf/SKILL.md",
        ("antigravity", false) => ".agents/skills/graf/SKILL.md",
        ("antigravity", true) => ".gemini/config/skills/graf/SKILL.md",
        ("kimi", _) => ".kimi/skills/graf/SKILL.md",
        ("amp", true) => ".config/agents/skills/graf/SKILL.md",
        ("devin", false) => ".devin/skills/graf/SKILL.md",
        ("devin", true) => ".config/devin/skills/graf/SKILL.md",
        _ => unreachable!(),
    })
}

fn guidance_path(host: &str, global: bool) -> Option<&'static str> {
    match (host, global) {
        ("aider", _) => Some(".aider.conf.yml"),
        ("claude", false) => Some("CLAUDE.md"),
        ("claude", true) => Some(".claude/CLAUDE.md"),
        ("gemini", false) => Some("GEMINI.md"),
        ("gemini", true) => Some(".gemini/GEMINI.md"),
        ("codex", true) => Some(".codex/AGENTS.md"),
        ("codex" | "agents" | "amp" | "opencode" | "droid", false) => Some("AGENTS.md"),
        _ => None,
    }
}

fn mcp_path(host: &str, global: bool) -> Result<(&'static str, &'static str)> {
    Ok(match (host, global) {
        ("claude", false) => (".mcp.json", "mcpServers"),
        ("claude", true) => (".claude.json", "mcpServers"),
        ("codex", _) => (".codex/config.toml", "mcp_servers"),
        ("cursor", _) => (".cursor/mcp.json", "mcpServers"),
        ("gemini", _) => (".gemini/settings.json", "mcpServers"),
        ("vscode", false) => (".vscode/mcp.json", "servers"),
        ("vscode", true) => ("mcp.json", "servers"),
        _ => bail!(
            "native MCP setup is not verified for {host} in this scope; install --skill and configure `graf serve` in the host manually"
        ),
    })
}

// Hermes' native Windows installer and runtime agree on this default. Keep the
// platform input separate so path selection can be checked without a Windows host.
fn hermes_root(home: &Path, windows: bool, local_appdata: Option<&Path>) -> Result<PathBuf> {
    if windows {
        let base = local_appdata
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join("AppData/Local"));
        ensure!(base.is_absolute(), "LOCALAPPDATA must be an absolute path");
        Ok(base.join("hermes"))
    } else {
        Ok(home.join(".hermes"))
    }
}

fn check_parents(path: &Path) -> Result<()> {
    for parent in path.ancestors().skip(1) {
        match fs::symlink_metadata(parent) {
            Ok(m) => ensure!(
                m.is_dir() && !m.file_type().is_symlink(),
                "directory must not be a symlink: {}",
                parent.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    check_parents(path)?;
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(
            m.is_file() && !m.file_type().is_symlink(),
            "expected regular file: {}",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(LIMIT * 16 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= LIMIT * 16,
        "file exceeds size limit: {}",
        path.display()
    );
    Ok(Some(bytes))
}

fn atomic(
    path: &Path,
    bytes: &[u8],
    expected: Option<&[u8]>,
    source: Option<&Path>,
    executable: bool,
) -> Result<()> {
    ensure!(
        read(path)?.as_deref() == expected,
        "file changed; refusing to replace {}",
        path.display()
    );
    let parent = path.parent().context("file has no parent")?;
    fs::create_dir_all(parent)?;
    check_parents(path)?;
    let staging = tempfile::tempdir_in(parent)?;
    switch_files::protect(staging.path())?;
    let mut temp = tempfile::NamedTempFile::new_in(staging.path())?;
    switch_files::protect(temp.path())?;
    if let Some(source) = source.or_else(|| expected.map(|_| path)) {
        switch_files::preserve_permissions(temp.as_file(), source)?;
    }
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        let mode = temp.as_file().metadata()?.permissions().mode();
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(mode | 0o100))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    ensure!(
        read(path)?.as_deref() == expected,
        "file changed during setup: {}",
        path.display()
    );
    switch_files::replace(temp, path, expected.is_some())
}

fn planned(path: PathBuf, after: Vec<u8>, dedicated: bool) -> Result<Change> {
    let before = read(&path)?;
    ensure!(
        !dedicated || before.is_none(),
        "unowned file already exists: {}",
        path.display()
    );
    ensure!(after.len() as u64 <= LIMIT, "configuration exceeds 8 MiB");
    Ok(Change {
        path,
        before,
        after,
        permission_source: None,
        executable: false,
        previous_after: None,
    })
}

fn edited(path: PathBuf, before: Option<Vec<u8>>, after: Vec<u8>) -> Result<Change> {
    let change = planned(path, after, false)?;
    ensure!(
        change.before == before,
        "file changed while planning installation"
    );
    Ok(change)
}

fn object_fields(bytes: &[u8], host: &str) -> Result<(BTreeMap<String, Range<usize>>, usize)> {
    use jsonc_parser::{CollectOptions, ParseOptions, common::Ranged};
    // VS Code's schema accepts JSONC; Gemini strips comments before JSON.parse.
    // Other hosts retain strict JSON until their contract establishes extensions.
    let parsed = jsonc_parser::parse_to_ast(
        std::str::from_utf8(bytes)?,
        &CollectOptions::default(),
        &ParseOptions {
            allow_comments: matches!(host, "vscode" | "gemini"),
            allow_trailing_commas: host == "vscode",
            allow_loose_object_property_names: false,
            allow_missing_commas: false,
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
        },
    )
    .context("invalid MCP configuration for this host")?;
    let object = parsed
        .value
        .as_ref()
        .and_then(|v| v.as_object())
        .context("MCP configuration section must be an object")?;
    let mut fields = BTreeMap::new();
    for property in &object.properties {
        let range = property.value.range();
        ensure!(
            fields
                .insert(property.name.as_str().to_owned(), range.start..range.end)
                .is_none(),
            "duplicate JSON key; configuration unchanged"
        );
    }
    // Insert the new first member, before all existing content, avoiding any
    // need to move comments or interpret an existing trailing comma ourselves.
    Ok((fields, object.range.start + 1))
}

fn mcp_bytes(
    path: &Path,
    old: Option<&[u8]>,
    key: &str,
    scope: &Path,
    global: bool,
    host: &str,
) -> Result<Vec<u8>> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_str().context("Graf executable path is not UTF-8")?;
    let db = scope.join(".graf/index.db");
    let args: Vec<&str> = if global {
        vec!["serve"]
    } else {
        vec![
            "--db",
            db.to_str().context("database path is not UTF-8")?,
            "serve",
        ]
    };
    if path.extension().is_some_and(|x| x == "toml") {
        let mut doc: toml_edit::DocumentMut = std::str::from_utf8(old.unwrap_or(b""))?.parse()?;
        if doc.get(key).is_none() {
            doc[key] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let table = doc[key]
            .as_table_like_mut()
            .context("mcp_servers must be a table")?;
        ensure!(
            !table.contains_key("graf"),
            "MCP server graf already exists and is not owned by this installation"
        );
        let mut server = toml_edit::Table::new();
        server["command"] = toml_edit::value(exe);
        let mut array = toml_edit::Array::new();
        for arg in args {
            array.push(arg);
        }
        server["args"] = toml_edit::value(array);
        table.insert("graf", toml_edit::Item::Table(server));
        return Ok(doc.to_string().into_bytes());
    }
    let entry = if key == "servers" {
        json!({"type":"stdio", "command":exe,"args":args})
    } else {
        json!({"command":exe,"args":args})
    };
    let old = old.unwrap_or(b"{}\n");
    ensure!(old.len() as u64 <= LIMIT, "MCP config exceeds 8 MiB");
    let (fields, root_start) = object_fields(old, host)?;
    let (position, comma, member) = if let Some(range) = fields.get(key) {
        let (servers, start) = object_fields(&old[range.clone()], host)?;
        ensure!(
            !servers.contains_key("graf"),
            "MCP server graf already exists and is not owned by this installation"
        );
        (
            range.start + start,
            !servers.is_empty(),
            format!("\"graf\": {entry}"),
        )
    } else {
        (
            root_start,
            !fields.is_empty(),
            format!("{}: {{\"graf\": {entry}}}", serde_json::to_string(key)?),
        )
    };
    let mut out = old[..position].to_vec();
    out.extend_from_slice(format!("\n  {member}{}\n", if comma { "," } else { "" }).as_bytes());
    out.extend_from_slice(&old[position..]);
    Ok(out)
}

fn tool_hook_bytes(old: &[u8], host: &str, scope: &Path) -> Result<Vec<u8>> {
    // One fixed executable on PATH; quote only the pinned project argument.
    let project = if cfg!(windows) {
        let value = scope.to_str().context("project path must be UTF-8")?;
        ensure!(
            !value.contains(['"', '%', '!', '$', '`', '\n', '\r', '\0']),
            "project path cannot be quoted safely for this host shell"
        );
        format!("\"{value}\"")
    } else {
        quote(scope)?
    };
    let command = format!("graf hook-guard --platform {host} --project {project}");
    let (event, matcher, timeout) = if host == "gemini" {
        ("BeforeTool", "read_file|list_directory", 10_000)
    } else {
        ("PreToolUse", "Read|Glob|Grep|Bash", 10)
    };
    let entry =
        json!({"matcher":matcher,"hooks":[{"type":"command","command":command,"timeout":timeout}]});
    let (fields, root_start) = object_fields(old, host)?;
    let (position, comma, member) = if let Some(range) = fields.get("hooks") {
        let (events, start) = object_fields(&old[range.clone()], host)?;
        if let Some(array) = events.get(event) {
            let offset = range.start + array.start;
            let bytes = &old[offset..range.start + array.end];
            // The complete configuration was already parsed with host-specific
            // strictness; inspect this array without rewriting its existing bytes.
            let parsed = jsonc_parser::parse_to_ast(
                std::str::from_utf8(bytes)?,
                &Default::default(),
                &Default::default(),
            )?;
            let array = parsed
                .value
                .as_ref()
                .and_then(|v| v.as_array())
                .context("tool hook event must be an array")?;
            ensure!(
                !String::from_utf8_lossy(bytes).contains("graf hook-guard"),
                "unowned Graf tool hook already exists"
            );
            (
                offset + array.range.start + 1,
                !array.elements.is_empty(),
                entry.to_string(),
            )
        } else {
            (
                range.start + start,
                !events.is_empty(),
                format!("{}: [{entry}]", serde_json::to_string(event)?),
            )
        }
    } else {
        (
            root_start,
            !fields.is_empty(),
            format!(
                "\"hooks\": {{{}: [{entry}]}}",
                serde_json::to_string(event)?
            ),
        )
    };
    let mut out = old[..position].to_vec();
    out.extend_from_slice(format!("\n  {member}{}\n", if comma { "," } else { "" }).as_bytes());
    out.extend_from_slice(&old[position..]);
    Ok(out)
}

fn receipt_path(scope: &Path, host: &str, component: &str) -> PathBuf {
    scope
        .join(".graf/setup")
        .join(format!("{host}-{component}.json"))
}

fn aider_config(old: Option<&[u8]>, guidance: &Path) -> Result<Vec<u8>> {
    use serde_yaml_ng::Value as Yaml;
    let old = old.unwrap_or(b"");
    ensure!(
        old.len() as u64 <= LIMIT,
        "Aider configuration exceeds 8 MiB"
    );
    let mut document: Yaml =
        serde_yaml_ng::from_slice(old).context("invalid Aider YAML configuration")?;
    if document.is_null() {
        document = Yaml::Mapping(Default::default());
    }
    document.apply_merge()?;
    let map = document
        .as_mapping_mut()
        .context("Aider configuration must be a YAML mapping")?;
    let key = Yaml::String("read".into());
    let previous = map.get(&key).cloned();
    let mut paths = match previous.clone() {
        None | Some(Yaml::Null) => vec![],
        Some(Yaml::String(path)) => vec![Yaml::String(path)],
        Some(Yaml::Sequence(paths)) if paths.iter().all(Yaml::is_string) => paths,
        _ => bail!("Aider read must be a filename or a list of filenames"),
    };
    let path = Yaml::String(
        guidance
            .to_str()
            .context("Aider guidance path is not UTF-8")?
            .into(),
    );
    if !paths.contains(&path) {
        paths.push(path);
    }
    map.insert(key, Yaml::Sequence(paths.clone()));
    // Most existing configs have no read key: appending preserves every byte.
    // Flow mappings, document terminators, and existing read keys use the YAML
    // parser's semantic merge; the receipt still restores the exact original.
    if previous.is_none() {
        let mut appended = old.to_vec();
        appended
            .extend_from_slice(format!("\nread: {}\n", serde_json::to_string(&paths)?).as_bytes());
        if serde_yaml_ng::from_slice::<Yaml>(&appended)
            .ok()
            .is_some_and(|mut value| value.apply_merge().is_ok() && value == document)
        {
            return Ok(appended);
        }
    }
    Ok(serde_yaml_ng::to_string(&document)?.into_bytes())
}

fn load(path: &Path, scope: &Path, allowed: &[PathBuf]) -> Result<Option<Receipt>> {
    let Some(bytes) = read(path)? else {
        return Ok(None);
    };
    let receipt: Receipt = serde_json::from_slice(&bytes).context("invalid setup receipt")?;
    ensure!(
        receipt.version == 1 && receipt.scope == scope && !receipt.changes.is_empty(),
        "setup receipt scope/version mismatch"
    );
    let mut paths = std::collections::BTreeSet::new();
    for change in &receipt.changes {
        ensure!(
            allowed.contains(&change.path) && paths.insert(&change.path),
            "receipt contains unexpected or duplicate destination"
        );
        ensure!(
            change
                .permission_source
                .as_ref()
                .is_none_or(|s| allowed.contains(s)),
            "receipt contains unexpected permissions source"
        );
    }
    Ok(Some(receipt))
}

// Claude's global MCP file also contains native first-run/account preferences.
// This exception is intentionally limited to uninstalling its unchanged MCP
// member. Install, guidance, hooks, and every other host retain exact receipts.
fn claude_mcp_cleanup(receipt: &mut Receipt) -> Result<()> {
    ensure!(
        receipt.changes.len() == 1,
        "unexpected Claude MCP receipt destinations"
    );
    let change = &mut receipt.changes[0];
    let current = read(&change.path)?;
    if recorded(change, &current) {
        return Ok(());
    }
    let before = strict_claude_json(change.before.as_deref().unwrap_or(b"{}"))?;
    let installed = strict_claude_json(&change.after)?;
    let before_servers = before
        .get("mcpServers")
        .map(|v| v.as_object().context("invalid original MCP server map"))
        .transpose()?;
    ensure!(
        before_servers.is_none_or(|s| !s.contains_key("graf")),
        "receipt does not establish Graf MCP ownership"
    );
    let expected = installed
        .get("mcpServers")
        .and_then(|s| s.get("graf"))
        .filter(|v| v.is_object())
        .context("receipt lacks an owned Graf MCP entry")?;
    let Some(bytes) = current.as_deref() else {
        // The whole configuration was removed by its owner; do not recreate it.
        change.before = None;
        return Ok(());
    };
    let value = strict_claude_json(bytes)?;
    let servers = value
        .get("mcpServers")
        .map(|v| v.as_object().context("invalid current MCP server map"))
        .transpose()?;
    let cleaned = if let Some(owned) = servers.and_then(|s| s.get("graf")) {
        ensure!(
            owned == expected,
            "Graf MCP entry changed after installation; refusing to modify configuration"
        );
        let keys: &[&str] = if before_servers.is_none() && servers.is_some_and(|s| s.len() == 1) {
            &["mcpServers"]
        } else {
            &["mcpServers", "graf"]
        };
        remove_json_property(bytes, keys)?
    } else {
        bytes.to_vec()
    };
    // Only the in-memory undo plan changes. The durable original receipt remains
    // until atomic replacement succeeds. On interruption the next uninstall sees
    // the already-absent owned key and completes without touching unrelated data.
    change.after = bytes.to_vec();
    change.before = Some(cleaned);
    change.previous_after = None;
    Ok(())
}

fn strict_claude_json(bytes: &[u8]) -> Result<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("invalid Claude MCP JSON; configuration unchanged"))?;
    ensure!(
        value.is_object(),
        "Claude MCP configuration must be an object"
    );
    let parsed = jsonc_parser::parse_to_ast(
        std::str::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("invalid Claude MCP JSON encoding"))?,
        &Default::default(),
        &Default::default(),
    )
    .map_err(|_| anyhow::anyhow!("invalid Claude MCP JSON; configuration unchanged"))?;
    let mut pending = vec![parsed.value.as_ref().context("empty Claude MCP JSON")?];
    while let Some(node) = pending.pop() {
        match node {
            jsonc_parser::ast::Value::Object(object) => {
                let mut names = std::collections::BTreeSet::new();
                for property in &object.properties {
                    ensure!(
                        names.insert(property.name.as_str()),
                        "duplicate JSON key; configuration unchanged"
                    );
                    pending.push(&property.value);
                }
            }
            jsonc_parser::ast::Value::Array(array) => pending.extend(&array.elements),
            _ => {}
        }
    }
    Ok(value)
}

fn remove_json_property(bytes: &[u8], keys: &[&str]) -> Result<Vec<u8>> {
    let parsed = jsonc_parser::parse_to_ast(
        std::str::from_utf8(bytes)?,
        &Default::default(),
        &Default::default(),
    )
    .map_err(|_| anyhow::anyhow!("invalid Claude MCP JSON; configuration unchanged"))?;
    let mut object = parsed
        .value
        .as_ref()
        .and_then(|v| v.as_object())
        .context("Claude MCP configuration must be an object")?;
    for key in &keys[..keys.len() - 1] {
        object = object
            .properties
            .iter()
            .find(|p| p.name.as_str() == *key)
            .and_then(|p| p.value.as_object())
            .context("MCP server map is not an object")?;
    }
    let index = object
        .properties
        .iter()
        .position(|p| p.name.as_str() == keys[keys.len() - 1])
        .context("owned MCP entry disappeared")?;
    let property = &object.properties[index];
    let comma = if let Some(next) = object.properties.get(index + 1) {
        bytes[property.range.end..next.range.start]
            .iter()
            .position(|b| *b == b',')
            .map(|i| property.range.end + i)
    } else if index > 0 {
        let start = object.properties[index - 1].range.end;
        bytes[start..property.range.start]
            .iter()
            .position(|b| *b == b',')
            .map(|i| start + i)
    } else {
        None
    };
    ensure!(
        object.properties.len() == 1 || comma.is_some(),
        "invalid MCP member separator"
    );
    let mut ranges: Vec<_> = std::iter::once(property.range.start..property.range.end).collect();
    if let Some(comma) = comma {
        ranges.push(comma..comma + 1);
    }
    ranges.sort_by_key(|r| std::cmp::Reverse(r.start));
    let mut result = bytes.to_vec();
    for range in ranges {
        result.drain(range);
    }
    // Keep all other bytes, including native host formatting and unrelated keys.
    strict_claude_json(&result)?;
    Ok(result)
}

fn recorded(change: &Change, current: &Option<Vec<u8>>) -> bool {
    current.as_deref() == Some(change.after.as_slice())
        || *current == change.before
        || change
            .previous_after
            .as_ref()
            .is_some_and(|old| current.as_deref() == Some(old.as_slice()))
}

fn verify(receipt: &Receipt) -> Result<()> {
    for change in &receipt.changes {
        let current = read(&change.path)?;
        ensure!(
            recorded(change, &current),
            "file changed after installation; refusing to modify {}",
            change.path.display()
        );
        if current.as_deref() == Some(change.after.as_slice()) {
            ensure!(
                executable_matches(change)?,
                "hook permissions changed after installation: {}",
                change.path.display()
            );
        }
    }
    Ok(())
}

fn executable_matches(change: &Change) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if change.executable {
            return Ok(fs::metadata(&change.path)?.permissions().mode() & 0o100 != 0);
        }
    }
    #[cfg(not(unix))]
    let _ = change;
    Ok(true)
}

// A persisted receipt precedes changes. Interrupted operations can be resumed or
// uninstalled; no unrecorded overwrite is needed to recover.
fn apply(path: &Path, receipt: &Receipt) -> Result<bool> {
    verify(receipt)?;
    let mut changed = false;
    let saved = read(path)?;
    let bytes = serde_json::to_vec(receipt)?;
    if saved.as_deref() != Some(bytes.as_slice()) {
        ensure!(
            bytes.len() as u64 <= LIMIT * 16,
            "setup receipt exceeds size limit"
        );
        atomic(path, &bytes, saved.as_deref(), None, false)?;
        changed = true;
    }
    for change in &receipt.changes {
        let current = read(&change.path)?;
        ensure!(
            recorded(change, &current),
            "file changed during setup: {}",
            change.path.display()
        );
        if current.as_deref() != Some(change.after.as_slice()) {
            atomic(
                &change.path,
                &change.after,
                current.as_deref(),
                change.permission_source.as_deref(),
                change.executable,
            )?;
            changed = true;
        }
    }
    if receipt.changes.iter().any(|c| c.previous_after.is_some()) {
        let mut completed = receipt.clone();
        for change in &mut completed.changes {
            change.previous_after = None;
        }
        atomic(
            path,
            &serde_json::to_vec(&completed)?,
            Some(&bytes),
            None,
            false,
        )?;
    }
    Ok(changed)
}

fn undo(path: &Path, receipt: &Receipt) -> Result<()> {
    verify(receipt)?;
    for change in receipt.changes.iter().rev() {
        let current = read(&change.path)?;
        ensure!(
            recorded(change, &current),
            "file changed during uninstall: {}",
            change.path.display()
        );
        if current == change.before {
            continue;
        }
        if let Some(before) = &change.before {
            // Hook originals remain in their sibling backup until the hook has
            // been restored. Use that backup to recover its exact access ACL.
            let backup = change.path.with_file_name(format!(
                "{}.graf-original",
                change.path.file_name().unwrap().to_string_lossy()
            ));
            let source = receipt
                .changes
                .iter()
                .any(|c| c.path == backup)
                .then_some(backup);
            atomic(
                &change.path,
                before,
                current.as_deref(),
                source.as_deref(),
                false,
            )?;
        } else {
            ensure!(
                read(&change.path)? == current,
                "file changed during uninstall"
            );
            fs::remove_file(&change.path)?;
        }
    }
    fs::remove_file(path)?;
    Ok(())
}

struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

fn lock(scope: &Path, installing: bool) -> Result<Lock> {
    let directory = scope.join(".graf/setup");
    check_parents(&directory.join("record"))?;
    fs::create_dir_all(&directory)?;
    // Only this dedicated receipt directory is made private, never the project.
    switch_files::protect(&directory)?;
    let path = directory.join("lock");
    fs::create_dir(&path).context("another setup operation is active; if interrupted, remove .graf/setup/lock before retrying")?;
    let lock = Lock(path);
    if installing {
        let ignore = directory.join(".gitignore");
        if read(&ignore)?.is_none() {
            atomic(&ignore, b"*\n", None, None, false)?;
        }
        // Keep databases and receipts out of normal git add, even before indexing.
        let ignore = scope.join(".graf/.gitignore");
        if read(&ignore)?.is_none() {
            atomic(&ignore, b"*\n", None, None, false)?;
        }
    }
    Ok(lock)
}

fn skill_bytes(host: &str, path: &Path) -> Vec<u8> {
    let header = if host == "aider" {
        ""
    } else if host == "cursor" && path.extension().is_some_and(|x| x == "mdc") {
        "---\ndescription: Navigate and maintain the local ctx code graph\nalwaysApply: true\n---\n\n"
    } else {
        "---\nname: ctx-graph\ndescription: Navigate stored code and document graphs; explicitly add sources, select providers, analyze, export, and refresh with ctx.\n---\n\n"
    };
    format!("{header}{}", guidance()).into_bytes()
}

fn append_guidance(original: Option<&[u8]>) -> Vec<u8> {
    let mut bytes = original.unwrap_or_default().to_vec();
    bytes.extend_from_slice(format!("\n\n{BEGIN}\n{}<!-- graf:end -->\n", guidance()).as_bytes());
    bytes
}

fn upgrade_guidance(receipt: &mut Receipt, host: &str, primary: &Path) -> Result<()> {
    ensure!(
        receipt.guidance_version.unwrap_or(0) <= GUIDANCE_VERSION,
                "installed guidance is newer than this executable; use the newer ctx or uninstall first"
    );
    let stamp = receipt
        .changes
        .iter()
        .find(|change| change.path == primary)
        .and_then(|change| guidance_stamp(&change.after));
    ensure!(
        stamp.is_none_or(|stamp| guidance_direction(stamp) != std::cmp::Ordering::Greater),
        "installed guidance is newer than this executable; upgrade ctx to avoid a downgrade"
    );
    if receipt.guidance_version == Some(GUIDANCE_VERSION)
        && stamp.is_some_and(|stamp| guidance_direction(stamp) == std::cmp::Ordering::Equal)
    {
        return Ok(());
    }
    for change in &mut receipt.changes {
        let after = if change.path == primary {
            skill_bytes(host, primary)
        } else if host == "aider" {
            continue;
        } else {
            append_guidance(change.before.as_deref())
        };
        if change.after != after {
            // Preserve the original pre-install bytes for eventual uninstall;
            // the receipt also admits the exact current bytes during this upgrade.
            let current = read(&change.path)?;
            ensure!(
                recorded(change, &current),
                "file changed during guidance upgrade: {}",
                change.path.display()
            );
            change.previous_after = current.filter(|bytes| Some(bytes) != change.before.as_ref());
            change.after = after;
        }
    }
    receipt.guidance_version = Some(GUIDANCE_VERSION);
    Ok(())
}

fn setup(args: &SetupArgs, remove: bool) -> Result<SetupReport> {
    ensure!(
        !args.global || args.project.is_none(),
        "--global conflicts with --project"
    );
    let host = platform(&args.platform)?;
    ensure!(
        args.global || (args.config_root.is_none() && args.profile.is_none()),
        "--config-root and --profile require --global"
    );
    ensure!(
        args.config_root.is_none() || args.profile.is_none(),
        "--config-root conflicts with --profile"
    );
    ensure!(
        args.config_root.is_none() || matches!(host, "claude" | "codex" | "hermes"),
        "--config-root is supported only for Claude, Codex and Hermes"
    );
    ensure!(
        args.profile.is_none() || host == "vscode",
        "--profile selects a VS Code user-profile directory only"
    );
    ensure!(
        !(host == "vscode" && args.global && args.mcp && args.profile.is_none()),
        "VS Code global MCP requires --profile PATH; locate its directory with MCP: Open User Configuration"
    );
    let default_scope = if args.global {
        home()?
    } else {
        root(args.project.as_deref())?
    };
    let scope = if let Some(selected) = args.config_root.as_deref().or(args.profile.as_deref()) {
        // An explicit host root/profile is never created or resolved by name.
        let metadata = fs::symlink_metadata(selected)
            .context("selected root/profile must be an existing directory")?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "selected root/profile must be a real directory, not a symlink"
        );
        root(Some(selected))?
    } else {
        default_scope.clone()
    };
    let mut report = SetupReport {
        status: "unchanged".into(),
        platform: host.into(),
        scope: scope.clone(),
        files: vec![],
        notes: vec![],
    };
    ensure!(
        !args.tool_hooks || (!args.global && matches!(host, "claude" | "codebuddy" | "gemini")),
        "--tool-hooks supports project installations for claude, codebuddy and gemini only"
    );
    let skill = args.skill || (!args.mcp && !args.tool_hooks);
    let mut groups = Vec::new();
    if skill {
        let primary = if host == "claude" && args.config_root.is_some() {
            scope.join("skills/graf/SKILL.md")
        } else if host == "hermes" && args.global {
            let base = if args.config_root.is_some() {
                scope.clone()
            } else {
                let local_appdata = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty());
                hermes_root(
                    &default_scope,
                    cfg!(windows),
                    local_appdata.as_deref().map(Path::new),
                )?
            };
            base.join("skills/graf/SKILL.md")
        } else {
            // Codex and VS Code discover personal skills outside their config/profile root.
            default_scope.join(skill_path(host, args.global)?)
        };
        let guidance = if host == "claude" && args.config_root.is_some() {
            Some(scope.join("CLAUDE.md"))
        } else if host == "codex" && args.config_root.is_some() {
            Some(scope.join("AGENTS.md"))
        } else {
            guidance_path(host, args.global).map(|p| default_scope.join(p))
        };
        let mut allowed = vec![primary];
        allowed.extend(guidance);
        groups.push(("skill", allowed));
    }
    if args.mcp && !(args.tool_hooks && host == "gemini") {
        let (path, _) = mcp_path(host, args.global)?;
        let path = if host == "claude" && args.config_root.is_some() {
            // Claude retains its legacy config when present, otherwise using
            // .claude.json directly beneath CLAUDE_CONFIG_DIR.
            let legacy = scope.join(".config.json");
            if legacy.try_exists()? {
                legacy
            } else {
                scope.join(".claude.json")
            }
        } else if host == "codex" && args.config_root.is_some() {
            scope.join("config.toml")
        } else {
            scope.join(path)
        };
        groups.push(("mcp", vec![path]));
        if args.global {
            report.notes.push("Global MCP uses graf serve in the host's working directory; the host must launch it in an indexed project.".into());
        }
        if host == "codex" && !args.global {
            report
                .notes
                .push("Codex loads project MCP configuration only for trusted projects.".into());
        }
    }
    if args.tool_hooks {
        let component = if host == "gemini" && args.mcp {
            "mcp-tool-hooks"
        } else {
            "tool-hooks"
        };
        groups.push((
            component,
            vec![scope.join(format!(".{host}/settings.json"))],
        ));
        report.notes.push("Tool hooks provide optional snapshot guidance and never deny source access. Freshness is not checked. Graf must be on the host's PATH.".into());
    }
    if host == "gemini" && (args.mcp || args.tool_hooks) {
        let selected = if args.mcp && args.tool_hooks {
            "mcp-tool-hooks"
        } else if args.mcp {
            "mcp"
        } else {
            "tool-hooks"
        };
        for component in ["mcp", "tool-hooks", "mcp-tool-hooks"] {
            ensure!(
                component == selected || !receipt_path(&scope, host, component).try_exists()?,
                "Gemini settings are already owned by {component}; uninstall that same selection before changing components, then install --mcp --tool-hooks together if both are wanted"
            );
        }
    }
    if skill && host == "cursor" && args.global {
        report.notes.push("Cursor global guidance is a local user skill in ~/.cursor/skills, loaded on demand. Always-on User Rules remain in Cursor's Customize > Rules UI; Graf does not edit them or sync cloud skills.".into());
    }
    if skill && args.global && matches!(host, "codex" | "vscode") {
        report.notes.push("Personal skills remain in the host's documented home-directory skill location and are shared across configurations/profiles; --config-root/--profile selects configuration and receipt storage only.".into());
    }
    if args.config_root.is_some() || args.profile.is_some() {
        report.notes.push("Use the same --global and root/profile selection for reinstall and uninstall. The selected directory holds the receipts; selecting it here does not change the host's active configuration.".into());
    }
    // No global environment overrides are silently ignored into a different home.
    if args.global && !remove && args.config_root.is_none() {
        let override_var = match host {
            "claude" => Some("CLAUDE_CONFIG_DIR"),
            "codex" => Some("CODEX_HOME"),
            "hermes" => Some("HERMES_HOME"),
            _ => None,
        };
        if let Some(var) = override_var {
            ensure!(
                std::env::var_os(var).is_none(),
                "{var} is set; select its existing directory explicitly with --config-root"
            );
        }
    }
    // Validate all groups before making changes; uninstalls never create state.
    if remove && !scope.join(".graf/setup").try_exists()? {
        return Ok(report);
    }
    let _lock = lock(&scope, !remove)?;
    let mut operations = Vec::new();
    for (component, allowed) in groups {
        let path = receipt_path(&scope, host, component);
        let receipt = if let Some(mut receipt) = load(&path, &scope, &allowed)? {
            if remove && args.global && host == "claude" && component == "mcp" {
                claude_mcp_cleanup(&mut receipt)?;
            }
            verify(&receipt)?;
            if component == "skill" && !remove {
                upgrade_guidance(&mut receipt, host, &allowed[0])?;
            }
            receipt
        } else if remove {
            continue;
        } else {
            let mut changes = Vec::new();
            if matches!(component, "mcp" | "tool-hooks" | "mcp-tool-hooks") {
                let old = read(&allowed[0])?;
                let mut after = if component != "tool-hooks" {
                    let (_, key) = mcp_path(host, args.global)?;
                    mcp_bytes(&allowed[0], old.as_deref(), key, &scope, args.global, host)?
                } else {
                    old.clone().unwrap_or_else(|| b"{}\n".to_vec())
                };
                if component != "mcp" {
                    after = tool_hook_bytes(&after, host, &scope)?;
                }
                changes.push(edited(allowed[0].clone(), old, after)?);
            } else {
                changes.push(planned(
                    allowed[0].clone(),
                    skill_bytes(host, &allowed[0]),
                    true,
                )?);
                if let Some(path) = allowed.get(1) {
                    if host == "aider" {
                        let original = read(path)?;
                        let guidance = if args.global {
                            allowed[0].clone()
                        } else {
                            PathBuf::from(".aider/graf.md")
                        };
                        let after = aider_config(original.as_deref(), &guidance)?;
                        changes.push(edited(path.clone(), original, after)?);
                    } else {
                        let original = read(path)?;
                        ensure!(
                            !String::from_utf8_lossy(original.as_deref().unwrap_or_default())
                                .contains(BEGIN),
                            "unowned ctx graph guidance block already exists"
                        );
                        let after = append_guidance(original.as_deref());
                        changes.push(edited(path.clone(), original, after)?);
                    }
                }
            }
            Receipt {
                version: 1,
                scope: scope.clone(),
                changes,
                guidance_version: (component == "skill").then_some(GUIDANCE_VERSION),
            }
        };
        operations.push((path, receipt));
    }
    for (path, receipt) in operations {
        report
            .files
            .extend(receipt.changes.iter().map(|c| c.path.clone()));
        if remove {
            undo(&path, &receipt)?;
            report.status = "uninstalled".into();
        } else if apply(&path, &receipt)? {
            report.status = "installed".into();
        }
    }
    if skill {
        report.notes.push(format!("Guidance version {GUIDANCE_VERSION}; executable {}. Reinstall the same selection after upgrading ctx to refresh owned guidance.", env!("CARGO_PKG_VERSION")));
        report.notes.push("Guidance documents graf on PATH. No source-read restrictions, services, or background watchers were installed.".into());
    }
    Ok(report)
}

pub fn install(args: &SetupArgs) -> Result<SetupReport> {
    setup(args, false)
}
pub fn uninstall(args: &SetupArgs) -> Result<SetupReport> {
    setup(args, true)
}

fn git(project: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "Git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?
        .trim_end_matches(['\r', '\n'])
        .to_owned())
}

fn quote(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .context("hook executable path must be UTF-8")?;
    ensure!(
        !value.contains(['\n', '\r', '\0']),
        "hook executable path contains a line break or NUL"
    );
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

const HOOKS: &[&str] = &["post-commit", "post-checkout", "post-merge"];

pub fn hook(args: &HookArgs) -> Result<SetupReport> {
    let (project, action) = match &args.command {
        HookCommand::Install { project } => (project, "install"),
        HookCommand::Uninstall { project } => (project, "uninstall"),
        HookCommand::Status { project } => (project, "status"),
    };
    let selected = root(project.as_deref())?;
    let scope = root(Some(Path::new(&git(
        &selected,
        &["rev-parse", "--show-toplevel"],
    )?)))?;
    let raw_hooks = PathBuf::from(git(
        &scope,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )?);
    let mut hooks = PathBuf::new();
    for part in raw_hooks.components() {
        match part {
            std::path::Component::ParentDir => {
                hooks.pop();
            }
            std::path::Component::CurDir => {}
            part => hooks.push(part.as_os_str()),
        }
    }
    let common = PathBuf::from(git(
        &scope,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .canonicalize()?;
    check_parents(&hooks.join("post-commit"))?;
    // A project selection must not mutate a user-global hooks directory.
    ensure!(
        hooks.starts_with(&scope) || hooks.starts_with(&common),
        "hooks directory is outside this repository; global/shared core.hooksPath is not supported"
    );
    let allowed: Vec<_> = HOOKS
        .iter()
        .flat_map(|name| {
            [
                hooks.join(name),
                hooks.join(format!("{name}.graf-original")),
            ]
        })
        .collect();
    let path = receipt_path(&scope, "git", "hooks");
    let mut report = SetupReport {
        status: "not-installed".into(),
        platform: "git".into(),
        scope: scope.clone(),
        files: vec![],
        notes: vec![],
    };
    if action != "install" && read(&path)?.is_none() {
        return Ok(report);
    }
    let _lock = if action == "status" {
        None
    } else {
        Some(lock(&scope, action == "install")?)
    };
    let saved = load(&path, &scope, &allowed)?;
    if action == "status" {
        if let Some(receipt) = saved {
            report.files = receipt.changes.iter().map(|c| c.path.clone()).collect();
            report.status = if receipt.changes.iter().all(|c| {
                read(&c.path).ok().flatten().as_deref() == Some(&c.after)
                    && executable_matches(c).unwrap_or(false)
            }) {
                "installed"
            } else {
                "modified"
            }
            .into();
        }
        return Ok(report);
    }
    if action == "uninstall" {
        if let Some(receipt) = saved {
            undo(&path, &receipt)?;
            report.status = "uninstalled".into();
        }
        return Ok(report);
    }
    let receipt = if let Some(receipt) = saved {
        receipt
    } else {
        let exe = quote(&std::env::current_exe()?)?;
        let mut changes = Vec::new();
        for name in HOOKS {
            let path = hooks.join(name);
            let original = read(&path)?;
            ensure!(
                read(&hooks.join(format!("{name}.graf-original")))?.is_none(),
                "unowned hook backup already exists"
            );
            if let Some(original) = &original {
                let mut backup = planned(
                    hooks.join(format!("{name}.graf-original")),
                    original.clone(),
                    true,
                )?;
                backup.permission_source = Some(path.clone());
                changes.push(backup);
            }
            let checkout = if *name == "post-checkout" {
                "[ \"${3:-}\" = 0 ] && exit \"$graf_hook_status\"\n"
            } else {
                ""
            };
            let script = format!(
                "#!/bin/sh\n# graf managed refresh hook\ngraf_hook_status=0\nif [ -x \"$0.graf-original\" ]; then\n  \"$0.graf-original\" \"$@\" || graf_hook_status=$?\nfi\n{checkout}graf_root=$(git rev-parse --show-toplevel) || exit \"$graf_hook_status\"\nif [ -f \"$graf_root/.graf/index.db\" ]; then\n  {exe} --db \"$graf_root/.graf/index.db\" update || printf '%s\\n' 'graf: refresh failed; run graf update manually' >&2\nfi\nexit \"$graf_hook_status\"\n"
            );
            let mut change = edited(path, original, script.into_bytes())?;
            change.executable = true;
            changes.push(change);
        }
        Receipt {
            version: 1,
            scope: scope.clone(),
            changes,
            guidance_version: None,
        }
    };
    report.files = receipt.changes.iter().map(|c| c.path.clone()).collect();
    report.status = if apply(&path, &receipt)? {
        "installed"
    } else {
        "unchanged"
    }
    .into();
    report.notes.push("Refresh runs in the foreground only when .graf/index.db exists. Existing executable hooks are chained; their exit status is preserved. Hooks never stage or commit files.".into());
    Ok(report)
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn guidance_version_order_is_numeric_and_newer_revisions_win() {
        assert!(package_core("0.10.0").unwrap() > package_core("0.9.27").unwrap());
        assert_eq!(package_core("1.2.3-rc.1+build.2"), package_core("1.2.3"));
        assert!(package_core("not-a-version").is_none());
        assert!(package_core("1.2.3.4").is_none());
        assert_eq!(
            guidance_direction(GuidanceStamp {
                package: (0, 0, 0),
                revision: GUIDANCE_VERSION + 1
            }),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn notice_headers_skip_nonregular_files_and_never_read_past_limit() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("SKILL.md");
        let header = "<!-- graf guidance version: 1; executable: 0.0.0 -->\n";
        fs::write(&skill, header).unwrap();
        assert!(read_guidance_stamp(&skill).is_some());
        fs::write(&skill, format!("{}\n{header}", "x".repeat(4096))).unwrap();
        assert!(read_guidance_stamp(&skill).is_none());
        assert!(read_guidance_stamp(temp.path()).is_none());
        #[cfg(unix)]
        {
            let link = temp.path().join("linked.md");
            std::os::unix::fs::symlink(&skill, &link).unwrap();
            assert!(read_guidance_stamp(&link).is_none());
        }
    }

    #[test]
    fn hermes_platform_roots_use_native_locations() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("synthetic home");
        let appdata = temp.path().join("custom local app data");
        for (windows, local, expected) in [
            (false, Some(appdata.as_path()), home.join(".hermes")),
            (true, Some(appdata.as_path()), appdata.join("hermes")),
            (true, None, home.join("AppData/Local/hermes")),
        ] {
            assert_eq!(hermes_root(&home, windows, local).unwrap(), expected);
        }
        assert!(hermes_root(&home, true, Some(Path::new("relative"))).is_err());
    }
}
