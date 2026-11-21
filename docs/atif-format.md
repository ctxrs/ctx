# ATIF v1.8 Import And Export

ctx supports importing and exporting agent sessions in **ATIF (Agent Trajectory
Interchange Format)** per Harbor RFC 0001, covering `ATIF-v1.0` through
`ATIF-v1.8` on import and emitting validated `ATIF-v1.8` JSON on export.

## Import

Import a single ATIF JSON file or a directory of ATIF JSON files:

```bash
ctx import --input-format atif-v1 --path ./trajectory.json
ctx import --input-format atif-v1 --path ./trajectories/
```

Import behavior:

- Accepts `schema_version` values `"ATIF-v1.0"` through `"ATIF-v1.8"`.
  Unsupported versions (such as `"ATIF-v2.0"`) fail cleanly.
- Rejects unknown fields on every ATIF object (`Trajectory`, `Agent`, `Step`,
  `ToolCall`, `Observation`, `ObservationResult`, `SubagentTrajectoryRef`,
  `Metrics`, `FinalMetrics`, `ContentPart`, `ImageSource`, `AudioSource`).
- Enforces ATIF v1.7 and v1.8 semantic rules:
  - `steps` must be non-empty on root trajectories and every embedded entry in
    `subagent_trajectories`, with sequential `step_id` values starting at `1`.
  - `session_id` is required on root `ATIF-v1.0` through `ATIF-v1.6` documents
    and optional on `ATIF-v1.7` and `ATIF-v1.8`.
  - `model_name`, `reasoning_effort`, `reasoning_content`, `tool_calls`, and
    `metrics` are permitted only when `source` is `"agent"`.
  - When `source` is `"agent"` and `llm_call_count` is `0` (deterministic
    dispatch), `metrics` and `reasoning_content` must be absent.
  - `observation.results[].source_call_id` is permitted only on `"agent"` steps
    and must match a `tool_call_id` in that same step's `tool_calls`. System
    steps may carry `observation` without `source_call_id` (for example
    `extra.context_management` compaction boundaries).
  - Every `SubagentTrajectoryRef` must set at least one of `trajectory_id`
    (embedded reference) or `trajectory_path` (external file reference);
    `session_id` alone is informational and is never a resolution key.
  - Every entry in `subagent_trajectories` must set a non-empty, sibling-unique
    `trajectory_id`.
  - Multimodal `ContentPart` arrays validate strict type/source correspondence:
    `type: "image"` accepts only the four canonical image MIME types
    (`image/jpeg`, `image/png`, `image/gif`, `image/webp`); `type: "audio"`
    accepts the eight canonical audio MIME types (`audio/wav`, `audio/mpeg`,
    `audio/mp4`, `audio/aac`, `audio/ogg`, `audio/flac`, `audio/webm`,
    `audio/aiff`) and normalizes RFC 0001 v1.8 aliases (such as `audio/mp3` to
    `audio/mpeg`, `audio/x-wav` to `audio/wav`, and `audio/m4a` to `audio/mp4`)
    with non-negative finite `duration_sec`.
- Preserves lineage for embedded `subagent_trajectories` as well as local
  relative `trajectory_path` and `continued_trajectory_ref` links inside the
  imported directory without following symlinks or path traversal outside the
  selected root. Non-ATIF JSON files (objects without `schema_version`) in a
  directory scan are ignored.
- Retains full ATIF step and trajectory metadata in Core structured content so
  imported ATIF sessions round-trip back to `ATIF-v1.8`.

## Export

### Single Session

Export one indexed session (including tool calls, observations, system prompts,
and any linked child/subagent sessions embedded in `subagent_trajectories`) as
validated `ATIF-v1.8` JSON:

```bash
ctx show session <ctx-session-id> --format atif
ctx show session <ctx-session-id> --format atif --out ./trajectory.json
ctx show session --provider codex --provider-session <provider-session-id> --format atif --out ./trajectory.json
```

`--format atif` is specific to `ctx show session` (it is rejected by
`ctx show event`). It exports the full session event log rather than the compact
lite transcript view so tool calls, observations, and system prompts are
preserved.

### Multiple Sessions

Export a filtered batch of sessions as `.atif.json` files in an output
directory:

```bash
ctx archive atif --out ./atif-export
ctx archive atif --out ./atif-export --provider codex --provider claude
ctx archive atif --out ./atif-export --workspace /work/repo \
  --since 2026-10-01T00:00:00Z --until 2026-11-01T00:00:00Z
ctx archive atif --out ./atif-export --session <ctx-session-id> --no-subagents --format json
```

Batch export behavior:

- Pins one immutable Core index generation across selection and file writes.
- Embeds child sessions in their parent's `subagent_trajectories` and avoids
  writing duplicate standalone files for children whose parent is already in the
  selected set. Pass `--no-subagents` to export every selected session as a
  standalone trajectory without `subagent_trajectories`.
- `--since` and `--until` must be supplied together as RFC 3339 timestamps
  (`[since, until)`).
- Every exported file is validated against the ATIF v1.8 schema before atomic
  publication into `--out`.
- Pass `--format json` for a machine-readable export receipt (`schema_version`,
  `operation: "archive_atif"`, `atif_schema_version: "ATIF-v1.8"`,
  `generation_id`, `output_dir`, `sessions_exported`, `subagents_embedded`,
  `steps_exported`, and `files`).
