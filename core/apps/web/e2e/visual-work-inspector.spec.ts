import { test, expect } from "./fixtures";
import {
  buildVisualName,
  captureVisual,
  prepareVisualPage,
  visualViewportLabel,
  type VisualTheme,
  type VisualViewportName,
} from "./utils/visual";

const THEMES = ["dark", "light"] as const satisfies VisualTheme[];
const workspaceId = "visual-workspace";
const png1x1 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+lmZYAAAAASUVORK5CYII=";

const okHealth = {
  version: "1.0.0",
  daemon_version: "1.0.0",
  pid: 1,
  data_root: "/tmp/ctx",
  daemon_url: "http://127.0.0.1:4399",
  auth_required: false,
  compatibility: {
    desktop_exact_version: "1.0.0",
    mobile_api_min: 1,
    mobile_api_max: 1,
  },
};

function inspectorReport(opts: {
  workId: string;
  title: string;
  trust: "failed" | "missing_evidence" | "verified";
  noEvidence?: boolean;
  unsafeArtifact?: boolean;
  longText?: boolean;
}) {
  const noEvidence = Boolean(opts.noEvidence);
  const longText = opts.longText
    ? "The inspector fixture includes a deliberately long objective, repeated validation notes, and enough detail to wrap through multiple lines without relying on raw JSON fields or private local paths. "
    : "Review the typed Work inspector fixture.";
  const evidence = noEvidence
    ? []
    : [
        {
          evidence_id: "wev_visual_test",
          work_id: opts.workId,
          workspace_id: workspaceId,
          kind: "test",
          status: opts.trust === "failed" ? "observed_fail" : "observed_pass",
          freshness: opts.trust === "verified" ? "fresh" : "stale",
          claim: opts.trust === "failed" ? "Focused web test failed with one assertion." : "Focused web test passed.",
          command: "pnpm --dir core/apps/web test -- WorkReportView.test.tsx",
          argv: ["pnpm", "--dir", "core/apps/web", "test", "--", "WorkReportView.test.tsx"],
          cwd: "[redacted:workspace_root]",
          exit_code: opts.trust === "failed" ? 1 : 0,
          head_sha: "abcdef1234567890",
          branch: "ctx/agent-work-semantics-primary",
          output_ref: null,
          artifact_ref: null,
          source: "worktree",
          fidelity: "exact",
          trust: "medium",
          started_at: "2026-06-21T13:00:00Z",
          finished_at: "2026-06-21T13:01:00Z",
          created_at: "2026-06-21T13:01:00Z",
          updated_at: "2026-06-21T13:01:00Z",
          schema_version: 1,
        },
      ];
  const artifacts = opts.unsafeArtifact
    ? [
        {
          id: "artifact_unsafe",
          kind: "screenshot",
          label: "Unsafe artifact URL was suppressed",
          missing: true,
          unavailable_reason: "artifact URL did not pass the safe route checks",
          render_kind: "unavailable",
          created_at: "2026-06-21T13:03:00Z",
        },
      ]
    : noEvidence
      ? []
      : [
          {
            id: "artifact_visual",
            artifact_id: "22222222-2222-4222-8222-222222222222",
            source_kind: "evidence",
            source_id: "wev_visual_test",
            kind: "screenshot",
            label: "Work inspector overview screenshot",
            display_name: "screenshots/work-inspector-overview.png",
            mime_type: "image/png",
            bytes: 2048,
            missing: false,
            render_kind: "raster_image",
            open_url: `/api/workspaces/${workspaceId}/work/${opts.workId}/artifacts/artifact_visual`,
            download_url: `/api/workspaces/${workspaceId}/work/${opts.workId}/artifacts/artifact_visual`,
            thumbnail_url: `/api/workspaces/${workspaceId}/work/${opts.workId}/artifacts/artifact_visual`,
            preview_url: `/api/workspaces/${workspaceId}/work/${opts.workId}/artifacts/artifact_visual`,
            created_at: "2026-06-21T13:03:00Z",
          },
        ];

  return {
    work: {
      work_id: opts.workId,
      workspace_id: workspaceId,
      title: opts.title,
      objective: `${longText}${longText}`,
      lifecycle: noEvidence ? "active" : "ready_for_review",
      primary_branch: "ctx/agent-work-semantics-primary",
      base_commit: "1234567890abcdef",
      head_commit: "abcdef1234567890",
      trust_verdict: opts.trust,
      summary_freshness: noEvidence ? "missing" : "stale",
      created_at: "2026-06-21T12:00:00Z",
      updated_at: "2026-06-21T13:05:00Z",
      schema_version: 1,
    },
    links: [],
    overview: {
      title: opts.title,
      objective: `${longText}${longText}`,
      lifecycle: noEvidence ? "active" : "ready_for_review",
      primary_branch: "ctx/agent-work-semantics-primary",
      base_commit: "1234567890abcdef",
      head_commit: "abcdef1234567890",
      created_at: "2026-06-21T12:00:00Z",
      updated_at: "2026-06-21T13:05:00Z",
    },
    trust: {
      verdict: opts.trust,
      reason:
        opts.trust === "missing_evidence"
          ? "No current validation evidence exists."
          : opts.trust === "failed"
            ? "At least one linked evidence item failed."
            : "Linked evidence is fresh and passing.",
      recommended_next_action:
        opts.trust === "verified" ? "Ready for reviewer handoff." : "Run or fix focused validation before review.",
      open_risks: opts.trust === "verified" ? [] : ["Validation state is not ready for reviewer signoff."],
    },
    context: {
      value: {
        objective: longText,
        recommended_next_action: "Run or fix focused validation before review.",
        raw_transcript_included: false,
      },
      redacted: true,
      redaction_notes: ["visual fixture"],
    },
    safe_json: { value: { safe_marker: "visual-safe-json" }, redacted: true, redaction_notes: ["visual fixture"] },
    raw_redacted_json: { value: { safe_marker: "visual-safe-json" }, redacted: true, redaction_notes: ["visual fixture"] },
    evidence_summary: {
      total: evidence.length,
      passing: opts.trust === "verified" ? evidence.length : 0,
      failing: opts.trust === "failed" ? 1 : 0,
      stale: opts.trust === "verified" ? 0 : evidence.length,
      missing: noEvidence ? 1 : 0,
    },
    evidence,
    change_summary: {
      change_sets: noEvidence ? 0 : 1,
      contributions: noEvidence ? 0 : 2,
      pull_requests: [
        {
          provider: "github",
          owner: "ctxrs",
          repo: "ctx",
          number: 456,
          title: "Work Inspector visual slice",
          url: "https://github.com/ctxrs/ctx/pull/456",
          state: "draft",
        },
      ],
      commits: ["abcdef1234567890"],
    },
    artifact_summary: { total: artifacts.length, refs: [] },
    change_sets: [
      {
        files: [
          { path: "core/apps/web/src/pages/workReport/WorkReportView.tsx", additions: 180, deletions: 40 },
          { path: "core/apps/web/src/styles/work-report.css", additions: 220, deletions: 90 },
        ],
      },
    ],
    contributions: [],
    summaries: [
      {
        summary_id: "wsum_visual",
        work_id: opts.workId,
        workspace_id: workspaceId,
        kind: "report_summary",
        audience: "reviewer",
        text: `${longText}The visible report uses typed commands, artifacts, timeline items, changes, claims, and safe redacted context.`,
        structured_json: null,
        generation_method: "deterministic",
        provider: null,
        model: null,
        template: "ctx.visual.work-inspector.v1",
        source_material_left_machine: false,
        freshness: noEvidence ? "missing" : "stale",
        source_revision_key: "visual-rev",
        generated_at: "2026-06-21T13:04:00Z",
        created_at: "2026-06-21T13:04:00Z",
        updated_at: "2026-06-21T13:04:00Z",
        schema_version: 1,
      },
    ],
    summary_claims: [],
    timeline: [],
    transcript: [
      {
        event_id: "msg_visual_user",
        sequence: 1,
        event_type: "user_message",
        event_time: "2026-06-21T12:01:00Z",
        actor_kind: "human",
        redaction_class: "local_redacted",
        text_preview: "Please implement the Work Inspector visual completeness slice.",
      },
      {
        event_id: "msg_visual_agent",
        sequence: 2,
        event_type: "assistant_message",
        event_time: "2026-06-21T12:02:00Z",
        actor_kind: "agent",
        redaction_class: "local_redacted",
        text_preview: longText,
      },
    ],
    commands: noEvidence
      ? []
      : [
          {
            id: "cmd_visual",
            evidence_id: "wev_visual_test",
            command: "pnpm --dir core/apps/web test -- WorkReportView.test.tsx",
            argv: ["pnpm", "--dir", "core/apps/web", "test", "--", "WorkReportView.test.tsx"],
            cwd: "[redacted:workspace_root]",
            exit_code: opts.trust === "failed" ? 1 : 0,
            status: opts.trust === "failed" ? "observed_fail" : "observed_pass",
            freshness: opts.trust === "verified" ? "fresh" : "stale",
            stdout_preview: opts.trust === "failed" ? "1 failing assertion in the visual fixture." : "7 tests passed.",
            stderr_preview: null,
            output_truncated: false,
            started_at: "2026-06-21T13:00:00Z",
            finished_at: "2026-06-21T13:01:00Z",
          },
        ],
    artifacts,
    timeline_items: [
      {
        sequence: 1,
        event_time: "2026-06-21T12:01:00Z",
        kind: "user_message",
        title: "User requested the Work Inspector visual completeness slice.",
        detail: "session",
        source_event_id: "msg_visual_user",
      },
      {
        sequence: 2,
        event_time: "2026-06-21T13:01:00Z",
        kind: noEvidence ? "note" : "test",
        title: noEvidence ? "No validation evidence has been recorded." : "Focused web validation completed.",
        detail: noEvidence ? "missing evidence" : "typed evidence",
        source_evidence_id: noEvidence ? undefined : "wev_visual_test",
      },
    ],
    duplicate_strong_links: [],
    raw_transcript_available: true,
    raw_transcript_included: false,
  };
}

async function openWorkInspectorVisualPage(
  page: Parameters<typeof prepareVisualPage>[0],
  report: ReturnType<typeof inspectorReport>,
  opts: { theme: VisualTheme; viewport: VisualViewportName; tab?: string },
) {
  await page.route("**/api/health", async (route) => {
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(okHealth) });
  });
  await page.route("**/api/workspaces/*/work/*/inspector", async (route) => {
    await route.fulfill({ contentType: "application/json", body: JSON.stringify(report) });
  });
  await page.route("**/api/workspaces/*/work/*/artifacts/**", async (route) => {
    await route.fulfill({ contentType: "image/png", body: Buffer.from(png1x1, "base64") });
  });
  await prepareVisualPage(page, {
    theme: opts.theme,
    viewport: opts.viewport,
    route: `/workspaces/${workspaceId}/work/${report.work.work_id}`,
    ready: page.locator(".work-report-page"),
  });
  if (opts.tab) {
    await page.getByRole("tab", { name: opts.tab }).click();
  }
  await expect(page.locator(".work-report-page")).toBeVisible();
}

test.describe.serial("visual: work inspector", () => {
  for (const theme of THEMES) {
    test(`long failure overview ${theme}`, async ({ page }) => {
      const report = inspectorReport({
        workId: `wrk_visual_long_${theme}`,
        title: "Long failing Work inspector fixture with wrapping pressure",
        trust: "failed",
        longText: true,
      });
      await openWorkInspectorVisualPage(page, report, { theme, viewport: "desktop" });
      await captureVisual(
        page,
        buildVisualName(["work-inspector", "long-failure-overview", theme, visualViewportLabel("desktop")]),
        { ready: page.locator(".work-report-page") },
      );
    });

    test(`no evidence mobile ${theme}`, async ({ page }) => {
      const report = inspectorReport({
        workId: `wrk_visual_no_evidence_${theme}`,
        title: "No evidence Work inspector fixture",
        trust: "missing_evidence",
        noEvidence: true,
      });
      await openWorkInspectorVisualPage(page, report, { theme, viewport: "mobile-narrow" });
      await captureVisual(
        page,
        buildVisualName(["work-inspector", "no-evidence", theme, visualViewportLabel("mobile-narrow")]),
        { ready: page.locator(".work-report-page") },
      );
    });
  }

  test("unsafe artifacts tight dark", async ({ page }) => {
    const report = inspectorReport({
      workId: "wrk_visual_unsafe_artifacts",
      title: "Unsafe artifact URL Work inspector fixture",
      trust: "failed",
      unsafeArtifact: true,
    });
    await openWorkInspectorVisualPage(page, report, { theme: "dark", viewport: "desktop-tight", tab: "Artifacts" });
    await captureVisual(
      page,
      buildVisualName(["work-inspector", "unsafe-artifacts", "dark", visualViewportLabel("desktop-tight")]),
      { ready: page.locator(".work-report-page") },
    );
  });
});
