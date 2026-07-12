# CLI Reference

ctx is a local CLI for indexing and searching agent session history.

## Global Options

```bash
ctx --data-root /tmp/ctx status
CTX_DATA_ROOT=/tmp/ctx ctx status
ctx --quiet setup
CTX_QUIET=1 ctx status
```

`--data-root` overrides the default ctx root for every command. The environment
variable `CTX_DATA_ROOT` provides the same value. The root is used directly; ctx
does not append another product directory.

`--quiet` suppresses successful human status/onboarding output for `setup` and
top-level `status`. `CTX_QUIET=1` provides the same default for scripts and
installer wrappers. JSON output, errors, and command results from commands such
as `search`, `show`, `sources`, and `docs` are not suppressed.

## Setup And Health

```bash
ctx setup
ctx setup --catalog-only
ctx setup --no-daemon
ctx setup --json
ctx setup --progress json --json
ctx status
ctx status --json
ctx doctor
ctx doctor --json
ctx daemon status
ctx daemon status --json
ctx daemon run
ctx daemon run --once --json
ctx daemon disable
ctx daemon enable
```

- `setup` creates the data root, opens or creates `work.sqlite`, discovers known
  provider history locations, inventories local history sources, imports
  discovered native provider sources, optimizes the local search index, and
  prints next steps. It does not write `config.toml` for implicit defaults and
  does not execute history-source plugin commands. When `[daemon].enabled` is
  true, setup may opportunistically start the ctx-owned background daemon after
  foreground setup work completes. Use `setup --no-daemon` for a one-run opt-out.
- `setup --catalog-only` stops after source discovery and inventory. The flag
  name is kept for compatibility; it is useful for fast troubleshooting, but it
  does not make history searchable and does not autostart daemon maintenance.
- `setup --quiet` performs setup without printing success status lines, import
  summaries, data-root details, or get-started tips. It still exits nonzero and
  prints errors on failure.
- `status` reports the ctx root, database path, config path, indexed item
  count, indexed source count, inventory counters, legacy Codex catalog
  counters, semantic coverage, daemon enabled/coordinator state,
  initialization state, local-only marker, and read-only marker. It does not
  initialize, migrate, or repair the store.
- `status --quiet` performs the same local checks but prints nothing on
  success. Use `status --json` when scripts need the actual state.
- `doctor` opens local storage and reports validation findings, including
  semantic sidecar/worker and daemon lock/status problems when present.
- `daemon status` reports the same ctx-owned daemon coordinator state without
  mutating storage.
- `daemon run` runs bounded local maintenance in the foreground. That means
  bounded native provider-history refresh followed by semantic catch-up when
  semantic is enabled. The daemon may acquire the local embedding model for
  semantic indexing. A looping daemon keeps the embedding model resident after
  cold start and performs recent-work freshness checks before settling into idle
  loops; cloud sync remains disabled with `enabled: false` and
  `network_allowed: false`.
- `daemon disable` and `daemon enable` update `[daemon].enabled` in
  `config.toml`. The prerelease default is disabled, so daemon maintenance is an
  explicit opt-in; `daemon run --force` overrides a disabled config for explicit
  manual troubleshooting.

Setup and health checks do not change shell startup files, install repository
integrations, write into source repositories, call model APIs, or require API
keys. Without semantic opt-in they do not download embedding models; with
semantic enabled, daemon maintenance may acquire the local embedding model.
Daemon maintenance is local-only and bounded; cloud sync remains disabled. Core
storage checks use the configured data root, and JSON stdout remains structured.
JSON-output commands do not autostart daemon maintenance. Installer-managed binaries can run a signed
background upgrade check after successful non-JSON commands other than
`ctx status`; that check is
separate from provider history indexing.

## Agent Skill

```bash
ctx integrations install skills
ctx integrations install skills --agent codex --agent claude-code --agent mimocode
ctx integrations install skills --all-agents
ctx integrations install skills --project
ctx integrations install skills --force
ctx integrations status skills
ctx integrations status skills --agent codex --json
```

`integrations install skills` installs or refreshes ctx's bundled
`ctx-agent-history-search` skill. With no target flags in an interactive
terminal, it opens a small agent picker with the universal `~/.agents/skills`
location selected plus detected agent-specific folders for tools that need
them. In non-interactive runs, it installs to the universal folder and also
writes detected agent-specific folders, such as Claude Code, only when ctx sees
evidence that the agent is installed. `--agent` targets native global skill
folders for supported agents such as Claude Code, Codex, Cursor, OpenCode,
MiMo Code, Gemini CLI, Antigravity, GitHub Copilot, Pi, and Goose.
`--all-agents` writes all supported target folders. `--project` switches from
global paths to the current project's skill folders.

`integrations status skills` reports whether the bundled skill is `current`,
`stale`, `modified`, or `missing`. `integrations install skills` refreshes
stale bundled copies automatically, but it refuses to overwrite locally
modified skill files unless you pass `--force`. The command only manages the
bundled ctx skill and does not fetch arbitrary remote skills.

## Integrations

```bash
ctx integrations install mcp
ctx integrations install mcp --agent codex
ctx integrations install mcp --agent mimocode
ctx integrations install mcp --provider cursor --project
ctx integrations install mcp --all-agents --json
ctx integrations install mcp --agent cursor --force
ctx integrations status mcp
ctx integrations status mcp --agent codex --json
ctx integrations install slash-commands
ctx integrations install slash-commands --agent opencode
ctx integrations install slash-commands --agent mimocode
ctx integrations install slash-commands --agent gemini-cli --project
ctx integrations install slash-commands --agent qwen-code
ctx integrations install slash-commands --agent windsurf
ctx integrations install slash-commands --all-agents
ctx integrations install slash-commands --force
ctx integrations install slash-commands --json
```

`integrations install mcp` adds a local MCP server named `ctx` to supported
coding-agent client configs. The server command is `ctx mcp serve`. With no
target flags, it installs for supported agents detected on the machine.
`--agent` targets one or more coding-agent clients, and `--provider` is accepted
as an alias for compatibility with provider-oriented workflows. `--project`
writes a project-scoped MCP config when that agent has a documented project
config location; without explicit agent flags, project mode only targets
project MCP config locations that already exist.

The MCP installer parses structured config files, preserves unrelated settings,
and is idempotent. If a config already contains a `ctx` MCP server with a
different command or args, install reports a conflict and leaves the file
untouched unless `--force` is passed. Invalid JSON, JSONC, TOML, or YAML configs are
reported and left untouched. `integrations status mcp` reports `current`,
`missing`, `conflict`, `invalid_config`, or `unsupported`.

`integrations install slash-commands` installs a `/ctx-history` entry point only
for providers where ctx has a documented, file-based command surface it can
manage safely: OpenCode, MiMo Code, Gemini CLI, Qwen Code, and Windsurf. With no
explicit agent flag, it writes detected file-based targets only. `--project`
installs into the current repository's command folder instead of the user/global
folder.

The installer writes `.ctx-slash-commands.json` metadata beside generated
command files. Re-running the command is idempotent, stale ctx-owned files are
refreshed automatically, and locally modified command files are preserved unless
you pass `--force`.

For Codex, Claude Code, Cursor, GitHub Copilot CLI, Pi, and other skill-first
agents, use `ctx integrations install skills`; those providers expose the
bundled skill through their own skill invocation surface rather than a separate
`/ctx-history` command file. See `ctx docs show slash-command-integrations` for
the provider matrix and rationale.

Run `ctx docs show mcp-integrations` for the MCP support matrix, config paths,
and manual snippets.

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
- native rows for supported Claude Code, Codex, Cursor, Pi, GitHub Copilot CLI, OpenCode, MiMo Code, Gemini CLI/Antigravity, Kilo Code, Kiro CLI, Crush, Goose, Tabnine, Windsurf, Zed, Factory AI Droid, Qwen Code, Kimi Code CLI, Auggie, Junie, Firebender, ForgeCode, Deep Agents, Mistral Vibe, Mux, Rovo Dev, Cline, Roo Code, Lingma, Qoder, Warp, CodeBuddy, Trae, OpenClaw, Hermes, NanoClaw, AstrBot, Shelley, Continue, and OpenHands local history locations;
- AstrBot `data_v4.db` history when those files exist;
- explicit-import rows for NanoClaw project roots when those paths are discoverable;
- local history-source plugin manifests under `$CTX_DATA_ROOT/plugins` or
  `CTX_HISTORY_PLUGIN_PATH`.

Native JSON rows include `provider`, `path`, `exists`, `source_format`,
`status`, `import_support`, `native_import`, `importable`, and any
`unsupported_reason`. Plugin JSON rows use
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
ctx import --provider mimocode
ctx import --provider forgecode
ctx import --provider deepagents
ctx import --provider mistral-vibe
ctx import --provider mux
ctx import --provider rovodev
ctx import --provider junie
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
ctx import --provider windsurf
ctx import --provider lingma
ctx import --provider codebuddy
ctx import --provider trae
ctx import --provider codex --path ~/.codex/sessions
ctx import --provider pi --path ~/.pi/agent/sessions
ctx import --format ctx-history-jsonl-v1 --path ./history.jsonl
ctx import --history-source example-agent/default
ctx import --history-source-manifest ./ctx-history-plugin.json
ctx import --history-source example-agent/default --reset-cursor
ctx import --resume
ctx import --partial
ctx import --no-daemon
ctx import --json
ctx import --progress json --json
```

`import` explicitly indexes provider history into the local SQLite store. The
normal first-run path is `ctx setup`, which already imports discovered native
provider sources.
Use `import` to repair, re-run, resume, or target a specific provider/path. It
creates the data root if needed, reads provider transcript files, and writes
indexed source metadata, sessions, events, searchable text, citations, and
import totals to SQLite. It does not write `config.toml` for implicit defaults.

Imports are source-atomic by default. If a source contains malformed rows, ctx
reports that source as failed and does not commit the valid rows from that same
source. Use `--partial` only when you explicitly want ctx to commit valid rows
and report malformed or skipped rows in the import summary.

When `[daemon].enabled` is true, `import` may opportunistically start a short
one-pass ctx-owned maintenance profile after the foreground import finishes.
The daemon work is local-only: bounded native provider-history refresh plus
semantic indexing when semantic is enabled. It may acquire the local embedding
model for semantic indexing, but it does not enable cloud sync. Use
`import --no-daemon` for a one-run opt-out. Custom
JSONL imports, explicit history-source-only imports, and `import --json` do not
autostart daemon maintenance.

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

NanoClaw is explicit-import only and is not included in `--all` or pre-search
refresh. Import it with `--provider` when discovery finds the desired source, or
add `--path` to target a specific source, then search the existing index. AstrBot
`data_v4.db` sources are supported for bounded default locations and remain
available for explicit `--path` imports.

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
ctx search "mail provider throttled bulk mailbox setup" --backend hybrid
ctx search "pricing for ctx cloud team history" --backend semantic
ctx search "release notes" --history-source example-agent/default
ctx search "release notes" --provider-key example-agent --source-id default
```

`search` defaults to `--refresh background`, which serves the existing index and
lets the ctx daemon refresh lexical and semantic indexes in the background when
daemon maintenance is enabled. If daemon maintenance is disabled, `background`
uses the bounded foreground text-refresh path for discovered native provider
sources and enabled auto history-source plugins. Semantic retrieval reads
existing local sidecar coverage when it is already available, and freshness is
visible through `ctx status`, `ctx index status`, and the search JSON
`retrieval.worker` report. ctx does not initialize or download embedding models
during search, does not create the semantic sidecar from the query path, and
does not start semantic indexing. Use `--refresh off` to search the existing
index without refreshing or scheduling semantic work, or `--refresh wait` to run
foreground text refresh and fail when it cannot complete. Foreground refresh skips isolated
malformed history records with a warning and commits valid records; source-level and system-level
failures remain fatal. Explicit-only native sources such as
NanoClaw, plus search-only sources without native import support, are searched
from the existing index until they are explicitly imported through a supported
path. Supported AstrBot `data_v4.db` locations participate in bounded native
discovery and may also be imported with an explicit `--path`. Search requires a
non-empty query, at least one non-empty `--term`, or
`--file <path>`; provider, workspace, time, session, event, source, and result
flags only narrow an actual search. Default results are session-diverse: ctx
returns the strongest matching span from each session, plus
`more_matches_in_session` and `session_importance` when more indexed events from
that session also matched. Use `--session <ctx-session-id>` after a default
search has identified a session to inspect; scoped session search returns dense
event hits. Session/event commands accept full ctx IDs or unambiguous ctx ID
prefixes of at least eight hex characters. Use `--events` without `--session`
for dense event-level results across sessions. Lexical search matches any word
in an ordinary multi-word query and ranks results matching more query words
ahead of partial matches. Repeat
`--term <query-or-keyword>` when you want to broaden a search across several
related queries or keywords and merge the ranked results; `--term` is OR-style
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
ctx index. Explicit semantic or hybrid requests may read an existing semantic
sidecar under `--refresh off`, but the command does not update the sidecar or
download models. They may ask the daemon query service to embed the query from
an already-cached local model.

Results are local hits over indexed history. Event hits include `ctx_event_id`;
hits with known session context include `ctx_session_id`; provider metadata
including `provider_session_id` is included when known. Results also include
title, snippet, rank, result scope, match reasons, source-path/cursor data,
citations, `suggested_next_commands`, a JSON `freshness` object, a JSON
`retrieval` object with backend, semantic coverage, worker status, and semantic
timing/scan diagnostics when vector retrieval runs, and pagination/truncation
fields in JSON. Default text output is compact and optimized for agent reading;
use `--verbose` for expanded text diagnostics.

Filters:

- `--provider codex|pi|claude|opencode|kilo|kiro-cli|crush|goose|antigravity|gemini|tabnine|cursor|windsurf|zed|copilot-cli|factory-ai-droid|qwen-code|kimi-code-cli|auggie|junie|firebender|forgecode|deepagents|mistral-vibe|mux|rovodev|openclaw|hermes|nanoclaw|astrbot|shelley|continue|openhands|cline|roo|lingma|qoder|warp|codebuddy|trae|custom`;
- `--workspace <name-or-path>`, substring match over stored workspace, cwd,
  source path, or repository-name text;
- `--since <rfc3339-or-days>d`, for example `2026-06-01T00:00:00Z` or `30d`;
- `--event-type <event-type>`, one of `message`, `tool_call`, `tool_output`,
  `command_started`, `command_output`, `command_finished`, `file_touched`,
  `vcs_change`, `artifact`, `summary`, or `notice`;
- `--file <path>`, indexed touched-file path metadata, not the current
  filesystem;
- `--session <ctx-session-id-or-prefix>`, for dense event results within one session;
- `--term <query-or-keyword>`, repeatable broadening queries or keywords merged with OR-style semantics;
- `--events`, for dense event-level results instead of the default session-diverse results;
- `--backend hybrid|semantic|lexical`, where `hybrid` blends lexical and
  semantic evidence only when existing sidecar coverage is complete and dirty
  work is drained, and falls back to lexical with a structured fallback reason
  when semantic prerequisites are missing. Explicit `semantic` reports a local
  error instead of downloading a model during search when the cache is missing,
  when the semantic worker is actively indexing, when filters/terms cannot be
  honored, or when the installed build target does not include a compatible
  local embedding/vector backend for the requested sidecar;
- `--semantic-weight <0.0-1.0>`, for hybrid ranking;
- `--include-subagents`;
- `--limit <n>`, capped at `200`;
- `--refresh background|off|wait`;
- `--include-current-session`.

CLI provider filters use kebab-case names. JSON output and stable SQL views use
provider IDs in ctx output; multiword IDs may be snake_case, such as
`copilot_cli`, `factory_ai_droid`, `qwen_code`, `kimi_code_cli`, `kiro_cli`, `mistral_vibe`, and `roo_code`; compact IDs such as `forgecode`, `deepagents`, `mux`, `rovodev`, `openclaw`, `nanoclaw`, `astrbot`, `shelley`, `continue`, and `openhands` stay compact.

`search` reads discovered native provider files and runs enabled auto
history-source plugin commands for pre-search text refresh, then queries SQLite.
Default daemon maintenance owns native and plugin refresh when enabled. If the
daemon is disabled, `--refresh background` bounds native and plugin work for
interactive use; run `--refresh wait` or `ctx import` for exhaustive foreground
plugin catch-up. Foreground refresh may write newly discovered provider or
plugin history into the local `work.sqlite` index before querying. Semantic retrieval reads the
`vectors.sqlite` sidecar when it already exists; search itself does not start
semantic indexing or download models. With semantic enabled and default
background refresh, search may start the configured daemon so the daemon-owned
query service can embed the query; `--refresh off` skips that autostart. Use
`ctx daemon run` for explicit foreground local native history refresh and
semantic catch-up. JSON status includes a top-level `semantic` object with worker
`status`, `running`, `pid`, heartbeat/error timestamps, `indexed_chunks`, and a
`coverage` object with `searchable_items`, `embedded_items`, `embedded_chunks`,
`dirty_items`, `queued_items_estimate`, and `coverage_ratio`, plus the private
local sidecar and worker status paths. JSON status also includes a top-level
`daemon` object
with coordinator status and `history_refresh`/`semantic_index`/`cloud_sync` job
state; cloud sync is disabled with `enabled: false` and
`network_allowed: false`. `ctx doctor` is the diagnostic surface for semantic
sidecar, worker, and daemon health.

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
repositories. If the store is missing or uses an old schema, run a writable
command such as `ctx setup` or `ctx import` first.

Prefer stable read-only `ctx_*` views for scripts:

- `ctx_sessions`, one row per indexed session;
- `ctx_events`, one row per indexed event;
- `ctx_files_touched`, one row per normalized touched-file record;
- `ctx_sources`, one row per indexed provider source session.

Advanced users can query internal tables directly, but internal table details
are not the compatibility surface. SQL output is private local history and can
include transcript payloads, paths, and source metadata.

Formats:

- default `table` output is compact and intended for humans and agents;
- `--format json` or `--json` returns a structured result with `columns`,
  array rows, limits, truncation flags, and `read_only: true`;
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
ctx integrations install mcp
```

`mcp serve` starts a read-only MCP server over newline-delimited stdio JSON-RPC.
It exposes tools for `status`, `sources`, `search`, `sql`, `show_session`, and
`show_event`. The MCP search and SQL tools query the existing index only; they
do not refresh or import provider history, and MCP search currently uses the
lexical search path only. Tool results include MCP text content plus
`structuredContent` JSON. Treat all MCP output as private local history: it may
include absolute paths, source metadata, snippets, transcript
text, and raw SQL result fields, and the MCP host may log or forward tool
output.

MCP search follows the same active Codex session-tree exclusion as the CLI when
`CODEX_THREAD_ID` is set. Pass `include_current_session: true` to the search
tool when the active session tree itself is the target.

The MCP server is optional. The CLI remains the primary interface, and MCP is
intended for agents or hosts that prefer tool discovery over shell commands.
Use `ctx integrations install mcp` to add the server to supported coding-agent
MCP configs.

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
auto-upgrade. Background checks never run for `ctx status`, `--json` commands,
MCP, `ctx docs`, `ctx sql`, `ctx upgrade`, CI, or unmanaged installs. They write
state and logs under the ctx data root and do not write to stdout or stderr. Use
`CTX_UPGRADE_OFF=1` or `CTX_DISABLE_AUTO_UPGRADE=1` for process-level opt-out,
or `ctx upgrade disable` to write `upgrade.auto = "off"` in `config.toml`.

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
extraction. It is private unless a user explicitly reviews it.

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
ctx integrations install mcp --json
ctx integrations status mcp --json
ctx upgrade --json
ctx upgrade check --json
ctx upgrade status --json
ctx doctor --json
```

See [contracts/json.md](contracts/json.md) for the current field-level contract
and known compatibility limits.
