<p align="center">
  <img src="assets/readme/header-homepage-20260403.png" alt="ctx records Work from coding agents" />
</p>

**A local Work record system for coding agents, with an optional desktop workbench.**

ctx records the Work around coding-agent changes: prompts, transcripts, commands,
diffs, pull requests, artifacts, checks, and review evidence. The goal is to make
agent work inspectable and reusable after the terminal tab, provider session, or
one-off worktree is gone.

The ctx desktop app is an Agentic Development Environment (ADE) over the same
local records. Use it when you want a rich Workbench for running, supervising,
reviewing, and landing agent work. Use the CLI when you want to inspect or move
local Work records without adopting the desktop app.

ctx is local-first. Your repos, workspace registry, transcripts, artifacts, and
Work records live on the machine running ctx. By default ctx stores private state
under `CTX_DATA_ROOT` or `~/.ctx`; repo-local `.ctx` files are explicit opt-in or
legacy `ctx init` behavior, not the default Work setup path.

<p align="center">
  <img src="assets/videos/ctx-homepage-demo-20260406-900w-6s-10fps.gif" alt="A short demo of ctx showing the workbench, task thread, and agent workflow." />
</p>

## Install

```bash
curl -fsSL https://ctx.rs/install | sh
```

Platform support: macOS and Linux today. Windows is on the roadmap.

- Website: https://ctx.rs
- Blog: https://ctx.rs/blog
- Install guide: https://ctx.rs/getting-started/install-and-launch/

## Quick Start

Use the desktop Workbench for the full task loop:

1. Install ctx.
2. Launch the app.
3. Connect a provider or harness you already use, such as Claude Code or Codex.
4. Add a workspace.
5. Run a small task and review the resulting diff, transcript, and artifacts.

Use the Work CLI for local records:

```bash
ctx setup workspace .
ctx work schema
ctx work list
ctx work link-pr https://github.com/owner/repo/pull/123
ctx work export --output work.json
ctx work validate work.json
```

`ctx setup` registers user-local workspaces and can install owned `git`/`gh`
shims under the ctx data root. `ctx work` covers local schema, validation, list,
show, import, export, inspect, redaction-preview, best-effort command capture,
notes, recent context, and explicit PR linking. Shim capture is context, not
tamper-proof audit; use `ctx work link-pr` when a GitHub PR URL is known.

## What ctx Helps You Do

- Keep coding-agent Work records local, durable, and inspectable.
- Link prompts, transcripts, tool activity, commands, diffs, PRs, artifacts, and checks.
- Review agent changes with the context that produced them, not just a bare diff.
- Let future agents and humans reuse prior task context instead of rediscovering it.
- Use installed agent harnesses on `PATH` where possible, with managed provider setup as an optional advanced path.
- Open the same records in a desktop Workbench when a GUI review and run surface is useful.
- Run ADE-managed sessions in host or containerized modes; container disk/network controls apply to those ADE/containerized runs, not to every CLI command.

## Capability Status

| Area | Status |
| --- | --- |
| Local desktop Workbench for tasks, sessions, transcripts, artifacts, diffs, worktrees, and review | Works now |
| Local data root under `CTX_DATA_ROOT` or `~/.ctx` | Works now |
| `ctx work` schema/list/show/import/export/validate/inspect/redaction-preview | Works now |
| `ctx setup` workspace/scratch/status/uninstall | Works now |
| Best-effort `git`/`gh` shim capture and explicit `ctx work link-pr` | Works now locally |
| Local plugin manifest validation/list/reload scan | Works now |
| Declarative host-owned Workbench plugin contributions | Experimental |
| ACP provider plugin direction | Experimental |
| Hosted/team sync, billing, organization policy, or tamper-proof hosted audit | Not part of this public local slice |
| Arbitrary executable plugin UI or webview runtime | Deferred |

## Why Work Records

A coding-agent result is more than a patch. The useful context includes the
objective, conversation, commands, files touched, artifacts produced, validation
attempts, and source-control links. ctx treats that as Work: a local record graph
that can be inspected, exported, redacted, and opened in the Workbench.

This matters for terminal-first workflows too. If you prefer running agents from
the shell, ctx should still be useful as the local record system around that
work. The ADE is the richer UI over the records, not the only way records should
exist.

## Plugin And Provider Direction

ctx already has a narrow local plugin substrate: manifest validation, local
list/reload scans, command and provider-adapter registration, and declarative
host-owned Workbench contributions. Those contributions describe supported
host-rendered templates, sections, cards, renderers, and actions; they do not
execute arbitrary plugin UI code.

Provider integration should converge on ACP. A ctx plugin can package or
register an ACP-compatible provider, while provider-owned slash commands remain
owned by the provider/protocol surface rather than becoming a competing ctx
command namespace.

## Under The Hood

ctx is built around a local Rust daemon, real agent harnesses, local SQLite
state, and per-task worktrees.

- The daemon owns workspace state, sessions, transcripts, artifacts, diffs,
  provider setup, plugin inventory, Work records, and merge queue state.
- ctx runs real agent harnesses instead of replacing them with one internal
  agent loop; adapters use structured runtime protocols where available.
- Work records model change sets and contributions so task, session, artifact,
  check, and PR relationships can be many-to-many.
- Each ADE task can run in its own worktree so review state is tied to the
  execution root that produced it.
- ADE sandbox runs are materialized into an isolated container data plane
  instead of mutating a host checkout in place.
- Containerized ADE runs can use explicit network egress policies such as
  LLM-provider-only, allowlist, or full access.
- The desktop app is Tauri with a TypeScript UI, while the runtime path is Rust.
  The repo uses Bazel for the larger validation graph alongside normal Cargo and
  pnpm workflows.

## Get Started

- [Install and launch](docs/getting-started/install-and-launch.mdx)
- [Add a workspace](docs/getting-started/add-workspace.mdx)
- [Run your first task](docs/getting-started/first-task.mdx)
- [Work records](docs/work-records.mdx)
- [Connect a provider](docs/getting-started/connect-provider.mdx)

## Learn More

- [Workbench tour](docs/workbench/tour.mdx)
- [Work Model](docs/agent-work-model.mdx)
- [Plugin Contribution Contract](docs/plugin-contribution-contract.mdx)
- [Containerization](docs/containerization.mdx)
- [ADE vs CLI](docs/ade-vs-cli.mdx)
- [ADE vs IDE](docs/ade-vs-ide.mdx)

## Build From Source

Prerequisites:

- Rust stable
- Node.js 20+ with Corepack
- Platform desktop dependencies if building the Tauri app, including GTK/WebKitGTK on Linux

Install dependencies and build the Rust workspace:

```bash
git clone https://github.com/ctxrs/ctx.git
cd ctx
cd core
corepack enable
pnpm install --frozen-lockfile
cargo build --workspace
```

Run the daemon and web workbench locally in separate terminals:

```bash
cd core
cargo run -p ctx-http --bin ctx -- serve --bind 127.0.0.1:4399 --data-dir "${CTX_DATA_ROOT:-$HOME/.ctx}"
```

```bash
cd core
pnpm -C apps/web dev
```

Launch the desktop app from source:

```bash
cd core
pnpm desktop:dev
```

Build the desktop app from source:

```bash
cd core
pnpm desktop:build
```
