# CLI Reference

ctx is a local CLI for importing and searching agent session history. Provider
histories remain the acquisition authority for import and refresh. Core/Tantivy
lexical search lives under `search/lexical`, optional flat-F32 semantic data
lives under `search/semantic`, and typed presentation reads complete stored Core
records.

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
ctx setup --pro
ctx setup --no-daemon
ctx setup --format json
ctx setup --progress json --format json
ctx status
ctx status --format json
ctx stats
ctx stats --detail
ctx stats --format json
ctx status --usage enable
ctx status --usage disable
ctx status --usage reset
ctx doctor
ctx doctor --format json
ctx index
ctx index --format json
ctx index mode
ctx index mode auto
ctx index mode manual
ctx index watch
ctx index wait
ctx semantic enable
ctx semantic enable --wait
ctx semantic status
ctx semantic disable
ctx daemon run
```

- `setup` creates the data root, discovers known provider history locations,
  scans current sources, builds and atomically publishes the Core/Tantivy
  generation, and prints next steps. It never opens, migrates, or deletes
  pre-v0.26 history. Old Store files are ignored and may be removed explicitly
  by their owner. Setup does not write `config.toml` for implicit defaults or
  execute history-source plugin commands. In automatic indexing mode, setup
  may opportunistically start the persistent ctx-owned daemon. In manual mode
  setup never starts a worker. Its concise human summary lists only agent
  histories that contributed indexed content and warns instead of claiming
  clean completion when a provider root is partial, excluded, or unknown. Use
  `setup --no-daemon` for a one-run opt-out.
- `setup --quiet` performs setup without printing success status lines, import
  summaries, data-root details, or get-started tips. It still exits nonzero and
  prints errors on failure.
- `setup --catalog-only` remains accepted only for command-line compatibility.
  It is deprecated and ignored; setup follows the same refresh lifecycle as
  when the flag is omitted.
- `status` reports the ctx root, source epoch, lexical and refresh readiness,
  semantic generation and coverage, daemon and supervisor health,
  initialization state, compact local usage health, local-only marker, and
  read-only marker. Human output uses contributing agent histories, provider
  roots, sessions, messages, tool calls, and processed history bytes; it does
  not expose internal source-key cardinality. Partial coverage distinguishes
  source failures from rejected records and keeps healthy prior history
  searchable. JSON retains the existing `indexed_sources` meaning and is the
  exact diagnostic drilldown. Status does not initialize or repair Core or
  semantic state or open old history.
- `stats` is the read-only, local, offline report for History retrieval, Code
  provenance, Measured delivery, and Estimated savings. Measured facts and
  model-based estimates are separate in JSON; the estimate model and
  coefficients are versioned. Delivery keeps exact CLI output, MCP transport,
  and one-copy semantic/context byte channels separate; transport bytes never
  drive context-token or savings estimates. Byte-derived token values include
  coverage and remain unavailable for unmeasured legacy rows rather than
  becoming false zeros. Completed companion-backed Blame appears with calls,
  technical success/failure, and duration; Core does not classify private
  results, and CLI Blame output is shown as unavailable. `stats --detail` adds
  CLI/MCP operation and latency breakdowns. Reporting is uncounted and does not
  create `usage.sqlite` on a pristine root.
- `status --usage disable` and `enable` write the canonical `[local_usage]
  enabled` override; `status --usage reset` atomically clears the usage
  aggregates. These controls are not counted and remain action-focused so they
  do not depend on a successful Core status read. Local usage is default-on
  product state in the separate owner-private `usage.sqlite` sidecar, has no
  network path or network-delivery identity, and remains independent of remote
  reporting controls.
- `status --quiet` performs the same local checks but prints nothing on
  success. Use `status --format json` when scripts need the actual state.
- `doctor` turns source-epoch, refresh, semantic, and daemon problems into a
  focused recovery action. It does not repeat the normal status inventory.
- `index` prints a one-shot indexing status view. It is the focused view of the
  current indexing mode, lexical publication, refresh progress, semantic
  coverage, and background process state. Use `--format json` for the
  readiness snapshot.
- `index mode` prints the current mode. Its setters persist the requested mode
  and reconcile supervision to the effective mode. When auto is not overridden,
  ctx installs or repairs supervision and starts the persistent background
  daemon. Manual mode stops it and removes supervision. In manual mode,
  `ctx import` and `ctx search --refresh wait` can still start finite workers
  for explicit refreshes.
- `index watch` redraws indexing progress until the default readiness target is
  met; `--format jsonl` emits one snapshot per line. `index wait` blocks until
  the selected lexical, semantic, or combined readiness target is met and
  supports one final `--format json` result.
- `semantic enable` persists the explicit semantic-search opt-in. In auto mode
  it starts or recovers daemon-owned model acquisition and semantic catch-up;
  `--wait` waits for the current projection. `semantic status` is read-only.
  `semantic disable` opts out without deleting downloaded model/runtime assets
  or derived semantic indexes. Plain enablement in manual mode does not change
  indexing mode.
- `daemon run` is an advanced command that runs persistent local maintenance in
  the foreground and blocks until stopped. It does not change the configured
  indexing mode. In manual mode, pass `--force` to run it explicitly. Each pass
  performs bounded native provider-history refresh followed by semantic
  catch-up when semantic is enabled. The daemon may acquire the local embedding
  model for semantic indexing. A looping daemon keeps the embedding model
  resident after cold start, reloads daemon/semantic configuration between
  cycles, and performs recent-work freshness checks before settling into idle
  loops.

Automatic indexing is the default. Its canonical configuration is
`[indexing] mode = "auto"`; the other supported value is `"manual"`.
Lexical search remains available while embeddings build, and hybrid search uses
lexical and semantic evidence automatically when semantic coverage is ready.

Setup and health checks do not change shell startup files, install repository
integrations, write into source repositories, call model APIs, or require API
keys. Without semantic opt-in they do not download embedding models; with
semantic enabled, daemon maintenance may acquire the local embedding model.
Each daemon maintenance pass is bounded and local. Core storage checks use the
configured data root, and JSON stdout remains structured.
Output format does not change lifecycle authority. Use `--no-daemon` or search
`--refresh off` for an invocation-level opt-out. The full automatic persistent
daemon is the sole driver for signed automatic upgrade checks. Without that
driver, including manual and source-refresh-only modes, no automatic upgrade
work runs. Ordinary foreground commands and finite indexing workers do not own
upgrade checks; explicit `ctx upgrade` remains available.

## Agent Skill

```bash
ctx integrations install skills
ctx integrations install skills --agent codex --agent claude-code --agent mimocode
ctx integrations install skills --all-agents
ctx integrations install skills --project
ctx integrations install skills --force
ctx integrations status skills
ctx integrations status skills --agent codex --format json
```

`integrations install skills` installs or refreshes ctx's bundled `ctx` skill.
With no target flags in an interactive
terminal, it opens a small agent picker with the universal `~/.agents/skills`
location selected plus detected agent-specific folders for tools that need
them. Safe existing `ctx` or managed `ctx-agent-history-search` copies in
recognized agent folders are also preselected. In non-interactive runs, it
maintains those safe existing copies alongside the universal folder and any
detected agent-specific folders, such as Claude Code, that are needed. Once a
picker selection is submitted, or when `--agent` or `--all-agents` is used,
only the selected folders are managed. `--agent` targets native global skill
folders for supported agents such as Claude Code, Codex, Cursor, OpenCode,
MiMo Code, Gemini CLI, Antigravity, GitHub Copilot, Pi, and Goose.
`--all-agents` writes all supported target folders. `--project` switches from
global paths to the current project's skill folders.

`integrations status skills` reports whether the bundled skill is `current`,
`stale`, `modified`, or `missing`. `integrations install skills` refreshes
stale bundled copies automatically, but it refuses to overwrite locally
modified skill files unless you pass `--force`. The command only manages the
bundled ctx skill and does not fetch arbitrary remote skills. Without target
flags, status uses the same default maintenance set as install.

The 1.0 installer performs a one-way migration from a managed
`ctx-agent-history-search` directory to `ctx`. It preserves a locally edited
legacy skill unless `--force` is passed.

## Integrations

```bash
ctx integrations install mcp
ctx integrations install mcp --agent codex
ctx integrations install mcp --agent mimocode
ctx integrations install mcp --provider cursor --project
ctx integrations install mcp --all-agents --format json
ctx integrations install mcp --agent cursor --force
ctx integrations status mcp
ctx integrations status mcp --agent codex --format json
ctx integrations install slash-commands
ctx integrations install slash-commands --agent opencode
ctx integrations install slash-commands --agent mimocode
ctx integrations install slash-commands --agent gemini-cli --project
ctx integrations install slash-commands --agent qwen-code
ctx integrations install slash-commands --all-agents
ctx integrations install slash-commands --force
ctx integrations install slash-commands --format json
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

`integrations install slash-commands` installs a `/ctx` entry point only
for providers where ctx has a documented, file-based command surface it can
manage safely: OpenCode, MiMo Code, Gemini CLI, and Qwen Code. With no
explicit agent flag, it writes detected file-based targets only. `--project`
installs into the current repository's command folder instead of the user/global
folder.

The installer writes `.ctx-slash-commands.json` metadata beside generated
command files. Re-running the command is idempotent, stale ctx-owned files are
refreshed automatically, and locally modified command files are preserved unless
you pass `--force`.

The 1.0 installer also migrates a managed `/ctx-history` file to `/ctx`. It
preserves a locally edited legacy command unless `--force` is passed.

For Codex, Claude Code, Cursor, GitHub Copilot CLI, Pi, and other skill-first
agents, use `ctx integrations install skills`; those providers expose the
bundled skill through their own skill invocation surface rather than a separate
`/ctx` command file. See `ctx docs show slash-command-integrations` for
the provider matrix and rationale.

Run `ctx docs show mcp-integrations` for the MCP support matrix, config paths,
and manual snippets.

## Sources

```bash
ctx sources
ctx sources --format json
ctx sources add personal --provider claude --root ~/.claude-personal --source-group personal
ctx sources add work --provider codex --root ~/.codex-work --source-group work
ctx sources add work --provider codex --root ~/.codex-relocated --source-group work --replace
ctx sources add openhands-cli --provider openhands --root ~/.openhands/conversations --kind current-conversations
ctx sources remove personal
```

`sources` lists bounded provider history locations selected for this machine.
Provider precedence is winner-only: an environment or persistent-config
replacement suppresses its lower-priority default. Current coexisting installed
surfaces or persisted profiles may produce separate rows. One-shot imports and
unconfigured automatic locations that are old, moved, or unreconstructible
require an exact `--path` and are not remembered. Most users need no source
configuration. For a provider with an enabled configured-root capability,
`sources add` registers an existing provider history root under a stable local
name in `config.toml`; `sources remove` removes that definition. The provider
capability determines whether the root
must be a file or directory; the
[provider support matrix](provider-support-matrix.json) publishes that state
and, when enabled, the path kind and expansion strategy for every provider.
Configured roots are
added to the provider's ordinary environment/default winner, and every distinct
configured root is indexed. Registering the already inferred root gives it a name and optional
group without indexing it twice. Other providers keep their ordinary discovery
behavior.

A named root is the persisted exception to one-shot discovery: it remains
configured and listed when its provider-owned path goes missing. Restore the
state at that path, replace the path atomically under the same name with the
`ctx sources add <name> --provider <provider> --root <replacement-path> --replace`
form, or remove the definition with `ctx sources remove <name>`. A missing
OpenClaw state root cannot safely invent its agent routes, so it appears as a
route-less configured-root diagnostic rather than an unrelated automatic
missing route. JSON and MCP identify that diagnostic with
`code: "configured_root_missing"` and an explicit `configured_root` object.

The equivalent editable configuration is:

```toml
[sources.roots.personal]
provider = "claude"
path = "/absolute/path/to/claude-personal"
group = "personal"

[sources.roots.work-codex]
provider = "codex"
path = "/absolute/path/to/codex-work"
group = "work"

[sources.roots.openhands-cli]
provider = "openhands"
path = "/absolute/path/to/openhands/conversations"
kind = "current-conversations"
```

Names and groups use up to 64 ASCII letters, digits, hyphens, or underscores;
paths are normalized absolute paths of the file or directory kind required by
the provider capability. At most 64 roots may be configured. A group is an
optional label shared by any number of roots and is used only when a search
explicitly supplies `--source-group`. For an additional root, the name is also
its stable local source identity: atomically replacing only its path preserves
ctx session IDs and citations after a move, while removing it and adding a
different name creates a new logical root. Naming the currently inferred root
keeps its existing released identities. In automatic indexing mode the daemon
reloads a valid source change as one full refresh. In manual mode, or when
immediate publication is desired, run `ctx import --all` or search with
`--refresh wait`.

`ctx sources add <name> ... --replace` is the safe atomic edit. The name must
refer to the supplied provider when it already exists; a mismatch fails. An
absent name is added, and identical canonical settings are a no-op.
`--source-group <group>` sets or replaces the group. Omitting `--source-group`
while using `--replace` clears it. Choose a new name instead of changing
provider under an existing name. The command holds one config transaction lock
through read, validation, and durable replacement, so daemon refresh never
observes an intermediate removed definition.

Treat the name as a durable local mount key rather than a display label.
Removing and later reusing the same name intentionally reuses that logical
namespace, including reconciliation of matching provider-native session ids;
use a new name for an unrelated history root. Replacing `path` under the same
name is the move operation, editing `group` only changes filtering metadata,
and changing the table name creates new logical identities.

OpenHands roots additionally require exactly one `--kind`. Use
`current-conversations` when `--root` is the direct current conversations
directory; that route accepts only
`<conversation>/events/event-*.json`. Use `legacy-persistence` for the released
recursive persistence-tree compatibility layout. `--kind` is rejected for
other providers, and a hand-edited OpenHands table must contain the equivalent
`kind = "current-conversations"` or `kind = "legacy-persistence"`. Nested
automatic/configured OpenHands roots and ancestor-related configured legacy
and current roots are rejected because they could select the same history.

In `sources add`/`remove --format json`, `root.kind` is present for OpenHands
and contains the selected string. The field is omitted for every other
provider, preserving the earlier schema-v1 shape. The ordinary human success
line remains a concise provider/name/path summary.

ctx 1.1 writes generation manifest v10 and cannot produce a v8 index readable
by ctx 1.0. To downgrade, use a fresh or separate data root and a 1.0-compatible
config; back up the current config or use a separate `XDG_CONFIG_HOME` as
appropriate. Then let the 1.0 binary rebuild from provider history. Never reuse
a 1.1 data root or expect a 1.1 import to make its storage readable by 1.0.

Names and groups are local provenance and query selectors. They are not upload
consent, access-control boundaries, tenant assignment, or retention policy; a
future sync product must ask for those decisions separately.

Advanced users can disable all automatic provider discovery while keeping named
provider history roots active:

```toml
[sources]
automatic = false
```

This does not erase already indexed history. It stops automatic roots from
being selected by future refreshes; named roots remain active. `ctx sources`
labels each row as automatic or configured and reports when automatic discovery
is disabled. JSON output includes the top-level `automatic_discovery` boolean
and a per-source `selection` object.
Current rows include:

- Codex session trees at `~/.codex/sessions`;
- Codex prompt history at `~/.codex/history.jsonl`;
- Pi session JSONL files under `~/.pi/agent/sessions`;
- automatic or unsupported-detection rows when a provider's bounded current
  location or strategy yields a source on this machine; dynamic project or
  service providers may have no row outside an authorized CWD or registration,
  and alternate roots remain available through exact compatible `--path`
  imports;
- AstrBot `data_v4.db` history when those files exist;
- NanoClaw project stores from exact CWD or official launchd/systemd service
  registration;
- local history-source plugin manifests under `$CTX_DATA_ROOT/plugins` or
  `CTX_HISTORY_PLUGIN_PATH`.

Native JSON rows include `provider`, `path`, `exists`, `source_format`,
`status`, `import_support`, `native_import`, `importable`, and any
`unsupported_reason`. Plugin JSON rows use
`kind: "history_source_plugin"` and include `plugin`, `plugin_source`,
`history_source`, `provider_key`, `source_id`, `manifest_path`, `enabled`,
and acquisition-source fields. Durable regular-file rows report
`status: "available"`, `importable: true`,
`import_mode: "explicit_source_backed"`, and
`provider_source_authority: true`. These compatibility field names describe
the acquisition route, not query-time content authority. Command-only
compatibility rows report `status: "unsupported"`, `importable: false`, and no
provider-source acquisition authority. Invalid installed plugin manifests
appear as non-importable plugin rows with `status: "invalid"` and an `error`.
`sources` reads path metadata and plugin manifests, writes nothing to provider
files or source repositories, and does not execute plugin commands.

A detected current format that ctx cannot import has `status: "unsupported"`
and `import_support: "unsupported"`. A supported source that requires user
selection has `import_support: "explicit"`; it can be imported by exact path
but does not participate in setup, `--all`, daemon refresh, or search refresh.

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
ctx import --provider lingma
ctx import --provider codebuddy
ctx import --provider codex --path ~/.codex/sessions
ctx import --provider pi --path ~/.pi/agent/sessions
ctx import --input-format ctx-history-jsonl-v2 --path ./history.jsonl
ctx import --history-source example-agent/default
ctx import --history-source-manifest ./ctx-history-plugin.json
ctx import --resume
ctx import --no-daemon
ctx import --format json
ctx import --progress json --format json
```

`hermes` selects the supported native `state.db` importer. On Linux, a non-root
ctx process with the certified read-only live-WAL path makes new sessions and
appended records converge on native-watch and search refreshes. Otherwise the
incremental attempt defers without copying the provider database. Structural
edits, deletions, and deferred increments reconcile in roughly 60–80 minutes
with a healthy daemon, or on `ctx import --provider hermes` or
`ctx import --all`.

`import` explicitly rebuilds Core history from provider sources. The
normal first-run path is `ctx setup`, which already imports discovered native
provider sources. Use `import` to repair, re-run, resume, or target a specific
provider/path. It creates the data root if needed, reads provider transcript
files, builds a private immutable Core/Tantivy candidate containing complete
normalized stored records plus lexical fields, identities, and filter metadata,
verifies it, and atomically publishes it under `search/lexical`. Before
returning, it waits only for that Core publication. Optional semantic indexing
advances independently and does not extend the foreground import boundary. It
does not write `config.toml` for implicit defaults.

History-source plugin import is explicit and single-source in 1.0. A selected
manifest declares a durable provider-owned `ctx-history-jsonl-v2` path; the
importer validates its schema and source identity, registers that same path as
the custom acquisition route, and waits for daemon-owned Core publication.
Command-only manifests are reported as unsupported and are never copied into
ctx storage. Plugins are not imported by `import --all` or setup.

Imports always commit valid records. Human receipts report one stable
`Skipped records` count, including zero; JSON retains the detailed
`rejected_records` diagnostics. An unreadable
or structurally incompatible input fails that source, while ctx-owned storage
or index failures abort the command. A source with only rejected records is a
failure; a source with valid content and rejections completes with an explicit
`completed_with_rejections` JSON outcome and a successful human receipt. A
structurally valid record with an
unrecognized provider-native discriminator is retained generically, counted as
ignored, or rejected at record scope; it does not make an otherwise compatible
source unreadable.

Import results report `change: changed|no_op` independently from import and
skip counters. `change: changed` remains truthful even when a source projects
to the same stable event identities.

After a manual re-import, the human receipt reports signed `Sessions` and
`Searchable events` under `Net index change`, followed by their current totals
under `Current index`. Positive, zero, and negative values are net changes in
the exact Core publication, not newly parsed or imported records. The first
import shows current totals without a delta because there is no preceding Core
generation to compare. If concurrent publication has already retired that
preceding generation, ctx omits the delta rather than guessing. Background
refresh and setup output do not gain this manual-import receipt section.

In automatic mode, `import` may start the persistent daemon and uses its
source-refresh endpoint for foreground Core publication. In manual mode, an
explicit import may start the same Core engine as a finite worker; it waits for
publication and the worker exits after admitted Core work is terminal and IPC
is quiescent. `import --no-daemon` never starts or restarts a process and
therefore requires an already-running endpoint. Import never falls back to a
foreground writer.

## Paid Companion Routes

Official managed distributions may install a separately signed private
companion alongside Core. Core-only channels retain the OSS commands. Paid
routes return a typed companion-unavailable failure when the companion is
absent. See [ctx Pro](managed-companion.md).
## Show

```bash
ctx show session <ctx-session-id>
ctx show session <ctx-session-id> --mode full --format text
ctx show session <ctx-session-id> --mode log --format jsonl
ctx show session <ctx-session-id> --max-events 4096 --format json
ctx show session <ctx-session-id> --format markdown --out transcript.md
ctx show session <ctx-session-id> --mode full --format markdown --out transcript.md
ctx show session --provider-session <provider-session-id> --provider-key <provider-key> --source-id <source-id>
ctx show event <ctx-event-id> --window 3 --format text
ctx show event <ctx-event-id> --before 5 --after 10 --format json
```

`show session` renders one transcript by ctx-owned session ID. It defaults to
`--mode lite`, a compact agent-readable transcript with user messages and final
assistant messages. `--mode full` keeps all user/assistant/system message
events, and `--mode log` renders all imported events including tool and command
activity. `--format` accepts `text`, `markdown`, `json`, or `jsonl`. Without
`--out`, `show session` writes to stdout. With `--out`, it writes the rendered
transcript artifact to that path and prints nothing on success. Sessions over
the bounded presentation limit require `--max-events`; the value is capped at
4096 and the response reports truncation.

`show event` renders one ctx-owned event hit. `--before` and `--after` include
neighboring events in the same session; `--window N` is shorthand for
`--before N --after N`. It accepts the same output formats as `show session`.
In ordinary human output, event-window `Time` rows use the
executing OS local time zone and the exact
`YYYY-MM-DD HH:MM:SS ZONE` shape, without milliseconds. The same local format
applies to ordinary `show session` output. Explicit text/Markdown
transcript artifacts and the separate Markdown renderer retain the stored UTC
RFC 3339 millisecond timestamps, as do JSON and JSONL.

Full JSON/JSONL event output can include content-governed `activity`, with
exact typed provider call identity, invocation and/or result channels, and
ordered literal provider facts. A qualified MCP invocation has `protocol:
"mcp"` plus exact source `server` and advertised `tool` strings. Present
arguments and structured results remain decoded JSON values. Ordinary tool
events require `--mode log`; the default `lite` and `full` selection rules do
not change. See
[`mcp-tool-call-attribution.md`](mcp-tool-call-attribution.md) and
[`mcp-exchange-capture.md`](mcp-exchange-capture.md).

## List

```bash
ctx list events --provider codex --content text --limit 1000 --format jsonl
ctx list events --since 2026-08-01T00:00:00Z --until 2026-08-02T00:00:00Z --format json
```

`list events` is the deterministic machine-oriented enumeration surface. It
accepts exact provider/source/session and parent/root-session filters,
provider-session and source-format filters, event type, role, agent type,
scope, branch, indexed workspace/file filters, and a paired half-open
`--since`/`--until` range. `--direction` controls the complete deterministic
order. `--content full|text|none` projects payload fields and `--limit` bounds
the result. JSON returns one internally bounded page;
JSONL streams bounded pages and ends with exactly one completion record. Opaque
cursors are bound to the exact selection and immutable Core generation. See
[`event-queries.md`](event-queries.md) for the wire contract and jq examples.

Only `--content full` can return `activity`. `--content text` keeps the
existing normalized event text but omits structured content and activity;
`--content none` omits all payload content. MCP activity has no list selector;
filter full JSON/JSONL rows client-side.

Show and list commands read complete policy-selected normalized records from the active
verified Core/Tantivy generation. They do not reopen provider history at query
time. Show preserves event order and never expands payload classes excluded by
import policy, such as binary data or provider-private blobs.

Provider-owned IDs are metadata, not positional IDs. Positional session and
event arguments are ctx-owned IDs. To look up a provider-owned session, use an
explicit provider lookup such as `--provider codex --provider-session
<provider-session-id>` on commands that support provider lookup. Custom
provider-session IDs can repeat across exporters, so add the exporter route
`--provider-key <provider-key> --source-id <source-id>` to disambiguate them.

JSON output may expose transcript content, MCP arguments/responses, and local
workspace metadata, so treat it as private local data.

## Locate

```bash
ctx locate session <ctx-session-id>
ctx locate session --provider codex --provider-session <provider-session-id> --format json
ctx locate session --provider-session <provider-session-id> --provider-key <provider-key> --source-id <source-id> --format json
ctx locate event <ctx-event-id> --format json
```

`locate` returns bounded source identity metadata stored in the active verified
Core/Tantivy generation. Session lookup accepts a ctx-owned ID or the explicit
provider-session selector; custom provider-session lookup also accepts the
paired `--provider-key`/`--source-id` route selector. Event lookup accepts a
ctx-owned event ID. `--format` accepts `text` or `json`.

The result identifies the Core source with `ctx_source_id`, `source_format`,
`schema_variant`, and `provider_identity_version`. It does not expose a provider
path, reopen provider history, or recreate provider-native locator state.
Custom history-source results also report their exporter-declared
`provider_key` and `source_id` beside the canonical `custom` provider.
Human event output also identifies the owning ctx session, exact event time,
and stored event sequence. Human session output labels its timestamp `First
event`: this is the first stored event in the indexed session, not a claimed
provider-session start. A missing supported timestamp is shown as `time
unavailable`. Both human time fields use the executing OS local time zone as
`YYYY-MM-DD HH:MM:SS ZONE`, honor `TZ`, and omit milliseconds. Locate JSON
retains the exact stored UTC RFC 3339 millisecond values.

## Search

```bash
ctx search "build failure"
ctx search "storage layout" --provider codex
ctx search "retry handling" --workspace checkout --since 60d
ctx search "tool output" --event-type tool_output
ctx search "permission denied" --content-scope outputs
ctx search --file crates/foo/src/lib.rs
ctx search "token budget" --refresh off
ctx search "signed metadata" --term checksum --term release
ctx search "token budget" --limit 5
ctx search "token budget" --session <ctx-session-id>
ctx search "token budget" --exclude-session <ctx-session-id-or-prefix>
ctx search "human decisions" --primary-only
ctx search "this current task" --include-current-session
ctx search "mail provider throttled bulk mailbox setup" --backend hybrid
ctx search "pricing decisions from the launch review" --backend semantic
ctx search "release notes" --history-source example-agent/default
ctx search "release notes" --provider-key example-agent --source-id default
ctx search "weekend prototype" --source-root personal
ctx search "incident follow-up" --source-group work
```

`search` defaults to `--refresh background`, which serves the published
Tantivy generation. In automatic indexing mode it may start or wake the
persistent daemon for lexical publication and optional semantic catch-up. In
manual mode, background refresh uses only the last published generation and
does not contact, start, or wake a process; there is no hidden foreground
bootstrap or importer. History-source plugins are searched from the published
generation after explicit import; search refresh does not execute their
commands in 1.0.
Semantic retrieval reads an existing compatible generation under
`search/semantic`; search does not initialize semantic storage, download
embedding models, or run semantic indexing. Use `--refresh off` to query the
published generations without starting or waking a process. Use
`--refresh wait` to request authoritative Core publication; in manual mode it
may start a finite worker that exits after admitted Core requests are terminal
and IPC is quiescent. Results are rendered from Core under every refresh mode.
Wait refresh skips isolated malformed records with a
warning and publishes valid records; source-level and system-level failures
remain fatal. NanoClaw and supported AstrBot `data_v4.db` locations participate
in bounded native discovery and may also be imported with an explicit `--path`.
Search-only sources without native import support are searched from the active
Core generation until they are explicitly imported through a supported path. Search
requires a non-empty query, at least one non-empty `--term`, or
`--file <path>`; provider, workspace, time, session, event, source, and result
flags only narrow an actual search. Ordinary results include primary and
subagent sessions. After filters and active-session exclusion, ctx selects the
strongest bounded candidate for each exact session, reads all selected
session/source coordinates in one bounded grouping query, and coalesces sparse
direct provider claims for each coordinate. Sessions with the same positive
coalesced root claim form one search family; an unclaimed session is its own
family. Missing, conflicting, corrupt, or over-bound grouping authority fails
the query instead of falling back to candidate-event roots or inferred
lineage. Families are ordered by their strongest session champion, then ctx
emits one remaining champion per family per relevance-stable round. Agent
scope does not promote a weaker champion. Results
include `more_matches_in_session` and
`session_importance` when more indexed events from the returned session also
matched. Use `--session <ctx-session-id>` after a default search has identified
a session to inspect; scoped session search returns dense event hits.
Session/event commands accept full ctx IDs or unambiguous ctx ID
prefixes of at least eight hex characters. Human search, show, locate, Markdown,
and MCP text render the shortest unambiguous lowercase no-dash prefix from 8
through 32 characters. Uniqueness is checked against the command's pinned Core
generation and its retained peer; JSON, JSONL, MCP structured content, cursors,
and stored data keep full UUIDs. Use `--events` without `--session`
for dense event-level results across sessions. Lexical search matches any word
in an ordinary multi-word query and ranks results matching more query words
ahead of partial matches. Repeat
`--term <query-or-keyword>` when you want to broaden a search across several
related queries or keywords and merge the ranked results; `--term` is OR-style
broadening, not a must-include filter.
`--content-scope all|transcript|calls|outputs` selects a searchable event class
at query time. Omission resolves to `all` and is identical to explicitly
passing `all`. In `all`, lexical class weights are `message` 1.0, `summary`
0.9, `tool_call` and `command_started` 0.8, `tool_output`, `command_output`, and
`command_finished` 0.6, and other or future searchable events 0.8.
`transcript` searches messages at 1.0 and summaries at 0.9; `calls` searches
only tool calls and command starts at ordinary lexical strength; `outputs`
searches only tool outputs, command outputs, and command finishes at ordinary
lexical strength. It does not add a diagnostic boost or collapse duplicate
events. `--content-scope` conflicts unconditionally with `--event-type`.
`all` and `transcript` retain normal semantic/hybrid behavior. Since the
semantic projection contains transcript messages, hybrid `calls` and `outputs`
requests report a lexical fallback, while semantic-only requests for those
scopes fail with a typed unsupported-scope error.
JSON echoes the normalized positional/`--term` alternatives in `query`, trims
surrounding whitespace, and joins nonempty alternatives with ` OR `. Scoped
follow-up commands preserve the positional and repeatable-term argument shape
with safe shell quoting, plus a shell-quoted `ctx --data-root <path>` prefix when
search uses a non-default data root. Each result's `rank` is its one-based
position in the final shaped window. `retrieval_score` preserves the backend's
diagnostic score, which can be non-monotonic after query-coverage and family
shaping.
Human output uses only the pluralized rendered-result count in the heading.
Separate `Event` and `Time` rows show the dynamic event reference and the
matched event in the executing OS local time zone as
`YYYY-MM-DD HH:MM:SS ZONE`, without milliseconds;
timestamps never re-sort results. `--verbose` additionally renders the stored
event sequence and available workspace/working-directory, branch, agent, and
parent/root lineage without repeating equal values.
Search JSON keeps the exact UTC RFC 3339 millisecond result timestamps and
`generated_at`; localization never changes filters, storage, other durable
processing, or machine output.
Ordinary search does not run a reverse-lineage lookup for each hit. A selected
event can still expose its own positive direct `event_copy` claim, but that
claim does not affect recall, ranking, grouping, or semantic eligibility.
`ctx show event` resolves that selected event or its one direct copied-from
target and renders up to 20 direct reverse occurrences. The target state is
`resolved` or `unresolved`; missing targets are ordinary unresolved references
and do not hide admitted occurrences. Counts are exact when the bounded direct
posting scan completes and otherwise are an `at least` lower bound. This
surface performs no parent, root, copy-component, cycle, or transitive lineage
walk. There is no exhaustive cursor.
Custom history imports can be filtered by canonical
`--history-source provider_key/source_id`, or by exact `--provider-key`,
`--source-id`, and `--source-format` values. The plugin/source alias is for
explicit plugin import selection. These search filters imply
`--provider custom` and cannot be combined with another provider.
`--source-root <name>` and `--source-group <group>` are repeatable. Values
across both flags form one union of configured history roots, then intersect
with provider, workspace, time, event, session, and other filters. Resolution
happens against the immutable generation being queried, not live config, so
lexical, semantic, hybrid, CLI, and MCP search select the same source keys. An
unknown name or group in that pinned generation fails instead of widening the
search. All history roots still share one local Core/Tantivy index; selecting a
root does not open or switch a separate index.
Ordinary search uses the all-agent, coalesced-session, literal-family shaping
described above. Use
`--primary-only` only when a deliberately narrow search should exclude
subagent work.

Direct CLI searches automatically exclude the current session tree for Codex,
DeepSeek Harness, Grok Build, Pi, Claude Code, Goose, Hermes, Shelley, Qwen
Code, and Mux when the current session can be identified unambiguously.
Unsupported or ambiguous detection fails open: ctx leaves the history
included. `--include-current-session` restores the automatically excluded
tree. Repeat `--exclude-session <ctx-uuid-or-unambiguous-prefix>` to exclude
exact named sessions; the option is repeatable and conflicts with `--session`.
MCP searches do not automatically exclude the caller's session.

`--refresh off` is read-only for ctx-derived storage, but it still serves
indexed snippets and typed show/locate data from the active Core generation.
Explicit semantic or hybrid
requests may read a compatible semantic generation and ask the retained daemon
query service to embed the query from an already-cached model.

Results are local hits over indexed history. Event hits include `ctx_event_id`;
hits with known session context include `ctx_session_id`; provider metadata
including `provider_session_id` is included when known. For Codex, that value
is the resume UUID. Results also include title, snippet, rank, result scope,
citations, `suggested_next_commands`, a JSON `freshness` object, a JSON
`retrieval` object with backend, semantic coverage, worker status, and semantic
timing/scan diagnostics when vector retrieval runs, a JSON `result_window`
object with `limit`, `returned`, and shaped-sentinel `more_available`, and
separate `diversification` and backend candidate-pool `truncation` fields.
Ordinary lexical session search performs one bounded batch with the fixed
candidate horizon `max(limit + 1, 256)`, capped by the lexical query maximum;
it never retries or doubles the pool. Dense `--events` and explicit
`--session` searches use event relevance with a `limit + 1` lookahead and do
not query grouping authority. Search does not expose a continuation cursor or
run a second count scan. Default text output is compact and optimized for agent
reading; it ends with exactly
`More results available.` only when one additional shaped result exists. Use
`--verbose` for expanded text diagnostics.

Filters:

- `--provider codex|pi|claude|opencode|kilo|kiro-cli|crush|goose|antigravity|gemini|tabnine|cursor|zed|copilot-cli|factory-ai-droid|qwen-code|kimi-code-cli|auggie|junie|firebender|forgecode|deepagents|mistral-vibe|mux|rovodev|openclaw|hermes|nanoclaw|astrbot|shelley|continue|openhands|cline|roo|lingma|qoder|warp|codebuddy|custom`;
- `--workspace <name-or-path>`, substring match over stored workspace, cwd,
  source path, or repository-name text;
- `--since <rfc3339-or-days>d`, for example `2026-06-01T00:00:00Z` or `30d`;
- `--event-type <event-type>`, one of `message`, `tool_call`, `tool_output`,
  `command_started`, `command_output`, `command_finished`, `file_touched`,
  `vcs_change`, `artifact`, `summary`, or `notice`;
- `--content-scope all|transcript|calls|outputs`, a class-aware query-time
  selection that cannot be combined with `--event-type`;
- `--file <path>`, indexed touched-file path metadata, not the current
  filesystem;
- `--session <ctx-session-id-or-prefix>`, for dense event results within one session;
- repeatable `--exclude-session <ctx-session-id-or-prefix>`, for exact named
  sessions to omit;
- `--term <query-or-keyword>`, repeatable broadening queries or keywords merged with OR-style semantics;
- `--events`, for dense event-level results instead of the default family-shaped results;
- `--backend hybrid|semantic|lexical`, where `lexical` queries
  `search/lexical`, `semantic` queries `search/semantic`, and `hybrid` blends
  both only when semantic coverage is complete and bound to the active lexical
  generation. Hybrid falls back to lexical with a structured reason when
  semantic prerequisites are missing. Explicit semantic reports a local error
  rather than downloading a model or using an incompatible generation;
- `--semantic-weight <0.0-1.0>`, for hybrid ranking;
- `--primary-only`;
- `--limit <n>`, capped at `200`;
- `--refresh background|off|wait`;
- `--include-current-session`.

CLI provider filters use kebab-case names. JSON output uses provider IDs in ctx
output; multiword IDs may be snake_case, such as
`copilot_cli`, `factory_ai_droid`, `qwen_code`, `kimi_code_cli`, `kiro_cli`, `mistral_vibe`, and `roo_code`; compact IDs such as `forgecode`, `deepagents`, `mux`, `rovodev`, `openclaw`, `nanoclaw`, `astrbot`, `shelley`, `continue`, and `openhands` stay compact.

Lexical matching indexes the policy-selected meaningful body. Search serializes
the indexed Core hit associated with the match; show/locate presentation reads
the complete policy-selected record and source identity stored in Core. Content
scope changes neither retained bodies nor the Core/index schema and requires no
rebuild. Search JSON always reports the resolved selection as
`filters.content_scope`, including `all` when the option was omitted.

In auto mode, persistent background maintenance owns provider/plugin refresh,
immutable candidate construction, atomic lexical publication, source discovery
state, and semantic catch-up. Manual mode retains explicit finite Core
publication. `ctx daemon run` is available for advanced foreground maintenance
and does not change the indexing mode. `ctx status` exposes `history_epoch`,
`lexical`, `refresh`, `semantic`, and `daemon` objects; `ctx doctor` is the
diagnostic surface for those components.

## Docs

```bash
ctx docs
ctx docs list
ctx docs list --format json
ctx docs search "upgrade"
ctx docs search "file path" --limit 5 --format json
ctx docs show cli-reference
ctx docs show search --format text
ctx docs show json-contracts --format json
ctx docs man --print ctx
ctx docs man --out ~/.local/share/man/man1
```

`docs` exposes a curated copy of the public ctx docs inside the binary. It is
intended for humans and agents that need local command help without opening the
website. `docs list`, `docs search`, and `docs show` read embedded text and do
not touch provider history, Core/Tantivy generations, semantic data, or the Pro
graph.
`docs show --out PATH` writes one embedded topic to that explicit path.
`docs man --print PAGE` prints one generated man page to stdout.
`docs man --out DIR` writes generated section-1 man pages for `ctx` and its
public subcommands.

Agents should usually use `ctx docs search` or `ctx docs show` rather than
shelling through `man`, because the docs commands return concise markdown/text
that is easier for agents to quote and inspect.

## MCP

```bash
ctx mcp serve
ctx integrations install mcp
```

`mcp serve` starts a local MCP server over newline-delimited stdio JSON-RPC.
It exposes the Core tools `status`, `sources`, `search`, `show_session`,
`show_event`, and `query_events`.

MCP `search` queries the active Core/Tantivy generation and can use a compatible
semantic generation under the normal search contract. It does not become an
importer. Tool results include MCP text content plus `structuredContent` JSON.
Treat all MCP output as private local history: it may include absolute paths,
source metadata, snippets, and transcript text, and the MCP host may log or
forward tool output.

MCP searches do not automatically exclude the caller's session; the CLI's
automatic current-session detection and `--include-current-session` behavior
do not apply to MCP calls.

MCP search accepts repeatable-value arrays named `source_roots` and `source_groups`.
When both are absent, it searches all indexed roots, matching the CLI.

MCP `show_event`, log-mode `show_session`, and full-content `query_events`
event rows expose the same optional snake_case `activity` value in
`structuredContent`. `query_events` with `content: "text"` or `content:
"none"` omits it. Filter bounded pages on the client and continue with their
existing opaque cursors; there are no dedicated MCP activity selectors or
search arguments. Ordinary CLI and MCP search can match retained searchable
activity values through the shared Core search projection. Keys inside captured
JSON values are unchanged.

Human CLI, Markdown, and MCP text views escape terminal controls and may bound
the rendered event. Use machine JSON/JSONL or MCP `structuredContent` for the
exact admitted activity value; see
[`mcp-tool-call-attribution.md`](mcp-tool-call-attribution.md).

The MCP server is optional. The CLI remains the primary interface, and MCP is
intended for agents or hosts that prefer tool discovery over shell commands.
Use `ctx integrations install mcp` to add the server to supported coding-agent
MCP configs.

## Upgrade

```bash
ctx upgrade status
ctx upgrade status --format json
ctx upgrade check
ctx upgrade check --format json
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
`ctx upgrade status --format json` also reports the current executable and every
executable `ctx` candidate found on `PATH`, with warnings when another binary
shadows the managed install or multiple `ctx` binaries are present. Diagnostics
identify candidates without executing a shadowing binary.

Official installer-managed installs use automatic upgrade by default; signed
release metadata must also explicitly allow it. Automatic indexing with the
full daemon profile uses the enabled persistent daemon as the sole automatic
check and apply driver. Manual indexing, source-refresh-only mode, ordinary
foreground commands, MCP, and finite Core workers do not schedule or spawn
automatic upgrades. Scheduler state is stored beside the managed executable.
Use `CTX_UPGRADE_AUTO=off` for a process-level opt-out, or `ctx upgrade disable`
to write `upgrade.auto = "off"` in `config.toml`. Explicit `ctx upgrade` remains
available independently of those automatic settings.

Manual `ctx upgrade` can print progress and errors. It verifies signed release
metadata, explicit self-upgrade policy, artifact SHA-256, the current managed
install marker, and the staged binary's `ctx --version` output before replacing
the installed binary. On Windows, replacement may be scheduled by a helper that
finishes after the running `ctx.exe` exits; JSON reports `status: "scheduled"`
and `applied: false` until replacement completes.

## Progress Output

`setup` and `import` accept `--progress auto|plain|json|none`. `auto` writes
plain progress only to an interactive stderr and stays quiet for `--format json` or
non-interactive stderr. `--progress json` writes newline-delimited progress
objects to stderr. It does not change stdout, so command result JSON remains a
single object when `--format json` is also present.

Progress JSON is a best-effort operation stream. Each object has
`type: "ctx_progress"` plus `operation`, `phase`, `message`,
`completed_bytes`, `total_bytes`, `percent`, `elapsed_seconds`, `eta_seconds`,
`completed_files`, `total_files`, `imported_events`, and `done`.

During daemon-owned history refresh, the same object also projects the physical
job authority and source progress: source counts, `source_completed_records`,
`source_completed_bytes`, the current source and typed source substep, logical
and physical request identifiers, and the progress owner. The record counter is
the accepted Core-record count, while the byte counter is authoritative logical
source progress; both may clear between sources or finalization phases. When no
authoritative total exists, ctx reports completed source work without inventing
a total or ETA; the common transfer fields use `total_bytes: 0` and
`percent: 0.0` as unknown-denominator sentinels.

Source refresh progress also includes `whole_run_stage` and
`estimated_remaining_millis`. The latter is a number only when ctx has a
credible estimate through verification and activation; otherwise it is
`null`. It is not promised for every provider mix or refresh mode, and the
legacy `eta_seconds` field remains unchanged.

## JSON Contract

JSON output is intended for local scripts, harnesses, and exact field
extraction. It is private unless a user explicitly reviews it.

Structured output is available for:

```text
ctx setup --format json
ctx status --format json
ctx index --format json
ctx index mode --format json
ctx index mode auto --format json
ctx index mode manual --format json
ctx index watch --format jsonl
ctx index wait --format json
ctx sources --format json
ctx import --format json
ctx show session <ctx-session-id> --format json
ctx show event <ctx-event-id> --format json
ctx search <query>|--term <term>|--file <path> --format json
ctx docs list --format json
ctx docs search <query> --format json
ctx docs show <topic> --format json
ctx integrations install mcp --format json
ctx integrations status mcp --format json
ctx upgrade --format json
ctx upgrade check --format json
ctx upgrade status --format json
ctx doctor --format json
```

See [contracts/json.md](contracts/json.md) for the current field-level contract
and known compatibility limits.
