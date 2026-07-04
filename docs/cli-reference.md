# CLI Reference

ctx is a local CLI for indexing and searching agent session history.

## Global Options

```bash
ctx --data-root /tmp/ctx status
CTX_DATA_ROOT=/tmp/ctx ctx status
```

`--data-root` overrides the default ctx root for every command. The environment
variable `CTX_DATA_ROOT` provides the same value. The root is used directly; ctx
does not append another product directory.

## Setup And Health

```bash
ctx setup
ctx setup --catalog-only
ctx setup --json
ctx setup --progress json --json
ctx status
ctx status --json
ctx doctor
ctx doctor --json
```

- `setup` creates the data root, opens or creates `work.sqlite`, writes
  `config.toml` when needed, discovers known provider history locations,
  catalogs Codex sessions, imports discovered native provider sources, optimizes
  the local search index, and prints next steps. It does not execute
  history-source plugin commands.
- `setup --catalog-only` stops after discovery/cataloging. It is useful for
  fast inventory or troubleshooting, but it does not make history searchable.
- `status` reports the ctx root, database path, config path, indexed item
  count, indexed source count, catalog session counters, initialization state,
  and local-only marker.
- `doctor` opens local storage and reports validation findings.

Setup and health checks do not change shell startup files, install repository
integrations, write into source repositories, call model APIs, or require API
keys. Core storage checks use the configured data root, and JSON stdout remains
structured. Installer-managed binaries can run a signed background upgrade
check after successful non-JSON commands; that check is separate from provider
history indexing.

## Agent Skill

```bash
ctx skill install
ctx skill install --agent codex --agent claude-code
ctx skill install --all-agents
ctx skill install --project
ctx skill install --force
ctx skill status
ctx skill status --agent codex --json
```

`skill install` installs or refreshes ctx's bundled
`ctx-agent-history-search` skill. With no target flags in an interactive
terminal, it opens a small agent picker with the universal `~/.agents/skills`
location selected plus detected agent-specific folders for tools that need
them. In non-interactive runs, it installs to the universal folder and also
writes detected agent-specific folders, such as Claude Code, only when ctx sees
evidence that the agent is installed. `--agent` targets native global skill
folders for supported agents such as Claude Code, Codex, Cursor, OpenCode,
Gemini CLI, Antigravity, GitHub Copilot, Pi, and Goose. `--all-agents` writes
all supported target folders. `--project` switches from global paths to the
current project's skill folders.

`skill status` reports whether the bundled skill is `current`, `stale`,
`modified`, or `missing`. `skill install` refreshes stale bundled copies
automatically, but it refuses to overwrite locally modified skill files unless
you pass `--force`. The command only manages the bundled ctx skill and does not
fetch arbitrary remote skills.

## Sources

```bash
ctx sources
ctx sources --json
```

`sources` lists provider history locations that ctx knows how to check on this
machine. Current rows include:

- Codex session trees at `~/.codex/sessions`;
- Codex prompt history at `~/.codex/history.jsonl`;
- Pi session JSONL files under `~/.pi/agent/sessions`;
- native rows for supported Antigravity, Claude, OpenCode, Kilo Code,
  ForgeCode, OpenClaw, Hermes, Gemini, Cursor, Zed, Kiro CLI, Copilot CLI,
  Factory AI Droid, Qwen Code, Kimi Code CLI, Autohand Code, iFlow CLI,
  CodeBuddy, Cline, and Roo Code local history locations;
- preview rows for NanoClaw project roots and AstrBot SQLite history when those
  paths are discoverable;
- local history-source plugin manifests under `$CTX_DATA_ROOT/plugins` or
  `CTX_HISTORY_PLUGIN_PATH`.

Native JSON rows include `provider`, `path`, `exists`, `source_format`,
`status`, `import_support`, `native_import`, `importable`, `raw_retention`, and
any `unsupported_reason`. Plugin JSON rows use
`kind: "history_source_plugin"` and include `plugin`, `history_source`,
`provider_key`, `source_id`, `manifest_path`, and `enabled`. Invalid installed
plugin manifests appear as non-importable plugin rows with `status: "invalid"`
and an `error`. `sources` reads path metadata and plugin manifests, writes
nothing to provider files or source repositories, and does not execute plugin
commands.

## Import

```bash
ctx import
ctx import --all
ctx import --provider codex
ctx import --provider pi
ctx import --provider antigravity
ctx import --provider claude
ctx import --provider opencode
ctx import --provider forgecode
ctx import --provider mistral-vibe
ctx import --provider mux
ctx import --provider reasonix
ctx import --provider kode
ctx import --provider neovate
ctx import --provider terramind
ctx import --provider openclaw
ctx import --provider hermes
ctx import --provider nanoclaw --path /path/to/nanoclaw-project
ctx import --provider astrbot --path /path/to/data/data_v4.db
ctx import --provider shelley --path ~/.config/shelley/shelley.db
ctx import --provider continue --path ~/.continue/sessions
ctx import --provider openhands --path ~/.openhands
ctx import --provider gemini
ctx import --provider cursor
ctx import --provider zed
ctx import --provider kiro-cli
ctx import --provider copilot-cli
ctx import --provider factory-ai-droid
ctx import --provider qwen-code
ctx import --provider kimi-code-cli
ctx import --provider autohand-code
ctx import --provider iflow-cli
ctx import --provider lingma
ctx import --provider codebuddy
ctx import --provider aider-desk
ctx import --provider codex --path ~/.codex/sessions
ctx import --provider pi --path ~/.pi/agent/sessions
ctx import --format ctx-history-jsonl-v1 --path ./history.jsonl
ctx import --history-source example-agent/default
ctx import --history-source-manifest ./ctx-history-plugin.json
ctx import --history-source example-agent/default --reset-cursor
ctx import --resume
ctx import --json
ctx import --progress json --json
```

`import` explicitly indexes provider history into the local SQLite store. The
normal first-run path is `ctx setup`, which already imports discovered native
provider sources.
Use `import` to repair, re-run, resume, or target a specific provider/path. It
creates the data root and default config if needed, reads provider transcript
files, and writes indexed source metadata, sessions, events, searchable text,
citations, and import totals to SQLite.

Custom history can be imported from an explicit JSONL file with
`--format ctx-history-jsonl-v1 --path <file>`. This path is not discovered or
remembered as a provider home; see `docs/custom-history-import-format.md` for
the schema and incremental semantics.

History-source plugins are local commands that stream `ctx-history-jsonl-v1` to
stdout. Use `--history-source <selector>` for an explicit plugin import, or
`--history-source-manifest <path>` to test a manifest without installing it.
Selectors are exact `plugin/source` or
`provider_key/source_id` values. `--reset-cursor` withholds the previous plugin
cursor for that run and asks the plugin to perform a full rescan. See
`docs/history-source-plugins.md`.

Import selection rules:

- with no arguments, import discovered native sources that exist;
- with `--all`, import discovered native sources that exist and enabled
  history-source plugin sources;
- with `--provider`, import discovered sources for that provider;
- with `--format ctx-history-jsonl-v1 --path <file>`, import that custom
  history JSONL file;
- with `--history-source`, import matching local plugin sources;
- with `--history-source-manifest`, import sources from that manifest path;
- with `--provider <provider> --path <path>`, import exactly that native
  provider path.

Preview providers such as NanoClaw and AstrBot are not included in `--all` or
pre-search refresh. Import them explicitly with `--provider` when discovery
finds the desired source, or add `--path` to target a specific source, then
search the existing index.

The current `--resume` flag is an idempotent-rescan mode marker. JSON reports
`resume: true` and `resume_mode: "idempotent_rescan"`, but provider-native
cursor resume is not a universal contract yet.

## Show And Locate

```bash
ctx show session <ctx-session-id>
ctx show session <ctx-session-id> --mode full --format text
ctx show session <ctx-session-id> --mode log --format jsonl
ctx show session <ctx-session-id> --format markdown --out transcript.md
ctx show session <ctx-session-id> --mode full --format markdown --out transcript.md
ctx show event <ctx-event-id> --window 3 --format text
ctx show event <ctx-event-id> --before 5 --after 10 --format json
ctx locate session <ctx-session-id>
ctx locate event <ctx-event-id>
```

`show session` renders one transcript by ctx-owned session ID. It defaults to
`--mode lite`, a compact agent-readable transcript with user messages and final
assistant messages. `--mode full` keeps all user/assistant/system message
events, and `--mode log` renders all imported events including tool and command
activity. `--format` accepts `text`, `markdown`, `json`, or `jsonl`. Without
`--out`, `show session` writes to stdout. With `--out`, it writes the rendered
transcript artifact to that path and prints nothing on success.

`show event` renders one ctx-owned event hit. `--before` and `--after` include
neighboring events in the same session; `--window N` is shorthand for
`--before N --after N`. It accepts the same output formats as `show session`.

`locate session` and `locate event` print provenance metadata: ctx IDs,
provider, provider-owned session IDs, source path and cursor, source
availability, import fidelity, and resume/cursor metadata when available.

Provider-owned IDs are metadata, not positional IDs. Positional session and
event arguments are ctx-owned IDs. To look up a provider-owned session, use an
explicit provider lookup such as `--provider codex --provider-session
<provider-session-id>` on commands that support provider lookup.

JSON output may expose local paths, event payloads, and compatibility field
names from the current store schema, so treat it as private local data.

## Search

```bash
ctx search "build failure"
ctx search "sqlite storage" --provider codex
ctx search "retry handling" --workspace checkout --since 60d
ctx search "tool output" --event-type tool_output
ctx search --file crates/foo/src/lib.rs
ctx search "token budget" --refresh off
ctx search "signed metadata" --term checksum --term release
ctx search "token budget" --limit 5
ctx search "token budget" --session <ctx-session-id>
ctx search "review findings" --include-subagents
ctx search "this current task" --include-current-session
ctx search "release notes" --history-source example-agent/default
ctx search "release notes" --provider-key example-agent --source-id default
```

`search` defaults to `--refresh auto`, which quietly refreshes discovered native
provider sources and enabled auto history-source plugins before querying indexed
sessions and events. The refresh is best-effort and keeps JSON stdout reserved
for the search result object. On large discovered sources or already-cataloged
indexes, `auto` serves current results without a foreground catch-up scan; use
`--refresh strict` or `ctx import --all` when you need a full catch-up before
querying. Use `--refresh off` to search the existing index without refreshing, or
`--refresh strict` to fail when the pre-search refresh cannot run or import
successfully. Preview native sources such as NanoClaw and AstrBot are searched
from the existing index until they are explicitly imported through a supported
path. Search requires a non-empty query, at least one non-empty `--term`, or
`--file <path>`; provider, workspace, time, session, event, source, and result
flags only narrow an actual search. Default results are session-diverse: ctx
returns the strongest matching span from each session, plus
`more_matches_in_session` and `session_importance` when more indexed events from
that session also matched. Use `--session <ctx-session-id>` after a default
search has identified a session to inspect; scoped session search returns dense
event hits. Session/event commands accept full ctx IDs or unambiguous ctx ID
prefixes of at least eight hex characters. Use `--events` without `--session`
for dense event-level results across sessions. Repeat
`--term <query-or-keyword>` when you want to broaden a search across several
related words or phrases and merge the ranked results; `--term` is OR-style
broadening, not a must-include filter.
Custom history imports can be filtered by `--history-source` using
`plugin/source` or `provider_key/source_id`, or by exact `--provider-key`,
`--source-id`, and `--source-format` values. These filters imply
`--provider custom` and cannot be combined with another provider.
Default search excludes subagent sessions so primary human-agent intent and
decisions stay prominent. Use `--include-subagents` when implementation details,
code review notes, test output, or failure analysis from subagent sessions
should be searched too.

When ctx is run from Codex and `CODEX_THREAD_ID` is available, search excludes
the active Codex session tree by default so the current task and its subagents
do not dominate historical retrieval. Use `--include-current-session` to opt
back in. Use `--refresh off` for a strictly read-only query over the existing
ctx index.

Results are local hits over indexed history. Event hits include `ctx_event_id`;
hits with known session context include `ctx_session_id`; provider metadata
including `provider_session_id` is included when known. Results also include
title, snippet, rank, result scope, match reasons, source-path/cursor data,
citations, `suggested_next_commands`, a JSON `freshness` object, and
pagination/truncation fields in JSON. Default text output is compact and
optimized for agent reading; use `--verbose` for expanded text diagnostics.

Filters:

- `--provider codex|pi|claude|opencode|kilo|kiro-cli|forgecode|mistral-vibe|mux|reasonix|kode|neovate|terramind|crush|goose|dexto|lingma|openclaw|hermes|nanoclaw|astrbot|shelley|continue|openhands|antigravity|gemini|cursor|zed|copilot-cli|factory-ai-droid|qwen-code|kimi-code-cli|autohand-code|iflow-cli|codebuddy|aider-desk|cline|roo|custom`;
- `--workspace <name-or-path>`, substring match over stored workspace, cwd,
  source path, or repository-name text;
- `--since <rfc3339-or-days>d`, for example `2026-06-01T00:00:00Z` or `30d`;
- `--event-type <event-type>`, one of `message`, `tool_call`, `tool_output`,
  `command_started`, `command_output`, `command_finished`, `file_touched`,
  `vcs_change`, `artifact`, `summary`, or `notice`;
- `--file <path>`, indexed touched-file path metadata, not the current
  filesystem;
- `--session <ctx-session-id-or-prefix>`, for dense event results within one session;
- `--term <query-or-keyword>`, repeatable broadening terms merged with OR-style semantics;
- `--events`, for dense event-level results instead of the default session-diverse results;
- `--include-subagents`;
- `--limit <n>`, capped at `200`;
- `--refresh auto|off|strict`;
- `--include-current-session`.

CLI provider filters use kebab-case names. JSON output and stable SQL views use
provider IDs in ctx output; multiword IDs may be snake_case, such as
`copilot_cli`, `factory_ai_droid`, `qwen_code`, `kimi_code_cli`,
`autohand_code`, `kiro_cli`, `iflow_cli`, `mistral_vibe`, or `aider_desk`,
while compact IDs such as `forgecode`, `mux`, `reasonix`, `kode`, `neovate`,
`terramind`, `openclaw`, `nanoclaw`, `astrbot`, `shelley`, `continue`, and
`openhands` stay compact.

`search` reads discovered native provider files and runs enabled auto
history-source plugin commands for pre-search refresh, then queries SQLite. It
may write newly discovered provider or plugin history into the local index before
querying.

## SQL

```bash
ctx sql "SELECT COUNT(*) AS sessions FROM ctx_sessions"
ctx sql "SELECT provider, COUNT(*) AS sessions FROM ctx_sessions GROUP BY provider"
ctx sql --file query.sql --format json
cat query.sql | ctx sql - --format csv
ctx sql "SELECT ctx_session_id FROM ctx_sessions LIMIT 5" --format raw
```

`sql` runs one read-only SQL statement against the existing local ctx SQLite
index. It does not create or migrate the store, refresh provider history, import
sources, run background upgrade checks, or write provider files or source
repositories. If the store is missing or uses an old schema, run `ctx setup`,
`ctx import`, or `ctx status` first.

Prefer stable read-only `ctx_*` views for scripts:

- `ctx_sessions`, one row per indexed session;
- `ctx_events`, one row per indexed event;
- `ctx_files_touched`, one row per normalized touched-file record;
- `ctx_sources`, one row per cataloged provider source session.

Advanced users can query internal tables directly, but internal table details
are not the compatibility surface. SQL output is private local history and can
include transcript payloads, paths, and source metadata.

Formats:

- default `table` output is compact and intended for humans and agents;
- `--format json` or `--json` returns a structured result with `columns`,
  array rows, limits, truncation flags, `read_only: true`, and
  `share_safe: false`;
- `--format csv` prints a CSV header unless `--no-header` is set;
- `--format raw` requires exactly one selected column and prints one value per
  line for piping.

Limits default to `--max-rows 100`, `--max-columns 64`,
`--max-value-bytes 512`, `--max-sql-bytes 65536`, and `--timeout 10s`.
SQLite-side value allocation is also bounded, so very large generated values
can fail before result truncation. `--timeout` accepts values such as `250ms`,
`5s`, or `1m`, capped at one minute.

## Docs

```bash
ctx docs
ctx docs list
ctx docs list --json
ctx docs search "upgrade"
ctx docs search "file path" --limit 5 --json
ctx docs show cli-reference
ctx docs show search --format text
ctx docs show json-contracts --format json
ctx docs man --print ctx
ctx docs man --out ~/.local/share/man/man1
```

`docs` exposes a curated copy of the public ctx docs inside the binary. It is
intended for humans and agents that need local command help without opening the
website. `docs list`, `docs search`, and `docs show` read embedded text and do
not touch provider history or the local SQLite index. `docs show --out PATH`
writes one embedded topic to that explicit path. `docs man --print PAGE` prints
one generated man page to stdout; `docs man --out DIR` writes generated
section-1 man pages for `ctx` and its public subcommands.

Agents should usually use `ctx docs search` or `ctx docs show` rather than
shelling through `man`, because the docs commands return concise markdown/text
that is easier for agents to quote and inspect.

## MCP

```bash
ctx mcp serve
```

`mcp serve` starts a read-only MCP server over newline-delimited stdio JSON-RPC.
It exposes tools for `status`, `sources`, `search`, `sql`, `show_session`, and
`show_event`. The MCP search and SQL tools query the existing index only; they
do not refresh or import provider history. Tool results include MCP text
content plus `structuredContent` JSON. Treat all MCP output as private local
history: it may include absolute paths, source metadata, snippets, transcript
text, and raw SQL result fields, and the MCP host may log or forward tool
output.

MCP search follows the same active Codex session-tree exclusion as the CLI when
`CODEX_THREAD_ID` is set. Pass `include_current_session: true` to the search
tool when the active session tree itself is the target.

The MCP server is optional. The CLI remains the primary interface, and MCP is
intended for agents or hosts that prefer tool discovery over shell commands.

## Upgrade

```bash
ctx upgrade status
ctx upgrade status --json
ctx upgrade check
ctx upgrade check --json
ctx upgrade --dry-run
ctx upgrade
ctx upgrade disable
ctx upgrade enable
```

`upgrade` checks and applies signed ctx CLI releases for binaries installed by
the official hosted installer. The installer writes a sidecar marker next to the
binary, such as `~/.local/bin/ctx.install.json`, recording the managed install
path, platform, version, channel, binary SHA-256, metadata URL, and artifact
URL. Source builds, `cargo install`, package-manager installs, copied binaries,
and mismatched sidecars are treated as unmanaged and will not self-upgrade.
`ctx upgrade status --json` also reports the current executable and every `ctx`
binary found on `PATH`, with warnings when an older binary shadows the managed
install or multiple `ctx` binaries are present.

Official installer-managed installs default to background auto-upgrade after
successful normal commands when signed release metadata explicitly allows
auto-upgrade. Background checks never run for `--json` commands, MCP, `ctx
docs`, `ctx upgrade`, CI, or unmanaged installs. They write state and logs under
the ctx data root and do not write to stdout or stderr. Use `CTX_UPGRADE_OFF=1`
or `CTX_DISABLE_AUTO_UPGRADE=1` for process-level opt-out, or `ctx upgrade
disable` to write `upgrade.auto = "off"` in `config.toml`.

Manual `ctx upgrade` can print progress and errors. It verifies signed release
metadata, explicit self-upgrade policy, artifact SHA-256, the current managed
install marker, and the staged binary's `ctx --version` output before replacing
the installed binary. On Windows, replacement may be scheduled by a helper that
finishes after the running `ctx.exe` exits; JSON reports `status: "scheduled"`
and `applied: false` until replacement completes.

## Progress Output

`setup` and `import` accept `--progress auto|plain|json|none`. `auto` writes
plain progress only to an interactive stderr and stays quiet for `--json` or
non-interactive stderr. `--progress json` writes newline-delimited progress
objects to stderr. It does not change stdout, so command result JSON remains a
single object when `--json` is also present.

Progress JSON is a best-effort operation stream. Each object has
`type: "ctx_progress"` plus `operation`, `phase`, `message`,
`completed_bytes`, `total_bytes`, `percent`, `elapsed_seconds`, `eta_seconds`,
`completed_files`, `total_files`, `imported_events`, and `done`.

## JSON Contract

JSON output is intended for local scripts, harnesses, and exact field
extraction. It is private unless a user explicitly reviews and redacts it.

Structured output is available for:

```text
ctx setup --json
ctx status --json
ctx sources --json
ctx import --json
ctx show session <ctx-session-id> --format json
ctx show event <ctx-event-id> --format json
ctx locate session <ctx-session-id> --format json
ctx locate event <ctx-event-id> --format json
ctx search <query>|--term <term>|--file <path> --json
ctx sql "SELECT COUNT(*) FROM ctx_sessions" --json
ctx docs list --json
ctx docs search <query> --json
ctx docs show <topic> --format json
ctx upgrade --json
ctx upgrade check --json
ctx upgrade status --json
ctx doctor --json
```

See [contracts/json.md](contracts/json.md) for the current field-level contract
and known compatibility limits.
