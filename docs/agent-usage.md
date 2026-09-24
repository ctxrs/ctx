# Agent Usage

Agents should query ctx before repeating investigation work.

## Recommended Flow

1. Run `ctx status --format json` to confirm local Core/search state is readable.
2. Run `ctx sources --format json` to see which local provider paths currently exist.
3. Search narrowly with provider, workspace, file, or date filters.
4. Use `ctx show event` for the best matching result before changing code.
5. Use `ctx locate event` or `ctx locate session` when source identity matters.
6. Cite ctx material in notes or final answers when it influenced the work.

Example:

```bash
ctx search "schema migration failed" --workspace ctx
ctx show event <ctx-event-id> --window 5
ctx locate event <ctx-event-id>
```

Normal `ctx search` uses `--refresh background`, which serves the active Core
generation while default automatic indexing requests a persistent daemon Core
refresh. In manual indexing mode it serves only the last published generation
without starting or waking a worker. Semantic coverage remains disabled unless
explicitly enabled. Use `--refresh wait` when the task authorizes an
authoritative refresh, or `--refresh off` when it must never start or wake a
process.

Use `ctx index mode` to inspect the mode. `ctx index mode auto` enables
persistent background indexing; `ctx index mode manual` stops the persistent
daemon and removes its supervision while leaving explicit `ctx import` and
`ctx search --refresh wait` available through finite workers.

When one query is not enough, vary the wording, add `--term`, narrow by
workspace/provider/file/session, or use `--events`. Ordinary search already
covers primary and subagent work with root-diverse results; use
`--primary-only` only for a deliberately narrow search. Search windows are
bounded, so do not infer exact corpus-wide counts from the number of returned
hits.

Direct CLI searches automatically exclude the current session tree for Codex,
DeepSeek Harness, Grok Build, Pi, Claude Code, Goose, Hermes, Shelley, Qwen
Code, and Mux when the current session can be identified unambiguously.
Unsupported or ambiguous detection fails open: ctx leaves the history
included. `--include-current-session` restores the automatically excluded
tree. Repeat `--exclude-session <ctx-uuid-or-unambiguous-prefix>` to exclude
exact named sessions; the option is repeatable and conflicts with `--session`.
MCP searches do not automatically exclude the caller's session.

## History Research Reports

Use the agent skill as a read-only research workflow when the task is to brief a
human or another agent about prior work:

```text
Use ctx to research prior local agent sessions about <topic>. Run multiple
searches, inspect the strongest events or sessions, and return a concise report
with ctx citations. Do not edit files.
```

The agent writes the report from retrieved evidence; ctx does not synthesize
reports. A practical command sequence is:

```bash
ctx search "<topic>" --refresh off
ctx search "<topic variant>" --workspace <workspace> --refresh off
ctx search "<topic>" --term "<related term>" --term "<error text>" --refresh off
ctx search "<topic>" --session <ctx-session-id> --refresh off
ctx show event <ctx-event-id> --window 5
ctx show session <ctx-session-id>
ctx locate event <ctx-event-id>
ctx locate session <ctx-session-id>
```

Start with broad `ctx search` queries when the topic may span multiple sessions,
then narrow by workspace, provider, file, date, or session. The agent writes the
final report and must inspect cited events or sessions before making claims.

For a concise report, include the finding, the strongest ctx IDs, and gaps. For
a longer report, include the question, search method, findings or chronology,
evidence table, conflicts, and follow-up searches. Summarize private transcript
content instead of pasting raw JSON or large transcript excerpts.

## Deterministic Use

Treat Sift output as retrieved source material. Do not state that ctx inferred a
decision unless the cited text explicitly says so. If you synthesize a conclusion
from multiple retrieved snippets, say that the conclusion is your synthesis and
cite the snippets that support it.

## When To Re-Import

Run `ctx import --all` when:

- `ctx sources` shows supported provider history on this machine;
- a search misses something you know happened recently;
- the current task depends on a previous session from another provider;
- you have an explicit supported provider path to import.

Use `ctx import --resume --format json` as an idempotent-rescan marker. It is not a
guarantee that every provider has native cursor resume.

## JSON For Harnesses

Agents should prefer default text for reading search and show output.
JSON is for scripts, harnesses, `jq`, or exact field extraction; it is usually
much larger and consumes more context.

```bash
ctx status --format json
ctx sources --format json
ctx search "release blocker"
ctx search "release blocker" --format json | jq '.results[0].ctx_event_id'
ctx show event <ctx-event-id> --window 5 --format json
ctx show session <ctx-session-id> --format json
```

Show's positional IDs are ctx-owned. `provider_session_id` is provider-owned
metadata; for Codex it is the resume UUID. To open a Codex session by that UUID,
use `ctx show session --provider codex --provider-session <uuid>`.

Use cited search snippets and `show` output as retrieved material when the next
step is to brief another agent.
