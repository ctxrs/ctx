# Work Observability Release-Hardening Plan

## Context

Task: `feb64c1c-e58c-40f8-b1e9-1094dca0646e`

Canonical worktree:

`/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/agent-work-semantics-primary`

Canonical branch:

`ctx/agent-work-semantics-primary`

This plan starts after:

`eaf7c6e Record Work observability done-ness pass`

The previous pass implemented the P0 local Work observability slice:

- durable Work records, links, events, evidence, summaries, claims, and search
  docs;
- CLI JSON agent contract;
- daemon Work routes;
- Work Report web page;
- redaction/privacy defaults;
- evidence freshness/trust states;
- focused validation and done-ness review.

The user is now asking for release-candidate hardening, not merely feature
completion.

## Goal

Take the Work observability product as close to release-ready as possible in
this local branch.

"Cannot be improved any further" is not a literal stopping condition. Treat the
done condition as:

> The branch has passed adversarial security, SDLC, documentation, UX, e2e, and
> search/future-readiness review; remaining gaps are either fixed or explicitly
> accepted as release deferrals.

Do not stop with a final message until a final done-ness subagent returns PASS
for this hardening plan.

## Scope

In scope:

- security review and fixes;
- privacy/redaction review and fixes;
- SDLC/process/release-readiness review;
- docs/spec/examples review and fixes;
- adversarial UI review of Work Report and Work observability pages;
- Playwright/browser screenshots where practical;
- a realistic e2e sample run using an agent-style task:
  - build a small web ping pong game;
  - record Work;
  - create evidence;
  - generate context/report;
  - create a dummy PR in a separate private scratch repo if available;
  - attach or link the Work report/evidence/background document;
- search and future-querying review:
  - SQLite/FTS performance and shape;
  - future graph/vector options;
  - whether current model blocks future hosted/team search;
- final validation and status docs.

Out of scope unless explicitly needed for the above:

- production release;
- public announcement;
- hosted/team backend buildout;
- irreversible mutation of real product repos;
- broad infrastructure rewrites.

## Private Dummy PR Policy

The user explicitly asked for a dummy PR in a different private repo.

Preferred path:

1. Use an existing private scratch/test repo if one is obvious and safe.
2. If no suitable repo exists, create a private scratch repo with an explicit
   name like `ctx-work-observability-e2e-scratch`.
3. Make only disposable sample code changes.
4. Open a draft PR.
5. Link the PR to the Work record.
6. Add a PR comment or attached markdown file only if it does not leak secrets
   and the repo is definitely private.
7. Record URL, repo, branch, and cleanup instructions.

If auth/repo creation is unavailable, record that as a blocker/deferral and run
the same e2e locally without remote PR creation.

Do not use `ctxrs/ctx`, `ctx-private`, or `control-plane` as the dummy PR target
unless the user explicitly says to.

## Security Review

Run a dedicated adversarial security/privacy review subagent after implementation
inspection.

Required focus:

- daemon route auth and workspace scoping;
- cross-workspace leakage;
- raw transcript expansion;
- default redaction before search/context/report/export;
- artifact path traversal and symlink safety;
- arbitrary local-file serving risk;
- XSS/HTML injection in Work Report;
- secret leakage through CLI JSON, report markdown, PR comments, logs,
  screenshots, artifact names, command args, env vars, and summaries;
- provider-backed summaries remain disabled/deferred or explicitly safe;
- user-space shim evidence is labeled untrusted/bypassable;
- Work Report does not overclaim trust.

Required tests/fixes if gaps are found:

- redaction unit tests;
- cross-workspace route tests;
- artifact path tests;
- report escaping/rendering tests;
- CLI JSON secret fixture tests.

## SDLC / Process Review

Run a dedicated SDLC reviewer.

Required focus:

- branch status;
- commit hygiene;
- plan/status notes;
- exact validation evidence;
- resource-safe build/test usage;
- no accidental push/release;
- no untracked generated artifacts;
- no dirty worktrees in canonical branch;
- no broad build that risks machine stability unless justified.

## Documentation, Specs, Examples

Review and improve:

- root README Work observability claims;
- Work records docs;
- Work source-of-truth contract;
- Work namespace compatibility;
- data/privacy docs;
- CLI help/examples;
- API/route specs;
- agent usage guide:
  - how an agent should call `ctx work search --json`;
  - how an agent should use `ctx work context`;
  - how to link PRs/commits/evidence;
  - how to treat stale/missing evidence;
- example Work Report markdown;
- sample e2e walkthrough.

Docs must be honest about:

- local-only scope;
- no hosted sync yet;
- no provider-backed LLM summaries yet;
- no MCP tools yet, if still deferred;
- local shim capture is useful context, not proof.

## UI / UX Adversarial Review

Run a Work Report UI review using real rendered pages where practical.

Required checks:

- first viewport answers:
  - what is this Work?
  - what PR/commit/change does it relate to?
  - what is the trust verdict?
  - what evidence exists?
  - what is stale/missing?
  - what should reviewer do next?
- no text overlap;
- mobile and desktop rendering;
- dark/light mode if supported;
- empty/partial/failure states;
- stale evidence state;
- missing evidence state;
- untrusted local-capture state;
- long titles/paths/URLs;
- artifact/link rendering;
- accessible labels and readable contrast;
- raw transcript not shown by default.

Use Playwright screenshots if feasible. Store screenshot paths/artifact notes in
this plan directory.

## E2E Sample Run

Goal: prove the product with a concrete agent-style task.

Scenario:

> Build a small web ping pong game.

Requirements:

- Use a separate scratch/private repo, not the ctx repo.
- Run through ctx setup/capture where possible.
- Generate Work records for commands, evidence, summaries, and PR/commit links.
- Add evidence:
  - build/test command where available;
  - screenshot or artifact if feasible;
  - report/context generation.
- Create a dummy draft PR in a private repo if possible.
- Link the PR with `ctx work link-pr`.
- Create a Work Report and a markdown/background document.
- Attach/link the report/background to the PR if safe.
- Record exact commands, IDs, URLs, and cleanup notes.

If live agent invocation is not feasible, run the workflow with deterministic
commands and explicitly note that the e2e is capture/report validation, not live
agent-quality validation.

## Search / Graph / Future Querying Review

Run a dedicated architecture/search reviewer.

Required analysis:

- is SQLite + FTS enough for months of local history?
- what indexes are missing?
- are query filters sufficient for agents?
- does the model support future semantic/vector search?
- does the relational graph support future hosted/team sync?
- would a graph DB help now, or is it premature?
- what should be benchmarked before any graph/vector migration?

Expected answer:

- keep SQLite/FTS unless evidence shows it is insufficient;
- add benchmark/seed fixtures if cheap;
- write future migration notes rather than adding graph DB now.

## Validation

Use resource-safe tiers.

Always run:

- `git diff --check`;
- formatting for touched packages;
- focused Rust tests for touched crates;
- focused web tests for touched UI/API;
- web typecheck/lint/build if web touched;
- docs/spec checks if available.

Run when safe:

- broader focused Rust package checks through `scripts/dev/cargo-safe.sh`;
- Playwright screenshot checks for Work Report;
- e2e sample script.

Avoid unless explicitly justified:

- broad Rust workspace tests;
- broad Bazel/Buildkite sweeps;
- desktop package build;
- release build.

## Subagent Program

Implementation/review subagents:

- security/privacy reviewer;
- SDLC/process reviewer;
- docs/spec/examples reviewer;
- Work Report UI reviewer;
- e2e sample-run worker;
- search/future-querying reviewer;
- test-coverage reviewer;
- final done-ness reviewer.

Use implementation workers only for clearly bounded fixes.

## Final Done Criteria

Do not send final done until all are true:

- security/privacy reviewer returns PASS or all findings fixed;
- SDLC reviewer returns PASS;
- docs/spec/examples review returns PASS;
- Work Report UI review returns PASS or accepted deferrals are explicit;
- e2e sample run completed or blocker is explicit and justified;
- search/future-querying review completed with recommendations;
- all focused validation passes;
- final status file records:
  - head commit;
  - validation commands/results;
  - review agent IDs and verdicts;
  - e2e sample artifacts/URLs/cleanup notes;
  - accepted deferrals;
- final done-ness subagent returns PASS;
- worktree is clean.

