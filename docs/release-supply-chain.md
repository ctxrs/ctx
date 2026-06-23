# Release Supply Chain

The current public release plan is non-publishing. Buildkite release dry-runs
build host binaries, write manifests, write SHA-256 checksum files, and produce
a completion certificate scaffold. They do not upload, sign, notarize, or move
a release channel.

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
approval by itself.
