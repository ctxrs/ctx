# Troubleshooting

Use this page when the local ctx recording flow looks incomplete or when a
provider claim does not match what the branch can actually prove.

## First checks

Start with the local health commands:

```bash
ctx status
ctx doctor
ctx validate
```

Use them to confirm:

- the data root exists;
- the SQLite store is initialized;
- the shim directory exists;
- the shim directory is active on `PATH` when you expect passive capture;
- the capture spool does not have stuck or failed files.

## Shim activation problems

If Git, jj, or gh activity is not showing up after capture import, check the
shim path first:

```bash
ctx shim env --dir ~/.ctx/shims
ctx status
```

Common causes:

- the shim directory is installed but not active on `PATH`;
- a custom shell rc block was removed or overwritten;
- commands are running in a shell that never sourced the activation block.

To reapply setup without changing shell startup files:

```bash
ctx setup --no-shell-update
```

To remove and reinstall the local shims:

```bash
ctx shim uninstall --dir ~/.ctx/shims
ctx setup
```

## Capture spool failures

If `ctx doctor` reports failed or stuck capture files:

```bash
ctx doctor
ctx repair --json
ctx validate
```

Successful repair moves the retried content into the normal store. Failed files
remain available for inspection with their `.error.json` sidecars.

Remember that the spool is sensitive local data. Review it like source code
before attaching logs or archives elsewhere.

## Provider import triage

Provider support in this branch is intentionally conservative:

- Codex has a `supported-import` path through explicit history import.
- Claude Code is `fixture-only`.
- Pi has a `supported-import` path through explicit session JSONL import.

If `ctx capture import-local-providers --json` reports a provider as
`detected-unsupported`, that is not a bug in the docs. It means the branch
found a local install but does not have a safe import or capture path to claim
publicly yet.

Useful commands:

```bash
ctx capture import-local-providers --json
ctx capture import-codex-history --input ~/.codex/history.jsonl --json
ctx capture import-pi-session --input ~/.pi/agent/sessions/<session>.jsonl --json
ctx capture import-provider --provider codex --input tests/fixtures/provider/codex.jsonl --json
```

If a provider path only has fixture proof or detection proof, keep the public
wording at `fixture-only`, `detected-unsupported`, or `blocked` until the
provider workstream lands real evidence.

## Publish flow checks

For PR publishing problems, validate the record and render locally first:

```bash
ctx show <record-id>
ctx publish pr-comment <record-id> --dry-run
```

That confirms the record exists, has the expected linked PR URL, and renders a
share-safe comment before any `gh` mutation happens.

## When to stop and keep the docs narrow

Do not "fix" a docs mismatch by upgrading a public claim without proof. If the
implementation only proves fixtures, explicit import, or unsupported detection,
the right action is to keep the docs narrow and hand the blocker back to the
provider or release workstream.
