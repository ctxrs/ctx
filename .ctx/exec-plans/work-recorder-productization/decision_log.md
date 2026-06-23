# Work Recorder Productization Decision Log

Updated: 2026-06-22T19:40:55-05:00

## Decisions

- Use `ctxrs/ctx` branch `work-record` for public Work Recorder productization.
- Preserve the reviewed manager plan in this worktree before implementation.
- Treat `ctxrs/ade` as frozen unless a maintenance-only need is discovered.
- Avoid the dirty canonical `ctx-private` checkout; hosted/private work will use
  a separate manual `ctx-private` worktree after reading private repo
  instructions.
- Use a separate private hosted worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx-private/work-recorder-hosted-team`
  on branch `ctx/work-recorder-hosted-team`.
- Sequence public local schema/storage before private hosted sync work, because
  hosted sync should follow the stabilized local JSON/schema contract.
- Prefer a new Work Recorder hosted worker/service in `ctx-private` rather than
  overloading the existing control-plane worker.
- Public Work Record and evidence IDs use UUIDv7 for sortable local/offline IDs.
- Public CLI JSON responses use schema-versioned envelopes. `ctx context --json`
  emits the public `AgentContextPacket` instead of the older internal context
  shape.
- Current evidence writes must be attached to a Work Record. When
  `ctx evidence run` is invoked without `--record`, the CLI creates a command
  evidence Work Record and attaches the evidence automatically.
- Full command output is stored as content-addressed local-only artifacts under
  `blobs/`; evidence rows keep bounded redacted previews and artifact pointers.
- Archive JSON carries both `schema_version: 1` and the existing `version: 1`
  field so public JSON is consistently versioned without breaking current archive
  compatibility.
- Agent context output preserves `local_only` visibility by default. Records
  must be explicitly promoted before future reportable/team-sync surfaces expose
  richer content.
- Evidence stream artifacts are represented through `evidence_artifacts` so both
  stdout and stderr can be attached. The single `evidence.artifact_id` column is
  retained as a primary compatibility pointer.
- Archive import preflights evidence references and then imports records,
  evidence rows, artifact rows, and evidence/artifact links in one DB
  transaction.
- Public JSON archives include evidence artifact payloads so local-only full
  stdout/stderr content can survive export/import. This is explicit portability
  behavior, not a default report/share surface; exported archives must be
  reviewed before leaving the machine.
- Local dashboard export is static-by-default: no JavaScript, no remote assets,
  no publish/sync side effect, safe workspace labels instead of absolute local
  paths, and redacted command/evidence previews in default HTML.

## Pending Decisions

- Exact public crate/module split after current-code mapper output.
- Whether any existing ADE surfaces are quarantined, hidden, or removed in this
  branch.
- Hosted staging environment choice and whether credentials allow deployment
  from this machine.
- Buildkite runner/platform availability and any required queue/pool changes.
- Whether legacy unattached evidence rows from pre-productization stores should
  be migrated into synthetic Work Records or only tolerated as legacy read data.
