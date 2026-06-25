# Release Install Notes

No public release install command is published for ctx yet. Release artifacts,
checksums, signatures, and verification instructions must exist before this
page can add a public installer command.

The current source build path is:

```bash
cargo build -p ctx
cargo install --path crates/ctx-cli
```

This source build path is not release approval. It proves that a local checkout
can build the CLI; it does not prove a public binary, package, signature,
notarization result, dependency advisory review, license inventory, SBOM,
provenance statement, R2 object upload, or R2 readback.

Public install instructions stay blocked until all of the following are true:

- Multi-platform release artifacts exist for `linux-x64`, `macos-arm64`,
  `macos-x64`, `windows-x64`, and `freebsd-x64`, or an explicit
  manager-approved release exception names the missing target.
- Published checksums match those artifacts.
- Each produced platform artifact has packaged artifact runtime smoke evidence
  from installing or extracting the exact staged artifact and running
  `ctx --version`, `setup`, `import`, `search`, `doctor`, and `validate`
  against the fixture data.
- Dependency advisory/license evidence is approved.
- Signing, notarization, SBOM, and provenance evidence is approved.
- R2 staging upload/readback has passed with approved credentials.
- The completion certificate records real release evidence, not a contract
  fixture self-test.
