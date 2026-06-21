import { useMemo, useState } from "react";
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
  WorkspaceWorkInspectorTimelineItem,
  WorkspaceWorkInspectorTranscriptItem,
  WorkspaceWorkReport,
  WorkspaceWorkTrustSummary,
} from "@ctx/types";
import { ExternalLink } from "../../components/ExternalLink";

type WorkInspectorTab =
  | "overview"
  | "transcript"
  | "commands"
  | "evidence"
  | "timeline"
  | "changes"
  | "artifacts"
  | "context"
  | "raw";

const tabs: { id: WorkInspectorTab; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "transcript", label: "Transcript" },
  { id: "commands", label: "Commands" },
  { id: "evidence", label: "Evidence" },
  { id: "timeline", label: "Timeline" },
  { id: "changes", label: "Changes" },
  { id: "artifacts", label: "Artifacts" },
  { id: "context", label: "Context" },
  { id: "raw", label: "Raw redacted JSON" },
];

const label = (value: string | null | undefined) =>
  String(value ?? "unknown").replaceAll("_", " ");

const shortSha = (value: string | null | undefined) => {
  if (!value) return "unknown";
  return value.length > 12 ? value.slice(0, 12) : value;
};

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
    return "Raw transcript detail is included in this response; review redaction before sharing.";
  }
  if (report.raw_transcript_available) {
    return "Raw transcripts are available locally but not included by default.";
  }
  return "Raw transcripts are not available in this inspector response.";
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
          <span>{report.work.work_id}</span>
          <span>{label(report.work.lifecycle)}</span>
          <span>{report.work.primary_branch || "branch unknown"}</span>
          <span>{shortSha(report.work.head_commit)}</span>
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
      <Metric label="Changes" value={report.change_summary.change_sets} />
      <Metric label="Summaries" value={label(report.work.summary_freshness)} />
    </section>
  );
}

export function InspectorTabs({
  selected,
  onSelect,
}: {
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
          id={`work-report-tab-${tab.id}`}
          key={tab.id}
          role="tab"
          tabIndex={selected === tab.id ? 0 : -1}
          type="button"
          onKeyDown={(event) => handleTabKeyDown(event, index)}
          onClick={() => onSelect(tab.id)}
        >
          {tab.label}
        </button>
      ))}
    </nav>
  );
}

function overviewSummary(report: WorkspaceWorkInspector) {
  return report.summaries.find((summary) => summary.audience === "reviewer") ?? report.summaries[0] ?? null;
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

export function TranscriptTab({ items }: { items: WorkspaceWorkInspectorTranscriptItem[] }) {
  return (
    <section className="work-report-panel" aria-label="Transcript">
      <div className="work-report-panel-header">
        <h2>Transcript</h2>
        <span>{items.length ? `${items.length} entries` : "none recorded"}</span>
      </div>
      {items.length ? (
        <ol className="work-report-stack">
          {items.map((item, index) => (
            <li className="work-report-message" key={item.event_id || item.id || index}>
              <div className="work-report-meta">
                <span>{label(item.actor_kind)}</span>
                <span>{label(item.event_type)}</span>
                {item.event_time ? <time dateTime={item.event_time}>{formatDate(item.event_time)}</time> : null}
                {item.redaction_class ? <span>{label(item.redaction_class)}</span> : null}
                {item.model ? <span>{item.model}</span> : null}
              </div>
              <p>{item.text_preview || "No redacted text is available."}</p>
            </li>
          ))}
        </ol>
      ) : (
        <Empty>No transcript entries are available.</Empty>
      )}
    </section>
  );
}

export function CommandsTab({ commands }: { commands: WorkspaceWorkInspectorCommand[] }) {
  return (
    <section className="work-report-panel" aria-label="Commands">
      <div className="work-report-panel-header">
        <h2>Commands</h2>
        <span>{commands.length ? `${commands.length} commands` : "none recorded"}</span>
      </div>
      {commands.length ? (
        <div className="work-report-evidence-list">
          {commands.map((command, index) => {
            const duration = durationLabel(command.started_at, command.finished_at);
            return (
              <article className="work-report-command" key={command.id || index}>
                <strong>{command.command || command.argv.join(" ") || command.id}</strong>
                <div className="work-report-meta">
                  {command.status ? <span>{label(command.status)}</span> : null}
                  {command.freshness ? <span>{label(command.freshness)}</span> : null}
                  {typeof command.exit_code === "number" ? <span>exit {command.exit_code}</span> : null}
                  {duration ? <span>{duration}</span> : null}
                  {command.cwd ? <span>{command.cwd}</span> : null}
                  {command.output_truncated ? <span>output truncated</span> : null}
                  {typeof command.stdout_size_bytes === "number" ? <span>stdout {bytesLabel(command.stdout_size_bytes)}</span> : null}
                  {typeof command.stderr_size_bytes === "number" ? <span>stderr {bytesLabel(command.stderr_size_bytes)}</span> : null}
                </div>
                {command.stdout_preview ? <p className="work-report-output">stdout: {command.stdout_preview}</p> : null}
                {command.stderr_preview ? <p className="work-report-output">stderr: {command.stderr_preview}</p> : null}
                {command.stdout_sha256 || command.stderr_sha256 ? (
                  <p className="work-report-ref">
                    {command.stdout_sha256 ? `stdout sha256 ${command.stdout_sha256.slice(0, 12)}` : null}
                    {command.stdout_sha256 && command.stderr_sha256 ? " · " : null}
                    {command.stderr_sha256 ? `stderr sha256 ${command.stderr_sha256.slice(0, 12)}` : null}
                  </p>
                ) : null}
                {!command.stdout_preview && !command.stderr_preview ? (
                  <p className="work-report-ref">No redacted output preview is available.</p>
                ) : null}
              </article>
            );
          })}
        </div>
      ) : (
        <Empty>No commands have been recorded.</Empty>
      )}
    </section>
  );
}

export function EvidenceTab({ evidence }: { evidence: WorkspaceWorkEvidence[] }) {
  return (
    <section className="work-report-panel work-report-evidence" aria-label="Evidence">
      <div className="work-report-panel-header">
        <h2>Evidence</h2>
        <span>{evidence.length ? `${evidence.length} observed` : "none recorded"}</span>
      </div>
      {evidence.length ? (
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
      ) : (
        <Empty>No evidence has been recorded for this Work record.</Empty>
      )}
    </section>
  );
}

export function TimelineTab({ items }: { items: WorkspaceWorkInspectorTimelineItem[] }) {
  return (
    <section className="work-report-panel work-report-timeline" aria-label="Timeline">
      <div className="work-report-panel-header">
        <h2>Timeline</h2>
        <span>{items.length ? `${items.length} events` : "none recorded"}</span>
      </div>
      {items.length ? (
        <ol>
          {items.map((item, index) => (
            <li key={item.source_event_id || item.source_evidence_id || `${item.sequence}:${index}`}>
              <span>{label(item.kind)}</span>
              <time dateTime={item.event_time}>{formatDate(item.event_time)}</time>
              <div>
                <p>{item.title}</p>
                <div className="work-report-evidence-detail">
                  {item.detail ? <span>{item.detail}</span> : null}
                  {item.source_event_id ? <span>{item.source_event_id}</span> : null}
                  {item.source_evidence_id ? <span>{item.source_evidence_id}</span> : null}
                </div>
              </div>
            </li>
          ))}
        </ol>
      ) : (
        <Empty>No timeline events are available.</Empty>
      )}
    </section>
  );
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
  return (
    <section className="work-report-panel" aria-label="Changes">
      <div className="work-report-panel-header">
        <h2>Changes</h2>
        <span>{report.change_summary.change_sets} change sets</span>
      </div>
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
    </section>
  );
}

function artifactKindLabel(artifact: WorkspaceWorkInspectorArtifact) {
  return artifact.kind ? label(artifact.kind) : artifact.mime_type || "artifact";
}

function artifactDisplayPath(artifact: WorkspaceWorkInspectorArtifact) {
  return safeDisplayPath(artifact.display_name);
}

function artifactPrimaryUrl(artifact: WorkspaceWorkInspectorArtifact) {
  return safeWorkArtifactPath(artifact.open_url) ?? safeWorkArtifactPath(artifact.download_url);
}

function artifactThumbUrl(artifact: WorkspaceWorkInspectorArtifact) {
  if (artifact.render_kind !== "raster_image") return null;
  const explicit = safeWorkArtifactPath(artifact.thumbnail_url);
  if (explicit) return explicit;
  const primary = artifactPrimaryUrl(artifact);
  if (primary) return primary;
  return null;
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
            const url = artifactPrimaryUrl(artifact);
            const thumb = artifactThumbUrl(artifact);
            const displayPath = artifactDisplayPath(artifact);
            const size = bytesLabel(artifact.bytes);
            return (
              <article className="work-report-artifact-card" key={artifact.id || index}>
                <div className="work-report-artifact-preview">
                  {thumb ? (
                    <img alt="" src={thumb} />
                  ) : artifact.mime_type?.startsWith("image/") || artifact.kind === "screenshot" ? (
                    <ImageIcon aria-hidden="true" size={24} />
                  ) : (
                    <FileText aria-hidden="true" size={24} />
                  )}
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
                    {artifact.preview_url
                      ? renderSafeLink(
                          safeWorkArtifactPath(artifact.preview_url),
                          <>
                            <FileText aria-hidden="true" size={14} />
                            Preview
                          </>,
                          "work-report-artifact-link",
                        )
                      : null}
                    {url
                      ? renderSafeLink(
                          url,
                          <>
                            <Download aria-hidden="true" size={14} />
                            Download
                          </>,
                          "work-report-artifact-link",
                        )
                      : null}
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

export function ContextTab({ report }: { report: WorkspaceWorkInspector }) {
  return (
    <section className="work-report-panel work-report-side" aria-label="Context">
      <h2>Context</h2>
      {report.summaries.length > 0 ? (
        report.summaries.map((summary) => (
          <article className="work-report-summary" key={summary.summary_id}>
            <div className="work-report-meta">
              <span>{label(summary.kind)}</span>
              <span>{label(summary.audience)}</span>
              <span>{label(summary.freshness)}</span>
              <span>{label(summary.generation_method)}</span>
            </div>
            <p>{summary.text}</p>
          </article>
        ))
      ) : (
        <Empty>No summary has been generated yet.</Empty>
      )}
      {report.summary_claims.length ? (
        <div className="work-report-claim-list" aria-label="Summary claims">
          {report.summary_claims.map((claim) => (
            <article className="work-report-claim" key={claim.claim_id}>
              <strong>{claim.claim_text}</strong>
              <div className="work-report-meta">
                <span>{claim.source_kind}</span>
                <span>{claim.source_id}</span>
                <span>{label(claim.freshness)}</span>
                <span>{label(claim.redaction_class)}</span>
              </div>
            </article>
          ))}
        </div>
      ) : null}
      <details className="work-report-details">
        <summary>Agent handoff JSON preview</summary>
        <pre className="work-report-json">{prettyJson(report.context.value)}</pre>
      </details>
    </section>
  );
}

export function RawRedactedJsonTab({ report }: { report: WorkspaceWorkInspector }) {
  const [expanded, setExpanded] = useState(false);
  return (
    <section className="work-report-panel" aria-label="Raw redacted JSON">
      <div className="work-report-panel-header">
        <h2>Raw redacted JSON</h2>
        <span>safe_json only</span>
      </div>
      <p className="work-report-raw-note">
        Collapsed by default. This payload is the redacted route projection, not raw local transcript or command output.
      </p>
      <button
        aria-expanded={expanded}
        className="work-report-refresh"
        type="button"
        onClick={() => setExpanded((value) => !value)}
      >
        <FileText aria-hidden="true" size={15} />
        <span>{expanded ? "Collapse JSON" : "Expand JSON"}</span>
      </button>
      {expanded ? <pre className="work-report-json">{prettyJson(report.raw_redacted_json.value)}</pre> : null}
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
  const selected = useMemo(() => tabs.find((tab) => tab.id === selectedTab) ?? tabs[0], [selectedTab]);
  return (
    <main className="work-report-page">
      <WorkInspectorHeader report={report} onRefresh={onRefresh} />
      <InspectorMetricStrip report={report} />
      <div className="work-report-content">
        <section className="work-report-primary" aria-label="Work Inspector detail">
          <InspectorTabs selected={selected.id} onSelect={setSelectedTab} />
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
