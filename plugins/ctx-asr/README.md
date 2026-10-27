# ctx-asr for Claude Code

Record a private, metadata-only [Agent Session Record](https://agentsessionrecord.org)
(ASR 0.1) stream while Claude Code runs. This optional plugin observes documented
hooks; it does not read transcripts, change tool decisions, or add model context.
It requires Python 3.10+ on a POSIX host and was exercised with Claude Code 2.1.305
on Linux with Python 3.12. Windows is not supported by this implementation.

```sh
claude --plugin-dir /absolute/path/to/ctx/plugins/ctx-asr
```

Claude supplies `CLAUDE_PLUGIN_DATA`. Output defaults to its `asr` subdirectory.
For a specific destination, set an absolute path before launching Claude:

```sh
CTX_ASR_DIR="$HOME/.local/state/ctx-asr" \
  claude --plugin-dir /absolute/path/to/ctx/plugins/ctx-asr
```

The output directory is created with mode 0700. Existing output directories must
be owned by the current user and have no group/other access. Output files use
0600. Unsupported hosts, unavailable Python, unsafe output permissions, invalid
payloads and storage failures cause capture to be skipped. Hooks produce no
stdout, stderr or decisions and exit successfully when the observer can run.
Claude's `onFailure: continue` also covers failures to launch the process.

There is one `<native-session-id>.asr.jsonl` per session. Subagent tool callbacks
use `agent_id` as their session ID, while start/stop announcements live in the
parent stream. A resumed session appends to its existing stream when its index
is intact. The header declares `producer.method: observer`,
`producer.source.format: claude-code-hooks`, and all known losses.
The containing session ID does not establish the direct spawner of a nested
child or internal agent, so no header lineage edge is invented from it.

## Capture and privacy

The plugin deliberately records event existence, native correlation IDs, known
built-in tool names, status enums, durations when supplied, and changes of
working directory using redacted root aliases. It omits arbitrary prompt and
response text, tool arguments/results, error details, notification text, paths,
custom tool/agent names, URLs, schemas, configuration bodies and unknown fields.
There is no full-content mode: unmarked secrets can occur in any of those
surfaces, so pattern redaction cannot guarantee their exclusion.

`capture` states distinguish omitted hook content from model-facing content that
the hook does not supply. An observed prompt submission does not prove human
authorship or eventual model delivery. A successful tool callback does not prove
an approval was requested or granted. Metadata remains private and can reveal
activity; these files are not automatically safe to publish.

Path equality is tracked with a per-stream random HMAC key in the private state
file. The stream contains only root aliases and `path: null` declarations. Do
not share the state file or its key; they are not part of ASR. The plugin does
not store plain paths or unkeyed content/path hashes.

## Hook mapping

All registered events emit a `hook` record, except nonfinal display batches,
which are folded away. The following additional records capture the evidence
available at each boundary. The table covers all 33 documented events.

| Claude event | Additional ASR evidence |
| --- | --- |
| `SessionStart` | Session header on first observation; `state: resumed` only for native resume |
| `SessionEnd` | `state: ended` |
| `Setup` | Hook occurrence |
| `UserPromptSubmit` | User-role message, model content absent, submitted text omitted |
| `UserPromptExpansion` | Hook occurrence; expansion content omitted |
| `PreToolUse` | Tool call with native `tool_use_id`; input omitted |
| `PostToolUse` | Completed result, native call link; raw output omitted; completed/async Agent result can update subagent status |
| `PostToolUseFailure` | Failed/interrupted result; error text omitted |
| `PostToolBatch` | Batch boundary; model-facing serialized results omitted |
| `PermissionRequest` | Pending request; derived ID; no invented call link or displayed options |
| `PermissionDenied` | Auto-mode rejection, linked to native tool ID when supplied |
| `Notification` | Harness notice with text omitted |
| `MessageDisplay` | One display-only assistant message on the final batch |
| `SubagentStart` | Running child announcement |
| `SubagentStop` | Child status `stop_requested` (vendor value), not completion |
| `TaskCreated`, `TaskCompleted`, `TeammateIdle` | Hook occurrence; task/team payloads omitted |
| `Stop` | Stop callback only; another hook may continue the turn |
| `StopFailure` | Error state; error details omitted |
| `InstructionsLoaded`, `ConfigChange` | Hook occurrence; arbitrary instructions/settings omitted |
| `CwdChanged` | Redacted root and working-directory change |
| `DirectoryAdded` | Redacted additional root |
| `PreCompact` | Proposed compaction boundary only |
| `PostCompact` | Completed summary compaction; unknown replaced range; summary omitted |
| `PreModelSwitch`, `PostModelSwitch` | Hook occurrence; model payload omitted |
| `Elicitation`, `ElicitationResult` | Input request and status revision where native request ID exists; no answers, URL or schema |
| `WorktreeCreate`, `WorktreeRemove` | Not registered: these participate in creating/removing worktrees and are not passive audit callbacks |
| `FileChanged` | Not registered: installing file watch lists would change behavior |

Recognized common `permission_mode` and effort levels produce configuration
snapshots on change. Common `cwd` also detects directory changes between
callbacks: in a tested `-p` run, Bash changed directory without a `CwdChanged`
callback. Callback execution order may differ from source event order when
asynchronous; `seq` means observer append order. `observedAt` is observer time,
not a fabricated native `ts`. Working-directory observations can therefore lag
or reorder. Enter/ExitWorktree tool boundaries are visible, but the plugin does
not claim a complete worktree lifecycle.

Hooks do not provide general per-inference reasoning, usage, costs, request
context or response IDs. Other losses include native transcript record UUIDs,
branch/copy history, file diffs, full assistant-message coverage, exact native
timestamps, and most permission decisions. `PermissionRequest` has no
`tool_use_id` or request ID; its suggestions are not a record of displayed
options. `PermissionDenied` covers auto-mode only. A tested manual noninteractive
refusal exposed a request but no decision hook; `dontAsk` exposed neither.
`PostToolUse.tool_response` is not necessarily the model-facing tool result.

## Native transcript associations

Each projected record carries `_meta["rs.ctx.claude-asr/source"]` with the event,
native session ID and supplied prompt/tool/agent/display IDs. The optional
`rs.ctx.claude-asr/hooks` extension documents this metadata and its vendor
values. Generic ASR readers can ignore it; the reference validator reports it
as an unsupported optional extension, separately from schema/semantic errors.

Record IDs and derived entity IDs are capture-scoped (`idScope: capture`). They
derive from observer positions and are stable while this stream is appended;
they must not be compared across independent captures. Native tool and prompt
IDs remain verbatim. `MessageDisplay.message_id` is a display ID explicitly
unrelated to native transcript message IDs, so it is never used as `messageId`.

After a session, explicitly associate native UUIDs offline:

```sh
python3 plugins/ctx-asr/scripts/correlate.py \
  --stream /private/output/SESSION.asr.jsonl \
  --transcript /private/native/SESSION.jsonl \
  --out /private/output/SESSION.links.jsonl
```

`--transcript` may be repeated. The output is an ordinary JSONL association map,
not an ASR stream, and must be a new file. Only exact session/agent/prompt/tool
keys are used; multiple candidates remain ambiguous. Other records remain
unresolved. A unique UUID means association with that source event, not content
equivalence or proof of a copy. The observer stream is unchanged. A future
converter can consume this map; automatic merging with `ctx export asr`
is not implemented here.

## Latency and failure behavior

27 hooks run asynchronously. `Stop`, `SubagentStop` and `SessionEnd` run
synchronously so `claude -p` does not kill final capture on exit. Each process
sets a 75 ms wall-clock alarm before loading the projection and storage modules.
The hook definition also has a one-second timeout. The alarm excludes interpreter
startup and cannot guarantee a deadline while the OS deschedules a process or
blocks it in uninterruptible I/O. This is a bounded best-effort observer, not a
durable audit log. Use local storage.

The per-session lock never waits: concurrent writers can drop events. Oversized
input (>1 MiB), malformed input and unsafe file types are skipped. The stream
has a 16 MiB limit (plus a terminal gap record), and the private index a 512 KiB
limit; reaching either seals capture with an explicit loss record. There is no
fsync, retry, background daemon or delivery guarantee. Missing callbacks,
timeouts, killed processes and disk failures may prevent even a gap record.

The `.state.json` index is replaced atomically after each stream append. A crash
between those writes, partial write, or manual modification can leave a size
mismatch; subsequent capture stops silently rather than changing existing
records or guessing IDs. Preserve the valid prefix and select a new
`CTX_ASR_DIR` for a new capture. Do not remove only the index of a nonempty
stream. Delete the stream and sidecars together when disposing of a capture.

Measured with Python 3.12, local Linux storage, 100 calls per event after five
warmups, including process startup, JSON parsing, path tracking and writes:

| Event | Median | p95 | Maximum |
| --- | ---: | ---: | ---: |
| UserPromptSubmit | 14.24 ms | 15.27 ms | 15.98 ms |
| PreToolUse | 14.24 ms | 15.46 ms | 15.98 ms |
| PostToolUse (8 KiB output) | 14.35 ms | 15.30 ms | 16.26 ms |
| Stop | 14.43 ms | 15.56 ms | 17.04 ms |
| SessionEnd | 14.29 ms | 15.09 ms | 15.42 ms |

All 525 calls, including warmups, were captured (949 records, 310,712 stream
bytes, 327 index bytes). These are process completion times: approximately the
work added by a synchronous hook, excluding Claude's dispatch overhead. They
are not measurements of the asynchronous caller's waiting time. Claude does
not await asynchronous completion; process launch still has a cost. Network
model latency is not included in this benchmark.

## Validation

```sh
scripts/bazelw test //plugins/ctx-asr:observer_tests --config=dev-linux
claude plugin validate plugins/ctx-asr --strict --json
python3 plugins/ctx-asr/scripts/benchmark.py --iterations 100
node /path/to/agent-session-record/tools/validator/bin/asr-validate.js \
  /private/output/SESSION.asr.jsonl --format json
```

Tests use authored inputs, including secret sentinels, malformed payloads,
lock contention, unsafe files, incomplete indexes, limits, child sessions,
revision chains and ambiguous UUID associations. Separate native validation
used isolated noninteractive scratch sessions for text, tools/directory changes,
an intentional tool failure, and a denied permission request. All three emitted
valid ASR. Subagents, compaction and the remaining hooks have authored protocol
coverage; they were not all exercised through the live CLI.

Protocol sources: [hooks](https://code.claude.com/docs/en/hooks) and
[plugins reference](https://code.claude.com/docs/en/plugins-reference), checked
against installed Claude Code 2.1.305. This plugin claims that tested scope,
not complete capture of Claude Code or a general ASR conformance certification.
