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
indexing begins. Daemon maintenance is enabled by default, so that setup run
requests the ctx-owned background daemon after setup output for native-history
freshness. Semantic
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

Setup creates the configured ctx data root, prepares the source-backed history
epoch, starts or health-checks the enabled persistent daemon, requests provider
source refresh, and prints next steps. It does not write `config.toml` for
implicit defaults and does not execute history-source plugin commands. The
default data root is `~/.ctx`. Use `ctx daemon disable` for a durable opt-out or
`ctx setup --no-daemon` for a one-run opt-out. Existing configurations that
already set `[daemon] enabled = false` remain disabled after upgrade.
Machine-readable setup follows the same lifecycle and reports schema version 2
with top-level `daemon_autostart` and `refresh_request` objects. The deprecated
`--catalog-only` flag no longer disables daemon maintenance.

Use a different root when testing:

```bash
ctx --data-root /tmp/ctx-demo setup
CTX_DATA_ROOT=/tmp/ctx-demo ctx status
```

Setup does not write to source repositories, call model APIs, download embedding
models, or require API keys while semantic search is disabled. If daemon and
semantic search are explicitly enabled, daemon maintenance may acquire the local
ONNX Runtime asset and embedding model needed for the installed platform.
ctx has no hosted-history client or `ctx cloud` subcommand. Official
installer-managed binaries can separately run signed CLI
upgrade checks; that updater does not collect provider history.

## 3. See Available Sources

```bash
ctx sources
ctx sources --format json
```

`sources` checks known provider locations on the current machine. Today it
reports supported Codex, Pi, Antigravity, Claude, OpenCode, Kilo Code, Gemini,
Cursor, Zed, Copilot CLI, Factory AI Droid, Warp, and other supported local
history paths. JSON rows include
`status` and `importable`; `status: "empty"` means the default location exists
but no provider-specific transcript files were found there, and
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

With default daemon maintenance, `ctx import` can start the same ctx-owned
background daemon profile after the foreground import finishes.
The daemon refreshes native history within local budgets and, when semantic is
enabled, may acquire the local embedding model and perform semantic catch-up.
Use `ctx import --no-daemon` for a one-run opt-out. JSON import output does not
start or nudge the daemon. Use a human-readable native import or an explicit
daemon command when background maintenance should start.

After upgrading an older data root to `0.10.x` or newer, the first refresh or
import may perform a one-time provider reimport. That rebuilds search content
with touched-file metadata and local/private transcript text.

Native provider `--path` imports require `--provider`. Custom JSONL imports use
`--input-format ctx-history-jsonl-v1 --path <file>` instead.

## 5. Search

```bash
ctx search "failed migration"
ctx search "failed migration" --term sqlite --term rollback
ctx show event <ctx-event-id> --window 3
ctx show session <ctx-session-id>
```

Lexical search treats words in a multi-word query as alternatives and ranks
results matching more of those words ahead of partial matches. Repeated
`--term` values merge additional queries or keywords into the same result set.

Use `ctx_event_id` with `ctx show event` when you need a hit plus surrounding
events. Use `ctx_session_id` with `ctx show session` when you need the
transcript. Commands accept full ctx IDs or unambiguous ID prefixes of at least
eight hex characters. Search also accepts filters such as `--provider`,
`--workspace`, `--since`, `--event-type`, `--file`, `--include-subagents`,
`--include-current-session`, `--term`, `--limit`, and
`--refresh background|off|wait`.
`--limit` is capped at `200`.
Search defaults to `--refresh background`, which serves existing indexes while
daemon maintenance refreshes lexical and semantic indexes when enabled. Use
`--refresh wait` for foreground text refresh, or `ctx import --all` for an
explicit import catch-up.

When ctx runs inside Codex, search excludes the active Codex session tree by
default when it can identify it. Use `--include-current-session` if the current
session or its subagent work is the history you want to search. Use
`--refresh off` when you need a strictly read-only query over the existing ctx
index.

Default background refresh may start the configured daemon for local history
freshness. Semantic and hybrid search read existing local sidecar coverage; when
semantic is enabled, the daemon-owned query service can embed the query.
`--refresh off` skips daemon autostart. Search does not run semantic catch-up or download embedding
models. Hybrid uses semantic evidence only after coverage is complete and dirty
work is drained; until then it falls back to lexical search with a structured
reason. Explicit semantic search can query partial coverage for diagnostics,
but reports a local error when the model cache is missing or the semantic worker
is actively indexing.

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

## 7. Optional Local Pro Work Graph

Local Pro is a separately installed paid helper that derives an encrypted work
graph from canonical local history. Source code, transcripts, and graph facts
remain local.

```bash
ctx pro
ctx blame file src/lib.rs --lines 42
ctx blame commit <sha> --format json
ctx blame pr 42 --repository forge:github.com/ctxrs/ctx
```

Repositories and worktrees are detected from indexed activity; setup does not
need a repository path. Query `--repository` accepts a logical repository
identity such as `forge:github.com/ctxrs/ctx`, rather than a filesystem path.
Numeric PR selectors require it; canonical supported PR/MR URLs do not.

Setup, daemon freshness, and blame can catch the derived graph up. Canonical
history is never changed by that work. Blame returns typed file, commit, or PR
matches with complete deduplicated evidence and optional continuation cursors.
PR activity remains separate from code production. Associated commits appear
only when a recognized structured forge record names the canonical PR and exact
Git object ID; without that proof, membership is explicitly unproven. The
helper uses the platform key store.

Bare `ctx pro` runs the idempotent setup, resume, repair, and graph catch-up
flow. `ctx pro setup` remains a supported explicit synonym. First use starts a
14-day trial without an account or payment method. The official interactive
installer can offer that trial before the initial import so Core and Pro index
in one pass; unattended installation remains Core-only unless Pro is explicitly
requested. Paid conversion and billing management use the browser later.
`ctx status` does not mutate canonical history or graph data;
entitlement authorization may advance nonsecret anti-clock-rollback metadata.

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
managed binary while the daemon is enabled; use `ctx upgrade disable` for a
persistent upgrade-only opt-out or `ctx daemon disable` to disable all daemon
maintenance, including automatic upgrade.
