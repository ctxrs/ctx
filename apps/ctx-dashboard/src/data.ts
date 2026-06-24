import type { DashboardData } from "./types";

export const sampleDashboardData: DashboardData = {
  schema_version: 1,
  product: "ctx",
  share_safe: true,
  summary: {
    record_count: 4,
    evidence_count: 8,
    linked_pr_count: 2,
    tags: [
      { tag: "dashboard", count: 2 },
      { tag: "provider-import", count: 2 },
      { tag: "fixture-only", count: 2 },
      { tag: "release", count: 1 }
    ]
  },
  privacy: {
    default_redacted: true,
    raw_transcripts_withheld: 3,
    redacted_previews: 15,
    withheld_links: 1,
    local_paths_redacted: true
  },
  views: [
    "Overview",
    "Workspace / Repo",
    "Provider Coverage",
    "Session Detail",
    "PR / Evidence",
    "Search / Explore",
    "Settings / Status",
    "Transcript, Messages, and Tool Calls",
    "Artifacts"
  ],
  records: [
    {
      id: "rec-dashboard",
      title: "Finish dashboard React export",
      body: "Built a share-safe dashboard over normalized work record DTOs with transcript, command, artifact, PR, and status views.",
      tags: ["dashboard", "react"],
      kind: "task",
      workspace: "workspace: ctx",
      pr_url: "https://github.com/ctxrs/ctx/pull/42",
      created_at: "2026-06-23T12:00:00Z",
      updated_at: "2026-06-23T12:38:00Z"
    },
    {
      id: "rec-provider",
      title: "Review provider import coverage",
      body: "Codex history import is shown as supported-import while Claude Code and Pi remain fixture-only with safe previews.",
      tags: ["provider-import", "fixture-only"],
      kind: "task",
      workspace: "workspace: ctx",
      created_at: "2026-06-23T11:15:00Z",
      updated_at: "2026-06-23T11:45:00Z"
    },
    {
      id: "rec-provider-detection",
      title: "Classify unsupported provider detection",
      body: "OpenCode is displayed as detected-unsupported until a stable local transcript or hook path is proven.",
      tags: ["provider-import", "classification"],
      kind: "provider-classification",
      workspace: "workspace: ctx",
      created_at: "2026-06-23T10:45:00Z",
      updated_at: "2026-06-23T11:05:00Z"
    },
    {
      id: "rec-release",
      title: "Check release evidence packet",
      body: "Release lane has one stale evidence item pending a fresh Buildkite result.",
      tags: ["release"],
      kind: "task",
      workspace: "workspace: ctx",
      pr_url: "https://github.com/ctxrs/ctx/pull/43",
      created_at: "2026-06-23T10:00:00Z",
      updated_at: "2026-06-23T10:50:00Z"
    }
  ],
  commands: [
    {
      id: "cmd-test",
      record_id: "rec-dashboard",
      command: "cargo test -p ctx --test cli dashboard_export_includes_records",
      exit_code: 0,
      duration_ms: 3200,
      started_at: "2026-06-23T12:21:00Z",
      output_preview: "test result: ok. 8 passed"
    },
    {
      id: "cmd-playwright",
      record_id: "rec-dashboard",
      command: "npm run test -- --update-snapshots",
      exit_code: 0,
      duration_ms: 9050,
      started_at: "2026-06-23T12:29:00Z",
      output_preview: "desktop, mobile, light, dark, populated, failure screenshots captured"
    },
    {
      id: "cmd-provider-import",
      record_id: "rec-provider",
      command: "ctx capture import-local-providers --json",
      exit_code: 0,
      duration_ms: 1840,
      started_at: "2026-06-23T11:33:00Z",
      output_preview: "codex: supported-import, claude-code: fixture-only, pi: fixture-only, opencode: detected-unsupported"
    },
    {
      id: "cmd-report",
      record_id: "rec-provider",
      command: "ctx report --record rec-provider --dry-run",
      exit_code: 0,
      duration_ms: 1120,
      started_at: "2026-06-23T11:39:00Z",
      output_preview: "share-safe report rendered with raw transcripts withheld and local paths redacted"
    },
    {
      id: "cmd-release",
      record_id: "rec-release",
      command: "buildkite-agent pipeline upload",
      exit_code: 1,
      duration_ms: 800,
      started_at: "2026-06-23T10:32:00Z",
      output_preview: "[REDACTED_ENV] missing for local CI preview"
    }
  ],
  sessions: [
    {
      id: "sess-1",
      work_record_id: "rec-dashboard",
      provider: "codex",
      support_status: "supported-import",
      capture_path: "import-local-providers",
      privacy_note: "prompt preview redacted; raw history withheld",
      role_hint: "manager session",
      agent_type: "manager",
      status: "imported",
      fidelity: "summary_only",
      is_primary: true,
      root_session_id: "sess-1",
      started_at: "2026-06-23T12:00:00Z",
      ended_at: "2026-06-23T12:38:00Z"
    },
    {
      id: "sess-claude",
      work_record_id: "rec-provider",
      provider: "claude-code",
      support_status: "fixture-only",
      capture_path: "normalized provider fixture JSONL",
      privacy_note: "fixture transcript redacted before dashboard export",
      role_hint: "review subagent",
      agent_type: "reviewer",
      status: "fixture-only",
      fidelity: "imported_fixture",
      is_primary: false,
      parent_session_id: "sess-1",
      root_session_id: "sess-1",
      started_at: "2026-06-23T11:18:00Z",
      ended_at: "2026-06-23T11:29:00Z"
    },
    {
      id: "sess-pi",
      work_record_id: "rec-provider",
      provider: "pi",
      support_status: "fixture-only",
      capture_path: "normalized provider fixture JSONL",
      privacy_note: "conversation fixture uses safe preview fields only",
      role_hint: "fixture import",
      agent_type: "assistant",
      status: "fixture-only",
      fidelity: "imported",
      is_primary: false,
      parent_session_id: "sess-1",
      root_session_id: "sess-1",
      started_at: "2026-06-23T11:15:00Z",
      ended_at: "2026-06-23T11:45:00Z"
    },
    {
      id: "sess-opencode",
      work_record_id: "rec-provider-detection",
      provider: "opencode",
      support_status: "detected-unsupported",
      capture_path: "install/config detection only",
      privacy_note: "no transcript importer enabled",
      role_hint: "local provider detection",
      agent_type: "provider-scan",
      status: "blocked",
      fidelity: "metadata_only",
      started_at: "2026-06-23T10:45:00Z",
      ended_at: "2026-06-23T11:05:00Z"
    }
  ],
  runs: [
    {
      id: "run-1",
      work_record_id: "rec-dashboard",
      session_id: "sess-1",
      run_type: "command",
      status: "succeeded",
      command_preview: "npm run build",
      exit_code: 0,
      started_at: "2026-06-23T12:28:00Z"
    },
    {
      id: "run-provider-import",
      work_record_id: "rec-provider",
      session_id: "sess-claude",
      run_type: "import",
      status: "succeeded",
      command_preview: "ctx capture import-provider --provider claude-code --input tests/fixtures/provider/claude-code.jsonl --json",
      exit_code: 0,
      started_at: "2026-06-23T11:20:00Z"
    },
    {
      id: "run-provider-scan",
      work_record_id: "rec-provider-detection",
      session_id: "sess-opencode",
      run_type: "detection",
      status: "blocked",
      command_preview: "ctx capture import-local-providers --json",
      exit_code: 0,
      started_at: "2026-06-23T10:50:00Z"
    }
  ],
  events: [
    {
      id: "evt-0",
      seq: 0,
      work_record_id: "rec-dashboard",
      session_id: "sess-1",
      event_type: "message",
      role: "user",
      preview: "Polish the ctx dashboard provider detail and CLI JSON outputs.",
      redaction_state: "redacted",
      occurred_at: "2026-06-23T12:01:00Z"
    },
    {
      id: "evt-1",
      seq: 1,
      work_record_id: "rec-dashboard",
      session_id: "sess-1",
      event_type: "message",
      role: "assistant",
      preview: "Normalized dashboard export DTOs before rendering.",
      redaction_state: "redacted",
      occurred_at: "2026-06-23T12:04:00Z"
    },
    {
      id: "evt-2",
      seq: 2,
      work_record_id: "rec-dashboard",
      session_id: "sess-1",
      run_id: "run-1",
      event_type: "tool_call",
      role: "assistant",
      preview: "exec_command npm run build",
      redaction_state: "safe_preview",
      occurred_at: "2026-06-23T12:28:00Z"
    },
    {
      id: "evt-3",
      seq: 3,
      work_record_id: "rec-dashboard",
      session_id: "sess-1",
      run_id: "run-1",
      event_type: "tool_output",
      role: "tool",
      preview: "built in 2.1s",
      payload_blob_id: "blob-tool-output-evt-3",
      redaction_state: "safe_preview",
      occurred_at: "2026-06-23T12:28:03Z"
    },
    {
      id: "evt-claude-0",
      seq: 0,
      work_record_id: "rec-provider",
      session_id: "sess-claude",
      event_type: "message",
      role: "user",
      preview: "Summarize provider fixture support without exposing raw project paths.",
      redaction_state: "redacted",
      occurred_at: "2026-06-23T11:19:00Z"
    },
    {
      id: "evt-claude-1",
      seq: 1,
      work_record_id: "rec-provider",
      session_id: "sess-claude",
      event_type: "message",
      role: "assistant",
      preview: "Fixture import produced prompts, assistant summaries, and tool-call previews; native Claude Code proof remains pending.",
      redaction_state: "safe_preview",
      occurred_at: "2026-06-23T11:21:00Z"
    },
    {
      id: "evt-claude-2",
      seq: 2,
      work_record_id: "rec-provider",
      session_id: "sess-claude",
      run_id: "run-provider-import",
      event_type: "tool_call",
      role: "assistant",
      preview: "read fixture transcript, normalize events, mark raw payload withheld",
      redaction_state: "safe_preview",
      occurred_at: "2026-06-23T11:22:00Z"
    },
    {
      id: "evt-pi-0",
      seq: 0,
      work_record_id: "rec-provider",
      session_id: "sess-pi",
      event_type: "message",
      role: "user",
      preview: "Import the Pi fixture and keep the support claim fixture-only.",
      redaction_state: "redacted",
      occurred_at: "2026-06-23T11:31:00Z"
    },
    {
      id: "evt-pi-1",
      seq: 1,
      work_record_id: "rec-provider",
      session_id: "sess-pi",
      event_type: "tool_output",
      role: "tool",
      preview: "2 messages normalized; no native history path claimed",
      redaction_state: "safe_preview",
      occurred_at: "2026-06-23T11:32:00Z"
    }
  ],
  vcs_workspaces: [
    {
      id: "vcs-1",
      kind: "git",
      repo: "ctxrs/ctx",
      root: "workspace: ctx",
      host: "github",
      owner: "ctxrs",
      name: "ctx",
      monorepo_subpath: "apps/ctx-dashboard"
    }
  ],
  vcs_changes: [
    {
      id: "change-1",
      vcs_workspace_id: "vcs-1",
      kind: "git_branch",
      change_id: "b7c61ab",
      branch_or_bookmark: "ctx/wr-finish-dashboard-react",
      tree_hash: "tree-safe"
    }
  ],
  pull_requests: [
    {
      url: "https://github.com/ctxrs/ctx/pull/42",
      title: "Dashboard v2",
      state: "open",
      head_ref: "ctx/wr-finish-dashboard-react",
      base_ref: "main"
    }
  ],
  artifacts: [
    {
      id: "artifact-1",
      kind: "screenshot",
      byte_size: 98423,
      media_type: "image/png",
      redaction_state: "safe_preview",
      preview: "dashboard desktop light screenshot"
    },
    {
      id: "artifact-2",
      kind: "transcript",
      byte_size: 2048,
      media_type: "application/jsonl",
      redaction_state: "raw",
      preview: "raw artifact content withheld"
    },
    {
      id: "artifact-provider-report",
      kind: "provider-report",
      byte_size: 6812,
      media_type: "application/json",
      redaction_state: "safe_preview",
      preview: "support_status fields exported for codex, claude-code, pi, and opencode"
    },
    {
      id: "artifact-pr-comment",
      kind: "pr-comment",
      byte_size: 3920,
      media_type: "text/markdown",
      redaction_state: "redacted",
      preview: "PR evidence preview: provider fixture claims remain clearly labeled"
    }
  ],
  evidence_metadata: [
    {
      id: "evidence-meta-1",
      work_record_id: "rec-dashboard",
      kind: "test",
      status: "passed",
      freshness: "fresh"
    },
    {
      id: "evidence-provider-import",
      work_record_id: "rec-provider",
      kind: "provider-import",
      status: "passed",
      freshness: "fresh"
    },
    {
      id: "evidence-provider-claim",
      work_record_id: "rec-provider-detection",
      kind: "provider-claim",
      status: "blocked",
      freshness: "fresh",
      stale_reason: "OpenCode detection has no supported import or wrapper proof yet"
    },
    {
      id: "evidence-meta-2",
      work_record_id: "rec-release",
      kind: "ci",
      status: "failed",
      freshness: "stale",
      stale_reason: "Buildkite token unavailable in local export"
    }
  ],
  files_touched: [
    {
      id: "file-1",
      work_record_id: "rec-dashboard",
      path: "apps/ctx-dashboard/src/main.tsx",
      change_kind: "modified",
      line_count_delta: 420,
      confidence: "explicit"
    },
    {
      id: "file-provider-1",
      work_record_id: "rec-provider",
      path: "apps/ctx-dashboard/src/data.ts",
      change_kind: "modified",
      line_count_delta: 180,
      confidence: "explicit"
    }
  ],
  summaries: [
    {
      id: "summary-1",
      work_record_id: "rec-dashboard",
      kind: "agent",
      model_or_source: "codex",
      text: "Dashboard export keeps useful review context visible while raw transcript payloads stay withheld."
    },
    {
      id: "summary-provider-1",
      work_record_id: "rec-provider",
      session_id: "sess-claude",
      kind: "provider_fixture",
      model_or_source: "claude-code",
      text: "Claude Code fixture data proves dashboard rendering only; native history and hooks are not claimed."
    },
    {
      id: "summary-provider-2",
      work_record_id: "rec-provider-detection",
      session_id: "sess-opencode",
      kind: "provider_blocker",
      model_or_source: "opencode",
      text: "Detected local provider surface, but no stable import or passive capture path is shipped."
    }
  ],
  status: {
    export_mode: "Static local export",
    local_only: true,
    javascript_app: "ctx dashboard",
    data_contract: "ctx dashboard export v1",
    search_command: "ctx search <query> --json"
  }
};

export function readDashboardData(): DashboardData {
  const node = document.getElementById("ctx-dashboard-data");
  const raw = node?.textContent?.trim();
  if (raw && raw !== "__CTX_DASHBOARD_DATA__") {
    try {
      return JSON.parse(raw) as DashboardData;
    } catch {
      return sampleDashboardData;
    }
  }
  return sampleDashboardData;
}
