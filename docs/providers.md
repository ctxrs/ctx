# Providers

ctx imports existing agent history through conservative provider adapters. Each
adapter makes a narrow, testable functional claim: the ordinary default source
for the normal supported product version is automatically located, imported by
`ctx import --all`, and exposed as meaningful user and assistant history through
search, show, and citations. An unchanged repeat import is a clean no-op on the
shared incremental, read-only architecture.

## Supported Local Imports

The public CLI supports these local-history harnesses:

Codex, Grok Build, DeepSeek Harness, Pi, Claude, OpenCode, Kilo Code, Kiro CLI, Crush, Goose, Lingma, Qoder, Warp, CodeBuddy, OpenClaw, Hermes Agent, NanoClaw, AstrBot, Shelley, Continue, OpenHands, Antigravity, Gemini, Tabnine, Cursor, Zed, Copilot CLI, Factory AI Droid, Qwen Code, Kimi Code CLI, Auggie, Junie, Firebender, ForgeCode, Deep Agents, Mistral Vibe, Mux, Rovo Dev, Cline, Roo Code, MiMo Code.

Use `ctx sources` for the truth on the current machine:

```bash
ctx sources
ctx sources --format json
ctx sources --all
```

Default `ctx sources` output keeps the common missing-location list compact. Use `--all` to inspect every recognized provider location. The CLI recognizes these provider names; recognition does not imply that every detected schema is importable:

```text
codex, grok-build, deepseek-harness, claude, cursor, pi, opencode, github-copilot, copilot-cli, antigravity, gemini, kilo, kiro-cli, crush, goose, tabnine, zed, factory-ai-droid, qwen-code, kimi-code-cli, auggie, junie, firebender, forgecode, deepagents, mistral-vibe, mux, rovodev, openclaw, hermes, nanoclaw, astrbot, shelley, continue, openhands, cline, roo, lingma, qoder, warp, codebuddy, mimocode
```

Aliases are accepted for common naming differences, for example `grok`, `dsh`, `deepseek_harness`, `claude-code`, `gemini-cli`, `github-copilot`, `droid`, `augment`, `qoder-cn`, and `roo-code`. The shorter name `deepseek` is not a DeepSeek Harness alias.

Custom history is separate: `ctx import --input-format ctx-history-jsonl-v2
--path <file>` reads an explicit JSONL interchange file from any exporter, and
history-source plugin manifests can register a durable provider-owned file.
The optional `provider_native_v1` lineage contract accepts typed relationships
and exact native-event copied-from selectors inside the v2 schema; the proof
name does not introduce a second session ID. The v1 schema is unsupported and
is neither accepted as an alias nor translated. Command-only plugins remain
lineage/origin unknown.

Exact MCP server/tool attribution is a separate, narrower event capability.
Supported provider import does not automatically qualify it. The complete
41-provider importable route/format partition is documented in
[`mcp-tool-call-attribution.md`](mcp-tool-call-attribution.md) and its
machine-readable
[`capability contract`](mcp-tool-call-attribution-capabilities.json).
Capability revision 4 exact providers are Codex, Warp, and Copilot CLI. Deep
Agents remains generally supported through its local SQLite import, which is
not qualified for exact attribution; its hosted trace is separately excluded
from this local-only capability boundary.

Provider activity is policy-selected content. For qualified tuples it preserves
each provider's native combined or split invocation/result event shape. See
[`mcp-exchange-capture.md`](mcp-exchange-capture.md).

## Location Selection

For each provider and product surface, ctx applies the provider's current
precedence and checks only the winning official root. A replacement environment
or persistent-config value replaces the lower-priority default; it is not added
beside it. Multiple roots are emitted only for current coexisting stores such as
installed clients, persisted profiles, or configured agents. See
[`provider-support-matrix.json`](provider-support-matrix.json) for every row;
each provider's `configured_root` object publishes whether named roots are
enabled or intentionally limited to automatic discovery and exact import, plus
the required path kind and expansion strategy when enabled.

Providers with an enabled configured-root capability additionally support
explicitly named history roots for work/personal, multi-profile, and moved-root
cases. Use `ctx sources add <name> --provider <provider> --root <path>
[--source-group <group>]`, or edit `[sources.roots.<name>]` in `config.toml`.
The provider capability determines whether `<path>` must be a file or directory
and how it expands into history sources. Named history roots are additive to
that provider's environment/default winner and do not affect discovery for any
other provider. A named root that resolves to the inferred physical root
annotates it rather than duplicating it. For example, a Claude history root
directory expands to `projects`; a Codex history root directory expands
independently to `sessions`, `archived_sessions`, and `history.jsonl` so one
unavailable source cannot hide a healthy peer.

OpenHands is the conditional exception to the provider-neutral command shape:
its configured root requires `--kind current-conversations` for the direct
current conversations directory or `--kind legacy-persistence` for the
released recursive persistence layout. The equivalent OpenHands config table
requires the same exact `kind` string. The current kind admits only current
`<conversation>/events/event-*.json` files. Nested automatic/configured roots
and ancestor-related configured legacy/current roots fail closed instead of
indexing overlapping history twice.

To move an existing root or change its group atomically, repeat `sources add`
with the same name and provider plus `--replace`. Supplying `--source-group`
sets the complete desired group; omitting it during replacement clears the
group. The safe editor rejects changing the provider under a stable name.
Set `[sources] automatic = false` only when all automatic provider discovery
should stop and every active configured history root should come from named
configuration; this does not delete already indexed history.

The configured name is the durable local identity of an additional history
root, not only a label. Keep the name and atomically replace its path when the
same root moves; choose a new name for an unrelated root. Reusing a removed
name intentionally reuses its logical namespace, while changing only its group
does not rotate source or citation identities.

One-shot flags, API constructor paths, old launch directories, container host
mounts, copies, and unreconstructible selectors are not automatic. Import one
with `ctx import --provider <provider> --path <path>`. That path bypasses
discovery precedence for the invocation, but not format checks, read bounds,
no-link checks, or read-only handling, and it is not remembered as a default.

Detected unsupported formats and sources marked `import_support: explicit` are
excluded from setup, `--all`, daemon refresh, and search refresh. Removing an
old automatic probe does not delete indexed history; a still-supported
compatible path can be selected explicitly.

Codex discovers current rollout leaves ending in `.jsonl` or `.jsonl.zst`
under the winning `sessions` and `archived_sessions` roots. Exact `--path`
imports accept either representation, including a renamed compressed file when
its bounded decoded catalog prefix contains the Codex session UUID. Raw and
compressed copies of the same UUID are one logical source: raw wins while both
exist, with lexical path order as the deterministic tie-breaker. Compressed
reads snapshot exactly the admitted compressed prefix and bound the combined
snapshot plus decoded spool to 256 MiB per leaf and 1 GiB across the route.

Grok Build selects absolute `$GROK_HOME/sessions` when `GROK_HOME` is set and
`~/.grok/sessions` otherwise. The override replaces the default. A native
session requires authoritative `updates.jsonl`; derived sidecars are not
discovery or import authority. Exact `updates.jsonl` files remain importable
with `--provider grok-build --path`.

DeepSeek Harness is Supported for its exact local session format version 0
only. Discovery selects absolute `$DSH_HOME/sessions` when
`DSH_HOME` is nonempty and absolute, or `~/.dsh/sessions` otherwise. Empty or
whitespace-only values are unset; relative values are not automatically
resolved because their meaning depends on the launch working directory.
Default-encoded leaves are nested `*/*/session.jsonl.zstd`; configured raw
history uses nested `*/*/session.jsonl`. Other layouts and format versions are
not supported. Hosted/cloud history is outside this local import. General
history support does not claim exact MCP server/tool attribution for this
provider. Unknown required events and future versions fail the source.
Delegated sessions remain independent imports; the immediate parent header
does not prove the transitive root identity required for typed lineage edges.

Hermes Agent is supported through the native `hermes_state_sqlite` route. On
Linux, a non-root ctx process with the certified read-only live-WAL path makes
new sessions and appended records converge on native-watch and search refreshes.
Where that fast path is unavailable, incremental refresh defers without copying
the provider database. Structural edits, deletions, and deferred increments
reconcile in roughly 60–80 minutes with a healthy daemon, or on
`ctx import --provider hermes` or `ctx import --all`. All scans are read-only
and never modify Hermes history.

Interactive discovery captures a fresh allowlisted environment and current
working directory. A long-lived daemon uses the named environment/CWD snapshot
from its launch, so restart it after changing provider root variables. A
coordinator that evaluates project-scoped providers across multiple worktrees
must call the injected discovery context once per already observed or explicitly
authorized worktree, then apply the normal bounded de-duplication. It
must not use provider roots as repository identity, infer worktrees from those
roots, or crawl for repositories; logical repository, checkout, and worktree
identity remain a separate activity-derived concern.

## Import Rules

Provider imports should be bounded, read-only, and tied to a documented source
format. Do not document a provider as locally importable until the CLI can
discover or parse that provider's real local history and the provider support
matrix marks the shipped path as Supported. Contributor-facing content and
fixture expectations are defined in
[`provider-import-policy.md`](provider-import-policy.md).
