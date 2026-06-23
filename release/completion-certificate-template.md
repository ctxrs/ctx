# Work Recorder Completion Certificate

- Schema version: `1`
- Program: `work-recorder-finished-product`
- Repository: `ctxrs/ctx`
- Git commit: `${git_commit}`
- Git branch: `${git_branch}`
- Buildkite build: `${buildkite_build_url}`
- Generated at Unix seconds: `${generated_at_unix_s}`
- Publishing status: `false`

## Required Evidence

- Pipeline contract artifact: `${pipeline_contract_artifact}`
- Linux x64 release dry-run manifest: `${linux_x64_manifest}`
- macOS arm64 release dry-run manifest: `${macos_arm64_manifest}`
- macOS x64 release dry-run manifest: `${macos_x64_manifest}`
- Windows x64 release dry-run manifest: `${windows_x64_manifest}`
- FreeBSD x64 blocker artifact: `${freebsd_x64_blocker}`
- Release install documentation: `docs/release-install.md`
- Release supply-chain documentation: `docs/release-supply-chain.md`

## External Release Blockers

- FreeBSD native release lane requires a documented native `freebsd-x64` Buildkite queue or a separately proven cross-build lane.
- Production release publication requires final release metadata with non-placeholder SHA-256 checksums for every published artifact.
- Signing, notarization, SBOM publication, and provenance publication require configured external credentials and policy approval.
