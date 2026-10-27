# Copilot scheduler journal

This is a deterministically sanitized native capture from GitHub Copilot CLI
1.0.104, driven through Copilot SDK 1.0.27 on Linux x86-64. It exercises an idle
one-shot schedule, its native prompt delivery, an explicit abort, and shutdown.
No successful model response is claimed. The SDK used a loopback provider with
no listening server; the native scheduler fired before the request was aborted.

To reproduce with those pinned packages and a disposable home, create an SDK
session, invoke `session.rpc.commands.invoke({name: "after", input: "10s authored
scheduling capture"})`, wait twelve seconds, then call `session.abort()` and
`client.stop()`. The evidence is the harness-written `events.jsonl` under the
isolated Copilot home, not the SDK event callback log. No login is required.

`sanitize.py SOURCE DESTINATION --private-prefix PREFIX` replaces UUIDs through
one stable mapping, substitutes the disposable workspace prefix, shifts recorded
timestamps by one constant, and replaces the unrelated system prompt template
with authored text. It preserves row order, parent references, native turn IDs,
schedule IDs/cadence, source labels, outcomes, and display/model distinctions.
This fixture does not claim literal prompt fidelity, native byte offsets, or
file lengths. Repeated sanitization produces identical bytes.

The source records `source: "schedule-1"`, `turnId: "0"`, and `delivery: "idle"`
on the injected user message. ASR should emit exactly one schedule trigger on
that turn's first record, retain `intervalMs: 10000`, and identify its author as
the harness. `session.schedule_created` alone is not a firing.

The Rust cases in `crates/ctx-history-asr/src/tests/journals*.rs` are separately
**authored semantic tests**, not captures of native releases.
