# ctx Local Recording Threat Model

This document covers the local ctx recording launch branch. It describes the
implemented local CLI and the near-term integration points that influence the
security design. Hosted Option A, hosted sync, hosted accounts, team policy,
organization dashboards, remote retention, and hosted publish commands are out
of scope for this launch branch. Local GitHub PR comment publishing through the
authenticated `gh` CLI is in scope.

## Security Goals

- Keep ctx useful without a hosted account.
- Avoid silent network upload of recorded work.
- Make capture explicit or locally inspectable.
- Preserve command behavior when optional capture shims are enabled.
- Keep archives, dashboards, reports, and pull request packets reviewable before
  they leave the machine.
- Keep redaction framed as heuristic defense for default review output, not as
  a guarantee that arbitrary raw transcripts or object payloads are safe.
- Label future inferred relationships as inferred rather than authoritative.

## Non-Goals

- Prevent local administrators or malware on the same machine from reading the
  data root.
- Make arbitrary command output safe to publish without review.
- Make arbitrary provider transcripts safe to import, sync, or publish without
  provider-specific tests and review.
- Guarantee that third-party tools run by the user do not use the network.
- Enforce centralized team retention, policy, or DLP controls.
- Import full historical provider transcripts in this branch. The only
  provider-history path is explicit local Codex prompt-history JSONL import,
  prompt-only and `summary_only`.
- Provide hosted/team pull request publishing; it remains outside launch scope
  in this branch.

## Assets

- Work record metadata: ids, titles, bodies, kinds, tags, workspace paths, pull
  request URLs, and timestamps.
- Command evidence: command strings, exit codes, timestamps, durations, stdout
  and stderr previews, and full stdout/stderr object payloads.
- Capture spool: pending, processing, done, failed, and error sidecar files in
  the local spool.
- Archives: JSON exports that may contain record metadata and evidence payloads.
- Dashboard and reports: generated local review artifacts.
- Shim directory: opt-in wrapper scripts for Git, jj, and GitHub CLI.
- Dependency and installer inputs: Rust crates, lockfiles, build scripts, and
  any future installer/update metadata.

## Trust Boundaries

### Data Root

The data root is local storage, normally `~/.ctx` or the root named by
`CTX_DATA_ROOT`/`--data-root`. ctx does not append an extra product directory.
Canonical local layout is `work.sqlite`, `objects/`, `spool/`, `shims/`,
`config.toml`, and `logs/` under that root. SQLite metadata, object payloads,
and spool files are trusted only as local user data. File permissions and disk
encryption are delegated to the operating system and user environment.

Risks:

- sensitive prompts, paths, source code, credentials, or customer data are
  stored in records or evidence;
- exported archives move local secrets to another machine;
- failed capture files remain readable after import errors;
- shared machines retain records longer than expected.

Controls:

- no hosted upload in the launch branch;
- explicit export/import commands;
- retention guidance in privacy docs;
- failed spool files are retained for inspection instead of discarded;
- `ctx uninstall` removes shims, shell integration, and any optional service
  while keeping recorded data. `ctx uninstall --delete-data` deletes the local
  store, objects, spool, logs, and config after explicit confirmation or force.

Follow-ups:

- document and test filesystem permission expectations per platform;
- add optional encrypted-at-rest storage design if product scope requires it;
- add retention or pruning commands if long-lived local stores become common.

### Shims and Hooks

The implemented shims are wrappers for `git`, `jj`, and `gh`. `ctx setup`
installs them under the data root, and they capture only after that directory is
active on `PATH`. They run the real tool later on `PATH`, preserve the exit
code, and best-effort spool local capture envelopes. Repository hooks and
provider-native hooks are not implemented in this branch. Shell rc activation is
available only through explicit marker-bounded commands that write backups.

Risks:

- a user may accidentally put the shim directory earlier on `PATH` than
  intended;
- command output can include secrets;
- a broken shim could alter command behavior;
- a malicious or stale shim directory could impersonate tooling.

Controls:

- shims are installed only by explicit command into a user-selected directory;
- `ctx shim env` prints the environment change instead of mutating shell startup
  files;
- `ctx shim uninstall` removes ctx-marked scripts;
- capture failure should not block the wrapped tool.

Follow-ups:

- add signature or marker validation tests for shim uninstall safety;
- add explicit docs for inspecting generated shim contents;
- consider denylisting high-risk `gh` subcommands or redacting auth output.

### Provider Transcript Import

Full provider transcript import for every local agent history is not
implemented. This branch includes normalized provider fixture import for Codex,
Claude, Pi, OpenCode, Antigravity, Gemini, and Cursor; explicit Codex
prompt-history JSONL import when the user provides an input path; and explicit
Pi session JSONL import. The Codex prompt-history path imports prompt rows only
and records `fidelity=summary_only`. The Pi session path records
`fidelity=imported`, preserves message entry ids and parent ids in metadata,
and does not convert those parent ids into ctx subagent edges or expand raw
image blocks into artifacts.

Future full-fidelity importers would cross from provider-owned storage into the
ctx data root.

Risks for future work:

- importing more data than the user expects;
- normalizing private provider metadata into shareable archives;
- mixing multiple projects or accounts into one record set;
- replaying malformed provider files into the capture importer.

Required design gates before implementation:

- explicit opt-in source selection;
- dry-run inventory with counts and path roots;
- provider-specific redaction tests;
- provider-specific malformed-input and replay/idempotency tests;
- clear provenance fields for imported records;
- documented raw-retention behavior for transcript text, command output,
  images, attachments, and local object payloads;
- threat-model updates for each new provider source format or hook boundary;
- no default hosted upload of imported provider data.

The Codex prompt-history importer satisfies only the explicit source-selection
and provenance gates for prompt logs. It does not satisfy full transcript,
assistant response, tool-call, command-output, or child-session capture gates.
The Pi session importer satisfies explicit source-selection and provenance
gates for bounded session JSONL, but not passive capture or full artifact
extraction gates.

### Capture Spool

The capture spool is a local JSONL queue. Current writers are fixtures and the
opt-in Git/jj/gh shims. Normal ctx work records commands import pending files
before serving results.

Risks:

- malformed JSONL can cause import failure or ambiguous partial imports;
- replayed envelopes can duplicate evidence if dedupe keys are wrong;
- pending files may expose secrets before import;
- failed files and error sidecars can leak sensitive paths or payload excerpts.

Controls:

- successful files move to `.done`;
- failed files move to `.failed` with `.error.json`;
- stable ids are derived from envelope dedupe keys when ids are omitted;
- `ctx status`, `ctx doctor`, `ctx validate`, and `ctx repair` expose local
  spool health.
- default review output redacts known secret-like values, but pending, failed,
  and malformed spool files are still raw private local data.

Follow-ups:

- harden JSON schema validation and size limits;
- add corpus-backed redaction tests before broadening capture writers;
- document atomic write requirements for future spool writers.
- coordinate provider/release CI so new hook writers prove pass-through,
  failure fallback, malformed-entry handling, and redaction before the public
  provider matrix is upgraded.

### Archive Import and Export

Archives are JSON files used to move records between machines. They can contain
full evidence payloads.

Risks:

- archives may be committed or uploaded accidentally;
- import may overwrite local records when `--overwrite` is used;
- archives from untrusted sources could exercise parser or storage edge cases;
- records from different confidentiality contexts can be merged.

Controls:

- export is explicit;
- import reads ctx archives only, not provider transcript directories;
- `--overwrite` is an explicit import mode;
- docs warn users to review archives before sharing.

Follow-ups:

- add archive schema versioning and compatibility policy;
- add size and record-count limits for untrusted archives;
- add a review or redaction command before export if launch scope expands.

### Dashboard and Report

`ctx report`, `ctx context`, and `ctx dashboard export` create local review
artifacts. The dashboard is a local React/Vite export with bundled local assets
and no hosted sync, tracking, or remote assets.

Risks:

- generated artifacts can contain sensitive record text, paths, PR links, and
  evidence previews;
- users may serve or attach dashboard output without reviewing it;
- future dashboard links could imply hosted sharing when only local export is
  implemented.

Controls:

- dashboard export writes local files only;
- docs say to review outputs before sharing;
- redaction coverage is corpus-backed for current shareable surfaces, but the
  corpus is heuristic and not a proof that arbitrary future transcript fields
  are safe;
- dashboard dogfood manifests use artifact-relative paths and omit raw local
  data-root, repository, home, and browser scratch paths;
- `CTX_DASHBOARD_URL` is only a link base for share-safe URLs in JSON packets,
  not hosted sync.

Follow-ups:

- add escaping tests for generated HTML;
- add fixture coverage for redacted report/dashboard examples;
- keep hosted dashboard claims out of launch docs until implementation exists.

### Pull Request Publishing

The launch branch parses PR URLs, links one PR URL to a local record, renders a
dry-run PR comment, and can create or update one marker-bounded ctx comment on
a linked GitHub pull request through the authenticated local `gh` CLI. GitLab,
hosted, and team publishing are outside launch scope.

Risks for current local behavior:

- private repository URLs may be stored in records, reports, dashboards, and
  archives;
- parsed URL confidence can be mistaken for verified repository access.
- accidentally posting raw prompts or command output;
- mutating a PR description instead of a separate ctx-owned comment;
- publishing stale or cross-repo work records;
- leaking private dashboard links.

Controls:

- `ctx link-pr` is local only;
- `ctx publish pr-comment --dry-run` provides an explicit preview step;
- PR comment rendering redacts command output and secret-like content by
  default;
- `--include-raw-transcript` is explicit opt-in for raw command stdout/stderr
  evidence;
- publishing uses an idempotent ctx-owned comment marker;
- inferred links should be confidence labeled;
- hosted/team publishing remains out of scope pending a separate threat model.

### Installer and Update Supply Chain

Public installer URLs are not documented as live for this branch. Users build
or install from source. The current supply chain is the repository checkout,
Cargo.lock, Rust toolchain, bundled SQLite through `rusqlite`, and the user's
local build environment.

Risks:

- compromised dependency version or crate registry account;
- stale lockfile or unreviewed dependency changes;
- future installer scripts executed from unauthenticated URLs;
- update channel confusion between local branch builds and public releases.

Controls:

- workspace packages declare `Apache-2.0`;
- dependency versions are pinned by `Cargo.lock`;
- launch docs do not claim public installer URLs;
- dependency/license audit decisions are documented separately.

Follow-ups:

- add automated vulnerability and license scanning before public installer
  publication;
- publish checksums/signatures for release artifacts;
- document update channel behavior before adding updater commands.

## Hosted Option A Out of Scope

Hosted Option A, including account login, hosted sync, team policy, centralized
dashboards, organization retention, and hosted publish workflows, is explicitly out of launch scope. Any future hosted design needs a separate threat model that
covers identity, authorization, tenant isolation, audit logging, retention,
deletion, incident response, and data residency.
