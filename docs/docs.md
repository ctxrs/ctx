# Docs

`ctx docs` exposes curated public ctx documentation embedded in the installed
binary. It is for humans and agents that need local command help without
opening a website or reading repository files.

```bash
ctx docs
ctx docs list
ctx docs list --json
ctx docs search "file path"
ctx docs search "upgrade" --limit 5 --json
ctx docs show cli-reference
ctx docs show search --format text
ctx docs show json-contracts --format json
ctx docs man --print ctx
ctx docs man --out ~/.local/share/man/man1
```

`ctx docs list`, `ctx docs search`, and `ctx docs show` read embedded text and
do not touch provider history or the local SQLite index. `ctx docs show --out
PATH` writes one embedded topic to that explicit path.

`ctx docs man --print PAGE` prints one generated man page to stdout. `ctx docs
man --out DIR` writes generated section-1 man pages for `ctx` and its public
subcommands.

Agents should usually use `ctx docs search` or `ctx docs show` rather than
shelling through `man`, because the docs commands return concise markdown,
text, or JSON that is easier for agents to inspect and cite.

Useful starting points:

- `ctx docs show search` for search filters and output behavior;
- `ctx docs show sql` for stable read-only SQL views;
- `ctx docs show provider-import-policy` for native history import policy;
- `ctx docs show mcp` for read-only MCP tools;
- `ctx docs show upgrade` for managed upgrade and auto-upgrade behavior;
- `ctx docs show unmanaged-installs` for GitHub release binaries, mise,
  Homebrew, and source builds;
- `ctx docs show json-contracts` for structured output contracts.
