import type { DashboardData } from "./types";

export const sampleDashboardData: DashboardData = {
  schema_version: 1,
  product: "ctx Work Recorder",
  share_safe: true,
  summary: {
    record_count: 3,
    evidence_count: 4,
    linked_pr_count: 2,
    tags: [
      { tag: "dashboard", count: 2 },
      { tag: "provider-import", count: 1 },
      { tag: "release", count: 1 }
    ]
  },
  privacy: {
    default_redacted: true,
    raw_transcripts_withheld: 1,
    redacted_previews: 7,
    withheld_links: 0,
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
      body: "Built a share-safe dashboard over normalized Work Recorder DTOs with transcript, command, artifact, PR, and status views.",
      tags: ["dashboard", "react"],
      kind: "task",
      workspace: "workspace: ctx",
      pr_url: "https://github.com/ctxrs/ctx/pull/42",
      created_at: "2026-06-23T12:00:00Z",
      updated_at: "2026-06-23T12:38:00Z"
    },
    {
      id: "rec-provider",
      title: "Import provider fixture sessions",
      body: "Provider fixture import normalized Codex, Claude, and Pi events with safe previews.",
      tags: ["provider-import"],
      kind: "task",
      workspace: "workspace: ctx",
      created_at: "2026-06-23T11:15:00Z",
      updated_at: "2026-06-23T11:45:00Z"
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
      id: "cmd-release",
      record_id: "rec-release",
      command: "buildkite-agent pipeline upload",
      exit_code: 1,
      duration_ms: 800,
      started_at: "2026-06-23T10:32:00Z",
      output_preview: "missing BUILDKITE_AGENT_TOKEN"
    }
  ],
  sessions: [
    {
      id: "sess-1",
      work_record_id: "rec-dashboard",
      provider: "codex",
      role_hint: "implementation worker",
      agent_type: "implementer",
      status: "completed",
      fidelity: "high",
      is_primary: false,
      started_at: "2026-06-23T12:00:00Z",
      ended_at: "2026-06-23T12:38:00Z"
    },
    {
      id: "sess-pi",
      work_record_id: "rec-provider",
      provider: "pi",
      role_hint: "fixture import",
      agent_type: "assistant",
      status: "completed",
      fidelity: "imported",
      is_primary: false,
      started_at: "2026-06-23T11:15:00Z",
      ended_at: "2026-06-23T11:45:00Z"
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
      preview: "Polish the Work Recorder dashboard provider detail and CLI JSON outputs.",
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
      redaction_state: "safe_preview",
      occurred_at: "2026-06-23T12:28:03Z"
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
      monorepo_subpath: "apps/dashboard"
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
      path: "apps/dashboard/src/App.tsx",
      change_kind: "modified",
      line_count_delta: 420,
      confidence: "explicit"
    }
  ],
  summaries: [
    {
      id: "summary-1",
      work_record_id: "rec-dashboard",
      kind: "agent",
      model_or_source: "codex",
      text: "Dashboard export is React/Vite and uses share-safe normalized DTOs."
    }
  ],
  status: {
    export_mode: "Static local export",
    local_only: true,
    javascript_app: "React/Vite",
    data_contract: "Work Recorder dashboard export v1",
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
