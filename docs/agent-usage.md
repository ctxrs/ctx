# Agent Usage

Agents should query ctx before repeating investigation work.

## Recommended Flow

1. Run `ctx status --json` to confirm the local store is readable.
2. Run `ctx sources --json` to see which local provider paths currently exist.
3. Search narrowly with provider, repository, file, or date filters.
4. Use `ctx show event --format json` for the best matching result before
   changing code.
5. Cite ctx material in notes or final answers when it influenced the work.

Example:

```bash
ctx search "sqlite migration failed" --repo ctx --json
ctx show event <ctx-event-id> --window 5 --format json
```

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

Agent harnesses should prefer JSON for routing and ranking:

```bash
ctx status --json
ctx sources --json
ctx search "release blocker" --json
ctx show event <ctx-event-id> --window 5 --format json
ctx show session <ctx-session-id> --mode lite --format json
```

Use cited search snippets and `show` output as retrieved material when the next
step is to brief another agent.
