# ctx agent plugin

Fast local search and 'git blame' for agent sessions.

This plugin bundles the `ctx` Agent Skill and a `/ctx` command for clients that
support plugin commands. The skill teaches agents to search local agent
history with ctx and use ctx blame to trace
a line, file, commit, or PR back to the original agent session that produced
it.

## Prerequisite

Install and set up the ctx CLI:

```bash
curl -fsSL https://ctx.rs/install | sh
```

The plugin uses your installed ctx CLI for search and Blame.

When upgrading from the former `ctx-agent-history-search` plugin, uninstall that
package in the client before installing `ctx`. Plugin managers treat the new
name as a separate package; the ctx CLI's managed skill migration cannot remove
an old plugin-owned copy.

## Use

Ask the agent to search prior work, invoke the `ctx` skill directly, or use
`/ctx <request>` in clients that expose plugin commands. The skill uses default
text output for agent reading and inspects cited events or sessions before
drawing conclusions.

Learn more at [ctx.rs](https://ctx.rs).
