import React from "react";
import ReactDOM from "react-dom/client";
import * as Tabs from "@radix-ui/react-tabs";
import {
  AlertTriangle,
  ArrowRight,
  BadgeInfo,
  BookOpen,
  Boxes,
  CheckCircle2,
  Download,
  ExternalLink,
  FileText,
  FolderKanban,
  GitPullRequest,
  Lock,
  Moon,
  Search,
  ShieldCheck,
  Sun,
  Terminal,
  Upload,
  Workflow
} from "lucide-react";
import { clsx } from "clsx";
import {
  boundaryCards,
  docsMap,
  heroHighlights,
  installChecklist,
  installCommands,
  privacyChecklist,
  privacyNotes,
  providerMatrix,
  providerNotes,
  providerTaxonomy,
  setupFlow,
  troubleshootingSteps,
  type ChecklistEntry,
  type FlowStep,
  type Highlight,
  type PreviewTab
} from "./site-preview-data";
import "./styles.css";

function App() {
  const [theme, setTheme] = React.useState<"light" | "dark">("light");
  const [activeTab, setActiveTab] = React.useState<PreviewTab>("overview");

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
    <div className="docs-page min-h-screen text-foreground">
      <header className="border-b border-border/80 bg-card/90 backdrop-blur">
        <div className="mx-auto flex max-w-7xl flex-col gap-4 px-4 py-4 sm:px-6 lg:flex-row lg:items-center lg:justify-between">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2 text-sm font-medium text-muted-foreground">
              <FolderKanban className="size-4" aria-hidden />
              <span>Local ctx preview</span>
              <span className="rounded-sm border border-border px-1.5 py-0.5 text-xs">0.1.0 candidate</span>
            </div>
            <h1 className="mt-1 text-2xl font-semibold tracking-normal sm:text-3xl">ctx records agent work</h1>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <StatusPill tone="ok" icon={<ShieldCheck className="size-3.5" />}>
              Local-first by default
            </StatusPill>
            <StatusPill tone="warn" icon={<AlertTriangle className="size-3.5" />}>
              Public preview only
            </StatusPill>
            <a className="badge-link" href="./index.html" title="Open the dashboard preview">
              <ExternalLink className="size-3.5" />
              Dashboard preview
            </a>
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
        <section className="docs-hero">
          <div className="grid gap-6 xl:grid-cols-[minmax(0,2.2fr)_minmax(320px,1fr)] xl:items-start">
            <div className="min-w-0">
              <div className="docs-kicker">ctx Release Candidate</div>
              <h2 className="docs-headline">
                Record what agents do so the work can be attached to PRs, searched later, and shared with teammates.
              </h2>
              <p className="docs-lead">
                This preview covers the public ctx CLI only. It documents the current local-first product
                honestly: work records, evidence, import, search, report, dashboard, and PR comment publishing
                through the local <code>gh</code> CLI. It is not the ctx ADE, does not publish or repoint{" "}
                <code>ctx.rs</code>, and does not claim hosted sync or passive provider hooks beyond the proven
                surfaces below.
              </p>
              <div className="mt-5 flex flex-wrap gap-2">
                <StatusPill tone="ok" icon={<CheckCircle2 className="size-3.5" />}>
                  Search, context, report
                </StatusPill>
                <StatusPill tone="ok" icon={<GitPullRequest className="size-3.5" />}>
                  PR evidence and publish dry-run
                </StatusPill>
                <StatusPill tone="warn" icon={<Upload className="size-3.5" />}>
                  Hosted sync remains future work
                </StatusPill>
              </div>
            </div>
            <section className="panel">
              <SectionHeader icon={<BookOpen className="size-4" />} title="Preview reviewer cues" />
              <div className="space-y-3 text-sm text-muted-foreground">
                <p>
                  The public language here is deliberately narrower than the full provider program. Only proven local
                  recorder surfaces are promoted.
                </p>
                <div className="grid gap-2">
                  <KeyValue label="Shipping today" value="Source build, local store, local shims, local review output" />
                  <KeyValue label="Public claim guardrail" value="No provider marked supported without real proof" />
                  <KeyValue label="Release posture" value="0.1.0 candidate wording; installer URLs still placeholders" />
                </div>
              </div>
            </section>
          </div>

          <div className="docs-highlight-grid mt-6">
            {heroHighlights.map((item) => (
              <HighlightCard key={item.title} item={item} />
            ))}
          </div>
        </section>

        <Tabs.Root
          value={activeTab}
          onValueChange={(value) => setActiveTab(value as PreviewTab)}
          className="mt-6 space-y-4"
        >
          <Tabs.List className="tab-list" aria-label="ctx docs preview views">
            <Tab value="overview" icon={<BadgeInfo className="size-4" />} label="Overview" />
            <Tab value="flow" icon={<Workflow className="size-4" />} label="Flow" />
            <Tab value="providers" icon={<Boxes className="size-4" />} label="Providers" />
            <Tab value="privacy" icon={<Lock className="size-4" />} label="Privacy" />
            <Tab value="install" icon={<Download className="size-4" />} label="Install" />
            <Tab value="boundaries" icon={<AlertTriangle className="size-4" />} label="Boundaries" />
          </Tabs.List>

          <Tabs.Content value="overview">
            <div className="grid gap-4 lg:grid-cols-[minmax(0,1.7fr)_minmax(320px,1fr)]">
              <section className="panel">
                <SectionHeader icon={<FileText className="size-4" />} title="What ctx records" />
                <div className="docs-copy space-y-4">
                  <p>
                    A work record is the durable history for one unit of agent-assisted work. In this candidate, ctx can
                    store the record body, tags, timestamps, optional workspace, linked pull request URL, command
                    evidence, share-safe previews, provider import summaries, and repository context that helps a
                    reviewer understand what changed.
                  </p>
                  <ul className="docs-bullet-list">
                    <li>Prompt or note content captured with <code>ctx record</code>.</li>
                    <li>Command evidence captured with <code>ctx evidence run</code> or imported from local Git/jj/gh shims.</li>
                    <li>Pull request links validated with <code>ctx pr parse</code> and attached with <code>ctx link-pr</code>.</li>
                    <li>Share-safe search, context, report, and dashboard export views for review and handoff.</li>
                    <li>Provider summaries only where the import or capture path is actually proven.</li>
                  </ul>
                </div>
              </section>

              <section className="panel">
                <SectionHeader icon={<AlertTriangle className="size-4" />} title="Non-ADE scope" />
                <div className="docs-copy space-y-4">
                  <p>
                    This preview is intentionally narrower than the rest of ctx. It does not document the ADE workbench,
                    the container runtime, provider connection UI, or a production hosted team surface.
                  </p>
                  <div className="docs-callout">
                    The public ctx story is a local CLI and review packet. Anything broader should stay clearly
                    labeled as future work until the separate program signs off.
                  </div>
                </div>
              </section>

              <section className="panel lg:col-span-2">
                <SectionHeader icon={<BookOpen className="size-4" />} title="Repo docs map" />
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
                  {docsMap.map((doc) => (
                    <article className="row-card docs-doc-card" key={doc.path}>
                      <div className="docs-doc-path">{doc.path}</div>
                      <h3 className="mt-3 text-base font-semibold">{doc.title}</h3>
                      <p className="mt-2 text-sm text-muted-foreground">{doc.body}</p>
                    </article>
                  ))}
                </div>
              </section>
            </div>
          </Tabs.Content>

          <Tabs.Content value="flow">
            <div className="grid gap-4 xl:grid-cols-[minmax(0,1.5fr)_minmax(360px,1fr)]">
              <section className="panel">
                <SectionHeader
                  icon={<Workflow className="size-4" />}
                  title="Setup, import, capture, search, report, publish"
                />
                <div className="space-y-4">
                  {setupFlow.map((step) => (
                    <FlowCard key={step.step} step={step} />
                  ))}
                </div>
              </section>

              <section className="panel">
                <SectionHeader icon={<Terminal className="size-4" />} title="Passive capture model" />
                <div className="docs-copy space-y-4">
                  <p>
                    The intended user experience stays passive after setup. ctx installs reversible Git/jj/gh wrappers
                    under the local data root, then imports the capture spool into the store when you run ctx commands or{" "}
                    <code>ctx capture import</code>.
                  </p>
                  <ul className="docs-bullet-list">
                    <li>No repository hooks are installed.</li>
                    <li>No daemon is required for the local CLI loop.</li>
                    <li>Provider-native passive capture is not claimed unless the provider workstream proves it.</li>
                    <li>Basic review output should not depend on remembering special publish-time commands.</li>
                  </ul>
                </div>
              </section>
            </div>
          </Tabs.Content>

          <Tabs.Content value="providers">
            <div className="grid gap-4">
              <section className="panel">
                <SectionHeader icon={<Boxes className="size-4" />} title="Provider support taxonomy" />
                <div className="table-scroll">
                  <table className="data-table">
                    <thead>
                      <tr>
                        <th>Status</th>
                        <th>Public meaning</th>
                      </tr>
                    </thead>
                    <tbody>
                      {providerTaxonomy.map((entry) => (
                        <tr key={entry.status}>
                          <td><code>{entry.status}</code></td>
                          <td>{entry.meaning}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </section>

              <section className="panel">
                <SectionHeader icon={<ShieldCheck className="size-4" />} title="Current 0.1.0 candidate matrix" />
                <div className="table-scroll">
                  <table className="data-table">
                    <thead>
                      <tr>
                        <th>Provider</th>
                        <th>Status</th>
                        <th>Current path</th>
                        <th>Fidelity</th>
                        <th>Notes</th>
                      </tr>
                    </thead>
                    <tbody>
                      {providerMatrix.map((entry) => (
                        <tr key={entry.provider}>
                          <td>{entry.provider}</td>
                          <td><StatusBadge status={entry.status} /></td>
                          <td>{entry.path}</td>
                          <td>{entry.fidelity}</td>
                          <td>{entry.notes}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </section>

              <section className="grid gap-4 lg:grid-cols-2">
                {providerNotes.map((note) => (
                  <div className="docs-callout" key={note}>
                    {note}
                  </div>
                ))}
              </section>
            </div>
          </Tabs.Content>

          <Tabs.Content value="privacy">
            <div className="grid gap-4 lg:grid-cols-[minmax(0,1.2fr)_minmax(320px,1fr)]">
              <section className="panel">
                <SectionHeader icon={<Lock className="size-4" />} title="Privacy defaults and storage model" />
                <div className="grid gap-2">
                  {privacyChecklist.map((entry) => (
                    <KeyValueRow key={entry.label} entry={entry} />
                  ))}
                </div>
                <div className="mt-4 space-y-3 text-sm text-muted-foreground">
                  {privacyNotes.map((note) => (
                    <p key={note}>{note}</p>
                  ))}
                </div>
              </section>

              <section className="panel">
                <SectionHeader icon={<Search className="size-4" />} title="Reviewer-safe access" />
                <div className="space-y-3">
                  <pre className="preview">ctx search checkout --json
ctx context checkout
ctx report
ctx publish pr-comment &lt;record-id&gt; --dry-run</pre>
                  <p className="text-sm text-muted-foreground">
                    These outputs are the public review surface. They should stay useful even when raw archives and
                    provider history remain private local data.
                  </p>
                </div>
              </section>
            </div>
          </Tabs.Content>

          <Tabs.Content value="install">
            <div className="grid gap-4 lg:grid-cols-[minmax(0,1.1fr)_minmax(0,1fr)]">
              <section className="panel">
                <SectionHeader icon={<Download className="size-4" />} title="Release and install posture" />
                <div className="grid gap-2">
                  {installChecklist.map((entry) => (
                    <KeyValueRow key={entry.label} entry={entry} />
                  ))}
                </div>
                <div className="mt-4 grid gap-4 lg:grid-cols-2">
                  <article className="row-card">
                    <div className="docs-mini-label">Source build now</div>
                    <pre className="preview">{installCommands.source}</pre>
                  </article>
                  <article className="row-card">
                    <div className="docs-mini-label">Future public install shape</div>
                    <pre className="preview">{installCommands.candidate}</pre>
                  </article>
                </div>
              </section>

              <section className="panel">
                <SectionHeader icon={<ShieldCheck className="size-4" />} title="Install security rules" />
                <ul className="docs-bullet-list text-sm text-muted-foreground">
                  <li>Installer scripts are downloaded as local files, not piped directly into a shell.</li>
                  <li>Metadata is the contract: artifact names, URLs, platforms, and SHA-256 digests are validated before install.</li>
                  <li>Non-HTTPS artifact URLs, placeholder checksums, and path traversal attempts are rejected.</li>
                  <li>The public docs must keep <code>ctx.rs/install</code> as a future path until a real release exists.</li>
                </ul>
              </section>
            </div>
          </Tabs.Content>

          <Tabs.Content value="boundaries">
            <div className="grid gap-4">
              <section className="grid gap-4 md:grid-cols-2">
                {boundaryCards.map((item) => (
                  <HighlightCard key={item.title} item={item} />
                ))}
              </section>

              <section className="panel">
                <SectionHeader icon={<AlertTriangle className="size-4" />} title="Troubleshooting and wording discipline" />
                <div className="space-y-4">
                  {troubleshootingSteps.map((step) => (
                    <FlowCard key={step.step} step={step} compact />
                  ))}
                </div>
              </section>
            </div>
          </Tabs.Content>
        </Tabs.Root>
      </main>
    </div>
  );
}

function Tab({ value, icon, label }: { value: PreviewTab; icon: React.ReactNode; label: string }) {
  return (
    <Tabs.Trigger className="tab-trigger" value={value} data-dashboard-tab={value}>
      {icon}
      <span>{label}</span>
    </Tabs.Trigger>
  );
}

function HighlightCard({ item }: { item: Highlight }) {
  return (
    <article className="docs-highlight-card">
      <div className="flex items-start justify-between gap-3">
        <div>
          <div className="docs-mini-label">{item.title}</div>
          <p className="mt-2 text-sm text-muted-foreground">{item.body}</p>
        </div>
        <ArrowRight className="mt-1 size-4 text-primary" aria-hidden />
      </div>
    </article>
  );
}

function FlowCard({ step, compact = false }: { step: FlowStep; compact?: boolean }) {
  return (
    <article className={clsx("row-card docs-flow-card", compact && "docs-flow-card-compact")}>
      <div className="flex flex-wrap items-center gap-3">
        <span className="docs-step-badge">{step.step}</span>
        <h3 className="text-base font-semibold">{step.title}</h3>
      </div>
      <p className="mt-3 text-sm text-muted-foreground">{step.body}</p>
      {step.command ? <pre className="preview">{step.command}</pre> : null}
    </article>
  );
}

function KeyValueRow({ entry }: { entry: ChecklistEntry }) {
  return <KeyValue label={entry.label} value={entry.value} multiline />;
}

function SectionHeader({ icon, title }: { icon: React.ReactNode; title: string }) {
  return (
    <div className="mb-4 flex items-center gap-3">
      <div className="section-icon">{icon}</div>
      <h2 className="text-lg font-semibold">{title}</h2>
    </div>
  );
}

function KeyValue({
  label,
  value,
  multiline = false
}: {
  label: string;
  value: React.ReactNode;
  multiline?: boolean;
}) {
  return (
    <div className={clsx("key-value", multiline && "docs-key-value-multiline")}>
      <span>{label}</span>
      <strong className={clsx("text-right", multiline && "docs-key-value-value")}>{value}</strong>
    </div>
  );
}

function StatusBadge({ status }: { status: string }) {
  const tone =
    status === "supported-import" || status === "supported-live" || status === "supported-wrapper"
      ? "ok"
      : status === "fixture-only" || status === "detected-unsupported"
        ? "warn"
        : "danger";
  return <span className={clsx("badge", `badge-${tone}`)}>{status}</span>;
}

function StatusPill({
  tone,
  icon,
  children
}: {
  tone: "ok" | "warn" | "danger";
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <span className={clsx("status-pill", `status-${tone}`)}>
      {icon}
      {children}
    </span>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
