# Product Contract

ctx is a local search CLI for existing agent history.

## Promise

Given local provider transcripts that ctx supports, the CLI builds a local
SQLite index and returns deterministic retrieval results with citations. The
paid Local Pro helper adds an encrypted local work graph with deterministic,
evidence-backed fact materialization. Neither path generates an LLM
interpretation, uploads transcript or repository content, or requires a hosted
research agent.

## In Scope

- `ctx setup` initializes local storage, indexes discovered supported local
  transcript formats, and can opportunistically start the default-on ctx-owned
  background daemon maintenance profile. An explicit `[daemon] enabled = false`
  remains a durable opt-out.
  `ctx setup --no-daemon`,
  `ctx setup --catalog-only`, and machine-readable setup do not autostart or
  nudge maintenance.
- `ctx sources` reports known local provider history paths, including whether a
  native source is currently importable.
- `ctx import` indexes supported local transcript formats and selected local
  history-source plugins, and can opportunistically start the same short
  one-pass ctx-owned maintenance profile when `[daemon].enabled` is true.
  Explicit custom JSONL and history-source imports use its required
  daemon-owned source-refresh endpoint. `ctx import --no-daemon` never starts
  it and therefore requires an already-running endpoint for those explicit
  source-backed routes.
- `ctx search` can refresh a bounded batch from discovered native provider
  sources before returning ranked local hits from the local index, with event
  IDs when a hit maps to an indexed event. History-source plugins enter the
  same source-backed index only through explicit single-source import in 1.0.
  Default background refresh may autostart daemon maintenance for
  human-readable search while
  semantic search is disabled. Semantic and hybrid search read existing local
  sidecar coverage only; search does not run vector backfill or download
  embedding models. Hybrid uses semantic evidence only after sidecar coverage
  is complete and dirty work is drained; explicit semantic search may query
  partial coverage for diagnostics.
- `ctx show session` and `ctx show event` render transcripts, hits, and context
  windows using ctx-owned IDs, and `ctx show session --out` writes transcript
  artifacts.
- `ctx locate session` and `ctx locate event` report provenance and resume
  metadata.
- `ctx pro` starts or resumes an anonymous 14-day trial without an account,
  authentication, or payment method, installs or repairs the signed
  target-specific helper, and catches the encrypted graph up. The official
  interactive installer offers the same trial with an explicit default-yes
  confirmation; unattended, CI, and machine-readable setup remain Core-only
  unless the installer receives an explicit Pro-trial option. Trial activation
  failure never changes Core setup success or daemon startup. WorkOS sign-in
  and Stripe are used only for paid conversion, account management, and
  explicit referral commands.
  `ctx pro setup` is a supported explicit synonym.
  `ctx status` reports Pro state and the next useful action without mutating
  canonical history or graph data. Entitlement authorization may advance
  nonsecret anti-clock-rollback metadata.
  `ctx pro manage` opens hosted account and billing
  management. Interactive `ctx pro uninstall` asks whether to delete local Pro
  data; noninteractive callers must explicitly pass `--delete-data` or
  `--keep-data`. The keep path is local-only and preserves local Pro data;
  verified `--delete-data` remains idempotently available after the helper is
  gone. Missing and never-Pro roots leave Pro state unchanged and report that
  Pro data is absent rather than preserved or deleted. This no-op statement is
  Pro-state-only: `pro_uninstall` remains an eligible Core foreground operation,
  so default-on local usage reporting may create or increment `usage.sqlite`.
  Interrupted initialization remains identity-aware and deletable before a
  helper or graph file exists. Destructive uninstall fails before deletion on
  corrupt credential inventory and persists an exact-root cleanup phase so
  retries remain verifiable after graph-key or credential records are already
  absent.
- Local Pro credentials prefer the platform vault: Secret Service on Linux,
  Keychain Services on macOS, and Credential Manager on Windows. A pristine
  canonical root may durably select the supported owner-private file backend
  only when the native adapter reports its platform's exact unavailable
  condition. Locked, denied, corrupt, ambiguous, canceled, access-control, and
  entitlement failures never select the file backend. A durable native
  selection never downgrades after a later outage, and read-only inspection of
  an unselected pristine root creates no selector, lock, directory, or record.
  Markerless public file-vault or interrupted-selector state is corrupt and is
  never reinterpreted as pristine. Public fallback selection also refuses a
  root that already contains private graph-store or graph-database state; the
  private graph selector remains independently owned and may be selected after
  public activation during the ordered fresh-trial flow.
  The public selector is `<data-root>/pro/.ctx-pro.credential-backend-v1` with
  exactly one of
  `ctx-pro-credential-backend-v1:{file,secret-service,keychain,credential-manager}\n`.
  Its owner-private records remain in the separate
  `<data-root>/pro/.ctx-pro.credentials-v1` namespace. Private graph-key
  records retain their own selector, namespace, and deletion lifecycle.
  Neither namespace permits an environment key, universal key, binary pepper,
  plaintext database key, or legacy Store fallback.
- `ctx pro --referral <codename>` is the sole referral-attribution input. The
  ordinary anonymous trial is 14 days; a code accepted with the first
  activation produces a 30-day referred trial. Attribution is immutable after
  that first accepted activation, the raw code is not retained after activation,
  and an existing nonreferred trial cannot attach one later. There is no
  website, cookie, creator-affiliate, annual-plan, or compatibility attribution
  path.
- Referral availability is a reviewed per-channel build decision and is
  disabled for both shipped channel configurations. Unavailable referral
  commands and `ctx pro --referral` fail before local identity creation,
  authentication, or browser side effects. The human-only `ctx blame` CTA is
  suppressed and does not consume its once-only marker while unavailable.
- Any WorkOS-verified person can use `ctx referral create` to claim one stable
  codename without a Pro trial or subscription.
  `ctx referral create|status|payout` are explicit hosted-service commands and
  form the complete referrer management surface. Human mode may start WorkOS
  AuthKit, and payout may open Stripe-hosted onboarding. JSON mode uses cached
  authentication only and never starts authentication or opens a browser.
  Referrer status is authenticated, private, and aggregate; it exposes no
  referred identity, invoice, or per-referral ledger. Its cash buckets are
  earned, pending, manual review, payable, processing, paid, and debt.
  Processing is sent but unsettled; paid remains historical cash actually
  settled, and a post-paid reversal increases debt rather than reducing paid.
  The aggregate identity is `earned + debt = pending + manual review + payable
  + processing + paid`.
- The referral commission is $10 cash for each distinct qualifying $20 monthly
  Pro invoice, invoices 1 through 12, with a $120 maximum per direct referral.
  Invoice 1 and invoice 2 commissions accrue pending and require invoice 2 to
  settle, the required 14-day hold, authoritative reconciliation, and manual
  review before payability. Invoices 3 through 12 each require their own 14-day
  hold, reconciliation, and manual review. Refunds and disputes void unpaid
  commissions. A paid commission reversal becomes debt, a negative adjustment
  against future earnings subject to manual review; ctx does not attempt an
  external clawback.
- Referral copy leads with
  `Refer a developer. Earn $10/month toward your agent bill.` and may follow
  with `Up to $120 per friend.` Routine status, MCP, and Core flows do not
  surface referral prompts. The only automatic mention is a
  nonsecret-marker-backed, shown-once line after a successful, nonempty,
  interactive human Pro blame result; machine-readable, noninteractive, empty,
  failed, install, setup, Core, and subsequent blame paths suppress it.
- Pro materialization is an internal idempotent capability invoked by setup,
  daemon freshness, and blame. Repository roots are inferred from
  canonical activity rather than accepted as setup flags.
- The only public Pro query is `ctx blame file|commit|pr`. It returns typed,
  bounded matches with complete deduplicated canonical evidence. OSS
  `ctx show session|event` and `ctx locate session|event` remain available;
  there are no Pro show, locate, timeline, facts, or related aliases. Blame may
  catch stale derived state up;
  that changes only the encrypted graph. Pure canonical tail appends
  resume from the durable frontier; incompatible mutation epochs, legacy
  derived state, or repository semantics trigger a token-checked derived reset
  and replay without changing canonical history. Materialization reports
  `NotMaterialized`, `Partial`, `NeedsResume`, `NeedsRebuild`, or `Ready`.
  Blame continuation cursors are opaque and graph-state-bound.
- PR activity is not code production. A PR-to-commit relationship is present
  only when a recognized structured forge record binds the canonical PR
  identity and exact Git object ID. Without that proof, associated commits are
  explicitly unproven.
- `ctx sql` runs one read-only SQL statement against the existing local index
  for advanced inspection when normal search is not expressive enough.
- `ctx doctor` reports local storage health.
- `ctx docs` exposes embedded public documentation and generated man pages.
- `ctx upgrade` checks and applies signed CLI releases for official
  installer-managed binaries.
- `ctx daemon` is the first-class local coordinator surface for status,
  enable/disable config, opportunistic maintenance started by setup/import when
  enabled, and foreground maintenance runs. The coordinator performs bounded
  native provider-history refresh and local semantic indexing/freshness work.
  Setup/import
  autostart reports semantic status read-only; explicit `ctx daemon run` is the
  path that may perform semantic catch-up.
- `ctx status` and `ctx doctor` report ctx-owned daemon coordinator state.
- `ctx stats` reports bounded local usage/value aggregates from the separate
  owner-private `usage.sqlite` sidecar. This default-on product state is
  independent of remote event reporting, has no network path or identity, keeps
  only daily UTC content-free aggregates for approximately 400 days, and fails
  open at foreground recording boundaries. The stats report is read-only,
  uncounted, and separates measured facts from versioned estimates. Detail is
  an option on `ctx stats`; enable, disable, and logical reset remain under
  `ctx status --usage`.
- JSON output supports local agents and scripts without daemon-start or
  daemon-nudge side effects. A daemon already running independently continues
  its own maintenance and automatic-upgrade cadence.

## Local Pro Access States

| State | Meaning | Local Pro graph access |
| --- | --- | --- |
| trial | anonymous 14-day trial; no account or payment method | allowed |
| active | paid monthly subscription | allowed |
| canceling_paid | canceled at period end, but the current period is paid | allowed through the paid deadline |
| offline_grace | the bounded signed offline grant remains valid | allowed through the final grace deadline |
| locked | no active access or valid grace remains | denied; encrypted graph is preserved for recovery |

`ctx pro manage` handles cancellation, payment recovery, and resubscription.
Pro is $20 USD per month.
Core OSS setup, search, indexed show/locate, and SQL remain available in every
state. Pro uninstall and explicit Pro data deletion also remain available.
Explicit status, MCP `pro_status`, and Pro management may show the
`$20/month` continuation action for trial access or a neutral
unpriced `pro_restore_access` action for locked access that confirms the local
graph is preserved. They do not show a purchase action for paid active,
`canceling_paid`, or `offline_grace` access, do not replace the existing next
action, do not open a browser from status, and never add marketing text to
blame citations.
Hosted traffic is limited to anonymous-trial challenge/evidence tokens,
optional first-challenge referral attribution and its opaque claim, explicit
referrer commands, identity and billing after conversion, entitlements, signed
release metadata, and anonymous signed artifact delivery. Trial evidence consists
only of challenge-bound application-specific digests; raw platform identifiers
are discarded inside the signed helper, and the service stores independently
keyed lookup tokens. The signal is best-effort abuse detection, not hardware
attestation. Hosted traffic excludes transcript text, source code, repository
paths or URLs, Git objects, graph facts, citations, and queries.

## Out Of Scope

- hosted model inference, hidden LLM calls, or API-key-dependent inference by
  ctx; local semantic embedding is allowed only as documented search behavior;
- team and enterprise seats, invitations, SSO, SCIM, or organization
  administration;
- annual plans, device caps, and device-management UI;
- hosted transcript storage, hosted work graphs, or remote repository analysis;
- ask/brief agents, cloud research agents, or universal deterministic detector
  accuracy;
- a ctx history or work-graph browser UI;
- source repository modification;
- shell startup-file modification;
- write-capable SQL access;
- API-key requirements for core setup/import/search;
- provider-owned history daemons, hooks, or background collection outside
  documented ctx-owned daemon maintenance;
- self-upgrade for unmanaged source builds, package-manager installs, or copied
  binaries;
- provider-native import claims that are not listed in the support matrix.

## Determinism

For the same database, query, filters, and result limit, search should return
the same ranked material in the same order. Timestamps such as `generated_at`
can differ between runs.

## Citation Contract

Results should preserve enough metadata for an agent to verify important
details:

- provider when known;
- ctx-owned session and event IDs;
- provider-owned session ID when known;
- event sequence when known;
- source path and cursor when available;
- source availability when checked.

Provider-owned IDs are metadata. Positional command arguments are ctx-owned
IDs unless a command explicitly accepts `--provider ... --provider-session ...`.

If raw source files move, ctx may still return indexed text from SQLite. Output
should make source availability visible when that information is known.

## Privacy Contract

The local index, encrypted Pro graph, and JSON output are private by default.
Identity, billing, entitlement, and signed-artifact requests do not carry
history, repository, graph, citation, or query content. A user must review
copied output before sharing it outside the machine.
