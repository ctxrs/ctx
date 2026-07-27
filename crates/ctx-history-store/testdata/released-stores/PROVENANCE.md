# Released v46 store fixtures

These small databases are migration evidence written through the public storage
APIs of immutable ctx releases. They are not databases assembled with synthetic
schema SQL.

| Fixture | Released tag | Immutable commit | Released schema |
| --- | --- | --- | --- |
| `v0.24.0-work.sqlite.zst` | `v0.24.0` | `460ad6f1c5fe5dd4465f0f1ddfb6c95c3d7a55c1` | 46 |
| `v0.25.0-work.sqlite.zst` | `v0.25.0` | `228e05fa0fd058822be7a362acd65cacdad24356` | 46 |

`generate.sh` exports each exact commit with `git archive`, creates a standalone
Cargo wrapper around the archived release's `ctx-history-store` and
`ctx-history-core` path dependencies, and uses `generator.rs` as the wrapper's
only source. The wrapper appends only its own package entry to the exact
released `Cargo.lock`; an offline Cargo metadata pass canonicalizes the selected
standalone graph while retaining the released dependency versions. Keeping the
wrapper outside the archived workspace also prevents Cargo from resolving
unrelated historical CLI packages. The generator writes a fixed capture source,
canonical session, message, and failed tool-output event through the released
`Store` API. It checks that the released search projection finds both canaries,
checkpoints the WAL, validates schema 46 and integrity, and normalizes the
wall-clock-only
`search_projection_stats.updated_at_ms` cache timestamp to the fixed event time.
That single SQL update does not create or alter capture, session, event, or
search-projection evidence. It then uses only `journal_mode=DELETE` plus
`VACUUM` to normalize the physical SQLite file. The script generates each
database twice and refuses to publish it unless the uncompressed files are
byte-identical. It then applies deterministic zstd compression and records
compressed and uncompressed SHA-256 values.

Regenerate from a checkout containing both tags:

```sh
crates/ctx-history-store/testdata/released-stores/generate.sh
```

Generation is deliberately offline (`cargo --offline`). Its Cargo dependencies
must already be cached. The committed release gate does not run the generator or
use the network; it decompresses these checked-in artifacts and verifies their
checksums and contents.

The historical CLI itself is not used because its import command creates a
history record with wall-clock timestamps and a UUIDv7 before writing the Store,
so it cannot reproduce a stable whole-file checksum. The fixture writer remains
the immutable released storage library; only the fixed, public input objects
live in `generator.rs`.

## Complete-content limitation

Neither tag contains `verified_content_locators_v1`,
`provider_source_locators`, or `capture_source_provider_routes`; those v0.26
contracts did not exist in the released writers. Therefore no honest
v0.24/v0.25 artifact can contain a previously working v0.26 complete-content
locator/route to preserve.

The upgrade test verifies the strongest available historical evidence: v47
preserves the exact capture-source path and canonical source identity, initially
fails closed with no invented route, then accepts a current v47 observation of
that same source through the public reconciliation API. Binding that observation
to the preserved capture source makes authorized source lookup succeed for the
preserved event. End-to-end body hydration from a historical event remains
unprovable because the released event has no typed verified-content locator.

All fixture strings and paths are synthetic public canaries. The databases
contain no user transcript, credential, machine path, or private repository
content.
