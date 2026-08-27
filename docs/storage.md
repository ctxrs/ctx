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
`CTX_UPGRADE_CHANNEL` and `CTX_DISABLE_SEMANTIC_SEARCH` with
`CTX_SEARCH_SEMANTIC=false`. `CTX_FUNCTIONS_BASE`,
`CTX_UPGRADE_FUNCTIONS_BASE`, and `upgrade.functions_base` are removed without
replacement: release metadata, signature, verification-key, and artifact-origin
authority is compiled into the production binary.

The duplicate `upgrade.interval_seconds` config key is also removed. Use
`upgrade.interval_hours` for persistent configuration or
`CTX_UPGRADE_INTERVAL_SECONDS` for a process-level override.

The canonical persisted indexing control is `[indexing] mode = "auto"` or
`"manual"`. Auto is the default. Use `ctx index mode` to read the effective
mode and `ctx index mode auto` or `ctx index mode manual` to persist a change.

ctx stores immutable Core/Tantivy search generations, optional semantic data,
and content-free local usage aggregates locally. Treat the ctx data root like
private source history.

## Local Layout

Default root:

```text
~/.ctx/
  search/
    lexical/
    semantic/
  usage.sqlite
  config.toml
  runtime/
    onnxruntime/
      <runtime-version>/
        <platform>/
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

## Local Usage Product State

`usage.sqlite` is an owner-private SQLite sidecar under the selected ctx data
root. It is separate from provider history and Core/Tantivy and semantic search
data. Local usage is product state, not telemetry: it has no network path or
analytics identity and remains available when analytics are disabled.

Version 1 stores daily UTC aggregate rows only. Its closed dimensions are UTC
day, usage-definition version, ctx binary/client version, surface (`cli` or
`mcp`), logical operation, technical outcome, result class, and duration
bucket. Aggregate counters cover calls, content-free result/citation totals
when a handler already has them, and serialized MCP response bytes. MCP
response bytes are factual JSON-RPC transport bytes, including the delivered
newline; they are not tokens, token savings, cost savings, or model context.

The sidecar never stores a query, path, repository, selector, argument, prompt,
session/event/citation ID, exact timestamp, transcript-derived data, output
body, machine identity, or analytics identity. Result class is explicitly
`result_bearing`, `empty`, or `not_applicable`; unclassified operations are
not reported as empty. Failures always use N/A with zero result and citation
totals. CLI status report/control operations are excluded from both recording
and the persisted operation vocabulary.

Only one completed foreground CLI command or recognized MCP `tools/call` is
counted. MCP records after the complete response has serialized, written, and
flushed. Initialization, ping, tool listing, notifications, unknown tools,
automatic daemon cycles, and liveness checks are excluded.

The sidecar uses WAL, `synchronous=NORMAL`, a short busy timeout, atomic
upserts, a fixed application ID/schema version, and owner-only file behavior
where the platform supports it. Recording is fail-open: contention or storage
failure does not change the foreground command result. Reporting instead
returns a stable content-free error code and never fabricates zero. Rows older
than approximately 400 UTC days are deleted at most once per UTC day; recording
does not compact or vacuum per call. A 4 KiB page size and 6 MiB main-database
cap, plus a 1 MiB WAL limit and frequent automatic checkpoints, keep the
quiescent `usage.sqlite`/WAL/SHM family below 8 MiB with headroom. Reaching the
cap is another silent recording failure.

Existing sidecars, data roots, and WAL/SHM members are preflighted as
owner-private regular non-symlinks before SQLite access. Existing application
ID, schema, 4 KiB page size, and current size are verified read-only before any
persistent PRAGMA; every SQLite connection requests no-follow behavior. A
complete new database is initialized privately and atomically published, so
concurrent first writers never expose a partial schema. Unknown sidecars are
not adopted or switched to WAL. Before any writable source open, a detached
image validates every aggregate and maintenance row; a nonempty existing WAL or
SHM is rejected without changing any family member. Ordinary recording remains
fail-open, while explicit reset reports a stable failure. Report discovery
opens the filesystem handle read-only. Reports query a checkpointed main
database without creating or changing WAL/SHM. A nonempty auxiliary state that
cannot be consumed portably without filesystem writes produces the explicit
stable report error instead.

The report path captures the main file twice through a retained native
read-only handle, requires byte-for-byte equality with family identity and link
checks before, between, and after the reads, and queries a detached read-only
in-memory SQLite image with `query_only=ON` and `temp_store=MEMORY`.
Initialization uses exactly eight fixed, owner-private no-replace staging
slots. Productive opens reclaim stale slots older than one hour by exact name,
independent of directory enumeration order; fresh or unsafe slots are left
untouched.

A future `daily_usage` UTC day is an integrity/clock error and blocks older
upserts. A future maintenance-only retention marker is repaired to the current
UTC day by the next productive aggregate write, so maintenance metadata alone
cannot permanently disable retention.

When release metadata includes ctx-managed ONNX Runtime assets, the official
installer and development installer place those native runtime files under
`${CTX_RUNTIME_DIR:-$HOME/.ctx/runtime}/onnxruntime/<runtime-version>/<platform>`.
They are product runtime assets, not provider-history storage, and may be shared
by multiple ctx data roots on the same machine.

## Provider sources, Core, and derived storage

Provider-owned history remains the acquisition authority for import and
refresh. Those flows read native JSONL, document/tree, and provider SQLite
formats plus explicit plugin/custom streams, apply content policy, and publish
a Core/Tantivy generation with stable ctx identities, complete policy-selected
normalized records, and source identity metadata. Provider SQLite adapters use
short read-only logical snapshots with normal committed WAL visibility and
never checkpoint or write provider data.

### Core refresh request boundary

Core persists one logical refresh intent: automatic maintenance, or a selected
import of all sources, one provider, or one exact source authority. A single
admission resolver turns that intent into certified exact routes. The executor
accepts only this admitted request; it does not discover providers, infer a
missing selection, or widen selected work. Interrupted requests persist their
logical intent and are re-admitted through the same resolver before execution,
so missing or changed selected routes fail closed.

Request receipts describe only the routes and rejections observed by that
request. Generation metadata separately describes the complete retained
publication. A selected no-op can therefore report its own rejection outcome
without changing generation-wide totals or the daemon's global watch catalog.

- `search/lexical` contains immutable verified Core/Tantivy generations used
  for lexical search and indexed snippets.
- `search/semantic` contains generation-bound flat-F32 vectors, hashes, and
  offsets derived from eligible Core content; it does not persist separate
  plaintext transcript chunks.
- `usage.sqlite` contains only the bounded content-free aggregates documented
  above. It is product state, not history or search authority.

A selected Core event may carry content-governed `activity` with exact typed
provider call identity, invocation and/or result channels, and ordered literal
provider facts. Present arguments and structured results are complete decoded
JSON, not raw source-byte representations. The normalized body,
provider-native structured content, and activity share the 16 MiB Core
selected-content budget. Oversized complete JSON or text channels become
explicit `omitted` capture states without truncation.

Activity is retrievable from full-content event output. For discovery-eligible
selected content, retained protocol/server/tool/present arguments, result
status/present text/structured content, and literal fact values enter the
shared Core search projection used by lexical search, snippets, and semantic
source text. Provider call identity, timing, and capture-disposition labels do
not. No activity value is written to `usage.sqlite`, and query paths do not
reconstruct it from provider-specific content or current MCP configuration.
`--content text` and `--content none` omit activity from chronology output;
Core non-selected content cannot carry it.

Capture can separately mark a complete record as ctx-retrieval-derived when
every body-contributing atom is an exactly recognized direct ctx retrieval call
or its uniquely linked, successful, payload-only result. Such records remain in
the stored Core generation for show and enumeration, but their bodies do not
enter ranked lexical or semantic discovery or term statistics. This policy is
fail-open: errors, warnings, stderr, mixed content, unknown status, unsupported
aliases, and ambiguous linkage remain discoverable. The marker is not
redaction, omission, or deletion.

Search projection is selective rather than field-addressable: present
invocation arguments and result content are included, while provider call IDs,
timing, and capture-state metadata are excluded. Result text with a
`normalized_body` disposition retains the event's existing body search behavior
exactly once. Activity adds no dedicated selector, filter, search result field,
`usage.sqlite` value, or SQL column. See
[`mcp-exchange-capture.md`](mcp-exchange-capture.md).

Search, show, list, locate, and MCP retrieval read verified Core/Tantivy
generations. List continuations remain bound to the named active or retained
generation, and JSONL holds one generation pin for its whole traversal. Show
and list present the exact, complete policy-selected normalized records stored
in Core, while search snippets come from the Core-backed searchable projection
and locate returns bounded Core source identity metadata. None of these read
paths reopens provider history. Provider changes become visible after import or
daemon refresh publishes a new Core generation. Search projection changes are
part of lexical generation identity, so an older active generation is rebuilt
or passes the narrow same-epoch preservation migration before newly projected
invocation terms become searchable.

The generation manifest commits the global automatic-discovery policy and, for
configured provider history roots, the normalized named-root definitions, their
configuration digest, optional group, and exact source-route membership. Search
resolves `--source-root` and `--source-group` only through this pinned manifest and
translates the result to exact indexed source keys. Live config is never mixed
with an older generation, and all roots remain in one Core/Tantivy index.
The ordinary inferred history root keeps its released source identities when
it is given a name. Additional named roots use the provider plus stable root
name as logical lineage, so matching native session ids in work and personal
remain independent while moving a named root does not rotate citations. Codex
`sessions`, `archived_sessions`, and prompt history under one configured root
share that logical root lineage; active and archived duplicate representations
coalesce.

Lexical publication keeps the active generation and one previous generation
for recovery and pinned readers; their manifests and integrity receipts use the
same two-generation bound. Append-only segments merge after sixteen comparable
segments accumulate. A refresh that rewrites, replaces, or deletes indexed
records marks the superseded documents deleted in their Tantivy segments. This
incremental behavior is independent of the removed `relational.sqlite` path.
The merge policy expunges an individual segment only when its deleted-document
share is strictly more than 25%; exactly 25% does not trigger reclamation. The
refresh waits for any triggered merge before publication completes. Thus every
published active segment has at most 25% deletions, while exact no-op refreshes
perform no writer or merge work.

Generation candidates hard-link unchanged base segments when the filesystem
supports it. For threshold planning, let `F` be the physical footprint of the
same live documents in a deletion-free generation. Assume stored bytes scale
with physical document slots, the active and retained previous generations are
distinct and each sits at exactly 25% deletions, and a worst-case compaction
rewrites all active live documents; semantic storage is excluded.
Under those assumptions, the active generation approaches `4/3 F` (about
`1.33 F`), and active plus previous approach `8/3 F` (about `2.67 F`). A
hard-linked candidate reuses the active segment bytes, so one deletion-free
merge output brings peak physical storage to about `11/3 F` (`3.67 F`). When
candidate cloning must copy instead, the extra `4/3 F` candidate copy brings
the peak to about `5 F`. These are conservative theoretical estimates, not hard
byte bounds: compression, changed content, partial-segment reclamation, and
segment sharing between retained generations can move actual ratios. Merge
latency and I/O are charged only to a mutating refresh that crosses the strict
deletion or fan-in bound. Delete reclamation rewrites only the affected segment
unless it is already part of scheduled fan-in work, so same-size cold segments
are not pulled into the rewrite. There are no unbounded background generations
or manifests.

Pre-v0.26 history is never opened, migrated, used as fallback, or deleted by
the new architecture. Old Store files are inert and may be removed explicitly
by their owner.

## What ctx Avoids By Default

The current CLI does not retain payload classes excluded by policy, including
binary artifacts, image payloads, raw diffs, and provider-private blobs. Core
retains complete policy-selected normalized message/result records, metadata,
and stable provider/source identities. Show reads those stored Core records.
See
[`provider-import-policy.md`](provider-import-policy.md) for the native adapter
content policy.

Provider-specific sensitive handles should stay out of normalized metadata when
they are not needed for local search. For example, the Warp SQLite importer
records only boolean presence for Warp server conversation tokens and does not
copy token values from `agent_conversations.conversation_data`.

No session text, prompts, transcripts, or indexed snippets are sent by ctx by
default.

Search, show, and MCP presentation consume the verified Core/Tantivy generation.
Output remains bounded, and content excluded by import policy is not
reconstructed.

## Provider-Owned Data

ctx does not own provider history roots. Import reads from configured or
discovered locations and records enough information to search and cite imported material.
Discovery reads only bounded path metadata and allowlisted persistent selector
files needed to choose the provider's winning root. It does not create provider
directories, migrate provider data, execute provider commands, or combine a
selected replacement with old defaults. Exact one-shot paths are read only
after the user supplies `--path` and are not remembered as discovery policy.
If a source moves, changes, or is deleted, the active Core/Tantivy generation
remains available for search and typed presentation. Refresh or explicit import
discovers the new source state and atomically publishes the corresponding
generation; a failed refresh leaves the prior verified generation active.

## Command Read/Write Behavior

This table describes core command effects. It excludes the independent
best-effort daily aggregate upsert to `usage.sqlite` and the optional
first-party analytics marker described under network behavior. Disable the
local upsert as described above.

| Command | Reads | Writes |
| --- | --- | --- |
| `ctx setup` | provider transcript files and bounded path metadata for source discovery | data root, source catalog/epoch metadata, `search/lexical`, and optional persistent daemon lock/status/job files in automatic mode; old Store artifacts are neither opened nor deleted |
| `ctx status` | data root metadata, source epoch, lexical/semantic generation metadata, daemon state, and compact local usage health | none; does not mutate provider history, Core generations, or usage aggregates |
| `ctx index` / `ctx index watch` / `ctx index wait` | indexing mode, lexical/semantic generation metadata, and daemon state | none |
| `ctx index mode` | `config.toml` when present | none when reading; `auto` or `manual` writes `config.toml` and establishes or removes persistent supervision |
| `ctx stats` | owner-private aggregate `usage.sqlite` when present | none; does not create pristine usage state or count itself |
| `ctx sources` | bounded provider path metadata, allowlisted persistent selector files, local history-source plugin manifests, and configured named history roots | none |
| `ctx sources add [--replace]` / `ctx sources remove` | `config.toml` and named provider history root path metadata used for validation | atomically updates `config.toml`; provider history is never modified |
| `ctx import` | provider transcript files and path metadata, the explicit custom history JSONL file passed with `--input-format ctx-history-jsonl-v2 --path`, or a durable provider-owned custom history JSONL file declared by an explicit history-source plugin manifest | immutable candidate Core/Tantivy generation and atomic publication, catalog/epoch metadata, and optional persistent or finite-worker daemon files; finite workers do not run semantic work |
| `ctx show session` / `ctx show event` | complete policy-selected records in the active verified Core/Tantivy generation | selected `--out` path for `show session` when provided |
| `ctx list events` | complete policy-selected records and existing index terms in one pinned verified Core/Tantivy generation | none; event enumeration is read-only |
| `ctx search` | active verified Core/Tantivy generation and existing semantic generation; when refresh has authority, bounded provider discovery/path metadata | candidate Core publication and daemon state only when refresh has authority; manual background and `--refresh off` do not start or wake a process |
| `ctx docs` | embedded documentation in the binary | selected topic `--out` path for `ctx docs show --out` or selected `--out` directory for `ctx docs man --out` |
| `ctx upgrade` | signed release metadata and installed binary/sidecar metadata | installed binary for manual upgrade, install sidecar, and executable-adjacent `.ctx.upgrade-state.json`, `.ctx.install.lock`, and transaction journal |
| `ctx doctor` | source epoch, lexical/semantic generation metadata, and ctx-owned daemon lock/status/job metadata | none |
| `ctx daemon run` | provider transcripts, active lexical and semantic generations, model-cache metadata, and daemon state | candidate lexical generation publication, semantic catch-up, and daemon state |

Setup, import, and default search do not require source repository writes, model
APIs, API keys, or remote accounts. Without semantic opt-in they do not download
models or runtime assets; with semantic enabled, installer/runtime acquisition
and daemon maintenance may acquire the local ONNX Runtime asset and embedding
model when the installed build supports that path. In automatic indexing mode,
setup and import may start the persistent ctx-owned daemon regardless of output
format. In manual mode, setup starts no worker; explicit imports may start only
a finite Core worker using the same source-refresh endpoint and publication
engine. Use `ctx setup --no-daemon` or `ctx import --no-daemon` for a one-run
opt-out; an explicit provider-source import with that opt-out requires an
existing endpoint.
The deprecated `ctx setup --catalog-only` flag is ignored and does not change
daemon-autostart behavior.
`ctx search --refresh off` does not refresh providers, run plugins, autostart
daemon maintenance, start semantic workers, schedule semantic indexing, or
write any derived generation. It serves results from the active Core
generation. Default `--backend hybrid --refresh off`
uses semantic evidence only when semantic coverage is complete and dirty work is
drained, and otherwise falls back to lexical. Explicit semantic searches may ask
the daemon query service to embed the query from an already-cached local model
and read partial existing semantic generation coverage, but they do not
download a model or write semantic catch-up work during search.
Semantic coverage is exact across the content-filter boundary. Persisted
flat-F32 source receipts account for every pre-filter Core candidate as either
an active projected event or an intentionally filtered event. Query metadata
filters score only the intersection with active projected events. Derived
semantic state from an older filter-unaware receipt contract is discarded and
rebuilt from the current committed Core generation; provider history is not
needed for that rebuild. In automatic indexing mode, the persistent scheduler
binds ready semantic jobs to the current source-projection contract fingerprint.
A missing or stale fingerprint forces writable maintenance even when the Core
generation is unchanged, so opening the derived store performs the reset and
rebuild. Manual indexing remains passive and performs no scheduled migration.
Explicit imports may best-effort mark recent semantic-eligible items dirty in
the semantic generation when it already exists; this does not create semantic
storage, initialize the model, or embed text.
Explicit semantic search also refuses to initialize or download the embedding
model when the required local cache is missing; hybrid falls back to lexical in
that case. In automatic mode, default `--refresh background` lets persistent
daemon maintenance own native provider refresh and may autostart the configured
daemon query service for semantic/hybrid retrieval. In manual mode, background
refresh serves the last published generation without starting or waking a
process. Explicit `--refresh wait` may start a finite Core worker, but that
worker never starts semantic services or catch-up. History-source plugins are
refreshed only by an explicit selected-plugin import in 1.0.

When auto mode or `ctx daemon run` starts the persistent coordinator, it stores
private lock/status files under `daemon/` in the ctx data root. Auto mode owns
background startup; there is no separate public daemon start command. Explicit
`ctx daemon run` runs the same coordinator in the foreground and blocks until
stopped without changing indexing mode; `--force` is required in manual mode.
The coordinator always bounds native provider-history refresh and local semantic
indexing by its local runtime/model availability. Foreground query activity
preempts background work.

Manual explicit import and search `--refresh wait` instead start the same Core
refresh engine in a finite worker. The finite profile does not install native
or detached supervision and does not run watcher, timer, semantic, scheduled
reconciliation, or upgrade maintenance. It waits for at least one admitted Core
request and exits only after all Core requests are terminal and its IPC endpoint
is quiescent. Post-ack observation recovery may start another finite worker to
observe or complete the authoritative request.

A hosted managed install probes systemd-user, the launchd GUI user domain, or
current-user Task Scheduler before changing native registration state. When
that manager is unavailable, automatic setup/import runs this same coordinator
as a persistent detached process, preserves any unverified native artifact for
a later retry, and reports that native automatic restart after failure, login,
or reboot is unavailable. The next eligible automatic ctx command self-heals
an absent process. This fallback does not create a second importer or index
writer.
Native ownership, identity, integrity, fencing, and security failures still
fail closed.
A looping daemon may keep the
local embedding model resident between passes and uses semantic projection state
to prioritize recent/stale events. Automatic background refresh may start the
configured persistent daemon for local history freshness. With semantic
enabled, the same daemon-owned query service can embed the query;
`ctx search --refresh off` and manual background refresh do not start it.

## Config Overrides

`ctx setup`, `ctx import`, and `ctx search` do not create `config.toml` for
implicit defaults. The config file is for user-managed overrides. Existing
config files are read and left in place.

Named provider history roots are an optional override for providers whose
configured-root capability is enabled. The capability defines whether the root
is a file or directory:

```toml
[sources.roots.personal]
provider = "claude"
path = "/absolute/path/to/claude-personal"
group = "personal"

[sources.roots.work]
provider = "codex"
path = "/absolute/path/to/codex-work"
group = "work"
```

The equivalent safe editor is `ctx sources add <name> --provider <provider>
--root <existing-path> [--source-group <group>]`; remove an entry with `ctx
sources remove <name>`. Configured entries are additive to the provider's
ordinary inferred root. If both select the same physical root, the configured
name and group annotate that one route instead of creating a duplicate. A
malformed edit is rejected as one config and does not publish a partial source
change; the previous verified generation remains the query authority. Removing
a valid entry withdraws its name, group, and future refresh ownership, but does
not delete history already retained in the verified generation. The removed
name and group stop matching that history. Exact-path imports and plugin
manifests remain one-shot authorities and are not promoted into named roots.

For an independently configured history root, `provider` plus the stable root
name is its logical source namespace. Replacing only `path` therefore preserves
source, session, route, and citation identity across a move; changing the name
creates a different logical root. A configured root that is physically the
ordinary inferred root retains the released automatic namespace for
compatibility.
The name is a durable local mount key, not a cosmetic label: removing and later
reusing it intentionally reuses that logical namespace. Use a new name when
registering an unrelated root, even if the old definition was removed; matching
provider-native session ids under a reused name reconcile as the same history.
Group changes do not change identity.
Groups and names remain local provenance/query metadata, not upload consent,
an ACL, a tenant boundary, or a retention rule.

To update an existing definition safely, repeat `sources add` with the same
name and provider and pass `--replace`. An absent name is added, identical
canonical settings are a no-op, and a provider mismatch is rejected. During a
replacement, `--source-group <group>` sets the complete desired group and
omitting it clears the group. The command holds the shared config lock across
read, validation, and durable replacement, so daemon refresh cannot observe an
intermediate removal.

Set `[sources] automatic = false` to stop all future automatic provider history
root selection while retaining named provider history roots. This policy
change does not erase already indexed automatic history. Searches remain
unfiltered by source by default; `--source-root` and `--source-group` are
explicit per-query filters.

Local usage aggregation is enabled by default and is independent of analytics:

```toml
[local_usage]
enabled = false
```

Use the exact value `CTX_LOCAL_USAGE_ENABLED=false` for a process-level hard
disable; the only accepted environment values are lowercase `true` and
`false`. Invalid or non-Unicode values fail closed for recording. A
persistent disable wins over `CTX_LOCAL_USAGE_ENABLED=true`. Disabled commands
do not create `usage.sqlite`. The equivalent durable controls are
`ctx status --usage disable` and `ctx status --usage enable`; use
`ctx status --usage reset` to clear all aggregates without deleting canonical
history. `ctx stats --detail` expands the read-only local
report with CLI/MCP operation and latency breakdowns.
Reset is logical SQLite deletion followed by a best-effort truncate checkpoint;
it is not a claim of forensic secure erasure on SSDs or other storage. The
enable, disable, and reset control invocations are not themselves counted.
Stats reporting is also uncounted and does not create the sidecar.

The p99/unit bounds in this repository exercise 1,000 samples of the warm
aggregate path (at most 10 ms p99 in an optimized release build), content
refresh, lock contention, and quiescent family-size arithmetic. The
debug/fastbuild warm-upsert smoke uses a coarse 500 ms runaway-I/O ceiling;
optimized release qualification enforces the 10 ms p99 contract.
Qualification on every supported filesystem/platform remains release evidence;
the repository tests do not claim that cross-platform qualification has already
been completed.

Indexing is automatic by default. Select manual indexing durably with:

```toml
[indexing]
mode = "manual"
```

Automatic mode allows eligible setup, import, and background search operations
to start or wake the persistent ctx-owned daemon. Manual mode runs no persistent
or background daemon: setup and ordinary/background search remain inert, while
explicit import and search `--refresh wait` may use a finite Core worker.
`--refresh off` and explicit `--no-daemon` controls never start or wake either
profile.

Use the public indexing controls to inspect or change the setting:

```bash
ctx index mode
ctx index mode auto
ctx index mode manual
```

Mode setters persist the requested canonical value and reconcile supervision to
the effective mode. When auto is not overridden, ctx installs or repairs
supervision and starts the persistent background daemon. Manual mode stops it
and removes supervision. An explicit manual config continues to win after CLI
upgrades and over `CTX_DAEMON_ENABLED=true`; `CTX_DAEMON_ENABLED=false` remains
a process-level manual-mode override.

For a daemon that serves and serializes only atomic Core refreshes,
set:

```toml
[daemon]
mode = "source-refresh-only"
```

The equivalent process override is
`CTX_DAEMON_MODE=source-refresh-only`; `full` selects the normal maintenance
profile. Autostart propagates the effective mode to its detached child. In
source-refresh-only mode, the source refresh IPC endpoint, all-provider capture
registry, atomic generation publication, status reporting, disable behavior,
and persistent process lifecycle remain active. History refresh, semantic
indexing and serving, canonical maintenance, and daemon-driven automatic
upgrades do not run. Ordinary foreground commands do not substitute for the
disabled maintenance paths. Manual finite workers always enforce the Core-only
exclusions independently of this persistent-daemon setting.

Local semantic search remains disabled by default and requires automatic
indexing. Its config opt-in is:

```toml
[search]
semantic = true
```

See [Retrieval backends](search.md#retrieval-backends) for the setup command and
readiness behavior.

Automatic upgrade uses `upgrade.auto = "apply"` by default for official
installer-managed binaries with a valid install sidecar. Automatic indexing
with the full daemon profile uses the enabled persistent daemon as the sole
automatic check and apply driver. Manual indexing, source-refresh-only mode,
ordinary foreground commands, MCP, and finite workers perform no automatic
upgrade work. Explicit `ctx upgrade` remains available. `ctx upgrade disable`
writes an explicit `upgrade.auto = "off"` opt-out. Unmanaged installs do not
self-upgrade.

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
ctx import --input-format ctx-history-jsonl-v2 --path ./history.jsonl
ctx import --history-source example-agent/default
```

Current adapters are safe to re-run. They rescan sources idempotently and keep
stable source identity and import-progress metadata. Imports always commit valid
records; human receipts summarize record-local rejections as `Skipped records`
while JSON preserves rejection diagnostics. Sources with no usable imported content
fail, as do unreadable or incompatible sources; ctx-owned generation failures
abort the command. Native provider progress is scoped by provider, source format,
and an opaque source identity derived from the released provider namespace or
the stable logical name of an independently configured provider root.

Refresh builds a private immutable Core/Tantivy candidate containing complete
normalized stored records from current provider sources, verifies its manifest
and policy identity, then atomically publishes it under `search/lexical`.
Published generations are never opened writable. Semantic data advances only
against a verified pinned Core generation and fails closed on generation
mismatch.

Custom history JSONL and history-source plugins follow the same import and Core
publication lifecycle. Failed plugin runs do not advance cursor state.
Explicit file paths and plugin manifests are not added to `config.toml` or
treated as fixed provider history roots.

## v0.26 epoch transition

v0.26 starts a fresh Core/Tantivy search epoch from provider-owned source
history. It does not migrate, read, or delete the legacy canonical Store or use
it as fallback. An old `work.sqlite` family is inert and remains an explicit
owner-managed artifact.

Setup discovers current provider sources and builds a new Core/Tantivy
generation, then schedules optional semantic work.
If a source needed for rebuilding no longer exists, ctx reports that source as
unavailable; it does not recover content from prior-epoch rows.

Generation policy includes complete normalized stored-record encoding,
meaningful-body lexical selection, event projector/class revision, tokenizer
and lexical schema revision, and semantic settings. A mismatch makes derived
storage stale and requires a Core rebuild rather than an in-place migration.

Activity participates in the current Core record contract fingerprint. An
incompatible predecessor requires the ordinary Core rebuild path; query code
does not reopen provider history or synthesize activity. A later provider
refresh or reimport can populate current activity when the qualifying source is
available. This does not read or migrate the legacy Store/SQL epoch described
above.

Remove a configured named history root from future refreshes:

```bash
ctx sources remove work
```

You can make the same change in `config.toml`. If the root is the last member
of a group, that group simply stops matching it. Default provider locations are
still discovered alongside any remaining named roots, and explicit `--path`,
custom JSONL, and plugin imports are not remembered as future defaults. The next
full refresh atomically withdraws the removed root's name, group, and active
ownership while preserving its already indexed history under the stable source
identity. To purge that retained history, remove the root, reset local search
storage as described below, and rebuild; the records can still return if the
same provider history is selected through another available route. A failed or
malformed refresh leaves the previous verified generation active.

## Reset And Inspect Local Search Storage

Reset and rebuild Core/Tantivy plus optional semantic data:

```bash
rm -rf ~/.ctx/search/lexical ~/.ctx/search/semantic
ctx setup
```

This removes the active Core/Tantivy generation and semantic sidecar, then
imports complete policy-selected normalized records and stable provider/source
identities from the available provider histories. It does not delete provider
transcripts or `usage.sqlite`.

Inspect storage size:

```bash
du -sh ~/.ctx
du -sh ~/.ctx/search/lexical ~/.ctx/search/semantic
ctx status --format json
```

Delete all ctx-owned data:

```bash
rm -rf ~/.ctx
```

This deletes ctx Core/Tantivy and semantic data, `usage.sqlite`, config, logs,
and root-local metadata. It does not remove provider-owned history such as
`~/.codex/sessions`.

## Privacy Truth

Provider transcripts, indexed terms, Core snippets, commands, and file
paths may contain credentials, customer data, private repository names, or
proprietary design notes.

Exact MCP server and tool names are opaque local data with the same handling
requirements. They may themselves contain credentials, paths, customer or
repository identifiers, Unicode controls, or terminal escape content. Exact
JSON/JSONL and MCP structured output is therefore not share-safe without
review. Captured MCP arguments and response payloads can contain the same data,
plus arbitrary provider-native output. Human terminal and Markdown views escape
identity controls and structure; a display-bounded value is visibly marked
rather than silently changing the machine value. See
[`mcp-tool-call-attribution.md`](mcp-tool-call-attribution.md).

Recommended handling:

- keep `~/.ctx` out of source repositories;
- do not share provider transcripts, ctx search generations, or logs;
  logs;
- review JSON output before sharing it outside the machine;
- delete or rebuild local Core and derived data when working on shared machines;
- use provider filters and result limits to keep agent retrieval focused on
  relevant material.

## Network Behavior

Core indexing work uses the local filesystem and Tantivy; optional semantic
indexing uses local flat-vector operations. The tools that
originally produced provider transcripts may have used the network according to
their own configuration; ctx indexing those transcripts does not repeat that
behavior.

Official installer-managed binaries can contact the signed release metadata
endpoint for an explicit `ctx upgrade` command. With automatic upgrades
enabled, automatic indexing with the full daemon profile uses the persistent
daemon for cadenced checks. Manual indexing, source-refresh-only mode, ordinary
foreground commands, MCP, and finite Core workers do not schedule automatic
upgrade work. An unmanaged install or a process-level `CTX_UPGRADE_AUTO=off`
opt-out performs no automatic upgrade network or filesystem work. Upgrade
metadata checks do not send provider
transcript text, search queries, result snippets, source paths, repository
names, or command output.

ctx first-party telemetry is default-on and uses four content-free, versioned
families only: `operation_completed@1`, `provider_refresh_completed@1`,
`runtime_observation@1`, and `install_stage@1`. The root-local `install.json`
is product state independent of analytics.

`operation_completed@1` reports one terminal outcome for eligible foreground
operations. Current CLI coverage includes setup, explicit import, status,
index, sources, show, search, docs, integrations, upgrade, and doctor. Help,
version output, and command-line parse errors are not
observed. MCP and the daemon are first-class reporting surfaces rather than

`provider_refresh_completed@1` carries only closed provider-refresh summaries
that a producer already has safely available. Source sizes use coarse buckets
with large-store boundaries at 1, 2, 5, 10, 25, 50, and 100 GiB. When the CLI
can read process resource counters around the exact provider call, the same
event may include bucketed CPU duration; combined multi-source importers
contribute that observation once. A command with exactly one
provider/source-mode aggregate may also include the process-lifetime RSS
high-water mark observed at completion. That field is explicitly not a
provider-window peak and is omitted from multi-aggregate batches and
long-lived daemon surfaces.
`runtime_observation@1` is for low-frequency daemon or MCP lifecycle and
liveness observations, not per-loop or per-request tracing. These three batch
families are delivered to
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
search query text, result rows or snippets, source bodies, source or
repository paths, target values, repository or branch names, native session
IDs, command text or output, raw error strings, credentials, authorization
headers, access tokens, secrets, usernames, hostnames, raw IP addresses, exact
CPU or GPU names, serial numbers, hardware IDs, exact resource values, live

The data-root identifier lives in `install.json` and represents that local
index even when analytics are disabled. The client-profile identifier is a
random UUID used only for analytics events; it lives outside the ctx data root
in OS user state, such as `$XDG_STATE_HOME/ctx/device.json` or
`~/.local/state/ctx/device.json` on Linux. When a capability snapshot is
eligible, ctx creates a private versioned claim in that state directory and
promotes it to a version marker after delivery. A failed or uncertain delivery
does not change command output or exit status and leaves the claim in place to
avoid replay. For an official hosted installation, eligible product-analytics
events may also carry the installer attempt identifier for less than seven days
after the marker's installation timestamp so aggregate reporting can measure
initial activation. ctx omits that bridge at the seven-day boundary and
whenever the marker timestamp is absent, malformed, or in the future. Managed
upgrades preserve the original timestamp instead of reopening the window.

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
