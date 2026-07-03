# Troubleshooting

## No Sources Found

Run:

```bash
ctx sources --json
```

Confirm the provider keeps history on this machine and pass an explicit path if
needed:

```bash
ctx import --provider codex --path ~/.codex/sessions
```

## Search Misses Recent Work

Re-run import:

```bash
ctx import --all
ctx search "the missing phrase"
```

Use `ctx import --resume --json` when you want output to mark the run as an
idempotent rescan.

After upgrading to `0.10.x` or newer, a refresh can take longer once because ctx marks
older provider import cache rows pending and re-reads source transcripts to
populate touched-file metadata and unredacted local transcript text.

If the raw provider file moved, indexed text may still be searchable, but source
citations should report that the raw path is unavailable.

## JSON Consumer Fails

Run the same command without `--json` to inspect warnings, then run:

```bash
ctx doctor --json
```

Check the command contract in [contracts/json.md](contracts/json.md), including
whether the field is documented as nullable or compatibility-only.

## Upgrade Problems

Run:

```bash
ctx upgrade status
ctx upgrade check
```

Self-upgrade requires an official installer-managed binary and matching
`ctx.install.json` sidecar. Source builds, `cargo install`, copied binaries,
package-manager installs, and binaries whose SHA-256 no longer matches the
sidecar are intentionally unmanaged.

Disable managed background auto-upgrade with:

```bash
ctx upgrade disable
```

or for one process:

```bash
CTX_UPGRADE_OFF=1 ctx search "query"
```

Background checks log to `~/.ctx/logs/upgrade.log` and should not write to
stdout or stderr.

## Store Problems

Find the active root:

```bash
ctx status
```

The default is `~/.ctx`. Check permissions and available disk space. Treat the
database and logs as private local history when collecting diagnostics.
