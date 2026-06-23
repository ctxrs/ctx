# Release Supply Chain

The current public release plan is non-publishing and launch-blocked.
Buildkite release dry-runs build host binaries, write manifests, write SHA-256
checksum files, and produce a completion certificate scaffold. They do not
upload, sign, notarize, or move a release channel.

The completion certificate for this phase is not a real public release
candidate approval. It records `launch_ready: false`, `release_approval:
false`, and `evidence_verified: false`; only `evidence_scaffold_verified` can
be true while any launch blocker remains.

## Finished-Product Evidence Matrix

The Buildkite pipeline includes non-publishing, resource-capped evidence lanes
for the finished-product review:

- provider fixture import validates inert Codex, Pi, and Claude provider JSONL
  fixtures and the focused provider replay import tests;
- rich search/context creates local records and evidence, then stores search
  and context JSON artifacts;
- dashboard/report artifact review exports local report JSON and dashboard HTML
  for inspection;
- PR publish dry-run renders the marker-bounded pull request comment without a
  network write;
- security/malicious archive fixtures check redaction corpus coverage and
  hostile archive test markers;
- jj e2e blocker status records whether `jj` is available on the runner without
  installing external tools;
- installer dry-run smoke validates local release metadata and an installer
  plan without downloading or installing binaries.

The completion certificate references these artifacts beside the platform
release dry-run manifests, provider live E2E lane definitions, the combined
release-candidate metadata, the R2 upload plan, and the FreeBSD blocker
artifact.

Current-head release completion is not implied by local self-tests. The
certificate validator requires Linux x64, macOS arm64, macOS x64, and Windows
x64 release dry-run manifests whose `git_commit` matches the current checkout,
plus the explicit FreeBSD blocker artifact. Synthetic self-test manifests are
marked as fixtures and are rejected by normal certificate runs. Even with those
checks passing, the certificate remains launch-blocked until the R2 upload,
public HTTPS installer smoke, SBOM/provenance/signing/notarization decisions,
and provider live evidence or accepted blockers are explicitly recorded.

Installer/release smoke status for this branch is dry-run only. The installer
smoke validates metadata parsing, unsafe input refusals, and the planned
install path, but it does not download, install, sign, notarize, or publish a
release artifact. The R2 staging smoke validates the generated upload and
cleanup plan, confirms no `ctx.rs/install` cutover is present, and records that
`upload_performed` is false; it is not executed by CI.

## Provider Live E2E

Normal CI records provider live E2E lane definitions in
`artifacts/buildkite/provider-live-e2e-lanes`. The live job is disabled by
default and is only scheduled when `CTX_LIVE_PROVIDER_E2E=1` and at least one
provider-specific opt-in variable such as `CTX_LIVE_PROVIDER_CODEX=1` is set.
The current release/CI slice emits blocker artifacts for live runs until
provider workers add real deterministic commands and redaction assertions.

No provider may be documented as `supported-live` unless its support-matrix row
has a real live E2E artifact from the gated lane.

Provider live or import lanes also need security evidence before release docs
can upgrade public fidelity claims: redaction corpus coverage for newly exposed
fields, malformed-input handling, raw-retention notes, and matching
threat-model updates. If those artifacts are absent, release workers should
leave the provider lane blocked and keep public docs at the narrower status.

## R2 Staging Layout

The release-candidate metadata lane writes:

- `install.sh`
- `install.ps1`
- `ctx-release-metadata.env`
- `checksums.sha256`
- `release-candidate-manifest.json`
- `r2-upload-plan.md`
- `r2-upload-commands.sh`
- `r2-staging-smoke.json`

The default staging prefix is
`ctx/records/release-candidate/v0.1.0/<git-commit>` in the
`ctx-release-artifacts` bucket. The public installer base URL must be
provided by `CTX_RELEASE_PUBLIC_BASE_URL` before an installer smoke can target
real R2 objects. Do not copy internal branch or crate names into public
installer URLs.

`scripts/release-candidate-metadata.sh` defaults to `https://example.invalid`
on purpose. That value is acceptable only for staging-plan validation and keeps
the certificate launch-blocked. A real RC requires an approved HTTPS staging
base URL, uploaded R2 objects, checksum verification against those objects, and
a recorded installer dry-run plus live install smoke in throwaway directories.

## Checksums

Every installable artifact must have one SHA-256 digest in release metadata and
in `checksums.sha256`. Installers verify the digest before copying a binary into
place and reject placeholder digests. Metadata is parsed as data, not executed.

## SBOM

SBOM publication is a release blocker until a concrete generator and output
format are selected. The preferred shape is one SBOM per platform artifact plus
a top-level index referenced by the completion certificate. Candidate formats
are SPDX JSON or CycloneDX JSON.

## Provenance

Build provenance is a release blocker until the release job can emit signed
provenance for each artifact. The expected evidence is an artifact-level
statement that binds repository, commit, Buildkite build URL, target triple,
artifact name, digest, and builder identity.

## Signing And Notarization

Signing is required before production publication:

- macOS artifacts require Developer ID signing and notarization before the
  installer metadata points at them.
- Windows artifacts require Authenticode signing before publication.
- Linux and FreeBSD artifacts should be signed with the selected release
  signing key, with public verification instructions published beside the
  checksums.

The current repository does not contain signing credentials or notarization
secrets. Release jobs must fail closed when credentials are absent.

## Completion Certificate

`scripts/release-completion-certificate.sh` writes a non-publishing certificate
artifact that lists required evidence and unresolved external blockers. The
certificate is a scaffold for finished-product review; it is not a release
approval by itself and must remain `launch_ready: false` until every blocker is
replaced by explicit PASS evidence.
