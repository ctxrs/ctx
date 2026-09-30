# Graph, Sift, Server and remote observations

These additive v1 observations use the existing consent, random profile/root
identity, outbox, HTTP collector and durable Queue. They contain closed typed
facts only. Engine callbacks contain exact local measurements; the public
serializer buckets them **before** building identity-bearing batches. Local
aggregate-state serde is not a wire serializer. No API accepts a caller's
arbitrary JSON properties.

| Event family | Surface / operation | Unit |
| --- | --- | --- |
| `operation_completed` | CLI or MCP / `graph` | One terminal Graph operation |
| `operation_completed` | CLI or MCP / `remote` | One remote client terminal |
| `operation_completed` | server / `server_request` | Explicit bounded terminal; normal server collection uses summaries |
| `runtime_observation` | CLI or MCP / `sift_summary` | One fixed operation/host/mode/entry/outcome cohort window |
| `runtime_observation` | server / `server_summary` | One operation and request/read/execution/upload/publication/index cohort window |
| `runtime_observation` | daemon / `sharing_summary` | One sharing operation/phase/outcome cohort window |
| `runtime_observation` | server, MCP or CLI / `product_runtime` | Server, Graph serve or Graph watch lifecycle sample |

Closed operation vocabularies live in the Rust `analytics/engines` module and
Worker `product-operation-contract.ts`; grouped optional diagnostics are admitted
by `product-fact-contract.ts` and `product-window-contract.ts`. Unknown keys or
values, raw numbers, incomplete required groups, and provably contradictory
bucket ranges are rejected both at HTTP admission and Queue revalidation.
Published fixtures are shared by Rust serializer and Worker tests, including
paired measured values, sample denominators, expansion and zero-input cases.
Hosted terminals also include archive verification; remote pause, resume, status
and removal; and the finite server collection, user, credential, publication and
status commands. They retain the established operation/output failure tuples. Old v1
terminals and hosted failure tuples remain accepted without new sidecars.
Deploy a receiver accepting these additions before releasing a new producer;
an older receiver rejects unknown operations/properties. Permanent rejection
can drop a queued optional event, so producer-first rollout is not compatible.

Graph records preserve nested global/provider/cache actions, native tools and
resource entries, protocol operations and known parse failures as closed enums.
Optional groups retain bounds and path disposition, parsed/rejected/unchanged/
deleted file counts, index disposition and commit phases, cache/artifact results,
algorithm convergence, and provider reservations versus actual receipts. Token
usage is a known subtotal **plus reporting receipt count for each category**;
missing reporters do not become zero-usage requests. Costs and estimated JSON
tokens are not reported as measured usage or savings. Operation completion must
be observed before constructing a completion; an unfinished library call is not
an execution success.

Sift summaries preserve entry (including MCP), terminal kind, fail-open versus
failure, child exit, delivery, skip and measurement-missingness cohorts. A
successful engine result does not prove host application or model consumption.
`flushed` means the writer accepted output; `unchanged` is distinct. Parents
aggregate actual comparable, presented input/output pairs, count their sample
population, and retain complete/partial/unmeasured invocation denominators.
Where a call has several streams, first sum the comparable pairs for that call
and count it once in that measurement's sample denominator. Do not treat an
unmeasured second stream as zero, nor mix protocol requests and sessions into
one invocation population. Stream reasons that disagree remain absent rather
than selecting an arbitrary reason.

`PresentedTotals` records actual input/output totals over the same subset. The
wire contains their count/byte buckets, signed change category, absolute delta
bucket and the fraction reduced computed from **unbucketed aggregate totals**.
Output expansion remains `increased`; zero input omits the fraction. Semantic
provider usage reports current-request measured tokens and sample denominators;
cache hits do not replay historical provider usage, and candidate selection
sizes are not delivered savings. Bucket centers must never be summed to invent
token totals, dollar savings or weighted fractions. Optional collection has no
exactly-once invocation-accounting guarantee.

Server summaries count requests separately from work execution and reads.
Only source-correlated optional execution/read facts belong in a request
summary; independent callbacks have separate populations. Handoff complete,
failed and unknown partition the request count. Returning an HTTP response is
not handoff; yielding a body is not proof of peer receipt. Read and indexing
measurements retain sample counts for coverage lag, completeness, exhaustive
reads, snippets, progress and availability. Boolean `MeasuredTotal.total` means
true samples and may not exceed its measured samples. Capped backlog readings
must not be serialized as if the cap were an exact count; omit them until a
bound-aware wire field exists. Sharing summaries retain validated acceptance,
recovered receipt, retry and selection facts without destinations or identities.

Fine durations are separate from the unchanged envelope buckets, in order:
`lt_1ms`, `1ms-5ms`, `5ms-10ms`, `10ms-25ms`, `25ms-50ms`, `50ms-100ms`,
`100ms-250ms`, `250ms-1s`, `1s-5s`, `5s-30s`, `30s-2m`, `2m-10m`,
`10m-1h`, `1h+`. Boundaries are lower-inclusive and upper-exclusive. Summary
latency arrays contain exact local sample counts in that order; every wire bin
and its measured denominator are count-bucketed. The window duration is not a
latency percentile. Missing arrays or measurements mean unknown, not zero.

Short-lived Sift hooks accumulate bounded fixed cohorts without per-hook
network, spool or fsync. Optional summaries enter the existing outbox through a
non-evicting quota; they cannot displace history events. Local counter files hold
at most 32 cohorts and 128 KiB. An immutable queued summary is at most 128 KiB,
with at most four globally and one per root/endpoint. Ordinary events may evict
optional summaries first. Checked merges mark collection limits on observed
overflow or cohort-cap loss. Existing history-daemon
drains remain supported. A running Server/Graph runtime may perform bounded
outbox drains, and standalone clients may request a throttled, outbox-only
one-shot sender (at least 60 seconds between launches). This is not permission
to start the history daemon or a persistent telemetry service. Foreground
commands and hooks do not wait for telemetry network I/O. Canonical owner and
consent are rechecked before collection/upload; Sift's local record-usage
setting does not determine global analytics consent. Loss and unmeasured work
are explicit; optional counters are not a journal.

The restricted SQL functions in migration `0054` expose at most 24 hours of
canonical observed telemetry, with Search baseline, closed cohorts, denominator
and bucket counts. Summary receipt counts are not invocation counts. Profiles
and roots are observed identity grains, never physical machines or people.
Staging synthetic qualification is explicitly separate from production public
traffic; effective exclusions still apply. Runtime measurements use the latest
ready/liveness sample per observed profile/root/kind in the requested window.
Late commits can change a repeated read; the functions claim no completeness
watermark, exact user totals, or adoption outside the observed population.
