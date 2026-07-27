# Storage And Privacy

## Canonical controls in 0.26

Version 0.26 gives each config-managed behavior one canonical environment
override. It still accepts the following old opt-out names as undocumented
compatibility inputs and emits one text-mode warning asking you to update:

- `CTX_ANALYTICS_OFF`, `CTX_DISABLE_ANALYTICS`, and
  `CTX_INSTALL_DIAGNOSTICS_OFF` map to `CTX_ANALYTICS_ENABLED=false`;
- `CTX_DAEMON_OFF` and `CTX_DISABLE_DAEMON` map to
  `CTX_DAEMON_ENABLED=false`;
- `CTX_UPGRADE_OFF` and `CTX_DISABLE_AUTO_UPGRADE` map to
  `CTX_UPGRADE_AUTO=off`.

The old names activate only for their historical truthy values. An active old
opt-out wins over a canonical enabling value. Update scripts and shell profiles
to use the canonical controls; the old names are not shown in CLI or installer
help.

Other aliases are removed in 0.26: replace `CTX_CHANNEL` with
`CTX_UPGRADE_CHANNEL`, `CTX_FUNCTIONS_BASE` with
`CTX_UPGRADE_FUNCTIONS_BASE`, and `CTX_DISABLE_SEMANTIC_SEARCH` with
`CTX_SEARCH_SEMANTIC=false`.

The duplicate `upgrade.interval_seconds` config key is also removed. Use
`upgrade.interval_hours` for persistent configuration or
`CTX_UPGRADE_INTERVAL_SECONDS` for a process-level override.

ctx stores search indexes locally. Treat the ctx data root like private source
history.

## Local Layout

Default root:

```text
~/.ctx/
  work.sqlite
  config.toml
  runtime/
    onnxruntime/
      <runtime-version>/
        <platform>/
  upgrade-state.json
  upgrade.lock
  logs/
    upgrade.log
```

`CTX_DATA_ROOT` or `--data-root` may point ctx somewhere else. The configured
root is used directly; ctx does not append another directory.

Official installer-managed binaries also have a sidecar next to the installed
binary, for example:

```text
~/.local/bin/ctx
~/.local/bin/ctx.install.json
```

The sidecar is outside the ctx data root because it describes ownership of the
installed executable, not indexed provider history.

When release metadata includes ctx-managed ONNX Runtime assets, the official
installer and development installer place those native runtime files under
`${CTX_RUNTIME_DIR:-$HOME/.ctx/runtime}/onnxruntime/<runtime-version>/<platform>`.
They are product runtime assets, not provider-history storage, and may be shared
by multiple ctx data roots on the same machine.

## What SQLite Stores

The SQLite store may contain:

- provider and source metadata;
- source file paths and import cursors when available;
- session IDs and event IDs;
- timestamps and working-directory metadata when known;
- normalized user, assistant, system, and developer conversation text;
- a bounded, versioned local source-record locator and verification digests for
  eligible truncated messages when the provider adapter can supply them;
- tool-call, command, file-touch, and lifecycle metadata;
- compact typed command/tool result outcome/evidence and an optional full-body
  `ContentRef` (SHA-256 plus exact normalized byte length);
- FTS-indexable text required for search;
- citations and offsets or line/cursor metadata when available;
- compatibility rows used by the current search implementation.

If text is searchable, assume a copy or normalized form exists in SQLite. Raw
provider transcript files may still remain in provider-owned locations such as
`~/.codex/sessions`, but the searchable parts are local ctx data too.

## Local Pro Storage

Local Pro uses one public, exact root-relative layout: the root identity is
`install.json`, the signed helper pair and transaction files are under
`pro/bin`, downloads are staged under `pro/downloads`, the encrypted derived
graph is `pro/ctx-pro.db`, and the persistent installer coordination lock is
`pro/.ctx-pro.lifecycle.lock`. A nonsecret
`pro/.ctx-pro.data-preserved` lifecycle marker distinguishes deliberate
keep-data uninstall from first use. The canonical `work.sqlite` history remains
separate and usable without Pro. The operating-system key store stores the
WorkOS session, an installation-scoped signing key, and a signed entitlement;
ctx has no plaintext credential fallback.

Key-store record identifiers are opaque hashes scoped to the root-local opaque
installation UUID and commercial environment, never to an absolute path.
Moving or renaming a complete ctx data root therefore preserves its credential,
graph-key, and entitlement-clock identity. Copying only part of a data root is
not an identity migration and fails closed when the identity is absent or
inconsistent. The persistent lifecycle lock serializes installer recovery,
staging, commit, cleanup, and final signed-pair verification across processes.

`ctx pro uninstall --keep-data` is entirely local, removes only the helper, and
records that local Pro data was deliberately preserved. It works even when
commercial configuration, network access, or the native key store is
unavailable. `ctx pro uninstall --delete-data` uses a public delete-only native
key-store adapter to remove and verify the complete local Pro inventory. It
does not need the helper and remains available after an earlier `--keep-data`
uninstall.
Interactive use asks whether to delete; noninteractive callers must explicitly
choose `--delete-data` or `--keep-data`. Neither form deletes canonical history.
A small installation-bound anti-rollback watermark may remain in the native key
store. It contains no graph key, transcript content, account token, or
entitlement body and does not make `ctx pro` report Pro as installed.

## What ctx Avoids By Default

The current CLI does not copy command/tool result bodies, stdout/stderr,
binary artifacts, image payloads, raw diffs, or provider-private blobs into
SQLite. Result events retain compact metadata, typed evidence, citations, and
an optional `ContentRef`; the original provider source remains authoritative.
See
[`provider-import-policy.md`](provider-import-policy.md) for the native adapter
content policy.

Provider-specific sensitive handles should stay out of normalized metadata when
they are not needed for local search. For example, the Warp SQLite importer
records only boolean presence for Warp server conversation tokens and does not
copy token values from `agent_conversations.conversation_data`.

No session text, prompts, transcripts, or indexed snippets are sent by ctx by
default.

`ctx show session --content complete` and
`ctx show event --content complete` read complete message bodies ephemerally from
their recorded local provider sources. Hydrated bodies are not written back to
SQLite, cached, or materialized into the Pro graph. The persisted locator is
capped at 4 KiB and the imported searchable
message prefix remains capped at 16,000 characters.

For local Pro materialization, eligible Codex result bodies are re-read from
their original JSONL records immediately before a journal page is sent. The
public resolver verifies immutable record and full-body hashes. Complete UTF-8
outputs of at most 256 KiB may be attached transiently, with at most 1 MiB total
per request; oversized, missing, changed, unreadable, or unsupported content is
omitted in full. Sidecar bytes are never written to canonical journal payloads,
digests, chunks, or the Pro graph handoff checkpoint.

## Provider-Owned Data

ctx does not own provider homes. Import reads from configured or discovered
locations and records enough information to search and cite imported material.
Discovery reads only bounded path metadata and allowlisted persistent selector
files needed to choose the provider's winning root. It does not create provider
directories, migrate provider data, execute provider commands, or combine a
selected replacement with old defaults. Exact one-shot paths are read only
after the user supplies `--path` and are not remembered as discovery policy.
If a raw source path moves or is deleted, `ctx show` and `ctx search` can still
return indexed text and should mark source availability when that information
is known.

## Command Read/Write Behavior

This table describes core command effects. It excludes the optional first-party
analytics marker described under network behavior.

| Command | Reads | Writes |
| --- | --- | --- |
| `ctx setup` | provider transcript files and home path metadata for source discovery | data root, `work.sqlite`, SQLite index, and optional daemon lock/status/job files when daemon autostart runs |
| `ctx status` | data root metadata, existing SQLite store, semantic sidecar/status metadata, ctx-owned daemon lock/status/job metadata, and Pro authorization state when installed | may advance nonsecret anti-clock-rollback security metadata during Pro entitlement authorization; does not mutate canonical history or local Pro graph data |
| `ctx sources` | bounded provider path metadata, allowlisted persistent selector files, and local history-source plugin manifests | none |
| `ctx import` | provider transcript files and path metadata, the explicit custom history JSONL file passed with `--format ctx-history-jsonl-v1 --path`, or stdout from an explicit history-source plugin command | data root, SQLite index, and optional daemon lock/status/job files when daemon autostart runs |
| `ctx show session` / `ctx show event` | SQLite index; with explicit `--content complete`, selected recorded provider source files | selected `--out` path for `show session` when provided |
| `ctx locate` | SQLite index and raw source path metadata | none |
| `ctx search` | native provider transcript files, path metadata, enabled auto history-source plugin stdout, SQLite index, and existing semantic sidecar/status metadata | SQLite index for newly discovered native provider or plugin history, and optional daemon lock/status files when background refresh autostarts maintenance; semantic-enabled search may also create query endpoint files |
| `ctx sql` | existing SQLite index only | none |
| `ctx pro` / `ctx pro setup` | operating-system key store, commercial account state, signed release metadata/artifact, canonical history | key store, signed helper installation, and encrypted derived graph; the explicit `setup` form is a synonym |
| `ctx pro manage` | key store and commercial account state | may refresh the WorkOS session in the key store and open a hosted billing-portal URL |
| `ctx pro uninstall` | helper and local Pro paths | requires or prompts for a data choice; `--keep-data` removes only the helper and records preserved local Pro data, while `--delete-data` removes and verifies local Pro data |
| `ctx docs` | embedded documentation in the binary | selected topic `--out` path for `ctx docs show --out` or selected `--out` directory for `ctx docs man --out` |
| `ctx upgrade` | signed release metadata and installed binary/sidecar metadata | installed binary for manual upgrade, install sidecar, `upgrade-state.json`, `upgrade.lock`, and `logs/upgrade.log` |
| `ctx doctor` | SQLite index, data root metadata, semantic sidecar/status metadata, and ctx-owned daemon lock/status/job metadata | none |
| `ctx daemon status` | semantic sidecar/status metadata and ctx-owned daemon lock/status/job metadata | none |
| `ctx daemon enable` / `ctx daemon disable` | `config.toml` | `config.toml` |
| `ctx daemon run` | native provider transcript files, SQLite index, semantic sidecar/status metadata, model-cache metadata, and ctx-owned daemon lock/status/job metadata | SQLite index for bounded native provider refresh, ctx-owned daemon lock/status/job metadata, and semantic sidecar/status metadata when local semantic indexing or dirty-queue freshness checks run |

Setup, import, and default search do not require source repository writes, model
APIs, API keys, or remote accounts. Without semantic opt-in they do not download
models or runtime assets; with semantic enabled, installer/runtime acquisition
and daemon maintenance may acquire the local ONNX Runtime asset and embedding
model when the installed build supports that path. Non-JSON setup and native provider imports may opportunistically start
the default-on ctx-owned background daemon maintenance profile when `[daemon].enabled` is true; use
`ctx setup --no-daemon` or `ctx import --no-daemon` for a one-run opt-out.
`ctx setup --catalog-only`, `ctx setup --json`, and `ctx import --json` do not
autostart daemon maintenance.
`ctx search --refresh off` does not refresh providers, run plugins, autostart
daemon maintenance, start semantic workers, schedule semantic indexing, or write
the main store or semantic sidecar. Default `--backend hybrid --refresh off`
uses semantic evidence only when sidecar coverage is complete and dirty work is
drained, and otherwise falls back to lexical. Explicit semantic searches may ask
the daemon query service to embed the query from an already-cached local model
and read partial existing sidecar coverage, but they do not download a model or
write semantic catch-up work during search.
Explicit imports may best-effort mark recent semantic-eligible items dirty in
the semantic sidecar when the sidecar already exists; this does not create the
sidecar, initialize the model, or embed text.
Explicit semantic search also refuses to initialize or download the embedding
model when the required local cache is missing; hybrid falls back to lexical in
that case. Default `--refresh background` lets daemon maintenance own enabled
auto history-source plugin refresh when possible, and may autostart the
configured daemon query service for semantic/hybrid retrieval; use
`--refresh wait` or `ctx import` for exhaustive foreground plugin catch-up.

When `ctx daemon run` or setup/import autostart runs the ctx-owned background
coordinator, it stores private lock/status files under `daemon/` in the ctx data
root. Setup/import autostart uses the normal background daemon profile and exits
after it becomes idle; explicit `ctx daemon run` runs the same coordinator in
the foreground. The coordinator always bounds native provider-history refresh
and local semantic indexing by its local runtime/model availability. Foreground
query activity preempts background work.
A looping daemon may keep the
local embedding model resident between passes and uses the sidecar dirty queue
to prioritize recent/stale events. Default background refresh may start the
configured daemon for local history freshness. With semantic enabled, the same
daemon-owned query service can embed the query; `ctx search --refresh off` does
not start it.

## Config Overrides

`ctx setup`, `ctx import`, and `ctx search` do not create `config.toml` for
implicit defaults. The config file is for user-managed overrides. Existing
config files are read and left in place.

Daemon maintenance is enabled by default. Disable it durably with:

```toml
[daemon]
enabled = false
```

`daemon.enabled = true` allows non-JSON setup and native provider imports to
opportunistically start the ctx-owned background daemon maintenance profile.
Use `ctx setup --no-daemon` or `ctx import --no-daemon` for a one-run opt-out.
`ctx daemon enable` and `ctx daemon disable` write only the `[daemon] enabled`
override. An explicit disabled override continues to win after CLI upgrades and
over `CTX_DAEMON_ENABLED=true`.

Local semantic search requires daemon maintenance and remains disabled by
default. Its opt-in is:

```toml
[search]
semantic = true
```

If daemon maintenance was previously disabled, re-enable it before enabling
semantic search.

Background auto-upgrade is disabled by default. `ctx upgrade enable` writes the
explicit `upgrade.auto = "apply"` opt-in for official installer-managed
binaries with a valid install sidecar. Unmanaged installs do not self-upgrade.

## Index Lifecycle

Find the active ctx root before destructive maintenance:

```bash
ctx status
```

The default root is `~/.ctx`. If you set `CTX_DATA_ROOT` or pass `--data-root`,
use that root in the commands below instead.

Re-import or update the index:

```bash
ctx import --all
ctx import --resume
ctx import --provider codex --path ~/.codex/sessions
ctx import --format ctx-history-jsonl-v1 --path ./history.jsonl
ctx import --history-source example-agent/default
```

Current adapters are safe to re-run. They rescan sources idempotently and keep
source paths or cursors when available. Imports always commit valid records and
report rejected records. Sources with no usable imported content fail, as do
unreadable or incompatible sources; ctx-owned storage or index failures abort
the command. Native
provider cursor progress is scoped by provider,
source format, and an opaque source identity derived from the configured root or
source path, so two roots for the same provider do not overwrite each other's
progress.
Custom history JSONL imports follow the same v1 lifecycle: ctx rescans the
explicit file, upserts already-imported records, stores supplied source cursor
metadata under ctx-owned custom cursor streams, and preserves event native
cursors. History-source plugins receive the previous stored cursor on each
explicit import and stream the same JSONL format to stdout. Failed plugin runs
do not advance cursors. Explicit file paths and plugin manifests are not added
to `config.toml` or treated as fixed provider homes.

## Upgrade Reindexing

When an existing `0.8.x` or `0.9.x` data root is opened by `0.10.x` or newer, ctx keeps
the SQLite database and migrates it in place. The migration rebuilds derived
search projections and marks prior provider import cache rows pending so the
next normal refresh can re-read original provider transcripts.

This is a one-time reimport, not a destructive wipe. It is needed because older
indexes can lack touched-file metadata or can contain text that was sanitized
before storage. If the original provider transcript files still exist, refresh
replaces those old rows with current local/private transcript text. If source
files were deleted or moved, ctx can still return indexed text from SQLite but
cannot reconstruct text that was already stored as a placeholder.

Writable opens also repair a historical provider-identity transition that
could leave multiple physical rows for one provider session. Rows are treated
as the same source when they share either a nonempty source identity or the
same nonempty raw source path, provided their known source formats are
compatible. The repair keeps the oldest session and event IDs canonical, moves
genuinely new events onto that session, retains the newer duplicate row's
session relationships and state, and keeps removed duplicate IDs as
compatibility aliases. Different raw paths with different source identities
remain distinct. The store also rejects future same-source duplicates at write
time.

Remove a source from future imports:

```bash
$EDITOR ~/.ctx/config.toml
```

The current CLI does not add provider source entries to `config.toml`; default
provider locations are discovered each time and explicit `--path` imports are
not remembered as future defaults. Custom history JSONL paths are also
one-shot explicit imports. To remove already indexed data, rebuild the index and
import only the sources you still want.

## SQL Inspection

`ctx sql` is a read-only advanced inspection command for cases normal search
does not express, such as exact counts, joins, audits, and one-off scripts. It
opens the existing SQLite store in read-only mode, rejects writes, rejects
multiple statements, enforces row/column/value caps, and times out long-running
queries. It also applies SQLite runtime limits to bound SQL text and generated
value allocation. It does not initialize or migrate the store; run a writable
command such as `ctx setup` or `ctx import` first when a schema migration is
required.

Stable read-only views are the preferred compatibility surface:

- `ctx_sessions`;
- `ctx_events`;
- `ctx_files_touched`;
- `ctx_sources`.

Run `ctx docs show sql` for view schemas, examples, limits, and output formats.
Internal tables remain local and queryable, but they are implementation details
and can change across versions. SQL output is private local history by default.

Reset and rebuild the index:

```bash
rm -f ~/.ctx/work.sqlite ~/.ctx/work.sqlite-wal ~/.ctx/work.sqlite-shm
ctx setup
```

This removes the local SQLite index and recreates it from provider history. It
does not delete raw provider transcript files.

Inspect storage size:

```bash
du -sh ~/.ctx
du -h ~/.ctx/work.sqlite*
ctx status --json
```

Delete all ctx data:

```bash
rm -rf ~/.ctx
```

This removes ctx's local index, config, and logs for the default root. It does
not remove provider-owned history such as `~/.codex/sessions`.

## Privacy Truth

Indexed prompts, code, commands, file paths, and failed-output diagnostic
previews may contain credentials, customer data, private repository names, or
proprietary design notes.

Recommended handling:

- keep `~/.ctx` out of source repositories;
- do not share SQLite databases or logs;
- review JSON output before sharing it outside the machine;
- delete or reinitialize the local store when working on shared machines;
- use provider filters and result limits to keep agent retrieval focused on
  relevant material.

## Network Behavior

Core indexing work uses local filesystem and SQLite operations. The tools that
originally produced provider transcripts may have used the network according to
their own configuration; ctx indexing those transcripts does not repeat that
behavior.

Local Pro setup and renewal contact WorkOS for identity, the ctx commercial API
for normalized billing state and signed entitlements, and the signed artifact
service for helper installation. `ctx pro manage` creates a hosted Stripe portal
session. These requests do not include transcript text, source content,
repository paths, facts, graph rows, queries, or query results. Valid offline
grants keep local graph operations usable when renewal is temporarily
unavailable.

An expired trial or subscription locks graph operations without deleting
canonical history, encrypted graph data, or key material. Resubscription plus
`ctx pro` refreshes the entitlement and restores the preserved graph.

Official installer-managed binaries can contact the signed release metadata
endpoint for an explicit `ctx upgrade` command. After `ctx upgrade enable`,
they can also perform background auto-upgrade checks after successful normal
commands. These checks are skipped for `ctx status`, JSON
commands, MCP, `ctx docs`, `ctx sql`, `ctx upgrade`, CI, unmanaged installs, and
the process-level `CTX_UPGRADE_AUTO=off` opt-out. Upgrade metadata checks do not send provider
transcript text, search queries, result snippets, source paths, repository
names, or command output.

ctx first-party telemetry is default-on and uses four content-free, versioned
families only: `operation_completed@1`, `provider_refresh_completed@1`,
`runtime_observation@1`, and `install_stage@1`. The root-local `install.json`
is product state independent of analytics.

`operation_completed@1` reports one terminal outcome for eligible foreground
operations. Current CLI coverage includes setup, explicit import, status,
index, sources, show, locate, search, read-only SQL, docs, integrations,
upgrade, and doctor. Help, version output, and command-line parse errors are not
observed. MCP and the daemon are first-class reporting surfaces rather than
being labeled as CLI traffic. Pro reporting is limited to the public host
surface; private Pro graph, entitlement, and query internals are not telemetry
inputs.

`provider_refresh_completed@1` carries only closed provider-refresh summaries
that a producer already has safely available. `runtime_observation@1` is for
low-frequency daemon or MCP lifecycle and liveness observations, not per-loop
or per-request tracing. These three batch families are delivered to
`https://cli.ctx.rs/functions/v1/analytics`; `file://` endpoints remain
available as local test sinks. `install_stage@1` is produced only by the hosted
shell and PowerShell installers and is sent as a standalone body to the hosted
install endpoint, not through the Rust analytics batch.

Batch events use a UUIDv4 event identifier, a minute-rounded occurrence time, a
closed outcome, and a duration bucket. Identity-bearing rows do not contain an
exact duration. Closed properties may include ctx version, OS, architecture,
fixed operation and option enums, booleans, selected provider enums, bucketed
counts or text lengths, and a coarse execution-capability snapshot. That
snapshot can describe available parallelism, host-visible memory range, CPU
vector support, and whether the platform is a candidate for Apple Neural
Engine or NVIDIA CUDA acceleration. It does not load an accelerator runtime,
collect component names, or derive a machine identity.

Telemetry never includes raw history or transcript data, prompts, responses,
SQL or search query text, result rows or snippets, source bodies, source or
repository paths, target values, repository or branch names, native session
IDs, command text or output, raw error strings, credentials, authorization
headers, access tokens, secrets, usernames, hostnames, raw IP addresses, exact
CPU or GPU names, serial numbers, hardware IDs, live utilization, or benchmark
results.

The data-root identifier lives in `install.json` and represents that local
index even when analytics are disabled. The client-profile identifier is a
random UUID used only for analytics events; it lives outside the ctx data root
in OS user state, such as `$XDG_STATE_HOME/ctx/device.json` or
`~/.local/state/ctx/device.json` on Linux. When a capability snapshot is
eligible, ctx creates a private versioned claim in that state directory and
promotes it to a version marker after delivery. A failed or uncertain delivery
does not change command output or exit status and leaves the claim in place to
avoid replay.

Disable telemetry with either CLI control:

```toml
[analytics]
enabled = false
```

```bash
export CTX_ANALYTICS_ENABLED=false
```

Either explicit opt-out disables CLI analytics. A config opt-out wins over
`CTX_ANALYTICS_ENABLED=true` and over an endpoint override.

### Installer diagnostics

The official shell and PowerShell installers produce `install_stage@1` with
exactly `event_name`, `event_version`, `install_attempt_id`, `stage`, `status`,
`platform`, `arch`, and `script_family`. The anonymous
per-response install-attempt identifier is stored only as a server-side hash.
Installer diagnostics follow the same content-free restrictions and never
include command output, raw errors, paths, credentials, or downloaded file
contents. Disable installer diagnostics and product telemetry during setup with
the same canonical process-level control:

```bash
export CTX_ANALYTICS_ENABLED=false
```
