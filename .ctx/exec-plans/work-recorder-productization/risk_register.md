# Work Recorder Productization Risk Register

Updated: 2026-06-22T20:02:05-05:00

| Risk | Impact | Current Mitigation |
| --- | --- | --- |
| Scope is large enough to span public local product, private hosted staging, CI/release, and dogfood. | High schedule and integration risk. | Milestone gates and status files will track concrete blockers instead of vague deferrals. |
| Private repo canonical checkout is dirty with unrelated work. | Risk of overwriting unrelated user/agent changes. | Use a separate manual `ctx-private` worktree before edits. |
| Broad Rust/Bazel/build verification can overload this host. | Machine instability and false failures. | Use existing resource-safe wrappers and avoid overlapping heavy jobs. |
| Dashboard can pass tests but remain visually sparse. | Product-quality failure. | Require screenshot generation, manual inspection, and adversarial UI review. |
| Hosted staging credentials or runner access may be unavailable. | External blocker for completion criteria. | Record exact attempted command, missing credential/runner, and remediation; keep unblocked tracks moving. |
| README/docs currently overclaim implemented behavior. | User confusion and false product promises. | Docs truth-pass worker is scoped to README/docs only. |
| Existing local store shape diverges from the product contract. | Capture/search/hosted sync churn if built on the wrong schema. | Land core schema/types and versioned store migrations before capture/search/dashboard work. |
| Buildkite/release platform matrix requires live runner proof. | Repo-owned config can be locally validated, but the release gate still needs real Buildkite green evidence. | Public pipeline now has a local contract check, native dry-run lanes for known Linux/macOS/Windows queues, and explicit blocker evidence for FreeBSD. |
| No FreeBSD x86_64 Buildkite queue is documented. | Native FreeBSD release artifact proof cannot pass yet. | Public pipeline emits a FreeBSD blocker artifact with missing runner label, attempted native command, proposed `queue=freebsd-x64` pool, and artifact status. |
| `/tmp` pressure and concurrent broad Cargo checks can freeze this host. | Local verification instability and interrupted agent work. | Use `TMPDIR=/var/tmp/ctxwr`, low `CARGO_BUILD_JOBS`, low `RUST_TEST_THREADS`, and avoid overlapping broad Cargo commands across agents. |
| Bazel is not installed in this environment. | Local `scripts/check.sh all` cannot prove Bazel lanes yet. | The script records the Bazel lane as skipped locally; the Buildkite Bazel lane sets `CTX_REQUIRE_BAZEL=1` so CI fails if Bazel/Bazelisk is missing. |
| Archive artifact payloads are string-only. | Future binary screenshots/reports cannot be faithfully exported through the current JSON artifact payload field. | Current foundation scope uses text stdout/stderr artifacts only; non-text artifact export should use an explicit encoded/binary-safe payload design before binary artifacts are added. |
| Chrome/headless screenshot capture can fail if it uses the default `/tmp` profile/cache. | Visual review can fail for environment reasons unrelated to dashboard rendering. | Use `/var/tmp` for `TMPDIR`, `--user-data-dir`, and `--disk-cache-dir` when capturing local dashboard screenshots on this host. |
| Local Git/jj/gh wrapper shims can capture sensitive command output. | Accidental local retention of secrets, source, paths, or private PR data. | Shims are opt-in, local-only, capped per stream, imported explicitly, documented as sensitive, and not connected to hosted sync in this branch. |
| Hosted worker could accidentally become raw transcript sync before policy exists. | Private prompts, tool output, source snippets, or credentials could leave the machine. | Hosted sync endpoint rejects raw transcript/prompt/tool-output-like keys by default; initial hosted API accepts metadata batches and explicit blob uploads only. |
| Buildkite runner routing can block live proof even when the pipeline syntax is valid. | Matrix jobs can remain scheduled and prevent release evidence. | Build 25 proved pipeline expansion; Linux verification lanes were rerouted from idle `main-linux` to the known working managed Linux queue and runner class. |
| Hosted staging is not provisioned yet. | Hosted/team story cannot be claimed as live. | Read-only readiness proves operator credentials exist, but Work Recorder-specific Infisical keys, Worker, and R2 bucket config are missing; keep public docs local-first until provisioning is completed. |

## Accepted Risks

- Local Bazel proof is accepted as skipped in this environment because Bazel is
  not installed. CI-facing scripts still require Bazel when
  `CTX_REQUIRE_BAZEL=1`.
