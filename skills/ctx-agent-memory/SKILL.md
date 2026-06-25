# ctx Agent Memory

Use this skill when an agent should consult local ctx history before acting.

## Goal

Recover relevant prior sessions, decisions, failed attempts, commands, and file
context from ctx search. Treat ctx output as cited retrieval material, not as a
generated conclusion.

## Workflow

1. Check health:

   ```bash
   ctx status --json
   ```

2. Refresh indexes when the task depends on recent local history:

   ```bash
   ctx sources --json
   ctx import --all --json
   ```

   Use `ctx import --resume --json` only as an idempotent rescan marker; it is
   not a guarantee that every provider has native cursor resume.

3. Search with tight filters whenever possible:

   ```bash
   ctx search "<query>" --provider codex --json
   ctx search "<query>" --repo <repo> --json
   ctx search "<query>" --file <path> --json
   ctx search "<query>" --since 30d --json
   ```

4. Inspect the best cited result:

   ```bash
   ctx search "<query>" --json
   ctx show <item-uuid> --json
   ```

5. Cite ctx material when it affects your answer or implementation. Include the
   provider, session ID, event ID or sequence, and source path/cursor when ctx
   provides them.

## Rules

- Do not state that ctx wrote model analysis.
- Do not say ctx inferred a decision unless the cited text explicitly states
  that decision.
- If you synthesize across multiple snippets, label it as your synthesis and
  cite the supporting snippets.
- Prefer JSON for programmatic ranking and cited result inspection.
- Treat `~/.ctx`, provider transcript paths, and JSON output as private local
  history unless the user explicitly asks to share reviewed excerpts.
- If a source citation is stale or unavailable, say that ctx returned indexed
  text but the raw source could not be opened.
