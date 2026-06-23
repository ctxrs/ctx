# Work Recorder Productization Risk Register

Updated: 2026-06-22T22:11:53-05:00

| Risk | Impact | Current Mitigation |
| --- | --- | --- |
| Scope is large enough to span public local product, private hosted staging, CI/release, and dogfood. | High schedule and integration risk. | Milestone gates and status files will track concrete blockers instead of vague deferrals. |
| Private repo canonical checkout is dirty with unrelated work. | Risk of overwriting unrelated user/agent changes. | Use a separate manual `ctx-private` worktree before edits. |
| Broad Rust/Bazel/build verification can overload this host. | Machine instability and false failures. | Use existing resource-safe wrappers and avoid overlapping heavy jobs. |
| Dashboard can pass tests but remain visually sparse. | Product-quality failure. | Require screenshot generation, manual inspection, and adversarial UI review. |
| Hosted staging credentials or runner access may be unavailable. | External blocker for completion criteria. | Staging Neon/R2/Worker credentials are now proven. Private Buildkite dry-run parses with the Infisical agent token; remote Buildkite proof still needs a Buildkite API token or active Buildkite job context. |
| README/docs currently overclaim implemented behavior. | User confusion and false product promises. | Docs truth-pass worker is scoped to README/docs only. |
| Existing local store shape diverges from the product contract. | Capture/search/hosted sync churn if built on the wrong schema. | Land core schema/types and versioned store migrations before capture/search/dashboard work. |
| Buildkite/release platform matrix requires live runner proof. | Repo-owned config can be locally validated, but the release gate still needs real Buildkite green evidence. | Public pipeline now has a local contract check, native dry-run lanes for known Linux/macOS/Windows queues, and explicit blocker evidence for FreeBSD. |
| No FreeBSD x86_64 Buildkite queue is documented. | Native FreeBSD release artifact proof cannot pass yet. | Public pipeline emits a FreeBSD blocker artifact with missing runner label, attempted native command, proposed `queue=freebsd-x64` pool, and artifact status. |
| `/tmp` pressure and concurrent broad Cargo checks can freeze this host. | Local verification instability and interrupted agent work. | Use `TMPDIR=/var/tmp/ctxwr`, low `CARGO_BUILD_JOBS`, low `RUST_TEST_THREADS`, and avoid overlapping broad Cargo commands across agents. |
| Bazel is not installed in this environment. | Local `scripts/check.sh all` cannot prove Bazel lanes unless explicitly requested. | The script records the Bazel lane as skipped locally; the Buildkite Bazel lane sets `CTX_REQUIRE_BAZEL=1`, bootstraps pinned Bazelisk under `target/tool-cache`, bootstraps Rust before invoking Bazel, and forwards Cargo/Rust paths into `//:cargo_tests`. |
| Archive artifact payloads are string-only. | Future binary screenshots/reports cannot be faithfully exported through the current JSON artifact payload field. | Current foundation scope uses text stdout/stderr artifacts only; non-text artifact export should use an explicit encoded/binary-safe payload design before binary artifacts are added. |
| Chrome/headless screenshot capture can fail if it uses the default `/tmp` profile/cache. | Visual review can fail for environment reasons unrelated to dashboard rendering. | Use `/var/tmp` for `TMPDIR`, `--user-data-dir`, and `--disk-cache-dir` when capturing local dashboard screenshots on this host. |
| Local Git/jj/gh wrapper shims can capture sensitive command output. | Accidental local retention of secrets, source, paths, or private PR data. | Shims are opt-in, local-only, capped per stream, imported explicitly, documented as sensitive, and not connected to hosted sync in this branch. |
| Hosted worker could accidentally become raw transcript sync before policy exists. | Private prompts, tool output, source snippets, or credentials could leave the machine. | Hosted sync endpoint rejects raw transcript/prompt/tool-output-like keys by default; initial hosted API accepts metadata batches and explicit blob uploads only. |
| Buildkite runner routing/tooling can block live proof even when the pipeline syntax is valid. | Matrix jobs can remain scheduled or fail before product checks, preventing release evidence. | Public builds 24-27 proved pipeline expansion, routing, and missing-Cargo host issues. Public scripts now bootstrap Rust tools through a locked no-op-first rustup path; the next public Buildkite run must prove it. |
| Production hosted endpoint is not deployed. | Team sync cannot be claimed production-ready. | Staging Worker, Neon role/migration, Infisical secrets, R2 buckets, readiness, and live smoke are proven; production route deployment remains deliberately uncut. |

## Accepted Risks

- Local Bazel proof is accepted as skipped in normal optional mode. CI-facing
  scripts require Bazel with `CTX_REQUIRE_BAZEL=1`, which bootstraps Bazelisk
  into repo-owned `target/tool-cache` paths when Bazel/Bazelisk is absent and
  carries the repo-owned Rust toolchain environment into the Bazel test sandbox.
