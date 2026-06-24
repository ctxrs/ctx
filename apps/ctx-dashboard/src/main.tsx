import React, { useMemo, useState } from "react";
import ReactDOM from "react-dom/client";
import * as Tabs from "@radix-ui/react-tabs";
import { ColumnDef, flexRender, getCoreRowModel, useReactTable } from "@tanstack/react-table";
import {
  Activity,
  AlertTriangle,
  Archive,
  Bot,
  CheckCircle2,
  Clock3,
  Command,
  Database,
  ExternalLink,
  FileText,
  GitBranch,
  GitPullRequest,
  MessageSquareText,
  Search,
  ShieldCheck,
  Terminal,
  Workflow,
  Wrench
} from "lucide-react";
import { clsx } from "clsx";
import ctxLogo from "./assets/ctx-logo.png";
import { readDashboardData } from "./data";
import type {
  DashboardArtifact,
  DashboardData,
  DashboardEvent,
  DashboardRecord,
  DashboardRun,
  DashboardSession,
  EvidenceCommand
} from "./types";
import "./styles.css";

const data = readDashboardData();

type StatusTone = "ok" | "warn" | "danger" | "neutral";

type RecordBundle = {
  record: DashboardRecord;
  commands: EvidenceCommand[];
  evidence: ReturnType<typeof evidenceForRecord>;
  sessions: DashboardSession[];
  runs: DashboardRun[];
  events: DashboardEvent[];
  artifacts: DashboardArtifact[];
  files: Record<string, unknown>[];
  summaries: Record<string, unknown>[];
  prs: string[];
  tone: StatusTone;
  statusLabel: string;
  nextAction: string;
};

type TimelineItem = {
  id: string;
  occurredAt?: string | null;
  recordId?: string | null;
  sessionId?: string | null;
  kind: string;
  title: string;
  preview: string;
  tone: StatusTone;
  icon: React.ReactNode;
};

function App() {
  const [query, setQuery] = useState("");
  const [activeTab, setActiveTab] = useState("overview");
  const [selectedRecordId, setSelectedRecordId] = useState(() => data.records[0]?.id ?? "");
  const bundles = useMemo(() => recordBundles(data), []);
  const selectedBundle = bundles.find((bundle) => bundle.record.id === selectedRecordId) ?? bundles[0];
  const attentionItems = useMemo(() => attentionQueue(bundles, data), [bundles]);

  React.useEffect(() => {
    if (selectedRecordId && bundles.some((bundle) => bundle.record.id === selectedRecordId)) return;
    setSelectedRecordId(bundles[0]?.record.id ?? "");
  }, [bundles, selectedRecordId]);

  React.useEffect(() => {
    const activeTrigger = document.querySelector<HTMLButtonElement>(`[data-dashboard-tab="${activeTab}"]`);
    const tabList = activeTrigger?.closest<HTMLElement>(".tab-list");
    if (!activeTrigger || !tabList) return;

    const scrollActiveTab = () => {
      activeTrigger.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "instant" });
    };

    scrollActiveTab();
    requestAnimationFrame(scrollActiveTab);
  }, [activeTab]);

  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="app-header">
        <div className="mx-auto flex max-w-7xl flex-col gap-4 px-4 py-4 sm:px-6 lg:flex-row lg:items-center lg:justify-between">
          <div className="brand-lockup">
            <img src={ctxLogo} alt="ctx" className="brand-logo" />
            <div className="min-w-0">
              <div className="text-sm font-medium text-muted-foreground">Local agent history</div>
              <h1 className="text-2xl font-semibold tracking-normal sm:text-3xl">Work Records</h1>
            </div>
          </div>
          <div className="header-signals" aria-label="Dashboard status">
            <StatusPill tone={data.share_safe ? "ok" : "warn"} icon={<ShieldCheck className="size-3.5" />}>
              Share-safe
            </StatusPill>
            <StatusPill tone={attentionItems.length > 0 ? "warn" : "ok"} icon={<AlertTriangle className="size-3.5" />}>
              {attentionItems.length === 0 ? "No blockers found" : `${attentionItems.length} need attention`}
            </StatusPill>
            <StatusPill tone="neutral" icon={<Search className="size-3.5" />}>
              Searchable history
            </StatusPill>
          </div>
        </div>
      </header>

      <main className="mx-auto max-w-7xl px-4 py-5 sm:px-6">
        <SignalGrid data={data} bundles={bundles} attentionItems={attentionItems} />

        <Tabs.Root value={activeTab} onValueChange={setActiveTab} className="space-y-4">
          <Tabs.List className="tab-list" aria-label="Dashboard views">
            <Tab value="overview" icon={<Activity className="size-4" />} label="Overview" />
            <Tab value="records" icon={<FileText className="size-4" />} label="Records" />
            <Tab value="timeline" icon={<Workflow className="size-4" />} label="Timeline" />
            <Tab value="evidence" icon={<ShieldCheck className="size-4" />} label="PR Evidence" />
            <Tab value="search" icon={<Search className="size-4" />} label="Search" />
            <Tab value="health" icon={<Database className="size-4" />} label="Setup Health" />
          </Tabs.List>

          <Tabs.Content value="overview">
            <Overview
              data={data}
              bundles={bundles}
              attentionItems={attentionItems}
              setSelectedRecordId={setSelectedRecordId}
              setActiveTab={setActiveTab}
            />
          </Tabs.Content>
          <Tabs.Content value="records">
            <RecordsView
              bundles={bundles}
              selectedRecordId={selectedBundle?.record.id ?? ""}
              setSelectedRecordId={setSelectedRecordId}
            />
          </Tabs.Content>
          <Tabs.Content value="timeline">
            <TimelineView data={data} />
          </Tabs.Content>
          <Tabs.Content value="evidence">
            <EvidenceView bundles={bundles} data={data} />
          </Tabs.Content>
          <Tabs.Content value="search">
            <SearchView data={data} query={query} setQuery={setQuery} />
          </Tabs.Content>
          <Tabs.Content value="health">
            <SetupHealthView data={data} />
          </Tabs.Content>
        </Tabs.Root>
      </main>
    </div>
  );
}

function Tab({ value, icon, label }: { value: string; icon: React.ReactNode; label: string }) {
  return (
    <Tabs.Trigger className="tab-trigger" value={value} data-dashboard-tab={value}>
      {icon}
      <span>{label}</span>
    </Tabs.Trigger>
  );
}

function SignalGrid({
  data,
  bundles,
  attentionItems
}: {
  data: DashboardData;
  bundles: RecordBundle[];
  attentionItems: ReturnType<typeof attentionQueue>;
}) {
  const linkedPrCount = uniquePullRequestUrls(data).length;
  const failedCommandCount = data.commands.filter((command) => command.exit_code !== 0).length;
  const transcriptEventCount = data.events.filter((event) => event.event_type === "message").length;

  return (
    <div className="signal-grid" aria-label="Work record signals">
      <SignalCard
        icon={<AlertTriangle className="size-4" />}
        label="Needs attention"
        value={attentionItems.length}
        detail={attentionItems.length === 0 ? "No failed or stale evidence in this export" : `${failedCommandCount} failed command${failedCommandCount === 1 ? "" : "s"} plus stale or blocked evidence`}
        tone={attentionItems.length > 0 ? "warn" : "ok"}
      />
      <SignalCard
        icon={<FileText className="size-4" />}
        label="Work records"
        value={bundles.length}
        detail={`${transcriptEventCount} redacted transcript event${transcriptEventCount === 1 ? "" : "s"}`}
        tone="neutral"
      />
      <SignalCard
        icon={<GitPullRequest className="size-4" />}
        label="PR-linked"
        value={linkedPrCount}
        detail="Records that can explain a review"
        tone={linkedPrCount > 0 ? "ok" : "neutral"}
      />
      <SignalCard
        icon={<Search className="size-4" />}
        label="Searchable history"
        value={data.commands.length + data.events.length + data.summaries.length}
        detail="Commands, messages, tools, and summaries"
        tone="neutral"
      />
    </div>
  );
}

function SignalCard({
  icon,
  label,
  value,
  detail,
  tone
}: {
  icon: React.ReactNode;
  label: string;
  value: number;
  detail: string;
  tone: StatusTone;
}) {
  return (
    <article className={clsx("signal-card", `signal-${tone}`)}>
      <div className="signal-icon">{icon}</div>
      <div>
        <div className="signal-value">{value}</div>
        <div className="signal-label">{label}</div>
        <div className="signal-detail">{detail}</div>
      </div>
    </article>
  );
}

function Overview({
  data,
  bundles,
  attentionItems,
  setSelectedRecordId,
  setActiveTab
}: {
  data: DashboardData;
  bundles: RecordBundle[];
  attentionItems: ReturnType<typeof attentionQueue>;
  setSelectedRecordId: (value: string) => void;
  setActiveTab: (value: string) => void;
}) {
  const recentTimeline = timelineItems(data).slice(0, 6);
  return (
    <div className="overview-grid">
      <section className="panel">
        <SectionHeader icon={<AlertTriangle className="size-4" />} title="What needs attention" />
        {attentionItems.length === 0 ? (
          <EmptyState title="No immediate blockers" text="No failed commands, stale evidence, or blocked provider captures were found in this export." />
        ) : (
          <div className="attention-list">
            {attentionItems.map((item) => (
              <button
                className="attention-item"
                key={item.id}
                onClick={() => {
                  if (item.recordId) setSelectedRecordId(item.recordId);
                  setActiveTab(item.recordId ? "records" : "health");
                }}
                type="button"
              >
                <span className={clsx("attention-dot", `attention-${item.tone}`)} />
                <span className="min-w-0">
                  <strong>{item.title}</strong>
                  <span>{item.detail}</span>
                </span>
              </button>
            ))}
          </div>
        )}
      </section>

      <section className="panel">
        <SectionHeader icon={<FileText className="size-4" />} title="Recent work records" />
        {bundles.length === 0 ? (
          <EmptyState text="No work records found in the local store." />
        ) : (
          <div className="record-list compact-record-list">
            {bundles.slice(0, 5).map((bundle) => (
              <button
                className="record-button"
                key={bundle.record.id}
                onClick={() => {
                  setSelectedRecordId(bundle.record.id);
                  setActiveTab("records");
                }}
                type="button"
              >
                <div className="min-w-0">
                  <div className="record-button-title">{bundle.record.title}</div>
                  <div className="record-button-meta">
                    <span className={clsx("badge", toneClass(bundle.tone))}>{bundle.statusLabel}</span>
                    {bundle.prs.length > 0 ? <span className="badge">PR linked</span> : null}
                    <span>{formatDate(bundle.record.updated_at)}</span>
                  </div>
                </div>
                <span className="next-action">{bundle.nextAction}</span>
              </button>
            ))}
          </div>
        )}
      </section>

      <section className="panel">
        <SectionHeader icon={<Workflow className="size-4" />} title="Latest timeline" />
        <TimelineList items={recentTimeline} emptyText="No transcript, command, or evidence timeline is available yet." compact />
      </section>

      <section className="panel">
        <SectionHeader icon={<Bot className="size-4" />} title="Hand context to an agent" />
        <div className="agent-handoff">
          <p>
            The most useful next step is usually search. Ask an agent to query old work before it starts a similar task.
          </p>
          <pre className="copy-block">{data.status.search_command}</pre>
          <div className="handoff-grid">
            <MiniFact label="Local only" value={data.status.local_only ? "yes" : "no"} />
            <MiniFact label="Raw transcripts" value={data.privacy.raw_transcripts_withheld > 0 ? "withheld by default" : "not present"} />
            <MiniFact label="Safe previews" value={data.privacy.redacted_previews} />
          </div>
        </div>
      </section>
    </div>
  );
}

function RecordsView({
  bundles,
  selectedRecordId,
  setSelectedRecordId
}: {
  bundles: RecordBundle[];
  selectedRecordId: string;
  setSelectedRecordId: (value: string) => void;
}) {
  const selectedBundle = bundles.find((bundle) => bundle.record.id === selectedRecordId) ?? bundles[0];
  return (
    <div className="records-layout">
      <section className="panel records-sidebar">
        <SectionHeader icon={<FileText className="size-4" />} title="Records" />
        {bundles.length === 0 ? (
          <EmptyState text="No work records found in this export." />
        ) : (
          <div className="record-list">
            {bundles.map((bundle) => (
              <button
                className={clsx("record-button", bundle.record.id === selectedBundle?.record.id && "record-button-active")}
                key={bundle.record.id}
                onClick={() => setSelectedRecordId(bundle.record.id)}
                type="button"
              >
                <div className="min-w-0">
                  <div className="record-button-title">{bundle.record.title}</div>
                  <div className="record-button-meta">
                    <span className={clsx("badge", toneClass(bundle.tone))}>{bundle.statusLabel}</span>
                    <span>{bundle.events.length} event{bundle.events.length === 1 ? "" : "s"}</span>
                  </div>
                </div>
              </button>
            ))}
          </div>
        )}
      </section>
      <RecordDetailPanel bundle={selectedBundle} />
    </div>
  );
}

function RecordDetailPanel({ bundle }: { bundle: RecordBundle | undefined }) {
  if (!bundle) {
    return (
      <section className="panel">
        <SectionHeader icon={<FileText className="size-4" />} title="Record detail" />
        <EmptyState text="Select a work record to inspect its transcript, commands, files, PRs, and evidence." />
      </section>
    );
  }

  const primarySessions = bundle.sessions.filter((session) => session.is_primary === true);
  const childSessions = bundle.sessions.filter((session) => session.parent_session_id || session.is_primary === false);
  const transcriptEvents = bundle.events.filter((event) => event.event_type === "message");
  const toolEvents = bundle.events.filter((event) => isToolEvent(event));
  const failedCommands = bundle.commands.filter((command) => command.exit_code !== 0);

  return (
    <section className="panel record-detail">
      <div className="record-detail-head">
        <div className="min-w-0">
          <div className="record-button-meta">
            <span className={clsx("badge", toneClass(bundle.tone))}>{bundle.statusLabel}</span>
            <span className="badge">{bundle.record.kind}</span>
            {bundle.record.workspace ? <span className="badge">{bundle.record.workspace}</span> : null}
          </div>
          <h2>{bundle.record.title}</h2>
          <p>{bundle.record.body}</p>
        </div>
        <div className="record-next-step">
          <span>Next action</span>
          <strong>{bundle.nextAction}</strong>
        </div>
      </div>

      <div className="record-detail-grid">
        <DetailSection icon={<ShieldCheck className="size-4" />} title="Evidence">
          <EvidenceSummary bundle={bundle} />
        </DetailSection>

        <DetailSection icon={<GitPullRequest className="size-4" />} title="PR links">
          {bundle.prs.length === 0 ? (
            <EmptyState text="No pull request is linked to this record yet." />
          ) : (
            <div className="link-list">
              {bundle.prs.map((url) => <ExternalLinkRow key={url} href={url} label={url} />)}
            </div>
          )}
        </DetailSection>

        <DetailSection icon={<Bot className="size-4" />} title="Agent sessions">
          <SessionTree primarySessions={primarySessions} childSessions={childSessions} sessions={bundle.sessions} />
        </DetailSection>

        <DetailSection icon={<Terminal className="size-4" />} title="Commands">
          {bundle.commands.length === 0 ? (
            <EmptyState text="No command evidence is linked to this record." />
          ) : (
            <CommandTable commands={bundle.commands} compact />
          )}
        </DetailSection>

        <DetailSection icon={<MessageSquareText className="size-4" />} title="Transcript preview">
          <EventList
            events={transcriptEvents.slice(0, 8)}
            emptyText="No redacted prompt or assistant message events are available for this record."
          />
        </DetailSection>

        <DetailSection icon={<Wrench className="size-4" />} title="Tools and raw payloads">
          <EventList
            events={toolEvents.slice(0, 8)}
            emptyText="No redacted tool-call or tool-output events are available for this record."
          />
          {toolEvents.some((event) => event.payload_blob_id) ? (
            <p className="privacy-note">Raw payloads are stored out-of-band and withheld from this share-safe view.</p>
          ) : null}
        </DetailSection>

        <DetailSection icon={<FileText className="size-4" />} title="Files, artifacts, and summaries">
          <div className="detail-columns">
            <MiniList
              icon={<FileText className="size-3.5" />}
              title="Files touched"
              items={bundle.files.map((file) => `${valueText(file.path, "file")} · ${valueText(file.change_kind, "changed")}`)}
              empty="No file touch metadata is linked to this record."
            />
            <MiniList
              icon={<Archive className="size-3.5" />}
              title="Artifacts"
              items={bundle.artifacts.map((artifact) => `${valueText(artifact.kind, "artifact")} · ${valueText(artifact.redaction_state, "redacted")}`)}
              empty="No artifacts are linked to this record."
            />
            <MiniList
              icon={<MessageSquareText className="size-3.5" />}
              title="Summaries"
              items={bundle.summaries.map((summary) => valueText(summary.text, valueText(summary.kind, "summary")))}
              empty="No generated summaries are linked to this record."
            />
          </div>
        </DetailSection>
      </div>

      {failedCommands.length > 0 ? (
        <div className="record-warning">
          <AlertTriangle className="size-4" aria-hidden />
          <span>{failedCommands.length} command{failedCommands.length === 1 ? "" : "s"} failed. Re-run or explain before using this record as PR evidence.</span>
        </div>
      ) : null}
    </section>
  );
}

function EvidenceSummary({ bundle }: { bundle: RecordBundle }) {
  const passed = bundle.commands.filter((command) => command.exit_code === 0).length + bundle.evidence.filter((item) => evidenceTone(item.status) === "ok").length;
  const failed = bundle.commands.filter((command) => command.exit_code !== 0).length + bundle.evidence.filter((item) => evidenceTone(item.status) === "danger").length;
  const stale = bundle.evidence.filter((item) => valueText(item.freshness).toLowerCase() === "stale").length;
  return (
    <div className="evidence-summary">
      <MiniFact label="Positive signals" value={passed} tone="ok" />
      <MiniFact label="Failed" value={failed} tone={failed > 0 ? "danger" : "neutral"} />
      <MiniFact label="Stale" value={stale} tone={stale > 0 ? "warn" : "neutral"} />
      <MiniFact label="PR links" value={bundle.prs.length} tone={bundle.prs.length > 0 ? "ok" : "neutral"} />
    </div>
  );
}

function TimelineView({ data }: { data: DashboardData }) {
  const items = timelineItems(data);
  return (
    <section className="panel">
      <SectionHeader icon={<Workflow className="size-4" />} title="Work timeline" />
      <TimelineList items={items} emptyText="No commands, transcript events, or evidence events are available yet." />
    </section>
  );
}

function TimelineList({ items, emptyText, compact = false }: { items: TimelineItem[]; emptyText: string; compact?: boolean }) {
  if (items.length === 0) return <EmptyState text={emptyText} />;
  return (
    <div className={clsx("timeline-list", compact && "timeline-list-compact")}>
      {items.map((item) => (
        <article className={clsx("timeline-item", `timeline-${item.tone}`)} key={item.id}>
          <div className="timeline-icon">{item.icon}</div>
          <div className="min-w-0">
            <div className="timeline-title-row">
              <span className="badge">{item.kind}</span>
              <strong>{item.title}</strong>
              {item.occurredAt ? <span>{formatDate(item.occurredAt)}</span> : null}
            </div>
            <p>{item.preview}</p>
          </div>
        </article>
      ))}
    </div>
  );
}

function EvidenceView({ bundles, data }: { bundles: RecordBundle[]; data: DashboardData }) {
  const prBundles = bundles.filter((bundle) => bundle.prs.length > 0 || bundle.commands.length > 0 || bundle.evidence.length > 0);
  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1.1fr)_minmax(0,0.9fr)]">
      <section className="panel">
        <SectionHeader icon={<ShieldCheck className="size-4" />} title="PR evidence readiness" />
        {prBundles.length === 0 ? (
          <EmptyState text="No PR links or evidence records are available yet." />
        ) : (
          <div className="evidence-card-list">
            {prBundles.map((bundle) => (
              <article className={clsx("evidence-card", `evidence-${bundle.tone}`)} key={bundle.record.id}>
                <div className="evidence-card-head">
                  <div className="min-w-0">
                    <h3>{bundle.record.title}</h3>
                    <p>{bundle.nextAction}</p>
                  </div>
                  <span className={clsx("badge", toneClass(bundle.tone))}>{bundle.statusLabel}</span>
                </div>
                <EvidenceSummary bundle={bundle} />
                {bundle.prs.length > 0 ? (
                  <div className="mt-3 link-list">
                    {bundle.prs.map((url) => <ExternalLinkRow key={url} href={url} label={url} />)}
                  </div>
                ) : null}
              </article>
            ))}
          </div>
        )}
      </section>

      <section className="panel">
        <SectionHeader icon={<Command className="size-4" />} title="Command evidence" />
        <CommandTable commands={data.commands} />
      </section>

      <section className="panel lg:col-span-2">
        <SectionHeader icon={<Archive className="size-4" />} title="Artifacts and attachments" />
        {data.artifacts.length === 0 ? (
          <EmptyState text="No artifacts are available in this export." />
        ) : (
          <div className="artifact-grid">
            {data.artifacts.map((artifact) => (
              <article className="artifact-card" key={artifact.id}>
                <div className="flex flex-wrap items-center gap-2">
                  <span className="badge">{valueText(artifact.kind, "artifact")}</span>
                  <span className="text-sm text-muted-foreground">{valueText(artifact.media_type, "media")} · {valueText(artifact.byte_size, "0")} bytes</span>
                  <span className="badge">{valueText(artifact.redaction_state, "redacted")}</span>
                </div>
                {artifact.preview ? <p>{artifact.preview}</p> : null}
              </article>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function SearchView({ data, query, setQuery }: { data: DashboardData; query: string; setQuery: (value: string) => void }) {
  const results = useMemo(() => searchResults(data, query), [data, query]);
  return (
    <section className="panel">
      <SectionHeader icon={<Search className="size-4" />} title="Search agent history" />
      <div className="search-box">
        <Search className="size-4 text-muted-foreground" />
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search prompts, commands, tool output previews, files, summaries, PRs"
        />
      </div>
      <p className="mt-2 text-sm text-muted-foreground">Agent-facing equivalent: <code>{data.status.search_command}</code></p>
      <div className="mt-4 search-results">
        {results.map((result) => (
          <article className="search-result" key={`${result.type}-${result.id}`}>
            <div className="search-result-head">
              <span className="badge">{result.type}</span>
              <strong>{result.title}</strong>
            </div>
            <p>{result.body || result.id}</p>
            {result.recordTitle ? <span className="text-xs text-muted-foreground">Record: {result.recordTitle}</span> : null}
          </article>
        ))}
      </div>
    </section>
  );
}

function SetupHealthView({ data }: { data: DashboardData }) {
  const providers = providerSummaries(data);
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <section className="panel">
        <SectionHeader icon={<Database className="size-4" />} title="Capture health" />
        <div className="health-list">
          <HealthRow
            label="Share-safe export"
            value={data.share_safe ? "enabled" : "check redaction"}
            detail={data.share_safe ? "Raw transcript content is not shown on this page." : "Review redaction settings before sharing."}
            tone={data.share_safe ? "ok" : "warn"}
          />
          <HealthRow
            label="Transcript payloads"
            value={`${data.privacy.raw_transcripts_withheld} withheld`}
            detail={`${data.privacy.redacted_previews} redacted previews are visible for review and search.`}
            tone={data.privacy.raw_transcripts_withheld > 0 ? "ok" : "neutral"}
          />
          <HealthRow
            label="Local paths"
            value={data.privacy.local_paths_redacted ? "redacted" : "visible"}
            detail="Local file paths should usually stay redacted in shareable reports."
            tone={data.privacy.local_paths_redacted ? "ok" : "warn"}
          />
          <HealthRow
            label="Search command"
            value={data.status.search_command}
            detail="Use this from agents or scripts to query prior work."
            tone="neutral"
          />
        </div>
      </section>

      <section className="panel">
        <SectionHeader icon={<Bot className="size-4" />} title="Capture sources" />
        {providers.length === 0 ? (
          <EmptyState title="No provider sessions" text={providerSparseText(data)} />
        ) : (
          <div className="provider-grid">
            {providers.map((provider) => (
              <article className="provider-card" key={provider.provider}>
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <div className="truncate font-semibold">{provider.provider}</div>
                    <div className="text-sm text-muted-foreground">{provider.sessions} session{provider.sessions === 1 ? "" : "s"} · {provider.events} event{provider.events === 1 ? "" : "s"}</div>
                  </div>
                  <span className={clsx("badge", provider.supportStatuses.some((status) => supportToneClass(status) === "badge-ok") ? "badge-ok" : "badge-warn")}>
                    {provider.supportStatuses[0] ?? "unclassified"}
                  </span>
                </div>
                <div className="mt-3 flex flex-wrap gap-2">
                  {provider.fidelities.map((fidelity) => <span className="badge" key={fidelity}>{fidelity}</span>)}
                  {provider.capturePaths.slice(0, 2).map((path) => <span className="badge" key={path}>{path}</span>)}
                </div>
              </article>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function DetailSection({ icon, title, children }: { icon: React.ReactNode; title: string; children: React.ReactNode }) {
  return (
    <div className="detail-section">
      <div className="detail-section-title">
        {icon}
        <h3>{title}</h3>
      </div>
      {children}
    </div>
  );
}

function SessionTree({
  primarySessions,
  childSessions,
  sessions
}: {
  primarySessions: DashboardSession[];
  childSessions: DashboardSession[];
  sessions: DashboardSession[];
}) {
  if (sessions.length === 0) {
    return <EmptyState text="No provider session metadata is linked to this record." />;
  }
  return (
    <div className="session-tree">
      {primarySessions.length > 0 ? (
        <MiniList
          icon={<Bot className="size-3.5" />}
          title="Primary"
          items={primarySessions.map(sessionLabel)}
          empty="No primary session is marked in this export."
        />
      ) : null}
      <MiniList
        icon={<Workflow className="size-3.5" />}
        title={childSessions.length > 0 ? "Subagents / child sessions" : "Sessions"}
        items={(childSessions.length > 0 ? childSessions : sessions).map(sessionLabel)}
        empty="No child sessions are marked in this export."
      />
      <MiniList
        icon={<ShieldCheck className="size-3.5" />}
        title="Capture notes"
        items={sessions.map((session) => valueText(session.privacy_note ?? session.capture_path, "share-safe capture")).filter(Boolean)}
        empty="No privacy or capture notes are attached."
      />
    </div>
  );
}

function EventList({ events, emptyText }: { events: DashboardEvent[]; emptyText: string }) {
  if (events.length === 0) return <EmptyState text={emptyText} />;
  return (
    <div className="event-list">
      {sortByTime(events, (event) => event.occurred_at).map((event) => (
        <article className="transcript-event" key={event.id}>
          <div className="mb-2 flex flex-wrap items-center gap-2 text-xs">
            {isToolEvent(event) ? <Wrench className="size-3.5 text-muted-foreground" /> : <MessageSquareText className="size-3.5 text-muted-foreground" />}
            <span className="badge">{valueText(event.event_type, "event")}</span>
            {event.role ? <span className="badge">{event.role}</span> : null}
            {event.seq !== undefined && event.seq !== null ? <span className="text-muted-foreground">#{event.seq}</span> : null}
            <span className="badge">{valueText(event.redaction_state, "redacted")}</span>
            {event.payload_blob_id ? <span className="badge">raw withheld</span> : null}
          </div>
          <p>{eventPreviewText(event)}</p>
        </article>
      ))}
    </div>
  );
}

function CommandTable({ commands, compact = false }: { commands: EvidenceCommand[]; compact?: boolean }) {
  const isMobile = useMediaQuery("(max-width: 640px)");
  const columns = useMemo<ColumnDef<EvidenceCommand>[]>(
    () => [
      { accessorKey: "command", header: "Command", cell: (info) => <code>{String(info.getValue())}</code> },
      {
        accessorKey: "exit_code",
        header: "Exit",
        cell: (info) => <ExitBadge exitCode={Number(info.getValue())} />
      },
      { accessorKey: "duration_ms", header: "Duration", cell: (info) => durationText(Number(info.getValue())) },
      { accessorKey: "output_preview", header: "Preview" }
    ],
    []
  );
  const table = useReactTable({ data: commands, columns, getCoreRowModel: getCoreRowModel() });
  if (commands.length === 0) return <EmptyState text="No command evidence has been captured yet." />;
  if (isMobile || compact) {
    return (
      <div className="command-card-list command-card-list-visible">
        {commands.map((command) => (
          <article className={clsx("command-card", command.exit_code !== 0 && "command-card-danger")} key={command.id}>
            <div className="command-card-command">
              <span>Command</span>
              <code>{command.command}</code>
            </div>
            <div className="command-card-meta">
              <KeyValue label="Exit" value={<ExitBadge exitCode={command.exit_code} />} />
              <KeyValue label="Duration" value={durationText(command.duration_ms)} />
            </div>
            {command.output_preview ? (
              <div className="command-card-preview">
                <span>Preview</span>
                <p>{command.output_preview}</p>
              </div>
            ) : null}
          </article>
        ))}
      </div>
    );
  }

  return (
    <div className="table-scroll">
      <table className="data-table">
        <thead>
          {table.getHeaderGroups().map((headerGroup) => (
            <tr key={headerGroup.id}>
              {headerGroup.headers.map((header) => (
                <th key={header.id}>{flexRender(header.column.columnDef.header, header.getContext())}</th>
              ))}
            </tr>
          ))}
        </thead>
        <tbody>
          {table.getRowModel().rows.map((row) => (
            <tr className={row.original.exit_code !== 0 ? "data-row-danger" : undefined} key={row.id}>
              {row.getVisibleCells().map((cell) => (
                <td key={cell.id}>{flexRender(cell.column.columnDef.cell, cell.getContext())}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function MiniList({ icon, title, items, empty }: { icon: React.ReactNode; title: string; items: string[]; empty: string }) {
  return (
    <div className="mini-list">
      <div className="mini-list-title">{icon}<span>{title}</span></div>
      {items.length === 0 ? (
        <p className="mini-list-empty">{empty}</p>
      ) : (
        <ul>
          {items.slice(0, 6).map((item) => <li key={item}>{item}</li>)}
        </ul>
      )}
    </div>
  );
}

function MiniFact({ label, value, tone = "neutral" }: { label: string; value: React.ReactNode; tone?: StatusTone }) {
  return (
    <div className={clsx("mini-fact", `mini-fact-${tone}`)}>
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function HealthRow({
  label,
  value,
  detail,
  tone
}: {
  label: string;
  value: React.ReactNode;
  detail: string;
  tone: StatusTone;
}) {
  return (
    <article className="health-row">
      <span className={clsx("health-dot", `attention-${tone}`)} />
      <div className="min-w-0">
        <div className="health-title">
          <strong>{label}</strong>
          <span>{value}</span>
        </div>
        <p>{detail}</p>
      </div>
    </article>
  );
}

function ExternalLinkRow({ href, label }: { href: string; label: string }) {
  return (
    <a className="link-row" href={href} rel="noreferrer">
      <span>{label}</span>
      <ExternalLink className="size-3.5" aria-hidden />
    </a>
  );
}

function SectionHeader({ icon, title }: { icon: React.ReactNode; title: string }) {
  return (
    <div className="mb-3 flex items-center gap-2">
      <div className="section-icon">{icon}</div>
      <h2 className="text-base font-semibold">{title}</h2>
    </div>
  );
}

function StatusPill({
  tone,
  icon,
  children
}: {
  tone: StatusTone;
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return <span className={clsx("status-pill", `status-${tone}`)}>{icon}{children}</span>;
}

function ExitBadge({ exitCode }: { exitCode: number }) {
  return <span className={clsx("badge", exitCode === 0 ? "badge-ok" : "badge-danger")}>Exit {exitCode}</span>;
}

function EmptyState({ title, text }: { title?: string; text: string }) {
  return (
    <div className="empty-state">
      {title ? <strong>{title}</strong> : null}
      <span>{text}</span>
    </div>
  );
}

function KeyValue({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="key-value">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function useMediaQuery(query: string) {
  const [matches, setMatches] = React.useState(false);

  React.useEffect(() => {
    const mediaQuery = window.matchMedia(query);
    const update = () => setMatches(mediaQuery.matches);
    update();
    mediaQuery.addEventListener("change", update);
    return () => mediaQuery.removeEventListener("change", update);
  }, [query]);

  return matches;
}

function recordBundles(dashboardData: DashboardData): RecordBundle[] {
  return sortByTime(dashboardData.records, (record) => record.updated_at).map((record) => {
    const commands = relatedToRecord(dashboardData.commands, record.id);
    const evidence = evidenceForRecord(dashboardData, record.id);
    const sessions = relatedToRecord(dashboardData.sessions, record.id);
    const runs = relatedToRecord(dashboardData.runs, record.id);
    const events = relatedToRecord(dashboardData.events, record.id);
    const artifacts = relatedToRecord(dashboardData.artifacts, record.id);
    const files = relatedToRecord(dashboardData.files_touched, record.id);
    const summaries = relatedToRecord(dashboardData.summaries, record.id);
    const prs = uniquePullRequestUrls(dashboardData, record);
    const health = bundleHealth(commands, evidence, sessions, prs);
    return {
      record,
      commands,
      evidence,
      sessions,
      runs,
      events,
      artifacts,
      files,
      summaries,
      prs,
      tone: health.tone,
      statusLabel: health.statusLabel,
      nextAction: health.nextAction
    };
  });
}

function bundleHealth(
  commands: EvidenceCommand[],
  evidence: ReturnType<typeof evidenceForRecord>,
  sessions: DashboardSession[],
  prs: string[]
) {
  const failedCommands = commands.filter((command) => command.exit_code !== 0).length;
  const failedEvidence = evidence.filter((item) => evidenceTone(item.status) === "danger").length;
  const staleEvidence = evidence.filter((item) => valueText(item.freshness).toLowerCase() === "stale").length;
  const blockedSessions = sessions.filter((session) => valueText(session.status).toLowerCase() === "blocked").length;

  if (failedCommands > 0 || failedEvidence > 0) {
    return {
      tone: "danger" as StatusTone,
      statusLabel: "Needs review",
      nextAction: "Fix or explain failed evidence"
    };
  }
  if (staleEvidence > 0 || blockedSessions > 0) {
    return {
      tone: "warn" as StatusTone,
      statusLabel: "Stale or partial",
      nextAction: "Refresh evidence or capture path"
    };
  }
  if (prs.length > 0) {
    return {
      tone: "ok" as StatusTone,
      statusLabel: "PR-ready",
      nextAction: "Use as review context"
    };
  }
  return {
    tone: "neutral" as StatusTone,
    statusLabel: "Recorded",
    nextAction: "Search or link to a PR"
  };
}

function evidenceForRecord(dashboardData: DashboardData, recordId: string) {
  return dashboardData.evidence_metadata.filter((item) => valueText(item.work_record_id) === recordId);
}

function attentionQueue(bundles: RecordBundle[], dashboardData: DashboardData) {
  const items: { id: string; recordId?: string; title: string; detail: string; tone: StatusTone }[] = [];
  for (const bundle of bundles) {
    for (const command of bundle.commands.filter((item) => item.exit_code !== 0)) {
      items.push({
        id: `command-${command.id}`,
        recordId: bundle.record.id,
        title: bundle.record.title,
        detail: `Command failed: ${command.command}`,
        tone: "danger"
      });
    }
    for (const evidence of bundle.evidence.filter((item) => evidenceTone(item.status) === "danger" || valueText(item.freshness).toLowerCase() === "stale")) {
      items.push({
        id: `evidence-${evidence.id}`,
        recordId: bundle.record.id,
        title: bundle.record.title,
        detail: `${valueText(evidence.kind, "evidence")} is ${valueText(evidence.status, "unknown")}${evidence.stale_reason ? `: ${evidence.stale_reason}` : ""}`,
        tone: evidenceTone(evidence.status) === "danger" ? "danger" : "warn"
      });
    }
  }

  const blockedProviders = dashboardData.sessions.filter((session) => valueText(session.status).toLowerCase() === "blocked");
  for (const session of blockedProviders.slice(0, 4)) {
    items.push({
      id: `provider-${session.id}`,
      recordId: valueText(session.work_record_id),
      title: `${valueText(session.provider, "provider")} capture`,
      detail: valueText(session.privacy_note ?? session.capture_path, "capture path is blocked or unsupported"),
      tone: "warn"
    });
  }

  return items.slice(0, 8);
}

function timelineItems(dashboardData: DashboardData): TimelineItem[] {
  const recordById = new Map(dashboardData.records.map((record) => [record.id, record]));
  const eventItems = dashboardData.events.map((event): TimelineItem => ({
    id: `event-${event.id}`,
    occurredAt: event.occurred_at,
    recordId: event.work_record_id,
    sessionId: event.session_id,
    kind: valueText(event.event_type, "event"),
    title: timelineEventTitle(event, recordById.get(valueText(event.work_record_id))),
    preview: eventPreviewText(event),
    tone: isToolEvent(event) ? "neutral" : "ok",
    icon: isToolEvent(event) ? <Wrench className="size-4" /> : <MessageSquareText className="size-4" />
  }));
  const commandItems = dashboardData.commands.map((command): TimelineItem => ({
    id: `command-${command.id}`,
    occurredAt: command.started_at,
    recordId: command.record_id,
    kind: "command",
    title: command.command,
    preview: command.output_preview ?? "No command preview captured.",
    tone: command.exit_code === 0 ? "ok" : "danger",
    icon: <Terminal className="size-4" />
  }));
  const evidenceItems = dashboardData.evidence_metadata.map((evidence): TimelineItem => ({
    id: `evidence-${evidence.id}`,
    occurredAt: null,
    recordId: evidence.work_record_id,
    kind: valueText(evidence.kind, "evidence"),
    title: valueText(recordById.get(valueText(evidence.work_record_id))?.title, "Evidence update"),
    preview: `${valueText(evidence.status, "unknown")} · ${valueText(evidence.freshness, "freshness unknown")}${evidence.stale_reason ? ` · ${evidence.stale_reason}` : ""}`,
    tone: evidenceTone(evidence.status),
    icon: <ShieldCheck className="size-4" />
  }));
  return sortByTime([...eventItems, ...commandItems, ...evidenceItems], (item) => item.occurredAt);
}

function timelineEventTitle(event: DashboardEvent, record: DashboardRecord | undefined) {
  const provider = valueText(event.role, valueText(event.event_type, "event"));
  return record ? `${provider} · ${record.title}` : provider;
}

function searchResults(dashboardData: DashboardData, query: string) {
  const records = new Map(dashboardData.records.map((record) => [record.id, record.title]));
  const haystack = [
    ...dashboardData.records.map((record) => ({ type: "record", title: record.title, body: record.body, id: record.id, recordTitle: record.title })),
    ...dashboardData.commands.map((command) => ({ type: "command", title: command.command, body: command.output_preview ?? "", id: command.id, recordTitle: records.get(valueText(command.record_id)) })),
    ...dashboardData.events.map((event) => ({ type: valueText(event.event_type, "event"), title: eventPreviewText(event).slice(0, 90), body: `${event.role ?? ""} ${eventPreviewText(event)}`, id: event.id, recordTitle: records.get(valueText(event.work_record_id)) })),
    ...dashboardData.files_touched.map((file) => ({ type: "file", title: valueText(file.path, "file"), body: `${valueText(file.change_kind)} ${valueText(file.confidence)}`, id: valueText(file.id, valueText(file.path)), recordTitle: records.get(valueText(file.work_record_id)) })),
    ...dashboardData.artifacts.map((artifact) => ({ type: "artifact", title: valueText(artifact.kind, "artifact"), body: valueText(artifact.preview), id: artifact.id, recordTitle: records.get(valueText(artifact.work_record_id)) })),
    ...dashboardData.summaries.map((summary) => ({ type: "summary", title: valueText(summary.kind, "summary"), body: valueText(summary.text), id: valueText(summary.id), recordTitle: records.get(valueText(summary.work_record_id)) }))
  ];
  const term = query.trim().toLowerCase();
  if (!term) return haystack.slice(0, 14);
  return haystack.filter((item) => `${item.type} ${item.title} ${item.body} ${item.recordTitle ?? ""}`.toLowerCase().includes(term)).slice(0, 24);
}

function uniquePullRequestUrls(dashboardData: DashboardData, record?: DashboardRecord) {
  if (record) {
    const urls = [record.pr_url];
    const recordChanges = relatedToRecord(dashboardData.vcs_changes, record.id);
    for (const change of recordChanges) {
      const prUrl = valueText(change.pr_url);
      if (prUrl) urls.push(prUrl);
    }
    for (const pr of dashboardData.pull_requests) {
      if (record.pr_url && pr.url === record.pr_url) urls.push(pr.url);
    }
    return Array.from(new Set(urls.filter((url): url is string => typeof url === "string" && url.trim().length > 0)));
  }
  const urls = [
    ...dashboardData.pull_requests.map((pr) => pr.url),
    ...dashboardData.records.map((item) => item.pr_url)
  ];
  return Array.from(new Set(urls.filter((url): url is string => typeof url === "string" && url.trim().length > 0)));
}

function providerSummaries(dashboardData: DashboardData) {
  const summaries = new Map<string, {
    provider: string;
    sessions: number;
    events: number;
    runs: number;
    fidelities: string[];
    supportStatuses: string[];
    capturePaths: string[];
  }>();

  for (const session of dashboardData.sessions) {
    const provider = valueText(session.provider, "unknown");
    const current = summaries.get(provider) ?? {
      provider,
      sessions: 0,
      events: 0,
      runs: 0,
      fidelities: [],
      supportStatuses: [],
      capturePaths: []
    };
    const id = valueText(session.id);
    current.sessions += 1;
    current.events += relatedBySession(dashboardData.events, id).length;
    current.runs += relatedBySession(dashboardData.runs, id).length;
    addUnique(current.fidelities, valueText(session.fidelity, "unknown"));
    addUnique(current.supportStatuses, valueText(session.support_status, "unclassified"));
    if (session.capture_path) addUnique(current.capturePaths, valueText(session.capture_path));
    summaries.set(provider, current);
  }

  return Array.from(summaries.values()).sort((left, right) => left.provider.localeCompare(right.provider));
}

function addUnique(values: string[], value: string) {
  if (!values.includes(value)) values.push(value);
}

function sessionLabel(session: DashboardSession) {
  const primary = session.is_primary ? "primary" : session.parent_session_id ? `child of ${session.parent_session_id}` : "session";
  return `${valueText(session.provider, "provider")} · ${valueText(session.role_hint ?? session.agent_type, primary)} · ${valueText(session.fidelity, "unknown fidelity")}`;
}

function relatedToRecord<T extends Record<string, unknown>>(items: T[], recordId: string) {
  return items.filter((item) => {
    const itemRecordId = valueText(item.work_record_id ?? item.record_id);
    return itemRecordId === recordId;
  });
}

function relatedBySession<T extends Record<string, unknown>>(items: T[], id: string) {
  if (!id) return [];
  return items.filter((item) => valueText(item.session_id) === id);
}

function sortByTime<T>(items: T[], getter: (item: T) => string | null | undefined) {
  return [...items].sort((left, right) => dateValue(getter(right)) - dateValue(getter(left)));
}

function dateValue(value: string | null | undefined) {
  if (!value) return 0;
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function valueText(value: unknown, fallback = "") {
  if (value === undefined || value === null || value === "") return fallback;
  return String(value);
}

function formatDate(value: string | null | undefined) {
  if (!value) return "unknown";
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) return value;
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit"
  }).format(new Date(parsed));
}

function durationText(value: number) {
  if (!Number.isFinite(value)) return "unknown";
  if (value < 1000) return `${value}ms`;
  return `${(value / 1000).toFixed(1)}s`;
}

function isToolEvent(event: DashboardEvent) {
  return event.event_type === "tool_call" || event.event_type === "tool_output";
}

function eventPreviewText(event: DashboardEvent): string {
  const preview = valueText(event.preview, "raw event payload withheld");
  const trimmed = preview.trim();
  if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) return preview;

  try {
    const parsed = JSON.parse(trimmed) as Record<string, unknown>;
    const body = parsed.body;
    if (typeof body === "string" && body.trim().length > 0) return body;
    if (body && typeof body === "object") {
      const bodyText = (body as Record<string, unknown>).text;
      if (typeof bodyText === "string" && bodyText.trim().length > 0) return bodyText;
    }

    const provider = typeof parsed.provider === "string" ? parsed.provider : null;
    const session = typeof parsed.provider_session_id === "string" ? parsed.provider_session_id : null;
    const cursor = typeof parsed.cursor === "string" ? parsed.cursor : null;
    const structuredPreview = [provider, session, cursor].filter(Boolean).join(" · ");
    return structuredPreview || "structured provider event preview withheld";
  } catch {
    return preview;
  }
}

function providerSparseText(dashboardData: DashboardData) {
  if (dashboardData.records.length === 0 && dashboardData.commands.length === 0 && dashboardData.events.length === 0) {
    return "No work has been recorded in this export yet.";
  }
  if (dashboardData.records.length > 0 && dashboardData.sessions.length === 0) {
    return "Work records exist, but this capture path did not provide provider session metadata. Summary-only imports can still appear as records, commands, and summaries.";
  }
  return "Provider metadata is present, but no matching redacted events are available for the selected session.";
}

function supportToneClass(status: unknown) {
  const normalized = valueText(status).toLowerCase();
  if (normalized === "supported-import" || normalized === "supported-live" || normalized === "supported-wrapper") {
    return "badge-ok";
  }
  if (normalized === "fixture-only" || normalized === "detected-unsupported") {
    return "badge-warn";
  }
  if (normalized === "blocked") return "badge-danger";
  return undefined;
}

function evidenceTone(status: unknown): StatusTone {
  const normalized = valueText(status).toLowerCase();
  if (normalized === "passed" || normalized === "succeeded" || normalized === "success") return "ok";
  if (normalized === "failed" || normalized === "error" || normalized === "failure" || normalized === "blocked") return "danger";
  return "warn";
}

function toneClass(tone: StatusTone) {
  if (tone === "ok") return "badge-ok";
  if (tone === "warn") return "badge-warn";
  if (tone === "danger") return "badge-danger";
  return undefined;
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
