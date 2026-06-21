import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { WorkspaceWorkReport } from "@ctx/types";
import { WorkReportView } from "./WorkReportView";

const baseReport = (): WorkspaceWorkReport => ({
  work: {
    work_id: "wrk_1234567890",
    workspace_id: "workspace-1",
    title: "Stabilize Work report route",
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
  trust: {
    verdict: "failed",
    reason: "At least one linked evidence item failed.",
    recommended_next_action: "Fix the failing evidence before marking this ready.",
    open_risks: ["At least one linked evidence item failed."],
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
      workspace_id: "workspace-1",
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
      output_ref: null,
      artifact_ref: null,
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
      workspace_id: "workspace-1",
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
        title: "Review Work report route",
        url: "https://github.com/ctxrs/ctx/pull/123",
        state: "draft",
      },
    ],
    commits: ["abcdef1234567890"],
  },
  change_sets: [],
  contributions: [],
  summaries: [
    {
      summary_id: "wsum_1",
      work_id: "wrk_1234567890",
      workspace_id: "workspace-1",
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
  summary_claims: [],
  timeline: [
    {
      event_id: "wev_1",
      work_id: "wrk_1234567890",
      workspace_id: "workspace-1",
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
  duplicate_strong_links: [],
  raw_transcript_available: false,
  raw_transcript_included: false,
});

describe("WorkReportView", () => {
  it("renders reviewer-critical trust, evidence, and raw-detail status", () => {
    const onRefresh = vi.fn();
    render(<WorkReportView report={baseReport()} onRefresh={onRefresh} />);

    expect(screen.getByRole("heading", { name: "Stabilize Work report route" })).toBeInTheDocument();
    expect(screen.getByLabelText("Work trust")).toHaveTextContent("failed");
    expect(screen.getByLabelText("Linked change")).toHaveTextContent("Review Work report route");
    expect(screen.getByLabelText("Linked change")).toHaveTextContent("commit abcdef123456");
    expect(screen.getByText("Fix the failing evidence before marking this ready.")).toBeInTheDocument();
    expect(screen.getByText("Observed cargo test exited 101")).toBeInTheDocument();
    expect(screen.getAllByText("worktree").length).toBeGreaterThan(0);
    expect(screen.getAllByText("exact").length).toBeGreaterThan(0);
    expect(screen.getAllByText("medium").length).toBeGreaterThan(0);
    expect(screen.getByText("Raw transcripts are not available in this report response.")).toBeInTheDocument();
    expect(screen.queryByText("payload_json")).not.toBeInTheDocument();
    expect(screen.queryByText("/home/daddy")).not.toBeInTheDocument();
  });

  it("renders first-viewport missing evidence and raw transcript availability states", () => {
    const report = baseReport();
    report.evidence_summary = {
      total: 0,
      passing: 0,
      failing: 0,
      stale: 0,
      missing: 1,
    };
    report.evidence = [];
    report.trust = {
      verdict: "missing_evidence",
      reason: "No evidence has been recorded for this Work record.",
      recommended_next_action: "Add evidence with ctx work evidence.",
      open_risks: ["No evidence has been recorded for this Work record."],
    };
    report.raw_transcript_available = true;

    render(<WorkReportView report={report} />);

    expect(screen.getByLabelText("Missing evidence")).toHaveTextContent("Evidence is missing");
    expect(screen.getByLabelText("Evidence summary")).toHaveTextContent("Missing");
    expect(screen.getByText("Raw transcripts are available locally but not included by default.")).toBeInTheDocument();
    expect(screen.getAllByText("No evidence has been recorded for this Work record.").length).toBeGreaterThan(0);
  });
});
