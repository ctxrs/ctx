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
ctx work list --data-dir "$CTX_DATA_ROOT" --json
```

Use the listed `work_id`:

```bash
WORK_ID=<work-id>

ctx work evidence "$WORK_ID" run --data-dir "$CTX_DATA_ROOT" --kind test \
  --cwd "$PWD" -- node test.mjs
ctx work evidence "$WORK_ID" run --data-dir "$CTX_DATA_ROOT" --kind build \
  --cwd "$PWD" -- node --check game.js
ctx work summarize "$WORK_ID" --data-dir "$CTX_DATA_ROOT" --kind report
ctx work context "$WORK_ID" --data-dir "$CTX_DATA_ROOT" --json > work-context.json
ctx work report "$WORK_ID" --data-dir "$CTX_DATA_ROOT" --markdown > work-report.md
ctx work evidence "$WORK_ID" freshness --data-dir "$CTX_DATA_ROOT" --cwd "$PWD" --json
```

If a disposable private remote is available, push a branch and link the draft PR:

```bash
git push -u origin e2e/ping-pong-game
PR_URL=$(gh pr create --draft --title "E2E sample: ping pong game" \
  --body "Disposable ctx Work observability e2e sample." --json url -q .url)
ctx work link-pr --data-dir "$CTX_DATA_ROOT" --cwd "$PWD" "$PR_URL" \
  --title "E2E sample: ping pong game" --state draft
```

Review `work-report.md` before posting it anywhere. Even local redacted reports
can contain project names or command output. Hosted sync, MCP tools, and
provider-backed LLM summaries are not part of this local slice.

Cleanup:

```bash
rm -rf "$CTX_DATA_ROOT" /tmp/ctx-work-observability-e2e
```
