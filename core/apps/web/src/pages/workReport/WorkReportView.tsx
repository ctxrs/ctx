import { useEffect, useMemo, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  Download,
  ExternalLink as ExternalLinkIcon,
  FileText,
  Image as ImageIcon,
  RefreshCw,
} from "lucide-react";
import type {
  JsonValue,
  WorkspaceWorkEvidence,
  WorkspaceWorkInspector,
  WorkspaceWorkInspectorArtifact,
  WorkspaceWorkInspectorCommand,
  WorkspaceWorkInspectorSubagent,
  WorkspaceWorkInspectorTimelineItem,
  WorkspaceWorkInspectorTranscriptItem,
  WorkspaceWorkReport,
  WorkspaceWorkSummary,
  WorkspaceWorkSummaryClaim,
  WorkspaceWorkTrustSummary,
} from "@ctx/types";
import { ExternalLink } from "../../components/ExternalLink";
import { authToken } from "../../api/clientBase";
import { getDaemonHttpUrl } from "../../api/daemonConnection";
import { buildDaemonRequestHeaders } from "../../api/daemonRequestHeaders";

type WorkInspectorTab =
  | "overview"
  | "transcript"
  | "subagents"
  | "commands"
  | "evidence"
  | "timeline"
  | "changes"
  | "artifacts"
  | "context"
  | "raw";

type WorkInspectorTabDef = {
  id: WorkInspectorTab;
  label: string;
  count?: number;
  emptyReason?: string;
};

const label = (value: string | null | undefined) =>
  String(value ?? "unknown").replaceAll("_", " ");

const shortSha = (value: string | null | undefined) => {
  if (!value) return "unknown";
  return value.length > 12 ? value.slice(0, 12) : value;
};

const headerBadges = (report: WorkspaceWorkInspector) =>
  [
    "Captured work",
    label(report.work.lifecycle),
    report.work.primary_branch ? `${report.work.primary_branch} branch` : null,
    report.work.summary_freshness ? `${label(report.work.summary_freshness)} summary` : null,
  ].filter(Boolean);

const trustClass = (verdict: string) => `work-report-trust work-report-trust-${verdict}`;

const evidenceClass = (item: WorkspaceWorkEvidence) =>
  `work-report-evidence-row work-report-evidence-${item.status} work-report-freshness-${item.freshness}`;

const prettyJson = (value: JsonValue) => JSON.stringify(value, null, 2);

const asRecord = (value: JsonValue | null | undefined): Record<string, JsonValue> | null => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value;
};

const asArray = (value: JsonValue | null | undefined): JsonValue[] => (Array.isArray(value) ? value : []);

const pickString = (record: Record<string, JsonValue> | null, keys: string[]) => {
  if (!record) return null;
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) return value;
    if (typeof value === "number") return String(value);
  }
  return null;
};

const pickNumber = (record: Record<string, JsonValue> | null, keys: string[]) => {
  if (!record) return null;
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "number") return value;
    if (typeof value === "string" && value.trim() && Number.isFinite(Number(value))) return Number(value);
  }
  return null;
};

const safeExternalUrl = (value: string | null | undefined) => {
  if (!value) return null;
  try {
    const url = new URL(value);
    return url.protocol === "https:" || url.protocol === "http:" ? value : null;
  } catch {
    return null;
  }
};

const safeSameOriginPath = (value: string | null | undefined) => {
  if (!value) return null;
  const trimmed = value.trim();
  if (!trimmed || trimmed.startsWith("//") || !trimmed.startsWith("/") || trimmed.includes("\\")) return null;
  return trimmed;
};

const WORK_ARTIFACT_PATH_PATTERN =
  /^\/api\/workspaces\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\/work\/[^/?#]+\/artifacts\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

const safeWorkArtifactPath = (value: string | null | undefined) => {
  const path = safeSameOriginPath(value);
  if (!path || !WORK_ARTIFACT_PATH_PATTERN.test(path)) return null;
  return path;
};

const safeDisplayPath = (value: string | null | undefined) => {
  if (!value) return null;
  const trimmed = value.trim();
  if (!trimmed || trimmed.startsWith("/") || trimmed.includes("\\") || trimmed.includes("..")) return null;
  return trimmed;
};

const safeArtifactUrl = (value: string | null | undefined) => safeSameOriginPath(value) ?? safeExternalUrl(value);

const fetchArtifactBlob = async (path: string): Promise<Blob> => {
  const response = await fetch(getDaemonHttpUrl(path), {
    headers: buildDaemonRequestHeaders({ token: authToken() }),
  });
  if (!response.ok) {
    throw new Error(`artifact fetch failed (${response.status})`);
  }
  return response.blob();
};

const useArtifactBlobUrl = (path: string | null) => {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (!path) {
      setUrl(null);
      setFailed(false);
      return;
    }
    let cancelled = false;
    let objectUrl: string | null = null;
    setUrl(null);
    setFailed(false);
    fetchArtifactBlob(path)
      .then((blob) => {
        if (cancelled) return;
        objectUrl = URL.createObjectURL(blob);
        setUrl(objectUrl);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL?.(objectUrl);
    };
  }, [path]);
  return { url, failed };
};

const renderSafeLink = (
  value: string | null | undefined,
  labelText: ReactNode,
  className?: string,
) => {
  const href = safeArtifactUrl(value);
  if (!href) return null;
  if (safeExternalUrl(href)) {
    return (
      <ExternalLink className={className} href={href}>
        {labelText}
      </ExternalLink>
    );
  }
  return (
    <a className={className} data-allow-raw-anchor href={href}>
      {labelText}
    </a>
  );
};

const formatDate = (value: string | null | undefined) => {
  if (!value) return null;
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return value;
  return date.toLocaleString();
};

const durationLabel = (started: string | null | undefined, finished: string | null | undefined) => {
  if (!started || !finished) return null;
  const start = new Date(started).valueOf();
  const end = new Date(finished).valueOf();
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) return null;
  const ms = end - start;
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)}s`;
};

const bytesLabel = (bytes: number | null | undefined) => {
  if (typeof bytes !== "number" || !Number.isFinite(bytes)) return null;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
};

const rawTranscriptStatus = (report: WorkspaceWorkInspector) => {
  if (report.raw_transcript_included) {
    return "Full transcript detail is included in this response; review redaction before sharing.";
  }
  if (report.raw_transcript_available) {
    return "Full transcripts stay local; this view uses share-safe summaries.";
  }
  return "Full transcripts are not available in this inspector response.";
};

const artifactUrlFromRef = (value: JsonValue | null | undefined) => {
  const record = asRecord(value);
  return safeWorkArtifactPath(pickString(record, ["download_url", "thumbnail_url"]));
};

const artifactPathFromRef = (value: JsonValue | null | undefined) =>
  safeDisplayPath(pickString(asRecord(value), ["display_path", "relative_path", "name"]));

const artifactMimeFromRef = (value: JsonValue | null | undefined) =>
  pickString(asRecord(value), ["mime_type", "mime"]);

const reportToInspector = (report: WorkspaceWorkReport): WorkspaceWorkInspector => {
  const evidence = report.evidence.map(({ output_ref: _outputRef, artifact_ref: _artifactRef, ...item }) => item);
  const commands = report.evidence
    .filter((item) => item.command || item.argv.length > 0)
    .map((item): WorkspaceWorkInspectorCommand => ({
      id: item.evidence_id,
      evidence_id: item.evidence_id,
      command: item.command,
      argv: item.argv,
      cwd: item.cwd,
      cwd_label: null,
      exit_code: item.exit_code,
      status: item.status,
      freshness: item.freshness,
      stdout_preview: pickString(asRecord(item.output_ref), ["stdout_redacted", "stdout_preview"]),
      stderr_preview: pickString(asRecord(item.output_ref), ["stderr_redacted", "stderr_preview"]),
      output_truncated: Boolean(asRecord(item.output_ref)?.truncated),
      preview_limit_bytes: pickNumber(asRecord(item.output_ref), ["preview_limit_bytes"]),
      stdout_size_bytes: pickNumber(asRecord(item.output_ref), ["stdout_size_bytes"]),
      stderr_size_bytes: pickNumber(asRecord(item.output_ref), ["stderr_size_bytes"]),
      stdout_sha256: pickString(asRecord(item.output_ref), ["stdout_sha256"]),
      stderr_sha256: pickString(asRecord(item.output_ref), ["stderr_sha256"]),
      stdout_truncated: Boolean(asRecord(item.output_ref)?.stdout_truncated),
      stderr_truncated: Boolean(asRecord(item.output_ref)?.stderr_truncated),
      started_at: item.started_at,
      finished_at: item.finished_at,
    }));
  const artifacts = report.evidence
    .filter((item) => item.artifact_ref)
    .map((item): WorkspaceWorkInspectorArtifact => ({
      id: item.evidence_id,
      kind: item.kind,
      label: item.claim || item.command || item.evidence_id,
      source_kind: "evidence",
      source_id: item.evidence_id,
      display_name: artifactPathFromRef(item.artifact_ref) ?? undefined,
      mime_type: artifactMimeFromRef(item.artifact_ref),
      missing: true,
      unavailable_reason: "artifact metadata requires the Work inspector v2 route",
      render_kind: "unavailable",
      download_url: artifactUrlFromRef(item.artifact_ref),
      created_at: item.created_at,
    }));
  const timelineItems = report.timeline.map((event) => ({
    sequence: event.sequence,
    event_time: event.event_time,
    kind: event.event_type,
    title: event.redacted_text || event.event_type,
    detail: event.source_kind,
    source_event_id: event.event_id,
  }));
  const overview = {
    title: report.work.title,
    objective: report.work.objective,
    lifecycle: report.work.lifecycle,
    primary_branch: report.work.primary_branch,
    base_commit: report.work.base_commit,
    head_commit: report.work.head_commit,
    created_at: report.work.created_at,
    updated_at: report.work.updated_at,
  };
  const safeValue = {
    work: report.work,
    links: report.links.map(({ target_json: _targetJson, ...link }) => link),
    overview,
    trust: report.trust,
    evidence_summary: report.evidence_summary,
    change_summary: report.change_summary,
    transcript: report.timeline.map((event) => ({
      event_id: event.event_id,
      sequence: event.sequence,
      event_type: event.event_type,
      event_time: event.event_time,
      actor_kind: event.actor_kind,
      redaction_class: event.redaction_class,
      text_preview: event.redacted_text,
    })),
    commands,
    artifacts,
    evidence,
    summaries: report.summaries,
    summary_claims: report.summary_claims,
    timeline: report.timeline,
    timeline_items: timelineItems,
    subagents: [],
    duplicate_strong_links: report.duplicate_strong_links,
    raw_transcript_available: report.raw_transcript_available,
    raw_transcript_included: false,
  } as unknown as JsonValue;

  return {
    ...report,
    overview,
    transcript: report.timeline.map((event): WorkspaceWorkInspectorTranscriptItem => ({
      event_id: event.event_id,
      sequence: event.sequence,
      event_type: event.event_type,
      actor_kind: event.actor_kind,
      event_time: event.event_time,
      redaction_class: event.redaction_class,
      text_preview: event.redacted_text,
    })),
    commands,
    artifacts,
    evidence,
    change_sets: [],
    contributions: [],
    artifact_summary: {
      total: artifacts.length,
      refs: [],
    },
    context: {
      value: safeValue,
      redacted: true,
      redaction_notes: ["compatibility projection from v1 report"],
    },
    safe_json: {
      value: safeValue,
      redacted: true,
      redaction_notes: ["compatibility projection from v1 report"],
    },
    raw_redacted_json: {
      value: safeValue,
      redacted: true,
      redaction_notes: ["compatibility projection from v1 report"],
    },
    timeline_items: timelineItems,
    subagents: [],
  };
};

function Empty({ children }: { children: string }) {
  return <p className="work-report-empty">{children}</p>;
}

function Metric({ label: labelText, value }: { label: string; value: string | number }) {
  return (
    <div className="work-report-metric">
      <span className="work-report-eyebrow">{labelText}</span>
      <strong>{value}</strong>
    </div>
  );
}

function SectionSummary({
  label: labelText,
  items,
}: {
  label: string;
  items: Array<{ label: string; value: string | number; note?: string }>;
}) {
  return (
    <div className="work-report-section-summary" aria-label={labelText}>
      {items.map((item) => (
        <div key={`${item.label}:${item.value}`}>
          <span className="work-report-eyebrow">{item.label}</span>
          <strong>{item.value}</strong>
          {item.note ? <p>{item.note}</p> : null}
        </div>
      ))}
    </div>
  );
}

export function WorkInspectorHeader({
  report,
  onRefresh,
}: {
  report: WorkspaceWorkInspector;
  onRefresh?: () => void;
}) {
  const title = report.work.title || "Untitled Work";
  return (
    <header className="work-report-header">
      <div>
        <span className="work-report-eyebrow">Work Inspector</span>
        <h1>{title}</h1>
        <div className="work-report-meta">
          {headerBadges(report).map((badge) => (
            <span key={badge}>{badge}</span>
          ))}
          {report.work.head_commit ? <span>commit {shortSha(report.work.head_commit)}</span> : null}
        </div>
      </div>
      {onRefresh ? (
        <button className="work-report-refresh" type="button" onClick={onRefresh}>
          <RefreshCw aria-hidden="true" size={15} />
          <span>Refresh</span>
        </button>
      ) : null}
    </header>
  );
}

export function TrustBanner({ trust }: { trust: WorkspaceWorkTrustSummary }) {
  const Icon = trust.verdict === "verified" ? CheckCircle2 : AlertTriangle;
  return (
    <section className={trustClass(trust.verdict)} aria-label="Work trust">
      <div className="work-report-trust-title">
        <Icon aria-hidden="true" size={18} />
        <div>
          <span className="work-report-eyebrow">Trust</span>
          <strong>{label(trust.verdict)}</strong>
        </div>
      </div>
      <p>{trust.reason}</p>
      <div className="work-report-next">{trust.recommended_next_action}</div>
    </section>
  );
}

export function InspectorMetricStrip({ report }: { report: WorkspaceWorkInspector }) {
  return (
    <section className="work-report-summary-grid" aria-label="Evidence summary">
      <Metric label="Evidence" value={report.evidence_summary.total} />
      <Metric label="Passing" value={report.evidence_summary.passing} />
      <Metric label="Failing" value={report.evidence_summary.failing} />
      <Metric label="Stale" value={report.evidence_summary.stale} />
      <Metric label="Commands" value={report.commands.length} />
      <Metric label="Artifacts" value={report.artifact_summary.total || report.artifacts.length} />
      <Metric label="Subagents" value={report.subagents.length} />
      <Metric label="Changes" value={changeSignalCount(report)} />
      <Metric label="Summaries" value={label(report.work.summary_freshness)} />
    </section>
  );
}

export function InspectorTabs({
  tabs,
  selected,
  onSelect,
}: {
  tabs: WorkInspectorTabDef[];
  selected: WorkInspectorTab;
  onSelect: (tab: WorkInspectorTab) => void;
}) {
  const moveTabFocus = (nextIndex: number) => {
    const boundedIndex = (nextIndex + tabs.length) % tabs.length;
    const nextTab = tabs[boundedIndex];
    onSelect(nextTab.id);
    window.requestAnimationFrame(() => {
      document.getElementById(`work-report-tab-${nextTab.id}`)?.focus();
    });
  };
  const handleTabKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    if (event.key === "ArrowRight" || event.key === "ArrowDown") {
      event.preventDefault();
      moveTabFocus(index + 1);
    } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
      event.preventDefault();
      moveTabFocus(index - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      moveTabFocus(0);
    } else if (event.key === "End") {
      event.preventDefault();
      moveTabFocus(tabs.length - 1);
    }
  };
  return (
    <nav className="work-report-tabs" role="tablist" aria-label="Work Inspector sections">
      {tabs.map((tab, index) => (
        <button
          aria-controls={`work-report-panel-${tab.id}`}
          aria-selected={selected === tab.id}
          aria-label={tab.label}
          className={tab.emptyReason ? "work-report-tab-sparse" : undefined}
          id={`work-report-tab-${tab.id}`}
          key={tab.id}
          role="tab"
          tabIndex={selected === tab.id ? 0 : -1}
          type="button"
          onKeyDown={(event) => handleTabKeyDown(event, index)}
          onClick={() => onSelect(tab.id)}
        >
          <span>{tab.label}</span>
          {typeof tab.count === "number" ? (
            <span className="work-report-tab-count">{tab.emptyReason ?? tab.count}</span>
          ) : null}
        </button>
      ))}
    </nav>
  );
}

function overviewSummary(report: WorkspaceWorkInspector) {
  return report.summaries.find((summary) => summary.audience === "reviewer") ?? report.summaries[0] ?? null;
}

function completenessRows(report: WorkspaceWorkInspector) {
  const changes = changeSignalCount(report);
  const availableArtifacts = usefulArtifactCount(report);
  return [
    {
      label: "Transcript",
      count: report.transcript.length,
      ok: report.transcript.length > 1,
      note: report.transcript.length ? "redacted session context captured" : "not captured",
    },
    {
      label: "Subagents",
      count: report.subagents.length,
      ok: report.subagents.length > 0,
      note: report.subagents.length ? "child session context captured" : "no child sessions recorded",
    },
    {
      label: "Commands",
      count: report.commands.length,
      ok: report.commands.length > 0,
      note: report.commands.length ? "redacted output previews available" : "not captured",
    },
    {
      label: "Changes",
      count: changes,
      ok: changes > 0,
      note: changes ? "commit, PR, or file metadata captured" : "no changed-file metadata",
    },
    {
      label: "Evidence",
      count: report.evidence.length,
      ok: report.evidence_summary.passing > 0,
      note: report.evidence_summary.passing ? "passing evidence present" : "no passing evidence",
    },
    {
      label: "Artifacts",
      count: availableArtifacts,
      ok: availableArtifacts > 0,
      note: availableArtifacts
        ? "safe artifact previews or downloads available"
        : report.artifacts.length
          ? "artifact refs exist but are unavailable"
          : "not captured",
    },
  ];
}

function changeSignalCount(report: WorkspaceWorkInspector) {
  return changedFileCount(report) + report.change_summary.commits.length + report.change_summary.pull_requests.length;
}

function InspectorCompleteness({ report }: { report: WorkspaceWorkInspector }) {
  return (
    <section className="work-report-panel" aria-label="Inspector completeness">
      <div className="work-report-panel-header">
        <h2>Inspector completeness</h2>
        <span>share-safe capture</span>
      </div>
      <div className="work-report-completeness-grid">
        {completenessRows(report).map((row) => (
          <div className={row.ok ? "work-report-completeness-ok" : "work-report-completeness-missing"} key={row.label}>
            <div>
              <strong>{row.label}</strong>
              <span>{row.count}</span>
            </div>
            <p>{row.note}</p>
          </div>
        ))}
      </div>
    </section>
  );
}

export function OverviewTab({ report }: { report: WorkspaceWorkInspector }) {
  const missingEvidence =
    report.evidence_summary.missing > 0 || report.trust.verdict === "missing_evidence";
  const summary = overviewSummary(report);
  return (
    <div className="work-report-tab-stack">
      <TrustBanner trust={report.trust} />
      {missingEvidence ? (
        <section className="work-report-warning" aria-label="Missing evidence">
          <strong>Evidence is missing</strong>
          <p>{report.trust.recommended_next_action}</p>
        </section>
      ) : null}
      {report.duplicate_strong_links.length > 0 ? (
        <section className="work-report-warning" aria-label="Duplicate Work links">
          <strong>Merge-needed links</strong>
          {report.duplicate_strong_links.map((item) => (
            <p key={`${item.target_kind}:${item.target_id}`}>
              {label(item.target_kind)} {item.target_id} is linked to {item.work_ids.length} Work records.
          </p>
        ))}
      </section>
      ) : null}
      <InspectorCompleteness report={report} />
      <section className="work-report-panel" aria-label="Objective">
        <div className="work-report-panel-header">
          <h2>Objective</h2>
          <span>{label(report.overview.lifecycle)}</span>
        </div>
        <p>{report.overview.objective || report.work.objective || "No objective has been recorded."}</p>
      </section>
      {summary ? (
        <section className="work-report-panel" aria-label="Reviewer summary">
          <div className="work-report-panel-header">
            <h2>Reviewer summary</h2>
            <span>{label(summary.freshness)}</span>
          </div>
          <p>{summary.text}</p>
        </section>
      ) : null}
      <section className="work-report-panel" aria-label="Inspector status">
        <h2>Inspector status</h2>
        <p>{rawTranscriptStatus(report)}</p>
      </section>
    </div>
  );
}

export function SubagentsTab({ subagents }: { subagents: WorkspaceWorkInspectorSubagent[] }) {
  const eventCount = subagents.reduce((total, subagent) => total + subagent.event_count, 0);
  const transcriptPreviewCount = subagents.reduce(
    (total, subagent) => total + subagent.transcript_preview.length,
    0,
  );
  return (
    <section className="work-report-panel" aria-label="Subagents">
      <div className="work-report-panel-header">
        <h2>Subagents</h2>
        <span>{subagents.length ? `${subagents.length} child sessions` : "none recorded"}</span>
      </div>
      {subagents.length ? (
        <>
          <SectionSummary
            label="Subagent capture summary"
            items={[
              { label: "Child sessions", value: subagents.length, note: "parent/child work is linked" },
              { label: "Subagent events", value: eventCount, note: "redacted contribution trail" },
              { label: "Preview entries", value: transcriptPreviewCount, note: "safe transcript excerpts" },
            ]}
          />
          <div className="work-report-evidence-list">
            {subagents.map((subagent) => {
              const outcome = subagent.transcript_preview.find(
                (item) => item.event_type === "assistant_message" && item.text_preview,
              );
              const contributionSummary = subagent.summary || outcome?.text_preview;
              return (
                <article className="work-report-subagent" key={subagent.id}>
                  <div className="work-report-panel-header">
                    <div>
                      <strong>{subagent.label || subagent.child_session_id || "Subagent session"}</strong>
                      <div className="work-report-meta">
                        {subagent.status ? <span>{label(subagent.status)}</span> : null}
                        <span>child reviewer</span>
                        {subagent.latest_event_time ? (
                          <time dateTime={subagent.latest_event_time}>{formatDate(subagent.latest_event_time)}</time>
                        ) : null}
                      </div>
                    </div>
                    <span>{subagent.event_count} events</span>
                  </div>
                  {contributionSummary ? (
                    <div className="work-report-callout">
                      <span className="work-report-eyebrow">Contribution</span>
                      <p>{contributionSummary}</p>
                    </div>
                  ) : null}
                  {subagent.transcript_preview.length ? (
                    <details className="work-report-details work-report-subagent-preview">
                      <summary>Review event trace</summary>
                      <ol className="work-report-stack">
                        {subagent.transcript_preview.map((item, index) => (
                          <li className="work-report-message" key={item.event_id || index}>
                            <div className="work-report-meta">
                              <span>{label(item.actor_kind)}</span>
                              <span>{label(item.event_type)}</span>
                              {item.event_time ? <time dateTime={item.event_time}>{formatDate(item.event_time)}</time> : null}
                            </div>
                            <p>{shareSafeTranscriptPreview(item)}</p>
                          </li>
                        ))}
                      </ol>
                    </details>
                  ) : (
                    <p className="work-report-ref">
                      Child session metadata was captured, but no redacted child transcript events are available.
                    </p>
                  )}
                </article>
              );
            })}
          </div>
        </>
      ) : (
        <Empty>No child or subagent sessions are linked to this Work record.</Empty>
      )}
    </section>
  );
}

export function TranscriptTab({ items }: { items: WorkspaceWorkInspectorTranscriptItem[] }) {
  const storyItems = transcriptStoryItems(items);
  return (
    <section className="work-report-panel" aria-label="Transcript">
      <div className="work-report-panel-header">
        <h2>Conversation Story</h2>
        <span>{items.length ? `${items.length} captured events` : "none recorded"}</span>
      </div>
      {items.length ? (
        <>
          <div className="work-report-narrative-grid">
            {storyItems.map((item) => (
              <article key={item.title}>
                <span className="work-report-eyebrow">{item.kicker}</span>
                <strong>{item.title}</strong>
                <p>{item.body}</p>
              </article>
            ))}
          </div>
          <details className="work-report-details">
            <summary>Share-safe event trace</summary>
            <ol className="work-report-stack">
              {items.map((item, index) => (
                <li className="work-report-message" key={item.event_id || item.id || index}>
                  <div className="work-report-meta">
                    <span>{label(item.actor_kind)}</span>
                    <span>{label(item.event_type)}</span>
                    {item.event_time ? <time dateTime={item.event_time}>{formatDate(item.event_time)}</time> : null}
                  </div>
                  <p>{shareSafeTranscriptPreview(item)}</p>
                </li>
              ))}
            </ol>
          </details>
        </>
      ) : (
        <Empty>No transcript entries are available.</Empty>
      )}
    </section>
  );
}

export function CommandsTab({ commands }: { commands: WorkspaceWorkInspectorCommand[] }) {
  const passing = commands.filter((command) => command.exit_code === 0).length;
  const outputPreviewCount = commands.filter(
    (command) => Boolean(command.stdout_preview) || Boolean(command.stderr_preview),
  ).length;
  return (
    <section className="work-report-panel" aria-label="Commands">
      <div className="work-report-panel-header">
        <h2>Commands</h2>
        <span>{commands.length ? `${commands.length} commands` : "none recorded"}</span>
      </div>
      {commands.length ? (
        <>
          <SectionSummary
            label="Command capture summary"
            items={[
              { label: "Commands", value: commands.length, note: "captured as evidence" },
              { label: "Exit 0", value: passing, note: "successful local checks" },
              { label: "Output previews", value: outputPreviewCount, note: "redacted stdout/stderr snippets" },
            ]}
          />
          <div className="work-report-evidence-list">
            {commands.map((command, index) => {
              const duration = durationLabel(command.started_at, command.finished_at);
              const hasTechnicalProof =
                typeof command.stdout_size_bytes === "number" ||
                typeof command.stderr_size_bytes === "number" ||
                command.stdout_sha256 ||
                command.stderr_sha256;
              return (
                <article className="work-report-command" key={command.id || index}>
                  <strong>{command.command || command.argv.join(" ") || command.id}</strong>
                  <div className="work-report-meta">
                    {command.status ? <span>{command.status === "observed_pass" ? "passed" : label(command.status)}</span> : null}
                    {command.freshness ? <span>{label(command.freshness)}</span> : null}
                    {command.cwd_label ? <span>{command.cwd_label}</span> : null}
                    {typeof command.exit_code === "number" ? <span>exit {command.exit_code}</span> : null}
                    {duration ? <span>{duration}</span> : null}
                    {command.output_truncated ? <span>output truncated</span> : null}
                  </div>
                  <p className="work-report-output">{commandOutputSummary(command)}</p>
                  {command.stdout_preview || command.stderr_preview ? (
                    <div className="work-report-command-previews" aria-label="Redacted command output previews">
                      {command.stdout_preview ? (
                        <pre><span>stdout</span>{command.stdout_preview}</pre>
                      ) : null}
                      {command.stderr_preview ? (
                        <pre><span>stderr</span>{command.stderr_preview}</pre>
                      ) : null}
                    </div>
                  ) : null}
                  {hasTechnicalProof ? (
                    <details className="work-report-details">
                      <summary>Validation proof</summary>
                      <div className="work-report-technical-proof">
                        {typeof command.stdout_size_bytes === "number" ? (
                          <span>stdout {bytesLabel(command.stdout_size_bytes)}</span>
                        ) : null}
                        {typeof command.stderr_size_bytes === "number" ? (
                          <span>stderr {bytesLabel(command.stderr_size_bytes)}</span>
                        ) : null}
                        {command.stdout_sha256 ? <span>stdout sha256 {command.stdout_sha256.slice(0, 12)}</span> : null}
                        {command.stderr_sha256 ? <span>stderr sha256 {command.stderr_sha256.slice(0, 12)}</span> : null}
                      </div>
                    </details>
                  ) : null}
                </article>
              );
            })}
          </div>
        </>
      ) : (
        <Empty>No commands have been recorded.</Empty>
      )}
    </section>
  );
}

function commandOutputSummary(command: WorkspaceWorkInspectorCommand) {
  if (command.stdout_preview || command.stderr_preview) {
    return "Output was captured and kept share-safe; raw stdout and stderr stay local.";
  }
  return "Command completed without stdout or stderr; exit code is the captured signal.";
}

export function EvidenceTab({ evidence }: { evidence: WorkspaceWorkEvidence[] }) {
  const passing = evidence.filter((item) => item.status === "observed_pass").length;
  const fresh = evidence.filter((item) => item.freshness === "fresh").length;
  const commandBacked = evidence.filter((item) => item.command || item.argv.length).length;
  return (
    <section className="work-report-panel work-report-evidence" aria-label="Evidence">
      <div className="work-report-panel-header">
        <h2>Evidence</h2>
        <span>{evidence.length ? `${evidence.length} observed` : "none recorded"}</span>
      </div>
      {evidence.length ? (
        <>
          <SectionSummary
            label="Evidence capture summary"
            items={[
              { label: "Passing", value: passing, note: "checks observed as green" },
              { label: "Fresh", value: fresh, note: "matches current capture" },
              { label: "Command-backed", value: commandBacked, note: "reproducible local commands" },
            ]}
          />
          <div className="work-report-evidence-list">
            {evidence.map((item) => (
              <article className={evidenceClass(item)} key={item.evidence_id}>
                <div>
                  <strong>{item.claim || item.command || item.evidence_id}</strong>
                  {item.command || item.argv.length ? <p>{item.command || item.argv.join(" ")}</p> : null}
                  <div className="work-report-evidence-detail">
                    <span>{label(item.source)}</span>
                    <span>{label(item.fidelity)}</span>
                    <span>{label(item.trust)}</span>
                    {item.head_sha ? <span>{shortSha(item.head_sha)}</span> : null}
                    {item.branch ? <span>{item.branch}</span> : null}
                  </div>
                </div>
                <div className="work-report-evidence-badges">
                  <span>{label(item.kind)}</span>
                  <span>{label(item.status)}</span>
                  <span>{label(item.freshness)}</span>
                </div>
              </article>
            ))}
          </div>
        </>
      ) : (
        <Empty>No evidence has been recorded for this Work record.</Empty>
      )}
    </section>
  );
}

export function TimelineTab({ items }: { items: WorkspaceWorkInspectorTimelineItem[] }) {
  const milestones = timelineMilestones(items);
  return (
    <section className="work-report-panel work-report-timeline" aria-label="Timeline">
      <div className="work-report-panel-header">
        <h2>Timeline</h2>
        <span>{items.length ? `${items.length} events` : "none recorded"}</span>
      </div>
      {items.length ? (
        <>
          <ol className="work-report-milestones">
            {milestones.map((item) => (
              <li key={`${item.title}:${item.time}`}>
                <div>
                  <span className="work-report-eyebrow">{item.kicker}</span>
                  <strong>{item.title}</strong>
                  <p>{item.body}</p>
                </div>
                <time dateTime={item.time}>{formatDate(item.time)}</time>
              </li>
            ))}
          </ol>
          <details className="work-report-details">
            <summary>Event trace</summary>
            <ol>
              {items.map((item, index) => (
                <li key={item.source_event_id || item.source_evidence_id || `${item.sequence}:${index}`}>
                  <span>{label(item.kind)}</span>
                  <time dateTime={item.event_time}>{formatDate(item.event_time)}</time>
                  <div>
                    <p>{shareSafeTimelineTitle(item)}</p>
                    <div className="work-report-evidence-detail">
                      {timelineSupportLabel(item) ? <span>{timelineSupportLabel(item)}</span> : null}
                    </div>
                  </div>
                </li>
              ))}
            </ol>
          </details>
        </>
      ) : (
        <Empty>No timeline events are available.</Empty>
      )}
    </section>
  );
}

function transcriptStoryItems(items: WorkspaceWorkInspectorTranscriptItem[]) {
  const humanCount = items.filter((item) => item.actor_kind === "human").length;
  const agentCount = items.filter((item) => item.actor_kind === "agent").length;
  const subagentCount = items.filter((item) => item.actor_kind === "subagent").length;
  const artifact = items.find((item) => item.event_type === "artifact_created");
  const toolCount = items.filter((item) => item.event_type === "tool_call_start" || item.event_type === "tool_output").length;
  return [
    {
      kicker: "Request",
      title: humanCount ? "Human objective captured" : "Objective inferred from Work metadata",
      body: humanCount
        ? `${humanCount} request events are linked to this work. Full prompt text stays local in the share-safe view.`
        : "No human prompt text is exposed in this share-safe view.",
    },
    {
      kicker: "Implementation",
      title: "Agent activity captured",
      body: `${agentCount} agent events and ${toolCount} tool events are summarized here; command evidence appears in its own tab.`,
    },
    {
      kicker: "Review",
      title: subagentCount ? "Child reviewer linked" : "No child reviewer linked",
      body: subagentCount
        ? `${subagentCount} subagent events are attached, including the reviewer contribution summary.`
        : "No child or subagent session is linked to this Work record.",
    },
    {
      kicker: "Artifact",
      title: artifact?.text_preview?.replace(/^Artifact created:\s*/, "") || "No artifact event in transcript",
      body: artifact
        ? "The generated artifact is attached and previewable without exposing local paths."
        : "No artifact event was captured for this transcript.",
    },
  ];
}

function timelineMilestones(items: WorkspaceWorkInspectorTimelineItem[]) {
  const milestones: Array<{ kicker: string; title: string; body: string; time: string }> = [];
  const first = items[0];
  if (first) {
    milestones.push({
      kicker: "Started",
      title: "Work session opened",
      body: "The Inspector connected task, session, and worktree context into one Work record.",
      time: first.event_time,
    });
  }
  const artifact = items.find((item) => item.kind === "artifact_created");
  if (artifact) {
    milestones.push({
      kicker: "Artifact",
      title: shareSafeTimelineTitle(artifact),
      body: "A previewable artifact was captured for reviewer inspection.",
      time: artifact.event_time,
    });
  }
  const subagent = items.find((item) => shareSafeTimelineTitle(item).includes("Subagent"));
  if (subagent) {
    milestones.push({
      kicker: "Review",
      title: "Subagent review attached",
      body: "A child session contributed review context to the Work record.",
      time: subagent.event_time,
    });
  }
  for (const evidence of items.filter((item) => item.source_evidence_id && shareSafeTimelineTitle(item).startsWith("Observed"))) {
    milestones.push({
      kicker: "Evidence",
      title: shareSafeTimelineTitle(evidence),
      body: "The command result is linked as fresh evidence.",
      time: evidence.event_time,
    });
    if (milestones.length >= 6) break;
  }
  return milestones.length ? milestones : items.slice(0, 4).map((item) => ({
    kicker: label(item.kind),
    title: shareSafeTimelineTitle(item),
    body: timelineSupportLabel(item) || "Captured Work event",
    time: item.event_time,
  }));
}

function shareSafeTranscriptPreview(item: Pick<WorkspaceWorkInspectorTranscriptItem, "actor_kind" | "event_type" | "text_preview">) {
  const preview = item.text_preview?.trim();
  if (!preview) return "Captured event has no share-safe preview.";
  if (preview.includes("message captured")) {
    if (item.actor_kind === "human") return "Human request captured. Full text stays local in this share-safe view.";
    if (item.actor_kind === "subagent") return "Subagent response captured. Full text stays local in this share-safe view.";
    return "Agent response captured. Full text stays local in this share-safe view.";
  }
  if (preview.includes("tool call captured")) {
    return "Tool call captured. Inputs stay local; command and evidence summaries are shown in their own tabs.";
  }
  if (preview.includes("tool output captured")) {
    return "Tool output captured. Raw output stays local; hashes and safe previews are shown in Commands.";
  }
  if (preview.startsWith("Subagent invocation ")) {
    return "Subagent review completed and is linked to this Work record.";
  }
  return preview;
}

function shareSafeTimelineTitle(item: WorkspaceWorkInspectorTimelineItem) {
  if (item.title.includes("message captured")) {
    if (item.kind === "user_message") return "Human request captured.";
    if (item.kind === "assistant_message") return "Agent response captured.";
    return "Message captured.";
  }
  if (item.title.includes("tool call captured")) return "Tool call captured.";
  if (item.title.includes("tool output captured")) return "Tool output captured.";
  if (item.title.startsWith("Subagent invocation ")) return "Subagent review completed.";
  if (item.title === "evidence_observed event recorded") return "Evidence record indexed.";
  if (item.title === "summary_generated event recorded") return "Summary refreshed.";
  return item.title;
}

function timelineSupportLabel(item: WorkspaceWorkInspectorTimelineItem) {
  if (item.source_evidence_id) return "Evidence-linked";
  if (item.detail?.includes("subagent_invocation")) return "Child session";
  if (item.detail?.includes("session_artifact")) return "Artifact";
  if (item.detail?.includes("session_state")) return "Session state";
  if (item.source_event_id) return "Session event";
  return null;
}

const pullRequestLabel = (value: JsonValue, index: number, fallback?: string | null) => {
  const outer = asRecord(value);
  const nested = asRecord(outer?.pull_request) ?? outer;
  const title = pickString(nested, ["title", "name"]);
  const url = safeExternalUrl(pickString(nested, ["url", "html_url"]));
  const state = pickString(nested, ["state"]);
  const number = pickString(nested, ["number", "pr_number"]);
  const labelParts = [title || fallback || (number ? `PR #${number}` : `PR ${index + 1}`), state ? label(state) : null].filter(Boolean);
  return { label: labelParts.join(" · "), url };
};

const changedFilesFromValues = (values: JsonValue[]) => {
  const files = new Map<string, { path: string; additions?: number | null; deletions?: number | null; status?: string | null }>();
  const visit = (value: JsonValue) => {
    const record = asRecord(value);
    if (!record) return;
    const directPath = safeDisplayPath(pickString(record, ["path", "file", "filename", "display_path", "relative_path"]));
    if (directPath) {
      files.set(directPath, {
        path: directPath,
        additions: pickNumber(record, ["additions", "added", "lines_added"]),
        deletions: pickNumber(record, ["deletions", "deleted", "lines_deleted"]),
        status: pickString(record, ["status", "change_type"]),
      });
    }
    for (const key of ["files", "changed_files", "file_changes", "diffs"]) {
      for (const child of asArray(record[key])) visit(child);
    }
  };
  values.forEach(visit);
  return Array.from(files.values()).slice(0, 30);
};

const fileSummariesFromValues = (values: JsonValue[]) => {
  const summaries = new Map<string, { path: string; summary: string }>();
  const visit = (value: JsonValue) => {
    const record = asRecord(value);
    if (!record) return;
    const path = safeDisplayPath(pickString(record, ["path", "file", "filename", "display_path", "relative_path"]));
    const summary = pickString(record, ["summary", "purpose", "review_note", "note"]);
    if (path && summary) summaries.set(path, { path, summary });
    for (const key of ["file_summaries", "source_outline", "review_notes"]) {
      for (const child of asArray(record[key])) visit(child);
    }
  };
  values.forEach(visit);
  return Array.from(summaries.values()).slice(0, 30);
};

const reviewNotesFromValues = (values: JsonValue[]) => {
  const notes = new Map<string, { title: string; detail: string; path?: string | null; excerpt?: string | null }>();
  const visit = (value: JsonValue) => {
    const record = asRecord(value);
    if (!record) return;
    const title = pickString(record, ["title", "label", "heading", "kind"]) ?? "Review note";
    const detail = pickString(record, ["detail", "summary", "review_note", "note", "text"]);
    const path = safeDisplayPath(pickString(record, ["path", "file", "filename", "relative_path"]));
    const excerpt = pickString(record, ["excerpt", "review_excerpt", "safe_excerpt"]);
    if (detail) notes.set(`${title}:${path ?? ""}:${detail}`, { title, detail, path, excerpt });
    for (const key of ["source_outline", "implementation_outline", "review_outline", "review_notes", "resume_notes", "implementation_notes"]) {
      for (const child of asArray(record[key])) {
        if (typeof child === "string") {
          notes.set(`Review note::${child}`, { title: "Review note", detail: child });
        } else {
          visit(child);
        }
      }
    }
  };
  values.forEach(visit);
  return Array.from(notes.values()).slice(0, 30);
};

type SourceSnapshot = {
  path: string;
  content: string;
  language?: string | null;
  kind?: string | null;
  lineCount?: number | null;
  sha256?: string | null;
};

const SOURCE_SNAPSHOT_EXCERPT_LINES = 10;
const SOURCE_SNAPSHOT_EXCERPT_CHARS = 1_200;

const sourceSnapshotExcerpt = (content: string) => {
  const lines = content.split(/\r?\n/);
  const head = lines.slice(0, SOURCE_SNAPSHOT_EXCERPT_LINES).join("\n");
  const bounded = head.length > SOURCE_SNAPSHOT_EXCERPT_CHARS
    ? `${head.slice(0, SOURCE_SNAPSHOT_EXCERPT_CHARS)}\n[excerpt truncated]`
    : head;
  const omittedLines = Math.max(0, lines.length - SOURCE_SNAPSHOT_EXCERPT_LINES);
  const omittedChars = Math.max(0, content.length - bounded.length);
  return {
    text: bounded,
    truncated: omittedLines > 0 || omittedChars > 0,
    omittedLines,
  };
};

const sourceSnapshotsFromValues = (values: JsonValue[]) => {
  const snapshots = new Map<string, SourceSnapshot>();
  const visit = (value: JsonValue) => {
    const record = asRecord(value);
    if (!record) return;
    const isExplicitShareSafe =
      record.share_safe === true && pickString(record, ["redaction_class"]) === "local_redacted";
    const path = safeDisplayPath(pickString(record, ["path", "file", "filename", "relative_path"]));
    const content = isExplicitShareSafe
      ? pickString(record, ["safe_content", "redacted_content", "content"])
      : null;
    if (path && content) {
      snapshots.set(path, {
        path,
        content,
        language: pickString(record, ["language", "lang", "syntax"]),
        kind: pickString(record, ["kind", "role", "type"]),
        lineCount: pickNumber(record, ["line_count", "lines"]),
        sha256: pickString(record, ["sha256", "content_sha256", "digest"]),
      });
    }
    for (const key of ["source_snapshots", "file_snapshots", "implementation_snapshots"]) {
      for (const child of asArray(record[key])) visit(child);
    }
  };
  values.forEach(visit);
  return Array.from(snapshots.values()).slice(0, 12);
};

const changedFileCount = (report: WorkspaceWorkInspector) =>
  changedFilesFromValues([...report.change_sets, ...report.contributions]).length;

const usefulArtifactCount = (report: WorkspaceWorkInspector) =>
  report.artifacts.filter((artifact) => !artifact.missing && artifact.render_kind !== "unavailable")
    .length;

const tabDefsForReport = (report: WorkspaceWorkInspector): WorkInspectorTabDef[] => {
  const changeCount =
    changedFileCount(report) +
    report.change_summary.commits.length +
    report.change_summary.pull_requests.length;
  const usefulArtifacts = usefulArtifactCount(report);
  return [
    { id: "overview", label: "Overview" },
    {
      id: "transcript",
      label: "Transcript",
      count: report.transcript.length,
      emptyReason: report.transcript.length ? undefined : "not captured",
    },
    {
      id: "subagents",
      label: "Subagents",
      count: report.subagents.length,
      emptyReason: report.subagents.length ? undefined : "none",
    },
    {
      id: "commands",
      label: "Commands",
      count: report.commands.length,
      emptyReason: report.commands.length ? undefined : "not captured",
    },
    {
      id: "evidence",
      label: "Evidence",
      count: report.evidence.length,
      emptyReason: report.evidence.length ? undefined : "missing",
    },
    {
      id: "timeline",
      label: "Timeline",
      count: report.timeline_items.length,
      emptyReason: report.timeline_items.length ? undefined : "not captured",
    },
    {
      id: "changes",
      label: "Changes",
      count: changeCount,
      emptyReason: changeCount ? undefined : "no files",
    },
    {
      id: "artifacts",
      label: "Artifacts",
      count: usefulArtifacts,
      emptyReason: usefulArtifacts ? undefined : report.artifacts.length ? "unavailable" : "not captured",
    },
    {
      id: "context",
      label: "Context",
      count: report.summaries.length + report.summary_claims.length,
      emptyReason: report.summaries.length || report.summary_claims.length ? undefined : "no summary",
    },
    { id: "raw", label: "Agent handoff" },
  ];
};

export function ChangesTab({ report }: { report: WorkspaceWorkInspector }) {
  const pullRequests = [
    ...report.change_summary.pull_requests.map((value, index) => pullRequestLabel(value, index)),
    ...report.links
      .filter((link) => link.target_kind === "pull_request")
      .map((link, index) => pullRequestLabel(link.target_json ?? null, index, link.target_id)),
  ].filter(
    (item, index, items) =>
      items.findIndex((candidate) => candidate.label === item.label && candidate.url === item.url) === index,
  );
  const commits = report.change_summary.commits.length
    ? report.change_summary.commits
    : report.links
        .filter((link) => link.target_kind === "commit" && link.target_id)
        .map((link) => link.target_id as string);
  const changedFiles = changedFilesFromValues([...report.change_sets, ...report.contributions]);
  const fileSummaries = fileSummariesFromValues([...report.change_sets, ...report.contributions]);
  const reviewNotes = reviewNotesFromValues([...report.change_sets, ...report.contributions]);
  const sourceSnapshots = sourceSnapshotsFromValues([...report.change_sets, ...report.contributions]);
  return (
    <section className="work-report-panel" aria-label="Changes">
      <div className="work-report-panel-header">
        <h2>Changes</h2>
        <span>{report.change_summary.change_sets} change sets</span>
      </div>
      <SectionSummary
        label="Change capture summary"
        items={[
          { label: "Files", value: changedFiles.length, note: "safe changed-file metadata" },
          { label: "Commits", value: commits.length, note: "linked implementation heads" },
          {
            label: "Review material",
            value: fileSummaries.length + reviewNotes.length + sourceSnapshots.length,
            note: "source outline and safe snapshots",
          },
        ]}
      />
      <div className="work-report-linked-items">
        {pullRequests.map((pr, index) =>
          pr.url ? (
            <ExternalLink key={`${pr.url}:${index}`} href={pr.url}>
              {pr.label}
            </ExternalLink>
          ) : (
            <span key={`${pr.label}:${index}`}>{pr.label}</span>
          ),
        )}
        {commits.map((commit) => (
          <span key={commit}>commit {shortSha(commit)}</span>
        ))}
        {report.change_summary.contributions > 0 ? <span>{report.change_summary.contributions} contributions</span> : null}
      </div>
      {changedFiles.length ? (
        <div className="work-report-file-list" aria-label="Changed files">
          {changedFiles.map((file) => (
            <div className="work-report-file-row" key={file.path}>
              <span>{file.path}</span>
              <div>
                {file.status ? <span>{label(file.status)}</span> : null}
                {typeof file.additions === "number" ? <span>+{file.additions}</span> : null}
                {typeof file.deletions === "number" ? <span>-{file.deletions}</span> : null}
              </div>
            </div>
          ))}
        </div>
      ) : (
        <Empty>No changed-file metadata is available.</Empty>
      )}
      {fileSummaries.length ? (
        <div className="work-report-source-outline" aria-label="Source outline">
          <h3>Source outline</h3>
          {fileSummaries.map((file) => (
            <article key={file.path}>
              <strong>{file.path}</strong>
              <p>{file.summary}</p>
            </article>
          ))}
        </div>
      ) : null}
      {reviewNotes.length ? (
        <div className="work-report-source-outline" aria-label="Review notes">
          <h3>Review notes</h3>
          {reviewNotes.map((note, index) => (
            <article key={`${note.title}:${note.path ?? ""}:${index}`}>
              <strong>{note.path ? `${note.path}: ${note.title}` : note.title}</strong>
              <p>{note.detail}</p>
              {note.excerpt ? <pre className="work-report-review-excerpt">{note.excerpt}</pre> : null}
            </article>
          ))}
        </div>
      ) : null}
      {sourceSnapshots.length ? (
        <div className="work-report-source-outline" aria-label="Implementation snapshot">
          <h3>Implementation snapshot</h3>
          <p className="work-report-source-note">
            Full share-safe source snapshots are available in the agent handoff JSON; the default UI shows bounded excerpts.
          </p>
          {sourceSnapshots.map((snapshot) => (
            <article key={snapshot.path}>
              <div className="work-report-source-heading">
                <strong>{snapshot.path}</strong>
                <span>
                  {[
                    snapshot.language,
                    snapshot.kind,
                    snapshot.lineCount ? `${snapshot.lineCount} lines` : null,
                    snapshot.sha256 ? `sha256 ${snapshot.sha256.slice(0, 12)}` : null,
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </span>
              </div>
              <pre className="work-report-review-excerpt">{sourceSnapshotExcerpt(snapshot.content).text}</pre>
              {sourceSnapshotExcerpt(snapshot.content).truncated ? (
                <p className="work-report-source-note">
                  {sourceSnapshotExcerpt(snapshot.content).omittedLines > 0
                    ? `${sourceSnapshotExcerpt(snapshot.content).omittedLines} additional lines are omitted from the default UI.`
                    : "Additional source content is omitted from the default UI."}
                </p>
              ) : null}
            </article>
          ))}
        </div>
      ) : null}
    </section>
  );
}

function artifactKindLabel(artifact: WorkspaceWorkInspectorArtifact) {
  return artifact.kind ? label(artifact.kind) : artifact.mime_type || "artifact";
}

function artifactDisplayPath(artifact: WorkspaceWorkInspectorArtifact) {
  return safeDisplayPath(artifact.display_name);
}

function artifactPrimaryPath(artifact: WorkspaceWorkInspectorArtifact) {
  return safeWorkArtifactPath(artifact.open_url) ?? safeWorkArtifactPath(artifact.download_url);
}

function artifactThumbPath(artifact: WorkspaceWorkInspectorArtifact) {
  if (artifact.render_kind !== "raster_image") return null;
  const explicit = safeWorkArtifactPath(artifact.thumbnail_url);
  if (explicit) return explicit;
  const primary = artifactPrimaryPath(artifact);
  if (primary) return primary;
  return null;
}

function ArtifactPreview({ artifact }: { artifact: WorkspaceWorkInspectorArtifact }) {
  const [imageFailed, setImageFailed] = useState(false);
  const thumb = artifactThumbPath(artifact);
  const blob = useArtifactBlobUrl(thumb);
  const isImageLike = artifact.mime_type?.startsWith("image/") || artifact.kind === "screenshot";
  const labelText = artifact.label || artifact.display_name || "Artifact";
  if (thumb && blob.url && !imageFailed && !blob.failed) {
    return (
      <img
        alt={`${labelText} preview`}
        src={blob.url}
        onError={() => {
          setImageFailed(true);
        }}
      />
    );
  }
  return (
    <div className="work-report-artifact-preview-fallback">
      {isImageLike ? <ImageIcon aria-hidden="true" size={24} /> : <FileText aria-hidden="true" size={24} />}
      {thumb && (imageFailed || blob.failed) ? <span>Preview unavailable</span> : null}
      {thumb && !blob.url && !blob.failed && !imageFailed ? <span>Loading preview</span> : null}
    </div>
  );
}

function ArtifactActionButton({
  artifact,
  action,
  children,
}: {
  artifact: WorkspaceWorkInspectorArtifact;
  action: "preview" | "download";
  children: ReactNode;
}) {
  const [failed, setFailed] = useState(false);
  const path = action === "preview"
    ? safeWorkArtifactPath(artifact.preview_url) ?? artifactPrimaryPath(artifact)
    : artifactPrimaryPath(artifact);
  if (!path) return null;
  const run = async () => {
    setFailed(false);
    try {
      const blob = await fetchArtifactBlob(path);
      const url = URL.createObjectURL(blob);
      if (action === "preview") {
        window.open(url, "_blank", "noopener,noreferrer");
        window.setTimeout(() => URL.revokeObjectURL?.(url), 60_000);
      } else {
        const anchor = document.createElement("a");
        anchor.href = url;
        anchor.download = artifact.display_name || artifact.label || artifact.artifact_id || "ctx-work-artifact";
        document.body.appendChild(anchor);
        anchor.click();
        anchor.remove();
        URL.revokeObjectURL?.(url);
      }
    } catch {
      setFailed(true);
    }
  };
  return (
    <>
      <button className="work-report-artifact-link" type="button" onClick={run}>
        {children}
      </button>
      {failed ? <span className="work-report-artifact-action-error">Artifact unavailable</span> : null}
    </>
  );
}

export function ArtifactsTab({ artifacts }: { artifacts: WorkspaceWorkInspectorArtifact[] }) {
  return (
    <section className="work-report-panel" aria-label="Artifacts">
      <div className="work-report-panel-header">
        <h2>Artifacts</h2>
        <span>{artifacts.length ? `${artifacts.length} artifacts` : "none recorded"}</span>
      </div>
      {artifacts.length ? (
        <div className="work-report-artifact-grid">
          {artifacts.map((artifact, index) => {
            const displayPath = artifactDisplayPath(artifact);
            const size = bytesLabel(artifact.bytes);
            return (
              <article className="work-report-artifact-card" key={artifact.id || index}>
                <div className="work-report-artifact-preview">
                  <ArtifactPreview artifact={artifact} />
                </div>
                <div className="work-report-artifact-body">
                  <strong>{artifact.label || artifact.display_name || artifact.artifact_id || artifact.id}</strong>
                  <div className="work-report-meta">
                    <span>{artifactKindLabel(artifact)}</span>
                    <span>{label(artifact.render_kind)}</span>
                    {artifact.mime_type ? <span>{artifact.mime_type}</span> : null}
                    {size ? <span>{size}</span> : null}
                    {artifact.missing ? <span>missing</span> : null}
                    {artifact.created_at ? <time dateTime={artifact.created_at}>{formatDate(artifact.created_at)}</time> : null}
                  </div>
                  {displayPath ? <p className="work-report-ref">{displayPath}</p> : null}
                  {artifact.unavailable_reason ? <p>{artifact.unavailable_reason}</p> : null}
                  <div className="work-report-artifact-actions">
                    <ArtifactActionButton action="preview" artifact={artifact}>
                      <FileText aria-hidden="true" size={14} />
                      Preview
                    </ArtifactActionButton>
                    <ArtifactActionButton action="download" artifact={artifact}>
                      <Download aria-hidden="true" size={14} />
                      Download
                    </ArtifactActionButton>
                  </div>
                </div>
              </article>
            );
          })}
        </div>
      ) : (
        <Empty>No artifacts have been recorded.</Empty>
      )}
    </section>
  );
}

function orderedSummaries(summaries: WorkspaceWorkSummary[]) {
  return [...summaries].sort((left, right) => Date.parse(right.generated_at) - Date.parse(left.generated_at));
}

function currentSummary(summaries: WorkspaceWorkSummary[]) {
  const ordered = orderedSummaries(summaries);
  return ordered.find((summary) => summary.freshness === "fresh" || summary.freshness === "locked") ?? ordered[0] ?? null;
}

function SummaryArticle({
  summary,
  current = false,
}: {
  summary: WorkspaceWorkSummary;
  current?: boolean;
}) {
  return (
    <article className={`work-report-summary${current ? " work-report-summary-current" : ""}`}>
      <div className="work-report-meta">
        {current ? <span>current summary</span> : null}
        <span>{label(summary.freshness)}</span>
      </div>
      <p>{summary.text}</p>
    </article>
  );
}

function SummaryClaimArticle({ claim }: { claim: WorkspaceWorkSummaryClaim }) {
  return (
    <article className="work-report-claim">
      <strong>{claim.claim_text}</strong>
      <div className="work-report-meta">
        <span>{label(claim.freshness)}</span>
      </div>
    </article>
  );
}

export function ContextTab({ report }: { report: WorkspaceWorkInspector }) {
  const current = currentSummary(report.summaries);
  const historicalSummaries = current
    ? orderedSummaries(report.summaries).filter((summary) => summary.summary_id !== current.summary_id)
    : [];
  const currentClaims = current
    ? report.summary_claims.filter((claim) => claim.summary_id === current.summary_id)
    : [];
  const historicalClaims = current
    ? report.summary_claims.filter((claim) => claim.summary_id !== current.summary_id)
    : report.summary_claims;
  const historicalCount = historicalSummaries.length + historicalClaims.length;
  const fileSummaryCount = report.contributions.reduce<number>(
    (count, contribution) => count + asArray(asRecord(contribution)?.file_summaries).length,
    0,
  );
  const freshSummaries = report.summaries.filter((summary) =>
    summary.freshness === "fresh" || summary.freshness === "locked"
  ).length;
  const contextCards = [
    {
      eyebrow: "What happened",
      title: report.work.title || "Captured work",
      body: report.work.objective || report.overview.objective || "A Work record was captured for review.",
    },
    {
      eyebrow: "Validation",
      title: `${report.evidence_summary.passing} passing, ${report.evidence_summary.failing} failing`,
      body: `${report.commands.length} commands are linked as evidence; raw command output stays local by default.`,
    },
    {
      eyebrow: "Review material",
      title: `${usefulArtifactCount(report)} artifacts, ${changeSignalCount(report)} change signals`,
      body: `${fileSummaryCount} file summaries, ${report.change_summary.commits.length} commits, and ${report.change_summary.pull_requests.length} pull requests are available.`,
    },
    {
      eyebrow: "Collaboration",
      title: `${report.subagents.length} child sessions`,
      body: report.subagents.length
        ? "Child-session contributions are projected into this Work record with redacted transcript previews."
        : "No child-session contributions were projected for this Work record.",
    },
    {
      eyebrow: "Context freshness",
      title: `${freshSummaries} fresh summaries`,
      body: `${historicalCount} historical summary or claim entries are retained behind the collapsed history view.`,
    },
    {
      eyebrow: "Share boundary",
      title: report.raw_transcript_included ? "raw transcript included" : "share-safe by default",
      body: rawTranscriptStatus(report),
    },
  ];
  return (
    <section className="work-report-panel work-report-side" aria-label="Context">
      <h2>Reviewer context</h2>
      <div className="work-report-narrative-grid">
        {contextCards.map((card) => (
          <article key={card.eyebrow}>
            <span className="work-report-eyebrow">{card.eyebrow}</span>
            <strong>{card.title}</strong>
            <p>{card.body}</p>
          </article>
        ))}
      </div>
      {current ? (
        <SummaryArticle current summary={current} />
      ) : (
        <Empty>No summary has been generated yet.</Empty>
      )}
      {currentClaims.length ? (
        <div className="work-report-claim-list" aria-label="Summary claims">
          {currentClaims.map((claim) => (
            <SummaryClaimArticle claim={claim} key={claim.claim_id} />
          ))}
        </div>
      ) : null}
      {historicalCount ? (
        <details className="work-report-details">
          <summary>Historical summaries and claims ({historicalCount})</summary>
          <div className="work-report-history-list">
            {historicalSummaries.map((summary) => (
              <SummaryArticle key={summary.summary_id} summary={summary} />
            ))}
            {historicalClaims.map((claim) => (
              <SummaryClaimArticle claim={claim} key={claim.claim_id} />
            ))}
          </div>
        </details>
      ) : null}
      <details className="work-report-details">
        <summary>Handoff projection</summary>
        <pre className="work-report-json">{prettyJson(report.context.value)}</pre>
      </details>
    </section>
  );
}

export function RawRedactedJsonTab({ report }: { report: WorkspaceWorkInspector }) {
  const [expanded, setExpanded] = useState(false);
  return (
    <section className="work-report-panel" aria-label="Agent handoff">
      <div className="work-report-panel-header">
        <h2>Agent handoff</h2>
        <span>ready to resume</span>
      </div>
      <div className="work-report-narrative-grid">
        <article>
          <span className="work-report-eyebrow">Resume point</span>
          <strong>{report.work.title || "Captured work"}</strong>
          <p>{report.work.objective || report.overview.objective || "Use the current Work record as the review handoff."}</p>
        </article>
        <article>
          <span className="work-report-eyebrow">Evidence</span>
          <strong>{report.evidence_summary.passing} passing checks</strong>
          <p>
            {report.commands.length} commands, {usefulArtifactCount(report)} artifacts, and {changeSignalCount(report)} change signals are
            captured for review.
          </p>
        </article>
        <article>
          <span className="work-report-eyebrow">Review state</span>
          <strong>{label(report.trust.verdict)}</strong>
          <p>{report.trust.recommended_next_action}</p>
        </article>
        <article>
          <span className="work-report-eyebrow">Privacy</span>
          <strong>Share-safe by default</strong>
          <p>{rawTranscriptStatus(report)}</p>
        </article>
      </div>
      <details className="work-report-details">
        <summary>{expanded ? "Hide projection payload" : "Show projection payload"}</summary>
        <button
          aria-expanded={expanded}
          className="work-report-refresh work-report-inline-toggle"
          type="button"
          onClick={() => setExpanded((value) => !value)}
        >
          <FileText aria-hidden="true" size={15} />
          <span>{expanded ? "Hide payload" : "Show payload"}</span>
        </button>
        {expanded ? <pre className="work-report-json">{prettyJson(report.raw_redacted_json.value)}</pre> : null}
      </details>
    </section>
  );
}

function WorkInspectorRightRail({ report }: { report: WorkspaceWorkInspector }) {
  return (
    <aside className="work-report-right-rail" aria-label="Work Inspector summary">
      <section className="work-report-panel">
        <h2>Next action</h2>
        <p>{report.trust.recommended_next_action}</p>
      </section>
      <section className="work-report-panel">
        <h2>Open risks</h2>
        {report.trust.open_risks.length ? (
          <ul className="work-report-rail-list">
            {report.trust.open_risks.map((risk) => (
              <li key={risk}>{risk}</li>
            ))}
          </ul>
        ) : (
          <Empty>No open risks are recorded.</Empty>
        )}
      </section>
      <section className="work-report-panel">
        <h2>Evidence mix</h2>
        <div className="work-report-rail-metrics">
          <Metric label="Passing" value={report.evidence_summary.passing} />
          <Metric label="Failing" value={report.evidence_summary.failing} />
          <Metric label="Stale" value={report.evidence_summary.stale} />
          <Metric label="Missing" value={report.evidence_summary.missing} />
        </div>
      </section>
      <section className="work-report-panel">
        <h2>Safe sharing</h2>
        <p>{rawTranscriptStatus(report)}</p>
      </section>
    </aside>
  );
}

function tabPanel(report: WorkspaceWorkInspector, selected: WorkInspectorTab) {
  switch (selected) {
    case "overview":
      return <OverviewTab report={report} />;
    case "transcript":
      return <TranscriptTab items={report.transcript} />;
    case "subagents":
      return <SubagentsTab subagents={report.subagents} />;
    case "commands":
      return <CommandsTab commands={report.commands} />;
    case "evidence":
      return <EvidenceTab evidence={report.evidence} />;
    case "timeline":
      return <TimelineTab items={report.timeline_items} />;
    case "changes":
      return <ChangesTab report={report} />;
    case "artifacts":
      return <ArtifactsTab artifacts={report.artifacts} />;
    case "context":
      return <ContextTab report={report} />;
    case "raw":
      return <RawRedactedJsonTab report={report} />;
    default:
      return null;
  }
}

export function WorkInspectorView({
  report,
  onRefresh,
}: {
  report: WorkspaceWorkInspector;
  onRefresh?: () => void;
}) {
  const [selectedTab, setSelectedTab] = useState<WorkInspectorTab>("overview");
  const tabs = useMemo(() => tabDefsForReport(report), [report]);
  const selected = useMemo(() => tabs.find((tab) => tab.id === selectedTab) ?? tabs[0], [selectedTab, tabs]);
  useEffect(() => {
    if (!tabs.some((tab) => tab.id === selectedTab)) {
      setSelectedTab("overview");
    }
  }, [selectedTab, tabs]);
  return (
    <main className="work-report-page">
      <WorkInspectorHeader report={report} onRefresh={onRefresh} />
      <InspectorMetricStrip report={report} />
      <div className="work-report-content">
        <section className="work-report-primary" aria-label="Work Inspector detail">
          <InspectorTabs tabs={tabs} selected={selected.id} onSelect={setSelectedTab} />
          <div
            aria-labelledby={`work-report-tab-${selected.id}`}
            className="work-report-tab-panel"
            id={`work-report-panel-${selected.id}`}
            role="tabpanel"
          >
            {tabPanel(report, selected.id)}
          </div>
        </section>
        <WorkInspectorRightRail report={report} />
      </div>
    </main>
  );
}

export function WorkReportView({
  report,
  onRefresh,
}: {
  report: WorkspaceWorkReport;
  onRefresh?: () => void;
}) {
  return <WorkInspectorView report={reportToInspector(report)} onRefresh={onRefresh} />;
}
