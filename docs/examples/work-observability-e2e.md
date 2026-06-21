# Work Observability E2E Walkthrough

This walkthrough validates the local Work record loop with a disposable repo.
Use an isolated data root so the sample does not pollute normal ctx state.

```bash
export CTX_DATA_ROOT=/tmp/ctx-work-observability-e2e-data
mkdir -p /tmp/ctx-work-observability-e2e
cd /tmp/ctx-work-observability-e2e
git init
```

Create a tiny project, for example a static ping pong game with `index.html`,
`styles.css`, `game.js`, and a dependency-free `test.mjs`.

```bash
ctx setup workspace --data-dir "$CTX_DATA_ROOT" .
git checkout -b e2e/ping-pong-game
git add .
git commit -m "Add sample ping pong game"

ctx work capture command --data-dir "$CTX_DATA_ROOT" --tool git --cwd "$PWD" \
  --exit-code 0 -- commit -m "Add sample ping pong game"
```

Use the `work: <work-id>` line printed by capture:

```bash
WORK_ID=<work-id>

ctx work evidence --data-dir "$CTX_DATA_ROOT" "$WORK_ID" run --kind test \
  --cwd "$PWD" -- node test.mjs
ctx work evidence --data-dir "$CTX_DATA_ROOT" "$WORK_ID" run --kind build \
  --cwd "$PWD" -- node --check game.js
ctx work summarize --data-dir "$CTX_DATA_ROOT" "$WORK_ID" --kind report
ctx work context --data-dir "$CTX_DATA_ROOT" "$WORK_ID" --json > work-context.json
ctx work report --data-dir "$CTX_DATA_ROOT" "$WORK_ID" --markdown > work-report.md
ctx work evidence --data-dir "$CTX_DATA_ROOT" "$WORK_ID" freshness --cwd "$PWD" --json
```

If the Work came from an ADE session, backfill durable session state before
opening the Inspector:

```bash
ctx work project --data-dir "$CTX_DATA_ROOT" session <session-id> --json
```

Serve the local desktop UI and open the tabbed Work Inspector:

```bash
CTX_WEB_DIST=/path/to/ctx/core/apps/web/dist \
  ctx serve --data-dir "$CTX_DATA_ROOT" --bind 127.0.0.1:4401

# In the browser:
# http://127.0.0.1:4401/workspaces/<workspace-id>/work/$WORK_ID
```

The Inspector shows overview, transcript, commands, evidence, timeline, changes,
artifacts, context, and whitelist redacted JSON. Default surfaces omit raw
transcripts, raw command output, host roots, and raw artifact paths.

If a disposable private remote is available, push a branch and link the draft PR:

```bash
git push -u origin e2e/ping-pong-game
PR_URL=$(gh pr create --draft --title "E2E sample: ping pong game" \
  --body "Disposable ctx Work observability e2e sample." --json url -q .url)
ctx work link-pr --data-dir "$CTX_DATA_ROOT" --cwd "$PWD" "$PR_URL" \
  --title "E2E sample: ping pong game" --state draft
```

After linking a PR, you can find the PR-linked record again with:

```bash
ctx work search --data-dir "$CTX_DATA_ROOT" --pr "$PR_URL" --json
```

Review `work-report.md` before posting it anywhere. Even local redacted reports
can contain project names or command output. Hosted sync, MCP tools, and
provider-backed LLM summaries are not part of this local slice.

Cleanup:

```bash
rm -rf "$CTX_DATA_ROOT" /tmp/ctx-work-observability-e2e
```
