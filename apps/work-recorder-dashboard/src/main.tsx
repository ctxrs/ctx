import React, { useMemo, useState } from "react";
import ReactDOM from "react-dom/client";
import * as Tabs from "@radix-ui/react-tabs";
import { ColumnDef, flexRender, getCoreRowModel, useReactTable } from "@tanstack/react-table";
import {
  Activity,
  AlertTriangle,
  Archive,
  CheckCircle2,
  Command,
  Database,
  FileText,
  GitBranch,
  Monitor,
  Moon,
  Search,
  Settings,
  ShieldCheck,
  Sun,
  Terminal,
  Workflow
} from "lucide-react";
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { clsx } from "clsx";
import { readDashboardData } from "./data";
import type { DashboardData, DashboardRecord, EvidenceCommand } from "./types";
import "./styles.css";

const data = readDashboardData();

function App() {
  const [theme, setTheme] = useState<"light" | "dark">("light");
  const [query, setQuery] = useState("");
  const [activeTab, setActiveTab] = useState("overview");
  const failedCommands = data.commands.filter((command) => command.exit_code !== 0).length;
  const linkedPrUrls = uniquePullRequestUrls(data);

  React.useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.documentElement.classList.toggle("dark", theme === "dark");
  }, [theme]);

  React.useEffect(() => {
    const activeTrigger = document.querySelector<HTMLButtonElement>(`[data-dashboard-tab="${activeTab}"]`);
    const tabList = activeTrigger?.closest<HTMLElement>(".tab-list");
    if (!activeTrigger || !tabList) return;

    const scrollActiveTab = () => {
      activeTrigger.scrollIntoView({ block: "nearest", inline: "center", behavior: "instant" });
    };

    scrollActiveTab();
    requestAnimationFrame(scrollActiveTab);
  }, [activeTab]);

  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="border-b border-border bg-card">
        <div className="mx-auto flex max-w-7xl flex-col gap-4 px-4 py-4 sm:px-6 lg:flex-row lg:items-center lg:justify-between">
          <div className="min-w-0">
            <div className="flex items-center gap-2 text-sm font-medium text-muted-foreground">
              <Monitor className="size-4" aria-hidden />
              <span>Local Work Recorder</span>
              <span className="rounded-sm border border-border px-1.5 py-0.5 text-xs">{data.status.javascript_app}</span>
            </div>
            <h1 className="mt-1 text-2xl font-semibold tracking-normal sm:text-3xl">Work Records</h1>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <StatusPill tone={data.share_safe ? "ok" : "warn"} icon={<ShieldCheck className="size-3.5" />}>
              Share-safe export
            </StatusPill>
            {failedCommands > 0 ? (
              <StatusPill tone="danger" icon={<AlertTriangle className="size-3.5" />}>
                {failedCommands} failing command{failedCommands === 1 ? "" : "s"}
              </StatusPill>
            ) : (
              <StatusPill tone="ok" icon={<CheckCircle2 className="size-3.5" />}>Evidence passing</StatusPill>
            )}
            <button
              className="icon-button"
              title={theme === "dark" ? "Use light theme" : "Use dark theme"}
              onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
              type="button"
            >
              {theme === "dark" ? <Sun className="size-4" /> : <Moon className="size-4" />}
            </button>
          </div>
        </div>
      </header>

      <main className="mx-auto max-w-7xl px-4 py-5 sm:px-6">
        <div className="mb-5 grid gap-3 md:grid-cols-4">
          <Metric label="Records" value={data.summary.record_count} />
          <Metric label="Evidence" value={data.summary.evidence_count} />
          <Metric label="Linked PRs" value={linkedPrUrls.length} />
          <Metric label="Raw transcripts withheld" value={data.privacy.raw_transcripts_withheld} />
        </div>

        <Tabs.Root value={activeTab} onValueChange={setActiveTab} className="space-y-4">
          <Tabs.List className="tab-list" aria-label="Dashboard views">
            <Tab value="overview" icon={<Activity className="size-4" />} label="Overview" />
            <Tab value="workspace" icon={<GitBranch className="size-4" />} label="Workspace" />
            <Tab value="session" icon={<Workflow className="size-4" />} label="Session" />
            <Tab value="evidence" icon={<ShieldCheck className="size-4" />} label="PR/Evidence" />
            <Tab value="search" icon={<Search className="size-4" />} label="Search" />
            <Tab value="settings" icon={<Settings className="size-4" />} label="Status" />
          </Tabs.List>

          <Tabs.Content value="overview">
            <Overview data={data} />
          </Tabs.Content>
          <Tabs.Content value="workspace">
            <WorkspaceView data={data} />
          </Tabs.Content>
          <Tabs.Content value="session">
            <SessionView data={data} />
          </Tabs.Content>
          <Tabs.Content value="evidence">
            <EvidenceView data={data} />
          </Tabs.Content>
          <Tabs.Content value="search">
            <SearchView data={data} query={query} setQuery={setQuery} />
          </Tabs.Content>
          <Tabs.Content value="settings">
            <SettingsView data={data} />
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

function Overview({ data }: { data: DashboardData }) {
  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,2fr)_minmax(340px,1fr)]">
      <section className="panel">
        <SectionHeader icon={<FileText className="size-4" />} title="Recent Records" />
        <div className="record-list">
          {data.records.length === 0 ? (
            <EmptyState text="No Work Records found in the local store." />
          ) : (
            data.records.map((record) => <RecordRow key={record.id} record={record} />)
          )}
        </div>
      </section>
      <div className="space-y-4">
        <section className="panel">
          <SectionHeader icon={<Activity className="size-4" />} title="Work Mix" />
          <div className="h-56">
            <ResponsiveContainer width="100%" height="100%">
              <BarChart data={tagChartData(data)}>
                <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" />
                <XAxis dataKey="name" tickLine={false} axisLine={false} />
                <YAxis allowDecimals={false} tickLine={false} axisLine={false} />
                <Tooltip cursor={{ fill: "hsl(var(--muted))" }} />
                <Bar dataKey="count" fill="hsl(var(--primary))" radius={[4, 4, 0, 0]} />
              </BarChart>
            </ResponsiveContainer>
          </div>
        </section>
        <section className="panel">
          <SectionHeader icon={<ShieldCheck className="size-4" />} title="Share and Publish Preview" />
          <p className="text-sm text-muted-foreground">
            Static local export with redacted summaries, command previews, safe PR links, and raw transcript content withheld by default.
          </p>
          <div className="mt-3 grid gap-2 text-sm">
            <KeyValue label="Records" value={data.records.length} />
            <KeyValue label="Commands" value={data.commands.length} />
            <KeyValue label="Withheld raw transcripts" value={data.privacy.raw_transcripts_withheld} />
          </div>
        </section>
      </div>
    </div>
  );
}

function WorkspaceView({ data }: { data: DashboardData }) {
  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      <section className="panel">
        <SectionHeader icon={<GitBranch className="size-4" />} title="Workspace / Repo" />
        {data.vcs_workspaces.length === 0 ? (
          <EmptyState text="No Git or jj state is available in this export." />
        ) : (
          <div className="space-y-3">
            {data.vcs_workspaces.map((workspace) => (
              <div className="row-card" key={String(workspace.id)}>
                <div className="flex items-center justify-between gap-3">
                  <div className="min-w-0">
                    <div className="font-medium">{String(workspace.repo ?? workspace.root ?? "workspace")}</div>
                    <div className="truncate text-sm text-muted-foreground">{String(workspace.monorepo_subpath ?? workspace.root ?? "")}</div>
                  </div>
                  <span className="badge">{String(workspace.kind ?? "vcs")}</span>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>
      <section className="panel">
        <SectionHeader icon={<FileText className="size-4" />} title="Files Touched" />
        {data.files_touched.length === 0 ? (
          <EmptyState text="No file touch metadata is available in this export." />
        ) : (
          <div className="overflow-auto">
            <table className="data-table">
              <thead>
                <tr><th>Path</th><th>Change</th><th>Delta</th></tr>
              </thead>
              <tbody>
                {data.files_touched.map((file) => (
                  <tr key={String(file.id)}>
                    <td><code>{String(file.path ?? "")}</code></td>
                    <td>{String(file.change_kind ?? "unknown")}</td>
                    <td>{String(file.line_count_delta ?? "")}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
      <section className="panel lg:col-span-2">
        <SectionHeader icon={<GitBranch className="size-4" />} title="Git and jj Changes" />
        {data.vcs_changes.length === 0 ? (
          <EmptyState text="No Git or jj changes are available in this export." />
        ) : (
          <div className="grid gap-3 md:grid-cols-2">
            {data.vcs_changes.map((change) => (
              <div className="row-card" key={String(change.id)}>
                <div className="flex items-center gap-2">
                  <span className="badge">{String(change.kind ?? "change")}</span>
                  <code>{String(change.change_id ?? "")}</code>
                </div>
                <div className="mt-2 text-sm text-muted-foreground">{String(change.branch_or_bookmark ?? "detached")}</div>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function SessionView({ data }: { data: DashboardData }) {
  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      <section className="panel">
        <SectionHeader icon={<Workflow className="size-4" />} title="Sessions and Runs" />
        {data.sessions.length === 0 && data.runs.length === 0 ? (
          <EmptyState text="No session or run metadata is available in this export." />
        ) : (
          <div className="space-y-3">
            {data.sessions.map((session) => (
              <div className="row-card" key={String(session.id)}>
                <div className="flex flex-wrap items-center gap-2">
                  <span className="badge">{String(session.provider ?? "provider")}</span>
                  <span className="badge">{String(session.status ?? "status")}</span>
                  <span className="text-sm text-muted-foreground">{String(session.role_hint ?? session.agent_type ?? "")}</span>
                </div>
              </div>
            ))}
            {data.runs.map((run) => (
              <div className="row-card" key={String(run.id)}>
                <div className="flex flex-wrap items-center gap-2">
                  <Terminal className="size-4 text-muted-foreground" />
                  <span className="font-medium">{String(run.command_preview ?? run.run_type ?? "run")}</span>
                  <span className={clsx("badge", run.status === "succeeded" ? "badge-ok" : "badge-warn")}>{String(run.status ?? "")}</span>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>
      <WorkTranscriptView data={data} />
      <section className="panel lg:col-span-2">
        <SectionHeader icon={<Command className="size-4" />} title="Commands" />
        <CommandTable commands={data.commands} />
      </section>
    </div>
  );
}

function WorkTranscriptView({ data }: { data: DashboardData }) {
  const transcriptEvents = data.events.filter((event) =>
    ["message", "tool_call", "tool_output"].includes(String(event.event_type))
  );
  return (
    <section className="panel">
      <SectionHeader icon={<Workflow className="size-4" />} title="Transcript, Messages, and Tool Calls" />
      {transcriptEvents.length === 0 ? (
        <EmptyState text="No redacted transcript events are available. Raw transcript blobs remain withheld." />
      ) : (
        <div className="transcript">
          {transcriptEvents.map((event) => (
            <article className="transcript-event" key={String(event.id)}>
              <div className="mb-2 flex flex-wrap items-center gap-2 text-xs">
                <span className="badge">{String(event.event_type)}</span>
                {event.role ? <span className="badge">{String(event.role)}</span> : null}
                <span className="text-muted-foreground">#{String(event.seq)}</span>
                <span className="text-muted-foreground">{String(event.redaction_state)}</span>
              </div>
              <p>{String(event.preview ?? "raw event payload withheld")}</p>
            </article>
          ))}
        </div>
      )}
    </section>
  );
}

function EvidenceView({ data }: { data: DashboardData }) {
  const linkedPrUrls = uniquePullRequestUrls(data);

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
      <section className="panel">
        <SectionHeader icon={<ShieldCheck className="size-4" />} title="Evidence Previews" />
        <CommandTable commands={data.commands} />
      </section>
      <section className="panel">
        <SectionHeader icon={<GitBranch className="size-4" />} title="PR Links" />
        {linkedPrUrls.length === 0 ? (
          <EmptyState text="No pull request links are available in this export." />
        ) : (
          <div className="space-y-3">
            {linkedPrUrls.map((url) => (
              <a className="link-row" href={url} key={url} rel="noreferrer">
                {url}
              </a>
            ))}
          </div>
        )}
      </section>
      <section className="panel">
        <SectionHeader icon={<Archive className="size-4" />} title="Artifacts" />
        {data.artifacts.length === 0 ? (
          <EmptyState text="No artifacts are available in this export." />
        ) : (
          <div className="space-y-3">
            {data.artifacts.map((artifact) => (
              <div className="row-card" key={String(artifact.id)}>
                <div className="flex flex-wrap items-center gap-2">
                  <span className="badge">{String(artifact.kind ?? "artifact")}</span>
                  <span className="text-sm text-muted-foreground">{String(artifact.byte_size ?? 0)} bytes</span>
                  <span className="badge">{String(artifact.redaction_state ?? "redacted")}</span>
                </div>
                {artifact.preview ? <pre className="preview">{String(artifact.preview)}</pre> : null}
              </div>
            ))}
          </div>
        )}
      </section>
      <section className="panel">
        <SectionHeader icon={<AlertTriangle className="size-4" />} title="Evidence Status" />
        {data.evidence_metadata.length === 0 ? (
          <EmptyState text="No typed evidence metadata is available in this export." />
        ) : (
          <div className="space-y-3">
            {data.evidence_metadata.map((evidence) => {
              const tone = evidenceTone(evidence.status);
              return (
                <div className={clsx("row-card", tone === "danger" && "row-card-danger")} key={String(evidence.id)}>
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="badge">{String(evidence.kind ?? "evidence")}</span>
                    <span className={clsx("badge", `badge-${tone}`)}>{String(evidence.status ?? "unknown")}</span>
                    <span className="text-sm text-muted-foreground">{String(evidence.freshness ?? "")}</span>
                  </div>
                  {evidence.stale_reason ? <p className="mt-2 text-sm text-muted-foreground">{String(evidence.stale_reason)}</p> : null}
                </div>
              );
            })}
          </div>
        )}
      </section>
    </div>
  );
}

function SearchView({ data, query, setQuery }: { data: DashboardData; query: string; setQuery: (value: string) => void }) {
  const results = useMemo(() => {
    const term = query.trim().toLowerCase();
    const haystack = [
      ...data.records.map((record) => ({ type: "record", title: record.title, body: record.body, id: record.id })),
      ...data.commands.map((command) => ({ type: "command", title: command.command, body: command.output_preview ?? "", id: command.id })),
      ...data.events.map((event) => ({ type: "event", title: String(event.event_type), body: String(event.preview ?? ""), id: String(event.id) })),
      ...data.artifacts.map((artifact) => ({ type: "artifact", title: String(artifact.kind), body: String(artifact.preview ?? ""), id: String(artifact.id) }))
    ];
    if (!term) return haystack.slice(0, 12);
    return haystack.filter((item) => `${item.title} ${item.body}`.toLowerCase().includes(term)).slice(0, 20);
  }, [data, query]);

  return (
    <section className="panel">
      <SectionHeader icon={<Search className="size-4" />} title="Search / Explore" />
      <div className="search-box">
        <Search className="size-4 text-muted-foreground" />
        <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search records, commands, transcript previews, artifacts" />
      </div>
      <p className="mt-2 text-sm text-muted-foreground">CLI equivalent: <code>{data.status.search_command}</code></p>
      <div className="mt-4 space-y-3">
        {results.map((result) => (
          <div className="row-card" key={`${result.type}-${result.id}`}>
            <div className="mb-1 flex items-center gap-2">
              <span className="badge">{result.type}</span>
              <span className="truncate font-medium">{result.title}</span>
            </div>
            <p className="text-sm text-muted-foreground">{result.body || result.id}</p>
          </div>
        ))}
      </div>
    </section>
  );
}

function SettingsView({ data }: { data: DashboardData }) {
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <section className="panel">
        <SectionHeader icon={<Settings className="size-4" />} title="Settings / Status" />
        <div className="grid gap-2 text-sm">
          <KeyValue label="Export mode" value={data.status.export_mode} />
          <KeyValue label="Dashboard app" value={data.status.javascript_app} />
          <KeyValue label="Data contract" value={data.status.data_contract} />
          <KeyValue label="Schema version" value={data.schema_version} />
          <KeyValue label="Local only" value={data.status.local_only ? "yes" : "no"} />
        </div>
      </section>
      <section className="panel">
        <SectionHeader icon={<Database className="size-4" />} title="Redaction and Privacy" />
        <div className="grid gap-2 text-sm">
          <KeyValue label="Default output" value={data.privacy.default_redacted ? "redacted/share-safe" : "not redacted"} />
          <KeyValue label="Raw transcripts withheld" value={data.privacy.raw_transcripts_withheld} />
          <KeyValue label="Redacted previews" value={data.privacy.redacted_previews} />
          <KeyValue label="Withheld links" value={data.privacy.withheld_links} />
          <KeyValue label="Local paths redacted" value={data.privacy.local_paths_redacted ? "yes" : "no"} />
        </div>
      </section>
    </div>
  );
}

function CommandTable({ commands }: { commands: EvidenceCommand[] }) {
  const isMobile = useMediaQuery("(max-width: 640px)");
  const columns = useMemo<ColumnDef<EvidenceCommand>[]>(
    () => [
      { accessorKey: "command", header: "Command", cell: (info) => <code>{String(info.getValue())}</code> },
      {
        accessorKey: "exit_code",
        header: "Exit",
        cell: (info) => {
          const exitCode = Number(info.getValue());
          return <ExitBadge exitCode={exitCode} />;
        }
      },
      { accessorKey: "duration_ms", header: "Duration" },
      { accessorKey: "output_preview", header: "Preview" }
    ],
    []
  );
  const table = useReactTable({ data: commands, columns, getCoreRowModel: getCoreRowModel() });
  if (commands.length === 0) return <EmptyState text="No evidence has been captured yet." />;
  if (isMobile) {
    return (
      <div className="command-card-list">
        {commands.map((command) => (
          <article className={clsx("command-card", command.exit_code !== 0 && "command-card-danger")} key={command.id}>
            <div className="command-card-command">
              <span>Command</span>
              <code>{command.command}</code>
            </div>
            <div className="command-card-meta">
              <KeyValue label="Exit" value={<ExitBadge exitCode={command.exit_code} />} />
              <KeyValue label="Duration" value={`${command.duration_ms}ms`} />
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

function RecordRow({ record }: { record: DashboardRecord }) {
  return (
    <article className="row-card">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <h2 className="truncate text-base font-semibold">{record.title}</h2>
          <p className="mt-1 text-sm text-muted-foreground">{record.body}</p>
        </div>
        <span className="badge shrink-0">{record.kind}</span>
      </div>
      <div className="mt-3 flex flex-wrap gap-2">
        {record.workspace ? <span className="badge">{record.workspace}</span> : null}
        {record.tags.map((tag) => <span className="badge" key={tag}>#{tag}</span>)}
        {record.pr_url ? <a className="badge-link" href={record.pr_url} rel="noreferrer">PR</a> : null}
      </div>
    </article>
  );
}

function Metric({ label, value }: { label: string; value: number }) {
  return (
    <div className="metric">
      <div className="text-2xl font-semibold">{value}</div>
      <div className="mt-1 text-sm text-muted-foreground">{label}</div>
    </div>
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

function StatusPill({ tone, icon, children }: { tone: "ok" | "warn" | "danger"; icon: React.ReactNode; children: React.ReactNode }) {
  return <span className={clsx("status-pill", `status-${tone}`)}>{icon}{children}</span>;
}

function ExitBadge({ exitCode }: { exitCode: number }) {
  return <span className={clsx("badge", exitCode === 0 ? "badge-ok" : "badge-danger")}>Exit {exitCode}</span>;
}

function EmptyState({ text }: { text: string }) {
  return <div className="empty-state">{text}</div>;
}

function KeyValue({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="key-value">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function tagChartData(data: DashboardData) {
  if (data.summary.tags.length > 0) {
    return data.summary.tags.slice(0, 6).map((tag) => ({ name: tag.tag, count: tag.count }));
  }
  return [
    { name: "records", count: data.records.length },
    { name: "commands", count: data.commands.length },
    { name: "events", count: data.events.length }
  ];
}

function uniquePullRequestUrls(data: DashboardData) {
  return Array.from(
    new Set(
      [
        ...data.pull_requests.map((pr) => pr.url),
        ...data.records.map((record) => record.pr_url)
      ].filter((url): url is string => typeof url === "string" && url.trim().length > 0)
    )
  );
}

function evidenceTone(status: unknown): "ok" | "warn" | "danger" {
  const normalized = String(status ?? "").toLowerCase();
  if (normalized === "passed" || normalized === "succeeded" || normalized === "success") return "ok";
  if (normalized === "failed" || normalized === "error" || normalized === "failure") return "danger";
  return "warn";
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
