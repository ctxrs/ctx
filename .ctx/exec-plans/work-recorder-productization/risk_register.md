# Work Recorder Productization Risk Register

Updated: 2026-06-22T19:40:55-05:00

| Risk | Impact | Current Mitigation |
| --- | --- | --- |
| Scope is large enough to span public local product, private hosted staging, CI/release, and dogfood. | High schedule and integration risk. | Milestone gates and status files will track concrete blockers instead of vague deferrals. |
| Private repo canonical checkout is dirty with unrelated work. | Risk of overwriting unrelated user/agent changes. | Use a separate manual `ctx-private` worktree before edits. |
| Broad Rust/Bazel/build verification can overload this host. | Machine instability and false failures. | Use existing resource-safe wrappers and avoid overlapping heavy jobs. |
| Dashboard can pass tests but remain visually sparse. | Product-quality failure. | Require screenshot generation, manual inspection, and adversarial UI review. |
| Hosted staging credentials or runner access may be unavailable. | External blocker for completion criteria. | Record exact attempted command, missing credential/runner, and remediation; keep unblocked tracks moving. |
| README/docs currently overclaim implemented behavior. | User confusion and false product promises. | Docs truth-pass worker is scoped to README/docs only. |
| Existing local store shape diverges from the product contract. | Capture/search/hosted sync churn if built on the wrong schema. | Land core schema/types and versioned store migrations before capture/search/dashboard work. |
| Buildkite/release platform matrix is absent. | Cannot satisfy release-verification criteria yet. | CI worker is scoped to resource-safe scripts and initial Buildkite wiring first. |
| `/tmp` pressure and concurrent broad Cargo checks can freeze this host. | Local verification instability and interrupted agent work. | Use `TMPDIR=/var/tmp/ctxwr`, low `CARGO_BUILD_JOBS`, low `RUST_TEST_THREADS`, and avoid overlapping broad Cargo commands across agents. |
| Bazel is not installed in this environment. | Local `scripts/check.sh all` cannot prove Bazel lanes yet. | The script records the Bazel lane as skipped; Buildkite or local Bazel/Bazelisk installation must satisfy the retained Bazel requirement later. |
| Archive artifact payloads are string-only. | Future binary screenshots/reports cannot be faithfully exported through the current JSON artifact payload field. | Current foundation scope uses text stdout/stderr artifacts only; non-text artifact export should use an explicit encoded/binary-safe payload design before binary artifacts are added. |
| Chrome/headless screenshot capture can fail if it uses the default `/tmp` profile/cache. | Visual review can fail for environment reasons unrelated to dashboard rendering. | Use `/var/tmp` for `TMPDIR`, `--user-data-dir`, and `--disk-cache-dir` when capturing local dashboard screenshots on this host. |

## Accepted Risks

None accepted yet.
