# Work Recorder Threat Model

This document covers the local Work Recorder launch branch. It describes the
implemented local CLI and the near-term integration points that influence the
security design. Hosted Option A, hosted sync, hosted accounts, team policy,
organization dashboards, remote retention, and hosted publish commands are out
of scope for this launch branch.

## Security Goals

- Keep Work Recorder useful without a hosted account.
- Avoid silent network upload of Work Recorder data.
- Make capture explicit or locally inspectable.
- Preserve command behavior when optional capture shims are enabled.
- Keep archives, dashboards, reports, and pull request packets reviewable before
  they leave the machine.
- Label future inferred relationships as inferred rather than authoritative.

## Non-Goals

- Prevent local administrators or malware on the same machine from reading the
  data root.
- Make arbitrary command output safe to publish without review.
- Guarantee that third-party tools run by the user do not use the network.
- Enforce centralized team retention, policy, or DLP controls.
- Import historical provider transcripts in this branch.
- Publish pull request comments in this branch.

## Assets

- Work Record metadata: ids, titles, bodies, kinds, tags, workspace paths, pull
  request URLs, and timestamps.
- Command evidence: command strings, exit codes, timestamps, durations, stdout
  and stderr previews, and full stdout/stderr blob payloads.
- Capture spool: pending, processing, done, failed, and error sidecar files in
  the local inbox.
- Archives: JSON exports that may contain record metadata and evidence payloads.
- Dashboard and reports: generated local review artifacts.
- Shim directory: opt-in wrapper scripts for Git, jj, and GitHub CLI.
- Dependency and installer inputs: Rust crates, lockfiles, build scripts, and
  any future installer/update metadata.

## Trust Boundaries

### Data Root

The data root is local storage, normally under `~/.ctx/work-record/` or under
`CTX_DATA_ROOT` when set. SQLite metadata, blob payloads, and inbox files are
trusted only as local user data. File permissions and disk encryption are
delegated to the operating system and user environment.

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
- `ctx uninstall --yes` removes the local Work Recorder product data store.

Follow-ups:

- document and test filesystem permission expectations per platform;
- add optional encrypted-at-rest storage design if product scope requires it;
- add retention or pruning commands if long-lived local stores become common.

### Shims and Hooks

The implemented shims are opt-in wrappers for `git`, `jj`, and `gh`. They run
the real tool later on `PATH`, preserve the exit code, and best-effort spool
local capture envelopes. Repository hooks, shell hooks, and provider-native
hooks are not implemented in this branch.

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

Provider transcript import for Codex, Claude, Cursor, and other local agent
histories is not implemented. Future importers would cross from provider-owned
storage into the ctx data root.

Risks for future work:

- importing more data than the user expects;
- normalizing private provider metadata into shareable archives;
- mixing multiple projects or accounts into one record set;
- replaying malformed provider files into the capture importer.

Required design gates before implementation:

- explicit opt-in source selection;
- dry-run inventory with counts and path roots;
- provider-specific redaction tests;
- clear provenance fields for imported records;
- no default hosted upload of imported transcripts.

### Capture Spool

The capture spool is a local JSONL inbox. Current writers are fixtures and the
opt-in Git/jj/gh shims. Normal Work Recorder commands import pending files
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

Follow-ups:

- harden JSON schema validation and size limits;
- add corpus-backed redaction tests before broadening capture writers;
- document atomic write requirements for future spool writers.

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
artifacts. The dashboard is static local HTML with no hosted sync, JavaScript,
tracking, or remote assets.

Risks:

- generated artifacts can contain sensitive record text, paths, PR links, and
  evidence previews;
- users may serve or attach dashboard output without reviewing it;
- future dashboard links could imply hosted sharing when only local export is
  implemented.

Controls:

- dashboard export writes local files only;
- docs say to review outputs before sharing;
- `CTX_DASHBOARD_URL` is only a link base for share-safe URLs in JSON packets,
  not hosted sync.

Follow-ups:

- add escaping tests for generated HTML;
- add fixture coverage for redacted report/dashboard examples;
- keep hosted dashboard claims out of launch docs until implementation exists.

### Pull Request Publishing

The launch branch parses PR URLs and links one PR URL to a local record. It
does not create, update, or delete pull request comments.

Risks for current local behavior:

- private repository URLs may be stored in records, reports, dashboards, and
  archives;
- parsed URL confidence can be mistaken for verified repository access.

Risks for future publishing:

- accidentally posting raw prompts or command output;
- mutating a PR description instead of a separate ctx-owned comment;
- publishing stale or cross-repo work records;
- leaking private dashboard links.

Controls:

- `ctx link-pr` is local only;
- product direction says future publishing should upsert a separate ctx comment
  by default;
- inferred links should be confidence labeled.

Required design gates before publisher implementation:

- explicit preview step;
- redaction pass over publish payloads;
- repository and PR identity verification;
- idempotent ctx-owned comment marker;
- tests for private URL handling and stale record selection.

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
dashboards, organization retention, and hosted publish workflows, is explicitly
out of launch scope. Any future hosted design needs a separate threat model that
covers identity, authorization, tenant isolation, audit logging, retention,
deletion, incident response, and data residency.
