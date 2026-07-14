# Agent Usage

Agents should query ctx before repeating investigation work.

## Recommended Flow

1. Run `ctx status --json` to confirm the local store is readable.
2. Run `ctx sources --json` to see which local provider paths currently exist.
3. Search narrowly with provider, workspace, file, or date filters.
4. Use `ctx show event` for the best matching result before changing code.
5. Cite ctx material in notes or final answers when it influenced the work.

Example:

```bash
ctx search "sqlite migration failed" --workspace ctx
ctx show event <ctx-event-id> --window 5
```

Normal `ctx search` uses `--refresh background`, which serves existing indexes
while daemon maintenance refreshes history and semantic coverage when enabled.
Rerun the same search with `--refresh off` when the task requires a strictly
read-only query over the existing index.

Use `ctx sql` only when normal search does not express the question, such as
exact counts, joins, audits, or scripting over stable `ctx_*` views. It is
read-only and does not refresh or import provider history. See
`ctx docs show sql` for stable view schemas and examples.

When ctx runs inside Codex and `CODEX_THREAD_ID` is available, search excludes
the active Codex session tree by default to avoid returning the current prompt
or its subagent work as the top match. Use `--include-current-session` only when
the active session tree is itself the target.

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
```

Start with broad `ctx search` queries when the topic may span multiple sessions,
then narrow by workspace, provider, file, date, or session. The agent writes the
final report and must inspect cited events or sessions before making claims.

For a concise report, include the finding, the strongest ctx IDs, and gaps. For
a longer report, include the question, search method, findings or chronology,
evidence table, conflicts, and follow-up searches. Summarize private transcript
content instead of pasting raw JSON or large transcript excerpts.

## Deterministic Use

Treat ctx output as retrieved source material. Do not state that ctx inferred a
decision unless the cited text explicitly says so. If you synthesize a conclusion
from multiple retrieved snippets, say that the conclusion is your synthesis and
cite the snippets that support it.

## When To Re-Import

Run `ctx import --all` when:

- `ctx sources` shows supported provider history on this machine;
- a search misses something you know happened recently;
- the current task depends on a previous session from another provider;
- you have an explicit supported provider path to import.

Use `ctx import --resume --json` as an idempotent-rescan marker. It is not a
guarantee that every provider has native cursor resume.

## JSON For Harnesses

Text or Markdown returned by search and show, plus SQL table output, is
automatically wrapped in one untrusted-history boundary. Retrieved historical
content is unchanged. Locate does not currently support the boundary.
JSON is for scripts, harnesses, `jq`, or exact field extraction; it is usually
much larger and consumes more context.

```bash
ctx status --json
ctx sources --json
ctx search "release blocker"
ctx search "release blocker" --json | jq '.results[0].ctx_event_id'
ctx show event <ctx-event-id> --window 5 --format json
ctx show session <ctx-session-id> --format json
```

Use cited search snippets and `show` output as retrieved material when the next
step is to brief another agent.
