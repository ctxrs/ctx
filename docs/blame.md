# Blame

`ctx blame` connects committed code to the coding-agent sessions that produced
it. It is included in every ctx build, including source and package-manager
installs. Results cite the original session records and tool calls so you can
inspect the evidence and recover why a change was made.

## Start from a file, commit, or pull request

```bash
ctx blame src/checkout.ts --lines 118:146
ctx blame file src/checkout.ts --lines 118:146
ctx blame commit <commit-id>
ctx blame pr https://github.com/your-org/your-repo/pull/42
```

Both the short form and the explicit `file`, `commit`, and `pr` forms are
supported. Use `--type file`, `--type commit`, or `--type pr` when a short-form
target could have more than one meaning. Use `--` before a target that begins
with a dash.

| Option | Meaning |
| --- | --- |
| `--lines START[:END]` | Inclusive committed-file line or range; only valid for file targets. |
| `--repository REPOSITORY` | Select a logical repository identity, such as `forge:github.com/org/repo`; required with a PR number. |
| `--limit COUNT` | Bound the number of complete matches returned; the CLI default is 20. |
| `--cursor CURSOR` | Continue the same query from a returned cursor. |
| `--format text` or `--format json` | Choose human-readable or structured output. |

Commit selectors accept a full or unambiguous abbreviated Git object ID. Pull
request selectors accept a positive number with a repository or a supported
GitHub, GitLab, or Codeberg PR/MR URL. ctx uses local recorded evidence; a URL
is a selector, not a request to fetch the pull request from its host.

## Inspect the evidence

Open the cited event or session after finding a match:

```bash
ctx show event <ctx-event-id> --window 3
ctx show session <ctx-session-id>
ctx search "why was this changed" --session <ctx-session-id>
```

Blame distinguishes proven production, possible attribution, conflicting
records, and missing evidence. Pull-request activity, membership, and merge
activity are separate from producing the code. A session mentioning a commit
or pull request is not by itself evidence that it produced the change.

File results refer to committed code. A dirty working copy can differ from the
commit being evaluated. Results retain their evidence limits, currentness, and
continuation information; an empty or partial result does not prove that no
agent worked on the code.

ctx preserves recorded amend/rebase replacement and cherry-pick derivation
when the tool-call evidence supports those relationships. Similar patches,
timestamps, and Git ancestry alone do not establish an agent attribution.

## Indexing and readiness

Blame indexing is enabled by default. Automatic indexing keeps the attribution index up to date with retained local
history. Use `ctx status` and `ctx doctor` to inspect health. For an explicit
completion, run:

```bash
ctx setup --wait
# Or refresh all sources and complete attribution:
ctx import --all
```

In manual indexing mode, these commands complete attribution in the calling
command, including when Core history was already current. `ctx index`,
`ctx index watch`, `ctx index wait`, status, and doctor observe existing work;
they do not start an attribution rebuild. `ctx setup --no-daemon`, even with
`--wait`, explicitly suppresses refresh.

To import history without waiting for Blame, use `ctx import --all --no-blame`
(also supported for a selected provider or path). This only skips the calling
command's Blame completion; an enabled daemon may still index in the background.
To disable both foreground and background Blame indexing, add this to the data
root's `config.toml`:

```toml
[blame]
enabled = false
```

The setting is read before each indexing pass; an already-running pass may
finish. History import and search continue, and existing Blame data is retained
and readable with its usual freshness limits. Set `enabled = true` (or remove
the setting), then run `ctx import --all` to catch up retained history.

`import` and `setup --wait` report Blame preparation, indexing and publication
with `--progress plain` or `--progress json`; `--progress none` stays silent.
Status reports active work separately from the last committed index's readiness.
Source counters count completed sources in this pass; changes count accepted
additions, replacements and deletions, not every record in retained history.

If work is pending or interrupted, retry `ctx import --all` or
`ctx setup --wait`. Committed history remains searchable while attribution is
pending. A completed index can still have no evidence for the requested code.

JSON status exposes the existing generation, coverage, availability, and
diagnostic fields under `attribution`; see [JSON contracts](contracts/json.md#blame-attribution-readiness).
Current empty or abstained coverage is terminal and needs no rebuild.

## Local storage and evidence limits

The derived attribution index lives under `search/attribution/` in the ctx data
root. It is unencrypted local data, like the other local search indexes. Protect
the data root and review output before sharing it. Blame reads retained ctx
history and locally available repository evidence; it does not edit provider
history or source repositories.

Retained history can be used after the provider's original log files disappear.
Missing or pruned Git objects, moved repositories, unavailable history, and
ambiguous records can reduce what ctx can prove. Output reports those limits
instead of fabricating an attribution. MCP hosts may log or forward the text
and structured results they receive.

## Continue a result

Use a returned cursor with the same target and query options. Cursors identify
a position in a particular attribution generation. If the generation, target,
file HEAD, or relevant line range changes, restart the query without the cursor.
Cursors from older formats also require a fresh query.

## MCP

The `blame` tool accepts a structured target with `kind: "file"`, `"commit"`,
or `"pull_request"`, plus optional `limit` and `cursor`. MCP `limit` accepts
integers from 1 through 8 and defaults to 8, independently of the CLI default.
For example, the arguments for file blame are:

```json
{
  "target": {
    "kind": "file",
    "path": "src/checkout.ts",
    "lines": { "start": 118, "end": 146 }
  },
  "limit": 8
}
```

The tool returns meaningful text and the same structured evidence available
through CLI JSON. Discover its current input schema through `tools/list`.
The Blame query reads committed attribution state and advertises
`readOnlyHint: true`; it does not start background catch-up. If indexing is
pending, run `ctx import --all` separately and retry. See [MCP](mcp.md) for
transport and privacy.

## Upgrading from earlier releases

In ctx 1.5, Blame needs no account, activation, or separate executable. Existing
history, citations, configuration, and local usage remain available. Old
separate graph files and credentials are left untouched and unused; the new
attribution index is rebuilt from retained history and available Git evidence.
This is not a lossless conversion of every earlier repository observation.
