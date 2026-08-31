# Getting Started

ctx indexes local agent history so an agent can search previous sessions before
it repeats work.

## 1. Install The CLI

```bash
curl -fsSL https://ctx.rs/install | sh
```

The Unix installer requires `curl` and OpenSSL to verify signed release
metadata. On Windows, use `irm https://ctx.rs/install.ps1 | iex`.

On Unix, the installer places `ctx` in `${CTX_BIN_DIR:-$HOME/.local/bin}`. If
that directory is not already on `PATH`, the installer adds an idempotent ctx
PATH snippet to your shell startup file and prints the command to use for the
current shell session. On Windows, the installer places `ctx.exe` in
`$HOME\.local\bin` by default, adds that directory to the user `Path`, and
updates the current PowerShell session. Use `sh -s -- --no-modify-path` on Unix,
`-NoModifyPath` on Windows, or set `CTX_INSTALL_NO_MODIFY_PATH=1` when you want
to manage `PATH` yourself.

The install script installs `ctx`, runs the bundled agent-history skill
installer, and runs `ctx setup` so discovered local history is inventoried and
indexing begins. Automatic indexing is the default, so ctx keeps native history
fresh in the background after setup. Semantic
catch-up remains disabled unless semantic search is explicitly enabled. The
skill installer opens an agent picker when interactive;
otherwise it installs the universal `~/.agents/skills` copy plus detected
agent-specific folders for tools that need them. Use `sh -s -- --no-setup` on
Unix, or set `CTX_INSTALL_NO_SETUP=1` on Windows, for install-only CI or
packaging flows. Install-only mode also skips skill setup unless you explicitly
pass a skill option.

To keep installer setup but opt out of setup daemon autostart, use
`sh -s -- --no-daemon` on Unix, `-NoDaemon` on Windows, or set
`CTX_INSTALL_NO_DAEMON=1`.

```bash
curl -fsSL https://ctx.rs/install | sh -s -- --no-daemon
```

```powershell
& ([scriptblock]::Create((irm https://ctx.rs/install.ps1))) -NoDaemon
```

To skip only the skill step, use `--no-skill` on Unix or `-NoSkill` on Windows,
or set `CTX_INSTALL_NO_SKILL=1`. To target agent-specific skill folders during
install, use `--skill-agent codex`, repeat `--skill-agent`, or use
`--all-skill-agents`; Windows exposes the same controls as `-SkillAgent` and
`-AllSkillAgents`.

For contributor builds from a checkout, use
`scripts/bazelw build //crates/ctx-cli:ctx --config=dev-linux`. For an unmanaged
source installation, use the Cargo install command documented in
[Package Managers And Unmanaged Installs](unmanaged-installs.md); that
installation path is not the contributor build/test workflow.

For GitHub release binaries, mise, Homebrew, and source builds, see
[Package Managers And Unmanaged Installs](unmanaged-installs.md).

## 2. Set Up And Index

```bash
ctx setup
ctx status
```

Setup creates the configured ctx data root, prepares an immutable Core/Tantivy
generation with complete policy-selected records and source identities,
requests a provider-source refresh, starts or health-checks automatic background
indexing, and prints next steps. It does not write `config.toml`
for implicit defaults and does not execute history-source plugin commands. The
human summary names only agent histories that contributed indexed content. A
partial, excluded, or unknown provider root produces a warning while healthy
prior history remains searchable; use `ctx doctor` for recovery and
`ctx status --format json` for exact diagnostics. The
default data root is `~/.ctx`. Use `ctx index mode manual` to select manual
indexing or `ctx setup --no-daemon` for a one-run process-start opt-out. Check or
change the mode with:

```bash
ctx index mode
ctx index mode auto
ctx index mode manual
```

The equivalent canonical configuration is:

```toml
[indexing]
mode = "manual"
```

The other supported value is `"auto"`, which is the default and permits
persistent background maintenance. Mode setters persist the requested choice
and reconcile supervision to the effective mode. When auto is not overridden,
ctx installs or repairs supervision and starts the daemon. Manual mode stops the
persistent daemon and removes its supervision; explicit `ctx import` and
`ctx search --refresh wait` can still use finite workers.

Machine-readable setup follows the same lifecycle and reports schema version 2
with top-level `daemon_autostart` and `refresh_request` objects. The deprecated
`--catalog-only` flag no longer disables daemon maintenance.

Use a different root when testing:

```bash
ctx --data-root /tmp/ctx-demo setup
CTX_DATA_ROOT=/tmp/ctx-demo ctx status
```

Setup does not write to source repositories, call embedding executors, download
embedding models, or require executor credentials while semantic search is
disabled. If automatic indexing and semantic search are explicitly enabled,
daemon maintenance uses the selected executor and may acquire the built-in ONNX
Runtime asset and embedding model needed for the installed platform.

Select the built-in multilingual E5 executor and enable semantic search:

```bash
ctx semantic enable --executor builtin
ctx semantic status
```

Bare `ctx semantic enable` preserves whichever executor is already selected;
on a new data root with no executor configuration, the default is built-in E5.

Built-in document indexing is throttled by default. To remove deliberate
inter-batch pacing and use the safely supported built-in thread and batch
maxima, set `builtin_throttling = false` under `[semantic]` in `config.toml`.
The setting is valid only for the built-in executor, does not change semantic
enablement or `--executor`, and leaves the pinned E5 model plus all admission,
integrity, cancellation, atomicity, and hard limits intact. `ctx semantic
status` reports its configured and effective values. See
[Built-in indexing throttling](semantic-executors.md#built-in-indexing-throttling)
for the complete contract.

To use an external executor's vector space instead, select its base URL
explicitly:

```bash
export CTX_SEMANTIC_EMBEDDING_TOKEN='your-endpoint-token'
ctx semantic enable --executor https://embeddings.example.test/ctx
```

Remote URLs require HTTPS. Plain HTTP is accepted only when the host is a
literal loopback IP address; a loopback executor may omit the token. Use
`ctx semantic enable --executor builtin` to return to the built-in executor.
Loopback is only ctx's first hop; the local process can retain or forward the
content it receives. URL selection tries V2 first and persists the endpoint's
opaque space identity and dimensions for this data root. A fixed-E5 V1 endpoint
is accepted only when V2 returns 404. The selected executor receives raw ctx
query text and document chunks and owns model preprocessing and tokenization.
It is used for both indexing and queries, with no silent built-in fallback.

If the endpoint later reports a different identity, semantic work fails closed.
Rerun the same `ctx semantic enable --executor URL` command to accept it. An
accepted identity change rebuilds derived semantic data without deleting
history or the lexical index.

Automatic indexing is the default, so enablement starts or recovers the daemon
that prepares the selected executor and builds the semantic projection. Add
`--wait` to wait for readiness. If you selected manual indexing, plain
enablement records the opt-in without changing modes; run `ctx index mode auto`
for automatic catch-up or use an explicit semantic search with `--refresh wait`.
Lexical search remains available while embeddings build; hybrid search uses
lexical and semantic evidence automatically when coverage is ready.
`ctx semantic disable` turns the feature off without deleting downloaded assets.

ctx has no hosted-history client or `ctx cloud` subcommand. Official
installer-managed binaries can separately run signed CLI
upgrade checks; that updater does not collect provider history.

## 3. See Available Sources

```bash
ctx sources
ctx sources --format json
```

`sources` checks known provider locations on the current machine. Its concise
default hides empty automatic locations while keeping configured roots visible;
`ctx sources --all` retains the full empty and missing inventory for diagnostics. Today it
reports supported Codex, Pi, Antigravity, Claude Code, OpenCode, Kilo Code,
Gemini, Cursor, Zed, GitHub Copilot, Factory AI Droid, Warp, and other supported local
history paths. JSON rows include
`status` and `importable`; `status: "empty"` means the automatic location or
configured root exists but no provider-specific transcript files were found there, and
`status: "unknown"` means the bounded transcript probe hit its scan budget.

## 4. Re-Run Or Target Imports

```bash
ctx import --all
ctx import --provider codex
ctx import --provider pi
ctx import --provider cursor
ctx import --provider zed
ctx import --provider codex --path ~/.codex/sessions
ctx import --resume --format json
ctx import --provider pi --path ./pi-session.jsonl
```

Setup already imports discovered sources. Use `ctx import` to repair, re-run,
resume, or target a specific provider/path. Current importers rescan sources
idempotently and skip or replace unchanged indexed rows. The `--resume` flag is
reported as `idempotent_rescan`; it does not yet mean every provider has a
native cursor-resume API. Imports keep valid records when isolated records are
malformed and report those rejections explicitly. Unreadable or incompatible
sources still fail without preventing `--all` from importing other sources.

With automatic indexing, `ctx import` can start the persistent ctx-owned daemon.
With manual indexing, the explicit import can instead start a finite Core worker,
waits for authoritative publication, and lets it exit without watcher,
semantic, timer, supervisor, or upgrade maintenance. Use
`ctx import --no-daemon` to forbid starting or restarting either process.
Output format does not change this authority.

After upgrading an older data root to `0.10.x` or newer, the first refresh or
import may perform a one-time provider reimport. That rebuilds search content
with touched-file metadata and local/private transcript text.

Native provider `--path` imports require `--provider`. Custom JSONL imports use
`--input-format ctx-history-jsonl-v2 --path <file>` instead. The former v1
schema is unsupported and is not accepted as an alias or translated.

## 5. Search

```bash
ctx search "failed migration"
ctx search "failed migration" --term schema --term rollback
ctx show event <ctx-event-id> --window 3
ctx show session <ctx-session-id>
```

Lexical search treats words in a multi-word query as alternatives and ranks
results matching more of those words ahead of partial matches. Repeated
`--term` values merge additional queries or keywords into the same result set.

Use `ctx_event_id` with `ctx show event` when you need a hit plus surrounding
events. Use `ctx_session_id` with `ctx show session` when you need the
transcript. Commands accept full ctx IDs or unambiguous ID prefixes of at least
eight hex characters. Rendered text uses the shortest unambiguous 8-to-32
character no-dash reference across the pinned and retained Core generations;
machine output retains full UUIDs. A canonical search hit can list sessions
that inherited the event, and `ctx show event` expands the bounded copied-event
lineage automatically. Search also accepts filters such as `--provider`,
`--workspace`, `--since`, `--event-type`, `--file`, `--primary-only`,
`--include-current-session`, `--term`, `--limit`, and
`--refresh background|off|wait`.
`--limit` is capped at `200`.
Ordinary search includes primary and subagent work and returns one best result
per exact root-session claim before repeats; sessions without one remain their
own groups. Use `--primary-only` only for a deliberately narrow search that
excludes subagent sessions.
Search defaults to `--refresh background`, which serves the active Core
generation. In automatic mode it may start or wake the persistent daemon for a
Core refresh and semantic catch-up when enabled. In manual mode it uses only the
last published generation without starting or waking a process. Use
`--refresh wait` for authoritative Core refresh, or `ctx import --all` for an
explicit import catch-up; either may use a finite Core worker in manual mode.

Direct CLI searches automatically exclude the current session tree for Codex,
DeepSeek Harness, Grok Build, Pi, Claude Code, Goose, Hermes, Shelley, Qwen
Code, and Mux when the current session can be identified unambiguously.
Unsupported or ambiguous detection fails open: ctx leaves the history
included. Use `--include-current-session` to restore the automatically
excluded tree. Repeat `--exclude-session <ctx-uuid-or-unambiguous-prefix>` to
exclude exact named sessions; it conflicts with `--session`. MCP searches do
not automatically exclude the caller's session. Use `--refresh off` when you
need a strictly read-only query over the active Core generation.

Automatic background refresh may start the configured persistent daemon for
local history freshness. Semantic and hybrid search read existing local sidecar
coverage; when semantic is enabled, the daemon-owned query service uses the
selected executor to embed the query. Manual background refresh and
`--refresh off` skip process startup. Search does not run semantic catch-up or
download embedding models. Hybrid uses semantic evidence only after coverage is
complete and dirty work is drained; until then it returns lexical results with
a structured reason. Explicit semantic search can query partial coverage for
diagnostics, but reports an explicit executor, model, or indexing error instead
of silently switching executors or retrieval modes.

## 6. Use JSON For Scripts

```bash
ctx search "failed migration" --format json | jq '.results[0].ctx_event_id'
ctx show event <ctx-event-id> --format json
ctx show session <ctx-session-id> --format json
```

Default text output is usually better for agent reading. Search JSON is the
supported machine-readable retrieval API for scripts and exact field
extraction. It contains cited snippets and source metadata, but it is retrieved
source material rather than generated analysis.

## 7. Optional Paid Companion

Official managed ctx installations may include a separately signed private
companion. Core-only installation channels retain all OSS setup, import, search,
and show commands. Paid routes return a typed companion-unavailable failure when
the companion is absent. See [ctx Pro](managed-companion.md).
## 8. Built-In Docs And Upgrades

```bash
ctx docs search "file path"
ctx docs show cli-reference
ctx docs man --print ctx
ctx upgrade status
ctx upgrade check
```

`ctx docs` reads embedded public docs from the installed binary. Agents should
prefer `ctx docs search` and `ctx docs show` over man pages; man pages are
available for human shell use.

`ctx upgrade` works for official installer-managed binaries. Source builds,
`cargo install`, package-manager installs, and copied binaries are treated as
unmanaged and will not self-upgrade. Automatic upgrade is on by default for a
managed binary. Only the enabled full automatic-indexing daemon drives
automatic checks; ordinary foreground commands, manual indexing, and the
source-refresh-only daemon profile do not. Explicit `ctx upgrade` remains
available in those modes. Use `ctx upgrade disable` for a persistent
upgrade-only opt-out.
