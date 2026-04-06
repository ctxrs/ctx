<p align="center">
  <img src="assets/readme/header-homepage-20260403.png" alt="ctx is an Agentic Development Environment (ADE)" />
</p>

**Any coding agent. Containerized workspaces. Unified transcripts and review.**

ctx is an Agentic Development Environment (ADE) for teams using multiple coding agents. It gives engineers one interface for the real Claude Code, Codex, Cursor, and more. It gives security and platform teams one controlled runtime with containerized disk and network isolation, one review surface, and durable transcripts.

Use ctx on your own machine or against a remote devbox or VPS you control. For normal local workflows, you do not need a ctx account, and you can bring your own providers, models, and credentials.

<p align="center">
  <img src="assets/videos/ctx-homepage-demo-20260406-900w-6s-10fps.gif" alt="A short demo of ctx showing the workbench, task thread, and agent workflow." />
</p>

## Install

```bash
curl -fsSL https://ctx.rs/install | sh
```

- Website: https://ctx.rs
- Blog: https://ctx.rs/blog
- Install guide: https://ctx.rs/getting-started/install-and-launch/

## What ctx helps you do

- Use Claude Code, Codex, Cursor, and other coding agents in one interface
- Run agents in isolated containers with explicit disk and network controls
- Let agents work with bounded autonomy instead of constant approval prompts
- Keep tasks, sessions, diffs, transcripts, and artifacts in one review surface
- Run work locally or on remote machines you control
- Keep parallel tasks isolated in separate worktrees and land them cleanly with the agent merge queue

## Why standardize on the environment

- Engineers can use the agents they prefer without fragmenting the workflow
- Security and platform teams can rely on one runtime model and one set of safety controls
- Review, provenance, and task history stay in one place instead of being scattered across tools
- You can change harnesses and models over time without rebuilding team workflows around each one

## Get started

Use one small, low-risk task to validate the whole loop end to end: install, connect a provider, open a workspace, run a task, and review the diff before you finalize changes.

- [Install and launch](docs/getting-started/install-and-launch.mdx)
- [Connect a provider](docs/getting-started/connect-provider.mdx)
- [Add a workspace](docs/getting-started/add-workspace.mdx)
- [Run your first task](docs/getting-started/first-task.mdx)

## Learn more

- [Workbench tour](docs/workbench/tour.mdx)
- [Containerization](docs/containerization.mdx)
- [Agent Merge Queue Overview](docs/agent-merge-queue-overview.mdx)
- [What is a worktree?](docs/what-is-a-worktree.mdx)
- [ADE vs CLI](docs/ade-vs-cli.mdx)
- [ADE vs IDE](docs/ade-vs-ide.mdx)
