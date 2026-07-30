# CLI Reference

ctx is a local CLI for indexing and searching agent session history. In v0.26,
provider sources are the sole content authority. Lexical search lives under
`search/lexical`, semantic search under `search/semantic`, and read-only SQL
uses the disposable `relational.sqlite` metadata projection. Rendered search,
show, and MCP content is hydrated from exact provider records.

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
ctx daemon status
ctx daemon status --format json
ctx daemon run
ctx daemon run --once --format json
ctx daemon disable
ctx daemon enable
```

- `setup` creates the data root, discovers known provider history locations,
  inventories current sources, builds and atomically publishes the
  source-backed lexical generation, catches up the relational projection, and
  prints next steps. It never opens or migrates pre-v0.26 history. After a
  source-backed Tantivy generation is verified and active, setup deletes old
  history artifacts; a failed build leaves cleanup for the next successful
  fix-forward setup.
  Setup does not write `config.toml` for implicit defaults or execute
  history-source plugin commands. When `[daemon].enabled` is true, setup may
  opportunistically start the ctx-owned background daemon after foreground
  work completes. Use `setup --no-daemon` for a one-run opt-out.
- `setup --catalog-only` stops after source discovery and inventory. The flag
  name is kept for compatibility; it is useful for fast troubleshooting, but it
  does not make history searchable and does not autostart daemon maintenance.
- `setup --quiet` performs setup without printing success status lines, import
  summaries, data-root details, or get-started tips. It still exits nonzero and
  prints errors on failure.
- `status` reports the ctx root, source epoch, lexical generation and policy,
  generation-bound catalog/resolver state, semantic generation and coverage,
  relational projection state, daemon state, initialization state, compact
  local usage health, local-only marker, and read-only marker. It does not
  include usage counts or estimates, initialize or repair derived Core storage,
  or open old history.
- `stats` is the read-only, local, offline report for History retrieval, Code
  provenance, Measured delivery, and Estimated savings. Measured facts and
  model-based estimates are separate in JSON; the estimate model and
  coefficients are versioned. Delivery keeps exact CLI output, MCP transport,
  and one-copy semantic/context byte channels separate; transport bytes never
  drive context-token or savings estimates. Byte-derived token values include
  coverage and remain unavailable for unmeasured legacy rows rather than
  becoming false zeros. `stats --detail` adds CLI/MCP operation and latency
  breakdowns. Reporting is uncounted and does not create `usage.sqlite` on a
  pristine root.
- `status --usage disable` and `enable` write the canonical `[local_usage]
  enabled` override; `status --usage reset` atomically clears the usage
  aggregates. These controls are not counted and remain action-focused so they
  do not depend on a successful Core status read. Local usage is default-on
  product state in the separate owner-private `usage.sqlite` sidecar, has no
  network path or network-delivery identity, and remains independent of remote
  reporting controls.
- `status --quiet` performs the same local checks but prints nothing on
  success. Use `status --format json` when scripts need the actual state.
- `doctor` validates source-epoch storage and reports lexical, semantic,
  relational, resolver, and daemon lock/status problems when present.
- `daemon status` reports the same ctx-owned daemon coordinator state without
  mutating storage. It separates current requested config from the last config
  applied by the running daemon, and reports observed semantic query-runtime
  ownership. `config_reload.status` exposes pending, applied, parse/read failure,
  or semantic activation failure rather than treating config-file mutation as a
  successful live reload. This diagnostic remains available when malformed
  config caused the retained reload failure; ordinary commands still reject the
  malformed file.
- `daemon run` runs bounded local maintenance in the foreground. That means
  bounded native provider-history refresh followed by semantic catch-up when
  semantic is enabled. The daemon may acquire the local embedding model for
  semantic indexing. A looping daemon keeps the embedding model resident after
  cold start, reloads daemon/semantic configuration between cycles, and performs
  recent-work freshness checks before settling into idle loops. Enabling
  `[search] semantic = true` and rerunning setup activates the existing daemon;
  no unrelated restart is required.
- `daemon disable` and `daemon enable` update `[daemon].enabled` in
  `config.toml`. Daemon maintenance is enabled by default; an explicit disable
  persists across upgrades. `daemon run --force` overrides a disabled config
  for explicit manual troubleshooting.

Setup and health checks do not change shell startup files, install repository
integrations, write into source repositories, call model APIs, or require API
keys. Without semantic opt-in they do not download embedding models; with
semantic enabled, daemon maintenance may acquire the local embedding model.
Daemon maintenance is bounded and local. Core storage checks use the configured
data root, and JSON
stdout remains structured.
Machine-readable foreground commands do not start or nudge daemon maintenance.
For human-readable commands, use `--no-daemon` or search `--refresh off` for an
invocation-level opt-out. The enabled daemon, not command dispatch, owns signed
automatic upgrade checks.

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
ctx integrations install mcp --all-agents --format json
ctx integrations install mcp --agent cursor --force
ctx integrations status mcp
ctx integrations status mcp --agent codex --format json
ctx integrations install slash-commands
ctx integrations install slash-commands --agent opencode
ctx integrations install slash-commands --agent mimocode
ctx integrations install slash-commands --agent gemini-cli --project
ctx integrations install slash-commands --agent qwen-code
ctx integrations install slash-commands --agent windsurf
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
ctx sources --format json
```

`sources` lists bounded provider history locations selected for this machine.
Provider precedence is winner-only: an environment or persistent-config
replacement suppresses its lower-priority default. Current coexisting installed
surfaces or persisted profiles may produce separate rows. One-shot, old, moved,
or unreconstructible roots require an exact `--path` and are not remembered.
Current rows include:

- Codex session trees at `~/.codex/sessions`;
- Codex prompt history at `~/.codex/history.jsonl`;
- Pi session JSONL files under `~/.pi/agent/sessions`;
- automatic or unsupported-detection rows for supported providers whose matrix
  entry has a bounded current location; providers with an empty
  `history_locations` list remain available through exact compatible
  `--path` imports;
- AstrBot `data_v4.db` history when those files exist;
- explicit-import rows for NanoClaw project roots when those paths are discoverable;
- local history-source plugin manifests under `$CTX_DATA_ROOT/plugins` or
  `CTX_HISTORY_PLUGIN_PATH`.

Native JSON rows include `provider`, `path`, `exists`, `source_format`,
`status`, `import_support`, `native_import`, `importable`, and any
`unsupported_reason`. Plugin JSON rows use
`kind: "history_source_plugin"` and include `plugin`, `plugin_source`,
`history_source`, `provider_key`, `source_id`, `manifest_path`, `enabled`,
and source-authority fields. Durable regular-file rows report
`status: "available"`, `importable: true`,
`import_mode: "explicit_source_backed"`, and
`provider_source_authority: true`. Command-only compatibility rows report
`status: "unsupported"`, `importable: false`, and no provider-source
authority. Invalid installed plugin manifests appear as non-importable plugin
rows with `status: "invalid"` and an `error`. `sources` reads path metadata and
plugin manifests, writes nothing to provider files or source repositories, and
does not execute plugin commands.

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
ctx import --provider factory-ai-droid --path /path/to/factory/sessions
ctx import --provider qwen-code
ctx import --provider kimi-code-cli
ctx import --provider windsurf
ctx import --provider lingma
ctx import --provider codebuddy
ctx import --provider trae
ctx import --provider codex --path ~/.codex/sessions
ctx import --provider pi --path ~/.pi/agent/sessions
ctx import --input-format ctx-history-jsonl-v1 --path ./history.jsonl
ctx import --history-source example-agent/default
ctx import --history-source-manifest ./ctx-history-plugin.json
ctx import --resume
ctx import --no-daemon
ctx import --format json
ctx import --progress json --format json
```

`import` explicitly rebuilds source-backed history from provider sources. The
normal first-run path is `ctx setup`, which already imports discovered native
provider sources. Use `import` to repair, re-run, resume, or target a specific
provider/path. It creates the data root if needed, reads provider transcript
files, builds a private immutable Tantivy candidate containing indexed-only
meaningful text plus locator/metadata fields, verifies it, and atomically
publishes it under `search/lexical`. It then advances disposable consumers such
as `relational.sqlite`; semantic catch-up is daemon-owned. It does not write
`config.toml` for implicit defaults.

History-source plugin import is explicit and single-source in 1.0. A selected
manifest declares a durable provider-owned `ctx-history-jsonl-v1` path; the
importer validates its schema and source identity, registers that same path as
the custom route, and waits for daemon-owned source-backed publication.
Command-only manifests are reported as unsupported and are never copied into
ctx storage. Plugins are not imported by `import --all` or setup.

Imports always commit valid records and report rejected records. An unreadable
or structurally incompatible input fails that source, while ctx-owned storage
or index failures abort the command. A source with only rejected records is a
failure; a source with valid content and rejections completes with an explicit
`completed_with_rejections` outcome.

Import results report `change: changed|no_op` independently from import and
skip counters. `change: changed` remains truthful even when a source projects
to the same stable event identities.

When `[daemon].enabled` is true, `import` may opportunistically start a short
one-pass ctx-owned maintenance profile after the foreground import finishes.
The daemon work includes bounded native provider-history refresh plus semantic
indexing when semantic is enabled. It may acquire the local embedding model for
semantic indexing. Explicit custom JSONL and history-source imports use the
daemon-owned source-refresh endpoint for their foreground publication and may
start it unless `import --no-daemon` is set. With `--no-daemon`, an
already-running source-refresh endpoint is required.

## Local Pro

Local Pro is a separately installed native helper and encrypted derived graph:

```bash
ctx pro [--format json]
ctx pro --referral <codename> [--format json]
ctx pro setup [--format json]
ctx pro manage [--no-open] [--format json]
ctx pro uninstall [--delete-data|--keep-data] [--format json]
```

Bare `ctx pro` starts or resumes the anonymous trial when needed, activates access, transactionally
installs or repairs a signed target-specific helper, and catches the graph up.
It is idempotent across first setup, resume, repair, and later catch-up.
`ctx pro setup` is a supported explicit synonym with the same setup JSON and
operation. Use `ctx status` (or the existing MCP `pro_status` tool) for
Pro state without mutating provider history, Core search generations, or graph
data. Entitlement
authorization may advance nonsecret anti-clock-rollback metadata. `manage`
opens hosted account and billing management.
Interactive `uninstall` asks exactly `Delete
all local Pro data? It can be rebuilt if you set up Pro again. [Y/n]`; Enter
chooses deletion and `n` preserves local Pro data.
Noninteractive and JSON callers must choose `--delete-data` or `--keep-data`.
Provider history and Core search generations are always preserved. If the
selected root has never held
Pro data, either explicit choice succeeds as an idempotent Pro-state no-op and
does not create a Pro directory or preservation marker. The foreground
`pro_uninstall` command remains eligible for independent default-on Core local
usage reporting, so it may create or increment `usage.sqlite` unless local usage
is disabled.

Trial setup does not open a browser or request an account or payment method.
It downloads a release-signed helper with a short-lived bootstrap credential,
uses that helper to produce bounded challenge-bound device evidence, obtains an
installation-bound signed entitlement, and installs the same verified helper.
The service uses the evidence only to prevent repeated trials. Raw platform
identifiers never leave the helper; the signal is not hardware attestation.
The ordinary anonymous trial remains 14 days. A first setup using exactly
`ctx pro --referral <codename>` receives a 30-day trial when the service
accepts the bounded ASCII codename. This is the sole attribution input: the
codename is sent only with the first trial challenge, the resulting attribution
is immutable, and only the service-issued opaque referral claim may be stored
in the selected credential store after activation. An existing nonreferred
trial cannot attach a code later. `ctx pro setup`, `manage`, `uninstall`, Core
commands, websites, and cookies do not accept or change referral attribution.

Paid conversion uses browser-based WorkOS sign-in and Stripe Checkout. Pro is
$20 USD per month; conversion does not add a second trial.
`manage --no-open` prints the hosted billing-portal URL instead of opening it.
With `--format json`, `manage` also reports
`access_state` plus any applicable
`refresh_after_unix`, `access_deadline_unix`, and `grace_deadline_unix` values.
The access state is one of `trial`, `active`, `canceling_paid`, `offline_grace`,
or `locked`; it is separate from helper and graph readiness.
A locked commercial state may retain an applicable deadline for recovery
diagnostics, but that deadline never grants access.

On explicit `ctx status`, MCP `pro_status`, and `ctx pro manage` surfaces, a
trial state may include a `$20/month` continuation action and a locked state may
include an unpriced `pro_restore_access` action using `ctx pro manage`, with the
local graph explicitly reported as preserved. Paid
`active`, `canceling_paid`, and `offline_grace` states do not show a purchase
action. Existing `next_action` remains separate, no browser opens from status,
and blame citations never include conversion copy.

The anonymous trial credential, optional WorkOS session material, the
installation signing key, and the signed entitlement are kept only in the
selected credential store. The platform-native store is preferred; on a
pristine root, an exact native-unavailable result may instead select a sticky
owner-private local file store. Those local bytes are protected from other OS
users but are not encrypted against the same OS user or root. Locked, denied,
corrupt, ambiguous, canceled, and other native-store failures do not downgrade
to files. Neither credential namespace accepts an environment-supplied key,
universal key, or binary-embedded pepper, and the Pro graph has no plaintext
database mode. Entitlements renew quietly before their seven-day grant expires;
a failed refresh does not block a still-valid offline grant and is retried with
a bounded backoff.

`--keep-data` is a local helper removal that preserves local Pro data so setup
can restore access later. It does not require commercial configuration,
network access, or credential-store access. `--delete-data` removes and verifies
local Pro data through the public delete-only adapter. It works after an earlier
plain uninstall and does not evaluate subscription or entitlement expiry. It
reports `local_pro_data: "deleted"` only after the authoritative local inventory
is absent and fails closed before removing the helper if that inventory cannot
be verified. A successful operation prints or returns `ctx pro` as the next
action only when graph data was actually preserved or deleted. A never-Pro or
already-empty root instead reports
`local_pro_data: "absent"` and has no next action. Setup records root-scoped
initialization before its first credential-store write, so `--delete-data` also
cleans and verifies credentials and recorded graph keys after an interrupted
artifact fetch or helper start, even if no graph database exists. The `absent`
result describes that filesystem graph state, not whether credential-store
cleanup work was required. Before deleting a graph key, uninstall durably
records the exact installation identity and bounded thumbprints in a nonsecret
local cleanup phase. Corrupt inventory fails before deletion; late failures
retain that phase so the next `--delete-data` can verify already-absent keys and
credentials without enumerating another installation. Setup and `--keep-data`
refuse to proceed until an interrupted deletion is completed.

Subscription lock does not delete `ctx.db`, the encrypted graph, or its key.
After renewal or resubscription, `ctx pro` refreshes authorization and restores
access to the preserved graph. Only explicit
`ctx pro uninstall --delete-data` removes Pro data and key material.
Run it before manually deleting the ctx data root so the root-local installation
identity remains available for selected credential-store cleanup. A small
installation-bound anti-rollback watermark may remain afterward; it contains no
graph key, transcript content, account token, or entitlement body. A failed
deletion may also leave root-local initialization and cleanup-phase metadata;
successful `--delete-data` removes both. The nonsecret lifecycle-lock file may
remain as local coordination metadata, in addition to the disclosed
selected-store watermark.

Materialization is internal and idempotent. Setup, daemon freshness, and blame
invoke it as needed. Repository and worktree roots come from canonical
activity; there is no `setup --repo` option.

Each root-local installation identity and production/staging environment is an
independent Local Pro credential-store namespace. Moving or renaming a complete
ctx data root preserves that identity and its selected backend; copying
`ctx-pro.db` alone does not.

The public Pro query surface is:

```bash
ctx blame file <path> [--lines <start[:end]>] [--repository <logical-repository>] [--limit N] [--cursor <cursor>] [--format json]
ctx blame commit <sha> [--repository <logical-repository>] [--limit N] [--cursor <cursor>] [--format json]
ctx blame pr <positive-number-or-canonical-url> [--repository <logical-repository>] [--limit N] [--cursor <cursor>] [--format json]
```

There are no Pro `show`, `locate`, `timeline`, `facts`, or `related`
compatibility aliases. OSS `ctx show session|event` and
`ctx locate session|event` remain unchanged. The CLI blame limit defaults to 20
and is bounded from 1 through 100.

Query `--repository` is an optional logical repository identity, such as
`forge:github.com/ctxrs/ctx`, recorded in the graph. It is never a local
checkout path or a raw credential-bearing remote URL. It is required for a
numeric PR selector and optional with a canonical GitHub, GitLab (including
canonical self-hosted `/-/merge_requests/<positive>`), or Codeberg URL.

`--lines` is a positive 1-based committed line or inclusive `start:end` range
and exists only for file blame. File blame binds the result to a Git HEAD
snapshot and may report that the worktree differs; returned ranges still refer
to committed HEAD lines. Only file blame negotiates the helper's Git-read
capability.

Every successful response is the typed protocol `BlameResult`: a resolved
file, commit, or PR target; typed matches; one complete deduplicated,
contiguously numbered evidence table; and an optional continuation. A page
never clips evidence for a returned match. Continuation cursors are opaque and
bound to the request and graph state.

Commit output groups assertions as `Produced by`, `Possible producers`, and
`Also recorded`; inspection or reference evidence never appears as production.
PR activity is separate from code production. A PR-to-commit relationship is
shown only when structured forge evidence binds the canonical PR identity and
exact Git object ID; otherwise output says `associated commits not proven`.

Stable Pro failure codes include `pro_not_installed`, `commercial_unavailable`,
`entitlement_expired`, `helper_upgrade_required`, `key_store_unavailable`,
`key_store_locked`, `not_materialized`, `protocol_mismatch`, `repository_unavailable`,
`line_out_of_range`, `stale_snapshot`, `stale_fact`, `ambiguous`,
`corrupt_graph`, `invalid_request`, `invalid_response`,
`helper_crashed`, and `helper_timeout`.

`key_store_locked` and `key_store_unavailable` are the only stable public names
for selected credential-store failures. Pre-release `credential_vault_*`
spellings were never shipped and have no compatibility alias.

The helper materializes deterministic repository/worktree/branch/remote,
commit, file, command/check, forge-reference/action, and agent relationship
facts. Public blame exposes only file, commit, and PR targets. It does not claim
universal shell-wrapper, forge, or provider accuracy; unknown or contradictory
evidence remains referenced, attempted, or ambiguous.
Deployments and ask/brief generation are outside this local work-graph surface.
Credential material is held by the selected credential store.

Materialization uses a frozen canonical `(mutation_epoch,
event_seq_high_water)` frontier. A pure tail append raises only the high-water
mark, so the next completed run reads and commits only the new observations:
work is O(delta), not a rescan of prior rows. A non-tail insertion, a relevant
event/session/run/source update or deletion, or a parser/policy projection
revision advances the mutation epoch. Repository authorization or observed
HEAD/ref semantics also participate in derived-graph compatibility.

The helper reports explicit graph states. `NotMaterialized` starts the first
build; `Ready` is fully current; `Partial` continues an interrupted finite walk;
`NeedsResume` consumes a newer canonical tail; and `NeedsRebuild` means the
epoch, schema/detector contract, repository set, or repository semantics cannot
safely continue. Rebuild uses a state-token-checked reset before replay. Legacy
derived graphs that predate the required immutable digests and epochs are reset
instead of being treated as a valid resume point. Canonical `ctx` history is
never deleted by this process.

## Referrals

The shipped referral surface is default-disabled on both staging and stable
until commercial credential and isolated-payout qualification is complete.
While disabled, every explicit referral command and `ctx pro --referral`
returns `referral_unavailable` before identity, authentication, or browser
side effects, and the one-time `ctx blame` referral CTA is not written or
marked as shown.

The referrer management surface is entirely in the CLI:

```bash
ctx referral create <codename> [--format json]
ctx referral status [--format json]
ctx referral payout [--no-open] [--country <CC>] [--entity-type <individual|company>] [--format json]
```

`create` lets any WorkOS-verified person claim one stable codename, or returns
that same claim when the request is replayed. A Pro trial or subscription is
not required. A codename is 3–32 bytes of lowercase ASCII letters, digits, or
hyphens and must start and end with a letter or digit. The client checks that
syntax before sending it, while the hosted service remains authoritative.
Human referral-command output leads with
`Refer a developer. Earn $10/month toward your agent bill.` and may follow with
`Up to $120 per friend.` Create and status also print the exact share command
`ctx pro --referral <codename>`.

The commission is $10 cash for each distinct qualifying $20 monthly Pro
invoice, covering subscription invoices 1 through 12 for each direct referral.
The maximum is $120 per direct referral. Invoice 1 and invoice 2 commissions
accrue as pending; neither can become payable until invoice 2 has settled, the
required 14-day hold has elapsed, authoritative reconciliation has completed,
and the earnings pass manual review. Each invoice 3 through 12 commission has
its own 14-day hold and authoritative reconciliation before manual review and
payability.

A refund or dispute voids an unpaid commission. If the corresponding
commission was already paid, its reversal becomes referral debt, a negative
adjustment against future earnings, and enters manual review; ctx does not
initiate an external clawback. Earnings are cash commissions, not Pro credits.
There is no creator-affiliate program, website or cookie attribution, annual
plan qualification, or compatibility attribution path.

`status` is the complete referrer summary. It reports the codename, exact share
command, attributed and subscribed totals, earned, pending, manual-review,
payable, processing, paid, and debt amounts, and payout state. `processing` is
cash sent for payout but not yet settled. `paid` remains the historical cash
actually settled and is never reduced by a later reversal; such a reversal
increases debt instead. The aggregate accounting identity is
`earned + debt = pending + manual review + payable + processing + paid`.
Status is private to the authenticated referrer and aggregate only: it exposes
no referred identity, invoice, or per-referral ledger. It does not add referral
copy or status to ordinary `ctx status`, MCP, setup, search, or other Core
flows.

`payout` requests a one-use Stripe-hosted payout-onboarding URL when the account
is eligible. Human mode opens that URL by default; `--no-open` prints it
instead. If onboarding requests identity type, `--country <CC>` accepts a
two-letter uppercase ISO country code and `--entity-type` accepts `individual`
or `company`; ctx never collects bank or card data. These commands are explicit
hosted-service operations. Human mode may start WorkOS AuthKit when no usable
session is cached, and payout may open Stripe's hosted onboarding.

Every referral command using `--format json` is noninteractive and browser-free. It
uses only a cached WorkOS session and returns the stable authentication-required
failure when none is available; it never starts AuthKit or invokes a browser
opener. JSON contains only the requested deterministic command data, with no
unsolicited referral slogan or promotional message. `payout --format json` returns
the hosted URL without opening it.

The sole automatic referral mention is human-only and shown once. After the
first successful, nonempty, interactive `ctx blame` result, ctx may write
`Refer a developer. Earn $10/month toward your agent bill.` followed by
`Up to $120 per friend.` and `ctx referral create <codename>` to stderr, then
record a nonsecret shown-once marker under the data root. It is suppressed for
JSON and JSONL, MCP, noninteractive output, empty or failed results, install and
setup, Core commands, and later blame results. Showing the copy makes no network
request and is not reported remotely.

## Show And Locate

```bash
ctx show session <ctx-session-id>
ctx show session <ctx-session-id> --mode full --format text
ctx show session <ctx-session-id> --content complete --format text
ctx show session <ctx-session-id> --mode log --format jsonl
ctx show session <ctx-session-id> --format markdown --out transcript.md
ctx show session <ctx-session-id> --mode full --format markdown --out transcript.md
ctx show event <ctx-event-id> --window 3 --format text
ctx show event <ctx-event-id> --before 5 --after 10 --format json
ctx show event <ctx-event-id> --content complete --format json
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

Both commands hydrate exact provider content through typed locators bound to
the active lexical generation. `--content indexed` remains the default
compatibility selector, but its name no longer means a body is stored in the
index. `--content complete` requests the strict complete-output policy; both
policies obtain displayed text from provider sources.

Hydration is source-grouped and preserves event order. An unsupported, missing,
changed, stale, unverifiable, or oversized source record fails the selected
event/session without a partial transcript, stale fallback, or empty
placeholder. Show never expands payload classes excluded by provider policy,
such as binary data or provider-private blobs.

`locate session` and `locate event` print provenance metadata: ctx IDs,
provider, provider-owned session IDs, source path and cursor, source
availability, import fidelity, and resume/cursor metadata when available.
Cursor fields are emitted only for real provider provenance cursors; ctx does
not turn a typed source-backed locator into a cursor. Event locations also
report safe source-record coordinates, whether a complete-content locator is
present, and conservative current content availability. A retained locator can
remain present after its known backing source is deleted, but availability is
then false. Locator key bytes, revision evidence, and expected digests are not
displayed.

Provider-owned IDs are metadata, not positional IDs. Positional session and
event arguments are ctx-owned IDs. To look up a provider-owned session, use an
explicit provider lookup such as `--provider codex --provider-session
<provider-session-id>` on commands that support provider lookup.

JSON output may expose local paths, hydrated event payloads, and compatibility
field names, so treat it as private local data.

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
ctx search "pricing decisions from the launch review" --backend semantic
ctx search "release notes" --history-source example-agent/default
ctx search "release notes" --provider-key example-agent --source-id default
```

`search` defaults to `--refresh background`, which serves the published
Tantivy generation while the ctx daemon owns lexical publication plus
relational and semantic catch-up. If no local generation exists yet, search
performs a bounded foreground lexical bootstrap. If daemon maintenance is
disabled, background mode uses the same bounded foreground source-refresh path
for discovered native providers. History-source plugins are searched from the
published generation after explicit import; search refresh does not execute
their commands in 1.0.
Semantic retrieval reads an existing compatible generation under
`search/semantic`; search does not initialize semantic storage, download
embedding models, or run semantic indexing. Use `--refresh off` to query the
published generations without refreshing or scheduling work, or
`--refresh wait` to run foreground source refresh and fail when it cannot
complete. Exact result rendering still hydrates provider records under every
refresh mode. Foreground refresh skips isolated malformed records with a
warning and publishes valid records; source-level and system-level failures
remain fatal. Explicit-only native sources such as
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
JSON echoes the normalized positional/`--term` alternatives in `query`, trims
surrounding whitespace, and joins nonempty alternatives with ` OR `. Scoped
follow-up commands preserve the positional and repeatable-term argument shape
with safe shell quoting.
Custom history imports can be filtered by canonical
`--history-source provider_key/source_id`, or by exact `--provider-key`,
`--source-id`, and `--source-format` values. The plugin/source alias is for
explicit plugin import selection. These search filters imply
`--provider custom` and cannot be combined with another provider.
Default search excludes subagent sessions so primary human-agent intent and
decisions stay prominent. Use `--include-subagents` when implementation details,
code review notes, test output, or failure analysis from subagent sessions
should be searched too.

When ctx is run from Codex and `CODEX_THREAD_ID` is available, search excludes
the active Codex session tree by default so the current task and its subagents
do not dominate historical retrieval. Use `--include-current-session` to opt
back in. `--refresh off` is read-only for ctx-derived storage, but it still
reads provider records to hydrate exact snippets. Explicit semantic or hybrid
requests may read a compatible semantic generation and ask the retained daemon
query service to embed the query from an already-cached model.

Results are local hits over indexed history. Event hits include `ctx_event_id`;
hits with known session context include `ctx_session_id`; provider metadata
including `provider_session_id` is included when known. Results also include
title, snippet, rank, result scope, match reasons, source-path/cursor data,
citations, `suggested_next_commands`, a JSON `freshness` object, a JSON
`retrieval` object with backend, semantic coverage, worker status, and semantic
timing/scan diagnostics when vector retrieval runs, a JSON `result_window`
object with `limit`, `returned`, and shaped-sentinel `more_available`, and
separate backend candidate-pool `truncation` fields. Search does not expose a
continuation cursor or run a second count scan. Any per-result cursor is source
provenance only and is omitted when unavailable. Default text output is compact
and optimized for agent reading; it ends with exactly
`More results available.` only when one additional shaped result exists. Use
`--verbose` for expanded text diagnostics.

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
- `--backend hybrid|semantic|lexical`, where `lexical` queries
  `search/lexical`, `semantic` queries `search/semantic`, and `hybrid` blends
  both only when semantic coverage is complete and bound to the active lexical
  generation. Hybrid falls back to lexical with a structured reason when
  semantic prerequisites are missing. Explicit semantic reports a local error
  rather than downloading a model or using an incompatible generation;
- `--semantic-weight <0.0-1.0>`, for hybrid ranking;
- `--include-subagents`;
- `--limit <n>`, capped at `200`;
- `--refresh background|off|wait`;
- `--include-current-session`.

CLI provider filters use kebab-case names. JSON output and stable SQL views use
provider IDs in ctx output; multiword IDs may be snake_case, such as
`copilot_cli`, `factory_ai_droid`, `qwen_code`, `kimi_code_cli`, `kiro_cli`, `mistral_vibe`, and `roo_code`; compact IDs such as `forgecode`, `deepagents`, `mux`, `rovodev`, `openclaw`, `nanoclaw`, `astrbot`, `shelley`, `continue`, and `openhands` stay compact.

Lexical matching indexes the complete policy-selected meaningful body but
stores no body or display preview. Before serialization, search hydrates every
returned hit from its exact typed locator through the resolver retained for
the active generation. Hydration is bounded and grouped by source. A stale,
changed, missing, or unsupported record produces a typed failure/refresh path;
ctx never emits an empty placeholder or falls back to prior-epoch content.

Default daemon maintenance owns provider/plugin refresh, immutable candidate
construction, atomic lexical publication, generation-bound resolver/catalog
lifetime, relational projection catch-up, and semantic catch-up. Use
`ctx daemon run` for explicit foreground maintenance. JSON status exposes
`lexical`, `catalog`, `resolver`, `semantic`, `relational`, and `daemon`
objects. `ctx doctor` is the diagnostic surface for those components.

## SQL

```bash
ctx sql "SELECT COUNT(*) AS sessions FROM ctx_sessions"
ctx sql "SELECT provider, COUNT(*) AS sessions FROM ctx_sessions GROUP BY provider"
ctx sql --file query.sql --format json
cat query.sql | ctx sql - --format csv
ctx sql "SELECT ctx_session_id FROM ctx_sessions LIMIT 5" --format raw
```

`sql` runs one read-only SQL statement against `relational.sqlite`, the
disposable metadata projection bound to the active lexical generation. On a
completely fresh root it may initialize an empty projection. If a lexical
generation exists but the projection is missing, behind, or generation-mismatched,
SQL fails closed until catch-up completes. It does not refresh provider history,
import sources, run background upgrade checks, or open/migrate prior-epoch
storage.

Prefer stable read-only `ctx_*` views for scripts:

- `ctx_sessions`, one row per indexed session;
- `ctx_events`, one row per indexed event;
- `ctx_files_touched`, one row per normalized touched-file record;
- `ctx_sources`, one row per indexed provider source session.

Advanced users can query internal projection tables directly, but they are not
the compatibility surface. SQL contains metadata rather than transcript
bodies; output can still expose private paths, IDs, and source metadata.

Formats:

- default `table` output is compact and intended for humans and agents;
- `--format json` returns a structured result with `columns`,
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
not touch provider history or derived search storage. `docs show --out PATH`
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

`mcp serve` starts a local MCP server over newline-delimited stdio JSON-RPC. It
exposes eight tools: six OSS tools (`status`, `sources`, `search`, `sql`,
`show_session`, `show_event`) and two Pro tools (`pro_status`, `blame`).
`pro_status` remains read-only. Pro blame advertises `readOnlyHint: false`
because bounded local catch-up updates the canonical Core index, writes the
encrypted derived Pro graph, and writes the projection acknowledgement. It
never writes provider history or repositories. Its final serialized response,
including exact structured content plus text fallback, is capped at 1 MiB; an
over-cap page fails intact with guidance to lower `limit` or use CLI JSON. The
MCP search and SQL tools query the existing index only; they do not refresh or
import provider history, and MCP search currently uses the lexical search path
only. Tool results include MCP text content plus
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

Official installer-managed installs use daemon-owned automatic upgrade by
default; signed release metadata must also explicitly allow it. The enabled
long-lived daemon is the only automatic scheduler, including cadence and
backoff. Command dispatch and MCP never schedule upgrades. Disabling the daemon
causes zero automatic checks, downloads, or application. Scheduler state is
stored beside the managed executable and does not write to foreground stdout or
stderr. Use
`CTX_UPGRADE_AUTO=off` for a process-level opt-out,
or `ctx upgrade disable` to write `upgrade.auto = "off"` in `config.toml`.

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

## JSON Contract

JSON output is intended for local scripts, harnesses, and exact field
extraction. It is private unless a user explicitly reviews it.

Structured output is available for:

```text
ctx setup --format json
ctx status --format json
ctx index status --format json
ctx index watch --format jsonl
ctx index wait --format json
ctx sources --format json
ctx import --format json
ctx show session <ctx-session-id> --format json
ctx show event <ctx-event-id> --format json
ctx locate session <ctx-session-id> --format json
ctx locate event <ctx-event-id> --format json
ctx pro --format json
ctx pro --referral <codename> --format json
ctx pro setup --format json
ctx pro manage --no-open --format json
ctx pro uninstall (--delete-data|--keep-data) --format json
ctx referral create <codename> --format json
ctx referral status --format json
ctx referral payout [--no-open] [--country <CC>] [--entity-type <individual|company>] --format json
ctx blame file <path> --format json
ctx blame commit <sha> --format json
ctx blame pr <number-or-url> [--repository <logical-repository>] --format json
ctx search <query>|--term <term>|--file <path> --format json
ctx sql "SELECT COUNT(*) FROM ctx_sessions" --format json
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
