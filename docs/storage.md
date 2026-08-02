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

ctx stores immutable Core/Tantivy search generations, optional semantic data,
content-free local usage aggregates, and optional encrypted Pro data locally.
Treat the ctx data root like private source history.

## Local Layout

Default root:

```text
~/.ctx/
  search/
    lexical/
    semantic/
  usage.sqlite
  config.toml
  pro/
    ctx-pro.db  # when Local Pro is installed
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
root. It is separate from provider history, Core/Tantivy and semantic search
data, and the encrypted Local Pro graph.
Local usage is product state, not telemetry: it has no network path or
analytics identity and remains available when analytics are disabled.

Version 1 stores daily UTC aggregate rows only. Its closed dimensions are UTC
day, usage-definition version, ctx binary/client version, surface (`cli` or
`mcp`), logical operation, technical outcome, result class, duration bucket,
blame target type, and Pro blame outcome. Aggregate counters cover calls,
content-free result/citation totals when a handler already has them, and
serialized MCP response bytes. MCP response bytes are factual JSON-RPC
transport bytes, including the delivered newline; they are not tokens, token
savings, cost savings, or model context.

The sidecar never stores a query, path, repository, selector, argument, prompt,
session/event/citation ID, exact timestamp, transcript-derived data, output
body, machine identity, or analytics identity. Result class is explicitly
`result_bearing`, `empty`, or `not_applicable`; unclassified operations are not
reported as empty. Pro blame outcomes are `produced`, `possible`, `none`, or
`error`, broken down across the exact typed `file`, `commit`, and `pull_request`
targets. `produced` requires an asserted `ProducedBy` fact; ambiguous,
contradicted, and superseded facts are handled conservatively.
Successful CLI blame and result-returning MCP tools must classify as nonempty
or empty; successful operations without a stable result collection must use
N/A. Failures always use N/A with zero result and citation totals. CLI status
report/control operations are excluded from both recording and the persisted
operation vocabulary.

Only one completed foreground CLI command or recognized MCP `tools/call` is
counted. MCP records after the complete response has serialized, written, and
flushed. Initialization, ping, tool listing, notifications, unknown tools,
automatic daemon cycles, liveness, and materialization are excluded. MCP Pro
blame is one local observation enriched from the Pro result, not separate MCP
and Pro observations.

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

- `search/lexical` contains immutable verified Core/Tantivy generations used
  for lexical search and indexed snippets.
- `search/semantic` contains generation-bound flat-F32 vectors, hashes, and
  offsets derived from eligible Core content; it does not persist separate
  plaintext transcript chunks.
- `pro/ctx-pro.db`, when Local Pro is installed, is an encrypted derived facts
  graph advanced from a pinned Core generation through the bounded Pro
  materialization protocol. It is independent of semantic readiness and is not
  Core search authority.
- `usage.sqlite` contains only the bounded content-free aggregates documented
  above. It is product state, not history or search authority.

Search, show, locate, and MCP retrieval read the active verified Core/Tantivy
generation. Show presents complete policy-selected normalized records stored in
Core, while locate returns bounded Core source identity metadata; neither
reopens provider history at query time. Provider changes become visible after
import or daemon refresh publishes a new Core generation.

Lexical publication keeps the active generation and one previous generation
for recovery and pinned readers; their manifests and integrity receipts use the
same two-generation bound. Append-only segments merge after eight comparable
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
rewrites all active live documents; semantic and Pro storage are excluded.
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

## Local Pro Storage

Local Pro uses one public, exact root-relative layout: the root identity is
`install.json`, the signed helper pair and transaction files are under
`pro/bin`, downloads are staged under `pro/downloads`, the encrypted derived
graph is `pro/ctx-pro.db`, and the persistent installer coordination lock is
`pro/.ctx-pro.lifecycle.lock`. The private, nonsecret
`pro/.ctx-pro.initialized` marker is durably published before setup or account
management may write credential-store records, so interrupted initialization
remains deletable even when no helper or graph file was created. Destructive
uninstall durably publishes the bounded, nonsecret, installation-bound
`pro/.ctx-pro.graph-key-cleanup.json` phase before deleting any recorded graph
key. It retains only the exact root identity and sorted public-key thumbprints,
survives interrupted graph-key, credential, or helper cleanup, and is removed
only after those deletion phases verify. Setup and keep-data uninstall fail
closed while that deletion phase remains. A separate nonsecret
`pro/.ctx-pro.data-preserved` lifecycle marker distinguishes deliberate
keep-data uninstall from first use, but it is created only when encrypted graph
data actually exists. Provider history and Core generations remain separate
and usable without Pro. The selected credential store holds an anonymous-trial
credential, an optional WorkOS session used for
explicit hosted account and referral commands, an installation-scoped signing
key, a signed entitlement, and, after accepted referral activation, an optional
opaque referral claim. The platform-native store is preferred: Secret Service
on Linux, Keychain Services on macOS, and Credential Manager on Windows. On a
pristine root, an exact native-unavailable result may instead select a sticky
owner-private local file store. That fallback persists the credential bytes
with owner-only permissions or a protected current-user-only Windows ACL; it
does not encrypt those bytes against the same OS user or root. Locked, denied,
corrupt, ambiguous, canceled, and other native-store failures do not downgrade
to files. Neither credential namespace accepts an environment-supplied key,
universal key, or binary-embedded pepper, and the Pro graph never falls back to
plaintext database mode. The raw referral code is never retained after
activation. The claim is the immutable result of the sole attribution input,
`ctx pro --referral <codename>`. The claim uses the same selected commercial
credential boundary and is removed by Pro commercial-credential cleanup.
A separate nonsecret marker under the selected data root records only that the
one-time human Pro blame referral line has been shown. It contains no codename,
claim, identity, payout data, or counts. JSON, JSONL, MCP, noninteractive,
empty, failed, install, setup, and Core paths neither read nor create it.

Key-store record identifiers are opaque hashes scoped to the root-local opaque
installation UUID and commercial environment, never to an absolute path.
Moving or renaming a complete ctx data root therefore preserves its credential,
graph-key, and entitlement-clock identity. Copying only part of a data root is
not an identity migration and fails closed when the identity is absent or
inconsistent. The persistent lifecycle lock serializes installer recovery,
staging, commit, cleanup, and final signed-pair verification across processes.

`ctx pro uninstall --keep-data` is entirely local, removes only the helper, and
records that local Pro data was deliberately preserved. It works even when
commercial configuration, network access, or the selected credential store is
unavailable. `ctx pro uninstall --delete-data` uses a public delete-only
credential-store adapter to remove and verify the complete local Pro inventory.
It does not need the helper and remains available after an earlier
`--keep-data` uninstall. Initialization or helper evidence causes deletion to
derive only this root identity's production/staging record IDs, collect the
thumbprints recorded in its installation-key and entitlement records, and
delete and verify those graph keys even when no graph file was completed. A
corrupt record makes that inventory unverifiable and fails the operation before
any graph key or graph file is deleted. Once the cleanup phase is published, a
retry uses its exact thumbprints instead of broad vault enumeration or
now-deleted credential records.
Interactive use asks whether to delete; noninteractive callers must explicitly
choose `--delete-data` or `--keep-data`. Neither form deletes provider history
or Core's derived search generations.
On a root that has never contained Pro data, either explicit choice is an
idempotent Pro-state no-op and reports `local_pro_data: "absent"` without
creating a Pro directory, initialization or preservation marker, vault access,
or restore action. The foreground `pro_uninstall` command remains eligible for
independent default-on Core local usage reporting, so it may create or increment
`usage.sqlite` unless local usage is disabled. `absent` classifies graph-file
state; verified deletion can still remove interrupted setup credentials or a
pre-database graph key before returning it.
A small installation-bound anti-rollback watermark may remain in the native key
store. It contains no graph key, transcript content, account token, or
entitlement body and does not make `ctx pro` report Pro as installed.
After successful deletion the initialization and cleanup-phase files are gone;
after a failed deletion they may remain as truthful retry metadata until the
same identity-aware `--delete-data` operation completes. The nonsecret
`pro/.ctx-pro.lifecycle.lock` coordination file may remain after success.

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

Search, show, and MCP presentation consume the verified Core/Tantivy generation,
while eligible Pro materialization consumes a bounded feed from the same pinned
Core generation. Output remains bounded, and content excluded by import policy
is not reconstructed.

## Provider-Owned Data

ctx does not own provider homes. Import reads from configured or discovered
locations and records enough information to search and cite imported material.
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
| `ctx setup` | provider transcript files and bounded path metadata for source discovery | data root, source catalog/epoch metadata, `search/lexical`, and optional daemon lock/status/job files when eligible human-readable daemon autostart runs; old Store artifacts are neither opened nor deleted |
| `ctx status` | data root metadata, source epoch, lexical/semantic generation metadata, daemon state, compact local usage health, and Pro authorization state when installed | may advance nonsecret anti-clock-rollback security metadata during Pro entitlement authorization; does not mutate provider history, Core generations, usage aggregates, or local Pro graph data |
| `ctx stats` | owner-private aggregate `usage.sqlite` when present | none; does not create pristine usage state or count itself |
| `ctx sources` | bounded provider path metadata, allowlisted persistent selector files, and local history-source plugin manifests | none |
| `ctx import` | provider transcript files and path metadata, the explicit custom history JSONL file passed with `--input-format ctx-history-jsonl-v1 --path`, or a durable provider-owned custom history JSONL file declared by an explicit history-source plugin manifest | immutable candidate Core/Tantivy generation and atomic publication, catalog/epoch metadata, and optional daemon files; Pro and semantic work is daemon-owned and does not delay foreground completion |
| `ctx show session` / `ctx show event` | complete policy-selected records in the active verified Core/Tantivy generation | selected `--out` path for `show session` when provided |
| `ctx search` | active verified Core/Tantivy generation and existing semantic generation; depending on refresh mode, bounded provider discovery/path metadata | candidate Core generation publication only when refresh runs; background mode may write daemon state, and semantic-enabled search may create query endpoint files |
| `ctx pro` / `ctx pro setup` | selected credential store, commercial account state, signed release metadata/artifact, pinned Core materialization feed, and an optional first-challenge codename only for `ctx pro --referral <codename>` | selected credential store, signed helper installation, encrypted derived graph, and an optional opaque referral claim after accepted activation; the raw codename is not retained, and the explicit `setup` form is a synonym without referral attribution |
| `ctx pro manage` | selected credential store and commercial account state | may refresh the WorkOS session in the selected credential store and open a hosted billing-portal URL |
| `ctx pro uninstall` | helper and local Pro paths | requires or prompts for a data choice; `--keep-data` removes only the helper and records preserved local Pro graph data when it exists, while `--delete-data` removes and verifies local Pro data; never-Pro roots leave Pro state unchanged, while independent default-on Core usage reporting may create or increment `usage.sqlite` |
| `ctx referral create` / `status` / `payout` | selected-store commercial session and explicit hosted referral state; status reads only the authenticated referrer's aggregate summary | may refresh the commercial session in the selected credential store; human mode may open WorkOS AuthKit, and payout may open a one-use Stripe-hosted onboarding URL; JSON mode never opens a browser |
| first successful nonempty interactive `ctx blame` | normal Pro blame inputs and whether the local shown-once marker already exists | may atomically create the private nonsecret shown-once marker after delivering the result; no referral network request or telemetry |
| `ctx docs` | embedded documentation in the binary | selected topic `--out` path for `ctx docs show --out` or selected `--out` directory for `ctx docs man --out` |
| `ctx upgrade` | signed release metadata and installed binary/sidecar metadata | installed binary for manual upgrade, install sidecar, and executable-adjacent `.ctx.upgrade-state.json`, `.ctx.install.lock`, and transaction journal |
| `ctx doctor` | source epoch, lexical/semantic generation metadata, and ctx-owned daemon lock/status/job metadata | none |
| `ctx daemon status` | lexical/semantic generation and ctx-owned daemon lock/status/job metadata | none |
| `ctx daemon enable` / `ctx daemon disable` | `config.toml` | `config.toml` |
| `ctx daemon run` | provider transcripts, active lexical and semantic generations, model-cache metadata, independent Pro source-generation state, and daemon state | candidate lexical generation publication, semantic and Pro catch-up, and daemon state |

Setup, import, and default search do not require source repository writes, model
APIs, API keys, or remote accounts. Without semantic opt-in they do not download
models or runtime assets; with semantic enabled, installer/runtime acquisition
and daemon maintenance may acquire the local ONNX Runtime asset and embedding
model when the installed build supports that path. Setup and native provider
setup may opportunistically start the default-on ctx-owned background daemon
maintenance profile when `[daemon].enabled` is true, regardless of output
format. Explicit custom JSONL and history-source imports may start the required
source-refresh endpoint even for machine-readable output. Use
`ctx setup --no-daemon` or `ctx import --no-daemon` for a one-run opt-out; an
explicit provider-source import with that opt-out requires an existing endpoint.
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
Explicit imports may best-effort mark recent semantic-eligible items dirty in
the semantic generation when it already exists; this does not create semantic
storage, initialize the model, or embed text.
Explicit semantic search also refuses to initialize or download the embedding
model when the required local cache is missing; hybrid falls back to lexical in
that case. Default `--refresh background` lets daemon maintenance own native
provider refresh and may autostart the configured daemon query service for
semantic/hybrid retrieval. History-source plugins are refreshed only by an
explicit selected-plugin import in 1.0.

When `ctx daemon run` or setup/import autostart runs the ctx-owned background
coordinator, it stores private lock/status files under `daemon/` in the ctx data
root. Setup/import autostart uses the normal background daemon profile and exits
after it becomes idle; explicit `ctx daemon run` runs the same coordinator in
the foreground. The coordinator always bounds native provider-history refresh
and local semantic indexing by its local runtime/model availability. Foreground
query activity preempts background work.
A looping daemon may keep the
local embedding model resident between passes and uses semantic projection state
to prioritize recent/stale events. Default background refresh may start the
configured daemon for local history freshness. With semantic enabled, the same
daemon-owned query service can embed the query; `ctx search --refresh off` does
not start it.

## Config Overrides

`ctx setup`, `ctx import`, and `ctx search` do not create `config.toml` for
implicit defaults. The config file is for user-managed overrides. Existing
config files are read and left in place.

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
history or the Pro graph. `ctx stats --detail` expands the read-only local
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

Daemon maintenance is enabled by default. Disable it durably with:

```toml
[daemon]
enabled = false
```

`daemon.enabled = true` allows setup and eligible native provider imports to
opportunistically start the ctx-owned background daemon maintenance profile.
Setup output format does not change this behavior.
Use `ctx setup --no-daemon` or `ctx import --no-daemon` for a one-run opt-out.
`ctx daemon enable` and `ctx daemon disable` write only the `[daemon] enabled`
override. An explicit disabled override continues to win after CLI upgrades and
over `CTX_DAEMON_ENABLED=true`.

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
and idle exit remain active. History refresh, semantic indexing and serving,
canonical and Pro maintenance, and automatic upgrades do not run.

Local semantic search requires daemon maintenance and remains disabled by
default. Its opt-in is:

```toml
[search]
semantic = true
```

If daemon maintenance was previously disabled, re-enable it before enabling
semantic search.

The enabled daemon is the sole automatic-upgrade authority and uses
`upgrade.auto = "apply"` by default for official installer-managed binaries
with a valid install sidecar. With the daemon disabled, no automatic upgrade
network or filesystem work occurs. `ctx upgrade disable` writes an explicit
`upgrade.auto = "off"` opt-out. Unmanaged installs do not self-upgrade.

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
ctx import --input-format ctx-history-jsonl-v1 --path ./history.jsonl
ctx import --history-source example-agent/default
```

Current adapters are safe to re-run. They rescan sources idempotently and keep
stable source identity and import-progress metadata. Imports always commit valid
records and report rejected records. Sources with no usable imported content
fail, as do unreadable or incompatible sources; ctx-owned generation failures
abort the command. Native provider progress is scoped by provider, source format,
and an opaque source identity derived from the configured source root.

Refresh builds a private immutable Core/Tantivy candidate containing complete
normalized stored records from current provider sources, verifies its manifest
and policy identity, then atomically publishes it under `search/lexical`.
Published generations are never opened writable. Semantic data and Pro
materialization are advanced only against a verified pinned Core generation;
each derived consumer maintains its own receipt and fails closed on generation
mismatch.

Custom history JSONL and history-source plugins follow the same import and Core
publication lifecycle. Failed plugin runs do not advance cursor state.
Explicit file paths and plugin manifests are not added to `config.toml` or
treated as fixed provider homes.

## v0.26 epoch transition

v0.26 starts a fresh Core/Tantivy search epoch from provider-owned source
history. It does not migrate, read, or delete the legacy canonical Store or use
it as fallback. An old `work.sqlite` family is inert and remains an explicit
owner-managed artifact.

Setup discovers current provider sources and builds a new Core/Tantivy
generation, then schedules optional semantic and independent Pro work.
If a source needed for rebuilding no longer exists, ctx reports that source as
unavailable; it does not recover content from prior-epoch rows.

Generation policy includes complete normalized stored-record encoding,
meaningful-body lexical selection, event projector/class revision, tokenizer
and lexical schema revision, and semantic settings. A mismatch makes derived
storage stale and requires a Core rebuild rather than an in-place migration.

Remove a source from future imports:

```bash
$EDITOR ~/.ctx/config.toml
```

The current CLI does not add provider source entries to `config.toml`; default
provider locations are discovered each time and explicit `--path` imports are
not remembered as future defaults. Custom history JSONL paths are also
one-shot explicit imports. To remove already imported data, rebuild the Core
generation with only the sources you still want.

## Reset And Inspect Local Search Storage

Reset and rebuild Core/Tantivy plus optional semantic data:

```bash
rm -rf ~/.ctx/search/lexical ~/.ctx/search/semantic
ctx setup
```

This removes the active Core/Tantivy generation and semantic sidecar, then
imports complete policy-selected normalized records and stable provider/source
identities from the available provider histories. It does not delete provider
transcripts, `usage.sqlite`, or Local Pro data.

Inspect storage size:

```bash
du -sh ~/.ctx
du -sh ~/.ctx/search/lexical ~/.ctx/search/semantic
ctx status --format json
```

Delete all ctx data:

```bash
ctx pro uninstall --delete-data
rm -rf ~/.ctx
```

Run the identity-aware Pro deletion command first, while the root-local
`install.json` identity still exists. Only after it succeeds should you remove
the Core root. For a custom root, pass the same root to both operations, for
example `ctx --data-root /path/to/ctx pro uninstall --delete-data` before
removing `/path/to/ctx`. Deleting the directory first can orphan Pro credentials
or graph keys in the selected credential store because their opaque record IDs
depend on that identity.

The final directory removal deletes ctx's Core/Tantivy and semantic data,
`usage.sqlite`, config, logs, lifecycle lock, and remaining root-local metadata.
It does not remove provider-owned history such as
`~/.codex/sessions`. The small installation-bound anti-rollback watermark
described above may remain in the selected credential store after verified Pro
deletion; it is security metadata outside the user-deletable Pro inventory.

## Privacy Truth

Provider transcripts, indexed terms, Core snippets, commands, and file
paths may contain credentials, customer data, private repository names, or
proprietary design notes.

Recommended handling:

- keep `~/.ctx` out of source repositories;
- do not share provider transcripts, ctx search generations, Local Pro data, or
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

Local Pro trial setup and renewal contact the ctx commercial API and signed
artifact service without an account. WorkOS and Stripe are contacted only for
paid conversion, account management, and explicit referral commands. Trial
setup sends challenge-bound,
application-specific device-anchor digests produced by the signed helper; raw
platform identifiers never leave it, and the service stores separately keyed
anti-repeat tokens rather than the submitted evidence. An optional referral
codename is sent only with the first anonymous-trial challenge. After accepted
activation, attribution is immutable; only the returned opaque claim may remain
in the selected credential store and survive credential refresh. Paid Checkout
sends only that opaque claim, never the raw codename. No website or cookie is an
attribution input. `ctx pro manage` creates a hosted Stripe portal session
after sign-in. `ctx referral create`, `status`, and `payout` are explicit
hosted-service operations; human mode may start WorkOS AuthKit, and eligible
payout can open Stripe-hosted onboarding. Referral JSON uses cached
authentication only and never starts AuthKit or opens a browser. These
requests do not include transcript text, source content,
repository paths, facts, graph rows, queries, or query results. Valid offline
grants keep local graph operations usable when renewal is temporarily
unavailable.

The referral feature writes no state to Core search generations,
`usage.sqlite`, the encrypted Pro graph, provider transcripts, Git data, local
analytics, routine `ctx status`, MCP, or ordinary Core flows. It emits no
referral telemetry. The hosted service is authoritative for distinct qualifying
$20 monthly invoice reconciliation, 14-day holds, the invoice 2 gate for
invoices 1 and 2, invoices 3 through 12, refunds, disputes, manual payability,
paid-reversal debt and negative adjustments, and the $120-per-referral cap. The
client stores no invoice-level or per-referral ledger and receives only the
authenticated referrer's private aggregate status. That summary distinguishes
earned, pending, manual-review, payable, sent-but-unsettled processing, settled
historical paid, and debt amounts. Paid cash is not decremented after a
reversal; debt records the negative adjustment, and the service never requests
an external clawback. Pro commercial-credential cleanup deletes the optional
opaque referral claim along with the other root-scoped commercial credentials.
The separate shown-once marker is content-free local output state, not
attribution, identity, or payout state.

An expired trial or subscription locks graph operations without deleting
provider history, Core search generations, encrypted graph data, or key
material. Resubscription plus
`ctx pro` refreshes the entitlement and restores the preserved graph.

Official installer-managed binaries can contact the signed release metadata
endpoint for an explicit `ctx upgrade` command. When the daemon and automatic
upgrades are enabled, the daemon alone performs cadenced automatic checks and
application. Foreground commands, including machine-readable commands and MCP,
never schedule this work. A disabled daemon, unmanaged install, or
process-level `CTX_UPGRADE_AUTO=off` opt-out performs no automatic upgrade
network or filesystem work. Upgrade metadata checks do not send provider
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
being labeled as CLI traffic. Pro reporting is limited to the public host
surface; private Pro graph, entitlement, and query internals are not telemetry
inputs.

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
utilization samples, or benchmark outputs. Referral codenames, opaque claims,
identity, payout URLs, email, and
identity-linked referral counts are also excluded.

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
