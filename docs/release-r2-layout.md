# Release R2 Layout

This page documents the intended R2 staging layout for ctx release candidates.
Normal CI validates the plan only; it does not upload objects or expose public
install instructions.

## R2 staging layout

Release-candidate objects use this prefix shape:

```text
ctx/releases/release-candidate/v<version>/<git-commit>/
```

The current non-publishing CI staging set contains:

- one binary artifact per install platform;
- `install.sh`;
- `install.ps1`;
- `ctx-release-metadata.env`;
- `checksums.sha256`;
- `release-candidate-manifest.json`.

The staging smoke expects nine objects when the evidence tree carries a
FreeBSD blocker or manager-approved release exception: four produced platform
artifacts, two installer scripts, metadata, checksums, and the manifest.

When native `freebsd-x64` evidence is present, the staging smoke expects ten
objects: five produced platform artifacts, two installer scripts, metadata,
checksums, and the manifest. The FreeBSD artifact object is
`ctx-0.1.0-x86_64-unknown-freebsd`.

## Cutover Rules

No installer endpoint cutover is allowed from normal CI. A manager-run staging
upload must use approved credentials, then verify public HTTPS reads, checksums,
and installer dry-runs before any public install page can change.

Cleanup commands must be recorded with the staging plan so a failed candidate
can be removed without guessing object names.

## Upload/readback proof

`scripts/release-r2-staging-readback-proof.sh` records the upload/readback
contract. Default CI mode validates the candidate object list and writes
`status=blocked_manual_required`, `upload_performed=false`, and
`readback_performed=false`.

Real R2 staging requires both `CTX_RELEASE_R2_UPLOAD_READBACK=1` and
`CTX_RELEASE_R2_MANAGER_APPROVED=1`. In that mode the script uses Wrangler to
put each staged object, fetch it back, and compare SHA-256 checksums. The proof
still records `no_ctx_rs_cutover=true`; public endpoint changes remain a
separate manager decision.
