export type PreviewTab = "overview" | "flow" | "providers" | "privacy" | "install" | "boundaries";

export type Highlight = {
  title: string;
  body: string;
};

export type DocEntry = {
  path: string;
  title: string;
  body: string;
};

export type FlowStep = {
  step: string;
  title: string;
  body: string;
  command?: string;
};

export type TaxonomyEntry = {
  status: string;
  meaning: string;
};

export type ProviderEntry = {
  provider: string;
  status: string;
  path: string;
  fidelity: string;
  notes: string;
};

export type ChecklistEntry = {
  label: string;
  value: string;
};

export const heroHighlights: Highlight[] = [
  {
    title: "Local-first",
    body: "Records, evidence, search, report, dashboard export, and PR comments work from a local CLI without a hosted account."
  },
  {
    title: "0.1.0 candidate posture",
    body: "Build from source today. Release installer URLs remain placeholders until signed public artifacts are actually published."
  },
  {
    title: "Honest provider language",
    body: "Codex and Pi have supported-import paths. Claude Code remains fixture-only in this preview until native history or hook proof lands."
  },
  {
    title: "Not the ADE",
    body: "This preview is for ctx local work records only. It does not claim the container runtime, workbench, or ADE cutover."
  }
];

export const docsMap: DocEntry[] = [
  {
    path: "README.md",
    title: "Product overview",
    body: "Positioning, quick start, scope boundaries, and the repo-level docs map."
  },
  {
    path: "docs/getting-started.md",
    title: "Install and first-run flow",
    body: "Source build, setup, local capture, provider import, search, report, dashboard export, and publish dry-run."
  },
  {
    path: "docs/provider-support.md",
    title: "Provider taxonomy",
    body: "Public support statuses, current candidate matrix, local discovery posture, and wording rules."
  },
  {
    path: "docs/privacy-storage.md",
    title: "Privacy defaults",
    body: "SQLite and object storage, share-safe review surfaces, raw transcript opt-ins, and local-only data handling."
  },
  {
    path: "docs/release-install.md",
    title: "Release install contract",
    body: "0.1.0 candidate installer shape, metadata verification, HTTPS-only artifact rules, and no pipe-to-shell guidance."
  },
  {
    path: "docs/hosted-sync-roadmap.md",
    title: "Hosted roadmap",
    body: "Future hosted sync direction with redacted-by-default posture and raw transcript sync as an explicit opt-in."
  },
  {
    path: "docs/troubleshooting.md",
    title: "Troubleshooting",
    body: "Status, doctor, repair, validate, shim activation, and provider import triage."
  }
];

export const setupFlow: FlowStep[] = [
  {
    step: "01",
    title: "Install from source for the candidate",
    body: "The public release contract is not live yet, so the truthful first-run path is a source build from this checkout.",
    command: "cargo build -p ctx\ncargo install --path crates/ctx-cli"
  },
  {
    step: "02",
    title: "Create the local ctx store",
    body: "Run setup once to create the flat ~/.ctx layout, install ctx-owned Git/jj/gh shims, import supported history, and open the dashboard when the session is interactive.",
    command: "ctx setup\nctx status\nctx setup --no-open\nctx setup --no-import\nctx setup --no-shell-update\nctx setup --service"
  },
  {
    step: "03",
    title: "Bring in work the recorder can prove",
    body: "Import pending shim spool files, Codex prompt history, explicit Pi sessions, or normalized provider fixtures. Do not claim native capture where the provider format or hook is still unproven.",
    command: "ctx capture import\nctx capture import-local-providers --json\nctx capture import-codex-history --input ~/.codex/history.jsonl --json\nctx capture import-pi-session --input ~/.pi/agent/sessions/<session>.jsonl --json\nctx capture import-provider --provider codex --input tests/fixtures/provider/codex.jsonl --json"
  },
  {
    step: "04",
    title: "Record, search, and report",
    body: "The durable loop is record plus evidence, then share-safe search, context, report, and dashboard review output.",
    command: "ctx record --title \"trace checkout retries\" --body \"Investigate flaky retry handling.\" --kind task --tag checkout --json\nctx evidence run --record <record-id> cargo test -p checkout\nctx search checkout\nctx context checkout\nctx report\nctx dashboard\nctx dashboard export --output ./ctx-dashboard"
  },
  {
    step: "05",
    title: "Publish to a pull request deliberately",
    body: "Link a PR, render the comment locally first, then publish through the authenticated local gh CLI only when the record is ready to share.",
    command: "ctx link-pr <record-id> https://github.com/example/project/pull/42\nctx publish pr-comment <record-id> --dry-run\nctx publish pr-comment <record-id>"
  }
];

export const providerTaxonomy: TaxonomyEntry[] = [
  {
    status: "supported-live",
    meaning: "Native or wrapper capture, real live proof, review surfaces, and gated evidence are all green."
  },
  {
    status: "supported-import",
    meaning: "Stable existing-history import is proven, but passive live capture is unavailable or intentionally not implemented yet."
  },
  {
    status: "supported-wrapper",
    meaning: "ctx can capture the surface through a wrapper or shim even when native provider hooks are unavailable."
  },
  {
    status: "fixture-only",
    meaning: "Normalized fixture import works, but no real provider data is proven in the public candidate yet."
  },
  {
    status: "detected-unsupported",
    meaning: "ctx can detect a local install or directory, but there is no safe import or capture path to claim publicly."
  },
  {
    status: "blocked",
    meaning: "A concrete blocker exists and needs provider-specific proof before the public docs can upgrade the claim."
  }
];

export const providerMatrix: ProviderEntry[] = [
  {
    provider: "Codex",
    status: "supported-import",
    path: "Explicit Codex history import or import-local-providers",
    fidelity: "summary_only prompt history",
    notes: "Imports local history.jsonl grouped by session_id. Assistant replies, tool calls, command output, artifacts, and child sessions are not claimed."
  },
  {
    provider: "Claude Code",
    status: "fixture-only",
    path: "Normalized provider fixture JSONL",
    fidelity: "imported fixture data",
    notes: "Real native history discovery, parser proof, and passive capture hooks are not documented as shipped in this candidate."
  },
  {
    provider: "Pi",
    status: "supported-import",
    path: "Explicit Pi session JSONL import or import-local-providers",
    fidelity: "imported session data",
    notes: "Imports bounded session JSONL. Parent ids remain metadata, raw images are not artifacts, and live passive capture is not implemented."
  }
];

export const providerNotes: string[] = [
  "Git, jj, and gh wrapper shims are the first shipped passive capture surface. They are recorder plumbing, not provider-native transcript capture.",
  "Broader provider work for Cursor, OpenCode, Gemini CLI, Antigravity CLI, Copilot CLI, and long-tail surfaces is being validated in parallel and is intentionally not hard-claimed in this preview."
];

export const privacyChecklist: ChecklistEntry[] = [
  {
    label: "Storage",
    value: "Flat ~/.ctx data root with work.sqlite, objects/, spool/, shims/, config.toml, and logs/."
  },
  {
    label: "Default review posture",
    value: "list, show, search, context, report, dashboard export, and PR comment rendering redact secret-like values and local paths."
  },
  {
    label: "Raw transcript sharing",
    value: "Withheld by default. PR comment publishing can include raw transcript content only through an explicit opt-in flag."
  },
  {
    label: "Hosted scope",
    value: "Hosted sync, hosted dashboards, team policy, and org retention controls are future work, not part of the current public candidate."
  }
];

export const privacyNotes: string[] = [
  "Treat the capture spool, exported JSON archives, and local data root as sensitive private data.",
  "ctx stores what you explicitly record around your tools. It does not make provider network traffic or package-manager traffic private by itself.",
  "The future hosted roadmap keeps redacted summaries as the default sync shape and reserves raw transcript sync for an explicit opt-in."
];

export const installChecklist: ChecklistEntry[] = [
  {
    label: "Today",
    value: "Source build or cargo install from this checkout."
  },
  {
    label: "Candidate installer contract",
    value: "Local-launch install script plus release metadata file, with SHA-256 verification before the ctx binary is copied into place."
  },
  {
    label: "Not allowed",
    value: "No curl-pipe-shell instructions and no live ctx.rs/install claim until the release URL is real."
  },
  {
    label: "Placeholder version",
    value: "Use v0.1.0 as candidate release wording only. The commands stay examples until real release assets exist."
  }
];

export const installCommands = {
  source: "cargo build -p ctx\ncargo install --path crates/ctx-cli\nctx status",
  candidate:
    "curl -fsSLO https://github.com/ctxrs/ctx/releases/download/v0.1.0/install.sh\ncurl -fsSLO https://github.com/ctxrs/ctx/releases/download/v0.1.0/ctx-release-metadata.env\nbash install.sh --metadata ./ctx-release-metadata.env"
};

export const boundaryCards: Highlight[] = [
  {
    title: "What this preview is",
    body: "A public local-first ctx preview: work records, evidence, search, context, reports, dashboard, provider import, and local GitHub PR comment publishing."
  },
  {
    title: "What it is not",
    body: "Not the ctx ADE, not the hosted team product, not a docs cutover, and not a promise that every historical provider surface is already supported."
  },
  {
    title: "Hosted roadmap",
    body: "Future hosted sync should keep local-first defaults, sync redacted summaries first, and treat raw transcripts as an opt-in path."
  },
  {
    title: "Public wording rule",
    body: "Do not call a provider supported until it has at least supported-import or supported-wrapper proof with docs and review output."
  }
];

export const troubleshootingSteps: FlowStep[] = [
  {
    step: "A",
    title: "Check setup and shim activation",
    body: "Use status first. If the shim directory is inactive on PATH, wrapper capture cannot land in the spool.",
    command: "ctx status\nctx shim env --dir ~/.ctx/shims"
  },
  {
    step: "B",
    title: "Inspect capture health",
    body: "Doctor surfaces stuck or failed spool files. Repair retries failed files after you inspect the cause.",
    command: "ctx doctor\nctx repair --json\nctx validate"
  },
  {
    step: "C",
    title: "Triage provider import claims",
    body: "If a provider path is only fixture-only or detected-unsupported, the right fix is not to upgrade the docs claim. Keep the blocker honest and gather proof in the provider workstream.",
    command: "ctx capture import-local-providers --json\nctx capture import-codex-history --input ~/.codex/history.jsonl --json"
  }
];
