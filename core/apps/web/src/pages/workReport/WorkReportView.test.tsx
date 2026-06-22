import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { WorkspaceWorkInspector } from "@ctx/types";
import { WorkInspectorView } from "./WorkReportView";

const createObjectUrl = vi.fn(() => "blob:ctx-work-artifact");
const revokeObjectUrl = vi.fn();
const originalCreateObjectUrl = URL.createObjectURL;
const originalRevokeObjectUrl = URL.revokeObjectURL;

beforeEach(() => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(new Blob(["artifact"], { type: "image/png" }), { status: 200 })),
  );
  Object.defineProperty(URL, "createObjectURL", {
    configurable: true,
    value: createObjectUrl,
  });
  Object.defineProperty(URL, "revokeObjectURL", {
    configurable: true,
    value: revokeObjectUrl,
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  if (originalCreateObjectUrl) {
    Object.defineProperty(URL, "createObjectURL", {
      configurable: true,
      value: originalCreateObjectUrl,
    });
  } else {
    Reflect.deleteProperty(URL, "createObjectURL");
  }
  if (originalRevokeObjectUrl) {
    Object.defineProperty(URL, "revokeObjectURL", {
      configurable: true,
      value: originalRevokeObjectUrl,
    });
  } else {
    Reflect.deleteProperty(URL, "revokeObjectURL");
  }
  createObjectUrl.mockClear();
  revokeObjectUrl.mockClear();
});

const baseReport = (): WorkspaceWorkInspector => ({
  work: {
    work_id: "wrk_1234567890",
    workspace_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    title: "Stabilize Work inspector route",
    objective: "Make local Work records legible",
    lifecycle: "ready_for_review",
    primary_branch: "ctx/work-observability",
    base_commit: null,
    head_commit: "abcdef1234567890",
    trust_verdict: "stale",
    summary_freshness: "stale",
    created_at: "2026-06-21T00:00:00Z",
    updated_at: "2026-06-21T00:01:00Z",
    schema_version: 1,
  },
  links: [],
  overview: {
    title: "Stabilize Work inspector route",
    objective: "Make local Work records legible",
    lifecycle: "ready_for_review",
    primary_branch: "ctx/work-observability",
    base_commit: null,
    head_commit: "abcdef1234567890",
    created_at: "2026-06-21T00:00:00Z",
    updated_at: "2026-06-21T00:01:00Z",
  },
  trust: {
    verdict: "failed",
    reason: "At least one linked evidence item failed.",
    recommended_next_action: "Fix the failing evidence before marking this ready.",
    open_risks: ["At least one linked evidence item failed."],
  },
  context: {
    value: {
      budget_tokens: 4000,
      summary: "Only redacted context is present.",
    },
    redacted: true,
    redaction_notes: ["test fixture"],
  },
  safe_json: {
    value: {
      safe_marker: "safe-visible-marker",
      redacted: "[redacted:workspace_root]",
    },
    redacted: true,
    redaction_notes: ["test fixture"],
  },
  raw_redacted_json: {
    value: {
      safe_marker: "safe-visible-marker",
      redacted: "[redacted:workspace_root]",
    },
    redacted: true,
    redaction_notes: ["test fixture"],
  },
  evidence_summary: {
    total: 2,
    passing: 1,
    failing: 1,
    stale: 1,
    missing: 0,
  },
  evidence: [
    {
      evidence_id: "wevdc_fail",
      work_id: "wrk_1234567890",
      workspace_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      kind: "test",
      status: "observed_fail",
      freshness: "stale",
      claim: "Observed cargo test exited 101",
      command: "cargo test -p ctx-http",
      argv: ["cargo", "test", "-p", "ctx-http"],
      cwd: "[redacted:workspace_root]",
      exit_code: 101,
      head_sha: "abcdef1234567890",
      branch: "ctx/work-observability",
      output_ref: { log: "redacted failure output" },
      artifact_ref: { path: "[redacted:artifact]" },
      source: "worktree",
      fidelity: "exact",
      trust: "medium",
      started_at: "2026-06-21T00:00:00Z",
      finished_at: "2026-06-21T00:01:00Z",
      created_at: "2026-06-21T00:01:00Z",
      updated_at: "2026-06-21T00:01:00Z",
      schema_version: 1,
    },
    {
      evidence_id: "wevdc_pass",
      work_id: "wrk_1234567890",
      workspace_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      kind: "lint",
      status: "observed_pass",
      freshness: "fresh",
      claim: "Observed lint exited 0",
      command: "pnpm lint",
      argv: ["pnpm", "lint"],
      cwd: "[redacted:workspace_root]",
      exit_code: 0,
      head_sha: "abcdef1234567890",
      branch: "ctx/work-observability",
      output_ref: null,
      artifact_ref: null,
      source: "worktree",
      fidelity: "exact",
      trust: "medium",
      started_at: "2026-06-21T00:02:00Z",
      finished_at: "2026-06-21T00:03:00Z",
      created_at: "2026-06-21T00:03:00Z",
      updated_at: "2026-06-21T00:03:00Z",
      schema_version: 1,
    },
  ],
  change_summary: {
    change_sets: 1,
    contributions: 2,
    pull_requests: [
      {
        provider: "github",
        owner: "ctxrs",
        repo: "ctx",
        number: 123,
        title: "Unsafe stored PR",
        url: "javascript:alert(1)",
        state: "draft",
      },
    ],
    commits: ["abcdef1234567890"],
  },
  artifact_summary: {
    total: 1,
    refs: [],
  },
  change_sets: [
    {
      files: [
        {
          path: "core/apps/web/src/pages/workReport/WorkReportView.tsx",
          additions: 42,
          deletions: 7,
          status: "modified",
        },
      ],
    },
  ],
  contributions: [
    {
      changed_files: [
        {
          filename: "core/apps/web/src/styles/work-report.css",
          lines_added: 20,
          lines_deleted: 3,
          change_type: "modified",
        },
      ],
      file_summaries: [
        {
          path: "core/apps/web/src/styles/work-report.css",
          summary: "Adds readable command-output and review-note panels for the Inspector.",
        },
      ],
      source_outline: [
        {
          title: "Inspector layout",
          path: "core/apps/web/src/pages/workReport/WorkReportView.tsx",
          detail: "Renders commands, source outline, review notes, and artifacts from typed Work fields.",
          excerpt: "export function CommandsTab({ commands }) { /* redacted fixture excerpt */ }",
        },
      ],
      review_notes: [
        {
          title: "Resume point",
          detail: "Re-run the focused Work Inspector tests and inspect the Changes and Commands tabs.",
        },
      ],
      source_snapshots: [
        {
          share_safe: true,
          redaction_class: "local_redacted",
          path: "core/apps/web/src/pages/workReport/WorkReportView.tsx",
          language: "tsx",
          kind: "implementation",
          line_count: 3,
          sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          safe_content:
            "export function CommandsTab({ commands }) {\n  return <section aria-label=\"Commands\">share-safe fixture</section>;\n}\nconst one = 1;\nconst two = 2;\nconst three = 3;\nconst four = 4;\nconst five = 5;\nconst six = 6;\nconst seven = 7;\nconst FULL_SOURCE_TAIL_SHOULD_NOT_RENDER = true;",
        },
        {
          share_safe: false,
          redaction_class: "local_redacted",
          path: "unsafe.ts",
          content: "UNSAFE_SOURCE_SNAPSHOT_SHOULD_NOT_RENDER",
        },
      ],
    },
  ],
  summaries: [
    {
      summary_id: "wsum_1",
      work_id: "wrk_1234567890",
      workspace_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      kind: "report_summary",
      audience: "reviewer",
      text: "Evidence is present but one item is stale.",
      structured_json: null,
      generation_method: "deterministic",
      provider: null,
      model: null,
      template: "ctx.work.deterministic.v1",
      source_material_left_machine: false,
      freshness: "stale",
      source_revision_key: "rev-1",
      generated_at: "2026-06-21T00:04:00Z",
      created_at: "2026-06-21T00:04:00Z",
      updated_at: "2026-06-21T00:04:00Z",
      schema_version: 1,
    },
  ],
  summary_claims: [
    {
      claim_id: "wclaim_1",
      summary_id: "wsum_1",
      work_id: "wrk_1234567890",
      workspace_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      claim_text: "Focused tests were run and one command failed.",
      claim_kind: "validation",
      source_kind: "evidence",
      source_id: "wevdc_fail",
      record_hash: null,
      freshness: "stale",
      redaction_class: "local_redacted",
      created_at: "2026-06-21T00:04:00Z",
      schema_version: 1,
    },
  ],
  timeline: [
    {
      event_id: "wev_1",
      work_id: "wrk_1234567890",
      workspace_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      sequence: 1,
      source_kind: "evidence",
      source_id: "wevdc_fail",
      event_type: "evidence_observed",
      event_time: "2026-06-21T00:01:00Z",
      actor_kind: "system",
      provider: null,
      harness: null,
      model: null,
      redaction_class: "local_redacted",
      source: "worktree",
      fidelity: "exact",
      trust: "medium",
      redacted_text: "Observed redacted command output.",
      created_at: "2026-06-21T00:01:00Z",
      schema_version: 1,
    },
  ],
  transcript: [
    {
      event_id: "msg_1",
      sequence: 1,
      event_type: "assistant_message",
      actor_kind: "agent",
      event_time: "2026-06-21T00:00:30Z",
      redaction_class: "local_redacted",
      text_preview: "I ran the focused tests.",
    },
  ],
  subagents: [
    {
      id: "wln_child_session",
      child_session_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
      run_id: "run_child_visual",
      label: "Visual reviewer",
      summary: "Confirmed the artifact rendered and the redacted report was enough to review the run.",
      status: "completed",
      role: "child_session",
      provider: "ctx",
      harness: "reviewer",
      model: "gpt-test",
      prompt_length: 244,
      event_count: 2,
      latest_event_time: "2026-06-21T00:00:45Z",
      transcript_preview: [
        {
          event_id: "msg_child_1",
          sequence: 10,
          event_type: "assistant_message",
          actor_kind: "subagent",
          event_time: "2026-06-21T00:00:45Z",
          redaction_class: "local_redacted",
          text_preview: "subagent message captured (71 chars, body omitted)",
        },
      ],
    },
  ],
  commands: [
    {
      id: "cmd_1",
      evidence_id: "wevdc_fail",
      command: "cargo test -p ctx-http",
      argv: ["cargo", "test", "-p", "ctx-http"],
      cwd_label: "project root",
      exit_code: 101,
      status: "observed_fail",
      freshness: "stale",
      stdout_preview: "running 1 test\nroute_work_report ... FAILED\nshare-safe fixture output",
      stderr_preview: null,
      output_truncated: false,
      stdout_size_bytes: 2048,
      stdout_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      started_at: "2026-06-21T00:00:00Z",
      finished_at: "2026-06-21T00:01:00Z",
    },
  ],
  artifacts: [
    {
      id: "artifact_1",
      kind: "screenshot",
      label: "Unsafe screenshot link",
      missing: true,
      unavailable_reason: "artifact metadata unavailable",
      render_kind: "unavailable",
      created_at: "2026-06-21T00:05:00Z",
    },
    {
      id: "artifact_2",
      artifact_id: "11111111-1111-4111-8111-111111111111",
      source_kind: "link",
      source_id: "wln_2",
      kind: "screenshot",
      label: "Inspector overview screenshot",
      display_name: "screenshots/work-inspector-overview.png",
      mime_type: "image/png",
      bytes: 1536,
      missing: false,
      render_kind: "raster_image",
      download_url: "/api/workspaces/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa/work/wrk_1234567890/artifacts/11111111-1111-4111-8111-111111111111",
      open_url: "/api/workspaces/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa/work/wrk_1234567890/artifacts/11111111-1111-4111-8111-111111111111",
      thumbnail_url: "/api/workspaces/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa/work/wrk_1234567890/artifacts/11111111-1111-4111-8111-111111111111",
      preview_url: "/api/workspaces/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa/work/wrk_1234567890/artifacts/11111111-1111-4111-8111-111111111111",
      created_at: "2026-06-21T00:06:00Z",
    },
  ],
  timeline_items: [
    {
      sequence: 1,
      event_time: "2026-06-21T00:00:30Z",
      kind: "assistant_message",
      title: "Assistant explained validation.",
      detail: "session",
      source_event_id: "msg_1",
    },
    {
      sequence: 2,
      event_time: "2026-06-21T00:01:00Z",
      kind: "test",
      title: "Observed cargo test exited 101",
      detail: "observed_fail / stale",
      source_evidence_id: "wevdc_fail",
    },
  ],
  duplicate_strong_links: [],
  raw_transcript_available: false,
  raw_transcript_included: false,
});

describe("WorkInspectorView", () => {
  it("renders the dashboard shell and overview metrics", () => {
    const onRefresh = vi.fn();
    render(<WorkInspectorView report={baseReport()} onRefresh={onRefresh} />);

    expect(screen.getByRole("heading", { name: "Stabilize Work inspector route" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Overview" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tab", { name: "Agent handoff" })).toBeInTheDocument();
    expect(screen.getByLabelText("Work trust")).toHaveTextContent("failed");
    expect(screen.getByLabelText("Evidence summary")).toHaveTextContent("Commands");
    expect(screen.getByLabelText("Evidence summary")).toHaveTextContent("Subagents");
    expect(screen.getAllByText("Full transcripts are not available in this inspector response.").length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(onRefresh).toHaveBeenCalledTimes(1);
  });

  it("switches between transcript, subagent, commands, and evidence tabs deterministically", () => {
    render(<WorkInspectorView report={baseReport()} />);

    fireEvent.click(screen.getByRole("tab", { name: "Transcript" }));
    expect(screen.getByRole("tab", { name: "Transcript" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Conversation Story");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Agent activity captured");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Share-safe event trace");
    expect(screen.queryByText("Observed cargo test exited 101")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: "Subagents" }));
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Visual reviewer");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Confirmed the artifact rendered");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Subagent response captured");
    expect(screen.getByRole("tabpanel")).not.toHaveTextContent("cargo test -p ctx-http");

    fireEvent.click(screen.getByRole("tab", { name: "Commands" }));
    expect(screen.getByRole("tabpanel")).toHaveTextContent("cargo test -p ctx-http");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("project root");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("exit 101");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Output was captured and kept share-safe");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("route_work_report ... FAILED");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Validation proof");
    expect(screen.getByRole("tabpanel")).not.toHaveTextContent("redacted failure output");

    fireEvent.click(screen.getByRole("tab", { name: "Evidence" }));
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Observed cargo test exited 101");
    expect(screen.getAllByText("worktree").length).toBeGreaterThan(0);
    expect(screen.getByRole("tabpanel")).not.toHaveTextContent("[redacted:artifact]");
  });

  it("supports arrow-key tab navigation", () => {
    render(<WorkInspectorView report={baseReport()} />);

    const overview = screen.getByRole("tab", { name: "Overview" });
    overview.focus();
    fireEvent.keyDown(overview, { key: "ArrowRight" });

    expect(screen.getByRole("tab", { name: "Transcript" })).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(screen.getByRole("tab", { name: "Transcript" }), { key: "End" });
    expect(screen.getByRole("tab", { name: "Agent handoff" })).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(screen.getByRole("tab", { name: "Agent handoff" }), { key: "Home" });
    expect(screen.getByRole("tab", { name: "Overview" })).toHaveAttribute("aria-selected", "true");
  });

  it("renders typed change and artifact fields without exposing unsafe URLs or raw refs", async () => {
    render(<WorkInspectorView report={baseReport()} />);

    fireEvent.click(screen.getByRole("tab", { name: "Changes" }));
    expect(screen.queryByRole("link", { name: "Unsafe stored PR · draft" })).not.toBeInTheDocument();
    expect(screen.getByText("Unsafe stored PR · draft")).toBeInTheDocument();
    expect(screen.getAllByText("core/apps/web/src/pages/workReport/WorkReportView.tsx").length).toBeGreaterThan(0);
    expect(screen.getAllByText("core/apps/web/src/styles/work-report.css").length).toBeGreaterThan(0);
    expect(screen.getByText("Source outline")).toBeInTheDocument();
    expect(screen.getAllByText("Review notes").length).toBeGreaterThan(0);
    expect(screen.getByText(/Inspector layout/)).toBeInTheDocument();
    expect(screen.getByText("Resume point")).toBeInTheDocument();
    expect(screen.getByText(/redacted fixture excerpt/)).toBeInTheDocument();
    expect(screen.getByText("Implementation snapshot")).toBeInTheDocument();
    expect(screen.getByText(/share-safe fixture/)).toBeInTheDocument();
    expect(screen.getByText(/additional lines are omitted from the default UI/)).toBeInTheDocument();
    expect(screen.queryByText(/FULL_SOURCE_TAIL_SHOULD_NOT_RENDER/)).not.toBeInTheDocument();
    expect(screen.queryByText(/UNSAFE_SOURCE_SNAPSHOT_SHOULD_NOT_RENDER/)).not.toBeInTheDocument();
    expect(screen.getByText(/sha256 bbbbbbbbbbbb/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: "Artifacts" }));
    const artifacts = screen.getByRole("tabpanel");
    expect(within(artifacts).queryByRole("link", { name: "javascript:alert(1)" })).not.toBeInTheDocument();
    expect(within(artifacts).queryByText("javascript:alert(1)")).not.toBeInTheDocument();
    expect(within(artifacts).getByText("Inspector overview screenshot")).toBeInTheDocument();
    expect(within(artifacts).getByRole("button", { name: "Preview" })).toBeInTheDocument();
    expect(within(artifacts).getByRole("button", { name: "Download" })).toBeInTheDocument();
    expect(within(artifacts).queryByRole("link", { name: "Preview" })).not.toBeInTheDocument();
    expect(within(artifacts).queryByRole("link", { name: "Download" })).not.toBeInTheDocument();
    await waitFor(() =>
      expect(within(artifacts).getByRole("img", { name: "Inspector overview screenshot preview" })).toHaveAttribute(
        "src",
        "blob:ctx-work-artifact",
      ),
    );
    expect(artifacts.innerHTML).not.toContain("token=");
    expect(artifacts.innerHTML).not.toContain("expires_at=");
    expect(within(artifacts).getByText("screenshots/work-inspector-overview.png")).toBeInTheDocument();
    expect(within(artifacts).getByText("artifact metadata unavailable")).toBeInTheDocument();
    expect(within(artifacts).queryByText(/\"mime\"/)).not.toBeInTheDocument();
  });

  it("falls back cleanly when an artifact thumbnail cannot be loaded", async () => {
    render(<WorkInspectorView report={baseReport()} />);

    fireEvent.click(screen.getByRole("tab", { name: "Artifacts" }));
    const artifacts = screen.getByRole("tabpanel");
    const thumbnail = await within(artifacts).findByRole("img", { name: "Inspector overview screenshot preview" });
    fireEvent.error(thumbnail);

    expect(within(artifacts).getByText("Preview unavailable")).toBeInTheDocument();
  });

  it("prioritizes current context and collapses historical summaries", () => {
    const report = baseReport();
    report.summaries.unshift({
      ...report.summaries[0],
      summary_id: "wsum_fresh",
      text: "Fresh reviewer summary.",
      freshness: "fresh",
      source_revision_key: "rev-current",
      generated_at: "2026-06-21T00:08:00Z",
      created_at: "2026-06-21T00:08:00Z",
      updated_at: "2026-06-21T00:08:00Z",
    });
    report.summary_claims.unshift({
      ...report.summary_claims[0],
      claim_id: "wclaim_fresh",
      summary_id: "wsum_fresh",
      claim_text: "Fresh context is current.",
      freshness: "fresh",
      created_at: "2026-06-21T00:08:00Z",
    });

    render(<WorkInspectorView report={report} />);
    fireEvent.click(screen.getByRole("tab", { name: "Context" }));
    const context = screen.getByRole("tabpanel");

    expect(within(context).getByText("current summary")).toBeInTheDocument();
    expect(within(context).getByText("Fresh reviewer summary.")).toBeInTheDocument();
    expect(within(context).getByText("Fresh context is current.")).toBeInTheDocument();
    expect(within(context).getByText("Historical summaries and claims (2)")).toBeInTheDocument();
  });

  it("uses typed timeline items instead of the raw event list", () => {
    render(<WorkInspectorView report={baseReport()} />);

    fireEvent.click(screen.getByRole("tab", { name: "Timeline" }));
    const timeline = screen.getByRole("tabpanel");

    expect(timeline).toHaveTextContent("Work session opened");
    expect(timeline).toHaveTextContent("Observed cargo test exited 101");
    expect(timeline).not.toHaveTextContent("Observed redacted command output.");
  });

  it("keeps agent handoff JSON collapsed and renders only safe_json when expanded", () => {
    const report = baseReport();
    const unsafePayload = { secret: "/home/daddy/private-token" };
    (report.raw_redacted_json as typeof report.raw_redacted_json & { unsafe_json?: unknown }).unsafe_json = unsafePayload;

    render(<WorkInspectorView report={report} />);
    fireEvent.click(screen.getByRole("tab", { name: "Agent handoff" }));

    expect(screen.getByRole("button", { name: "Show payload" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("safe-visible-marker")).not.toBeInTheDocument();
    expect(screen.queryByText("/home/daddy/private-token")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Show payload" }));
    expect(screen.getByRole("button", { name: "Hide payload" })).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText(/safe-visible-marker/)).toBeInTheDocument();
    expect(screen.queryByText("/home/daddy/private-token")).not.toBeInTheDocument();
  });

  it("renders missing-evidence and no-artifact states without clipping core controls", () => {
    const report = baseReport();
    report.trust = {
      verdict: "missing_evidence",
      reason: "No current validation evidence exists.",
      recommended_next_action: "Run the focused validation commands.",
      open_risks: [],
    };
    report.evidence_summary = {
      total: 0,
      passing: 0,
      failing: 0,
      stale: 0,
      missing: 1,
    };
    report.evidence = [];
    report.commands = [];
    report.artifacts = [];
    report.artifact_summary = { total: 0, refs: [] };
    report.raw_transcript_available = true;

    render(<WorkInspectorView report={report} />);

    expect(screen.getByLabelText("Missing evidence")).toHaveTextContent("Run the focused validation commands.");
    expect(screen.getAllByText("Full transcripts stay local; this view uses share-safe summaries.").length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("tab", { name: "Commands" }));
    expect(screen.getByText("No commands have been recorded.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: "Artifacts" }));
    expect(screen.getByText("No artifacts have been recorded.")).toBeInTheDocument();
  });
});
