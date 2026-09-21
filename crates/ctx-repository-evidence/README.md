# ctx-repository-evidence

Bounded repository discovery, Git certification, shell/tool interpretation,
identity reconciliation, outcome parsing, pull-request association and typed
abstention for ctx.

`CoreRepositoryEvidenceAdapter` evaluates retained `ctx-history-core` records.
Call `begin_source` at each independent source boundary, then `evaluate_record`
for each immutable event. The adapter bounds unresolved call/result joins;
source-wide copy and producer eligibility remain the caller's responsibility.
Callers with literal inputs can use `RepositoryEvidenceResolver::evaluate` or
the stateless `evaluate` function with `facts::NeutralRepositoryFacts`.

Evaluation derives evidence without modifying source repositories or making
network requests. Local Git certification preserves SHA-1/SHA-256 object domains,
linked and moved worktrees, route recertification, bounded probes and ambiguity.
Exact provider-bound outcomes can remain usable without a live local checkout;
other evidence requires the corresponding Git objects and certified route.

Codex outer-exec compatibility uses Oxc with a bounded, non-executing static
evaluator. It recovers only exact, straight-line tool calls with immutable
literal arguments and aliases. Input size and raw complexity are bounded before
parsing; AST depth, work, expanded values, bindings and calls are also bounded.
Dynamic control flow, mutation, ambiguous expressions and budget excess produce
abstention. No recovered command is executed.

Shared `GitObjectFormat` and `RepositoryFileInvocationKind` types are re-exported
from `ctx-attribution-model`. Repository certification evidence remains separate
from retained Core content and query results.

Exact JSON parsing uses `ctx-history-capture-model`'s shared parser, which
rejects duplicate decoded object keys at every depth and bounds total members.

Run the library tests from the workspace root:

```sh
scripts/bazelw test //crates/ctx-repository-evidence:unit_tests --config=ci
```

Tests use authored literals and disposable Git repositories. The Git-backed
suite currently uses Unix system Git paths; platform-specific coverage is
conditional.

The repository evaluator and exact-JSON helper originated in the Apache-2.0
public `ctxrs/ctx` source at commit
`12e27f410f3c6808622010d8c5399a15dec2b7e8`, in
`crates/ctx-history-repository-evidence` and
`crates/ctx-history-capture-model/src/exact_json.rs`, respectively.
