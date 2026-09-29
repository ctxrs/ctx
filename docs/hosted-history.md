# Back up and share history

ctx works locally by default. You can also export a portable backup or run a
server that receives selected histories and lets authorized people search them.
Neither requires changing how ordinary local search works.

The server is one process with its own storage directory. It uses SQLite for
permissions and accepted uploads, immutable files for retained history, and
ctx's existing lexical index for search. Clients keep their local indexes unless
you deliberately use a separate, remote-only reader.

## Personal backup

Export a committed snapshot of the history ctx has retained:

```sh
ctx archive export --output ./history-snapshot --origin my-laptop
ctx archive verify ./history-snapshot
```

Reuse the same origin name for later snapshots from this history. Each export
needs a new output directory. `--source` and `--session` select full Core identity
digests; without them, export includes all indexed sources and sessions.

The archive contains retained normalized records, including structured content
and explicit omission states. It contains no provider credentials or executable
configuration. Content that ctx never retained cannot be recovered from it. This
is a searchable history backup, not an agent's native resumable-session backup.

Use an established backup tool for encryption and transport. For example, after
configuring a [restic repository](https://restic.readthedocs.io/en/stable/030_preparing_a_new_repo.html):

```sh
restic backup ./history-snapshot
restic snapshots
restic check
```

Keep the repository password somewhere that survives losing this machine.
restic can use local storage, SFTP, S3-compatible storage and other supported
backends; ctx does not add another storage credential system.

To search a restored archive without its original provider files:

```sh
ctx --data-root ./recovered-history archive restore ./history-snapshot
ctx --data-root ./recovered-history search "database migration"
```

Choose a new or previously archive-owned data root. Automatic indexing and
provider discovery stay off in that root; searches read its committed archive
snapshot, including when `--refresh wait` is supplied. Reimporting the same content
is a no-op. A corrected session requires `--expected-predecessors FILE`, a JSON
object mapping that member's inventory path to its currently imported SHA-256.
Missing sessions in a later archive do not delete earlier imports.

When exporting a restored root, reuse the original `--origin` and `--view`. ctx
exports the retained original records, preserving their identities and provenance.
If the root contains several original identities, export each separately; output
lists those not included. Conflicting versions imported under different namespaces
require an explicit `--source` selection.

## Start a server

Initialize your server and run it:

```sh
ctx server init
ctx server run
```

ctx chooses a separate server directory under its data root, saves the initial
administrator credential privately, and configures the local administrator
connection. You do not need to copy a collection ID or choose credential files.
Use `--root PATH` when you want to choose the server's storage location.

The default listener is `127.0.0.1:7332`. The server runs in the foreground;
leave that terminal open and use another terminal for administration. Use your
existing service manager to keep it running after logout or reboot. ctx does
not install a server service or auto-upgrade the server.

For other machines, put the loopback listener behind an HTTPS reverse proxy and
use that HTTPS URL when connecting. `--trusted-ingress` permits an explicit
non-loopback backend bind; it does not configure TLS or trust identity headers.
Keep the backend inaccessible to untrusted networks.

Live transcripts, SQLite data, the search index, and exported checkpoints are
not encrypted by ctx. Put them on encrypted storage if you need encryption at
rest. restic encrypts its backup repository separately.

## Invite someone

While the server is running:

```sh
ctx server invite alice
```

ctx saves a short-lived, single-use invitation in a protected file and explains
how to use it. Deliver that file securely. On Alice's machine:

```sh
ctx remote connect https://history.example.com
```

Paste the invitation when prompted. For scripts, use `--enrollment-file PATH`
instead. ctx saves the credential privately and reports the connection name.
Connecting does not upload any history or enable local indexing. Device and
operator credentials stay valid until revoked by default; invitations expire
quickly and can be used only once. An administrator can choose a finite lifetime
when issuing a credential.

A collection is a sharing audience. Read, publish and manage are separate rights.
A member can publish without reading other people's history, read without
publishing, or do both. Use separate collections for separate audiences; there
is no implicit organization-wide read permission. Invitations normally grant
read and publish. Use `--read-only` for a reader or `--manage` for an administrator.

An administrator can set exact rights or revoke collection access through a
saved connection:

```sh
ctx server --remote team grant --user PRINCIPAL_ID --publish
ctx server --remote team revoke --user PRINCIPAL_ID
```

Omitted grant flags remove those rights. Credential rights are intersected with
the current grant, so increasing rights may require issuing a new credential.
The `--remote` form revokes membership in that connection's collection. For
server-wide revocation, stop the server and use
`ctx server --root PATH revoke --user PRINCIPAL_ID`; it refuses while the root
is busy rather than silently narrowing its scope. Revocation prevents future
access; it cannot retract copies already retrieved.

## Select history explicitly

List indexed profiles with `ctx sources`. To add a new profile, register its
absolute root and import it first:

```sh
ctx sources add work --provider codex --root /path/to/agent-profile
ctx import --all
```

Then authorize that registered profile and a project scope:

```sh
ctx remote share team --profile-root /path/to/agent-profile \
  --work-root /path/to/team-project
ctx remote sync team
```

A standalone `ctx import --path ...` does not establish a registered profile
identity. Use the registered root for `--profile-root`.

Use the profile root registered with ctx, or select exact indexed sources with
repeatable `--source` digests. Work-directory evidence must fit the selected
roots; unknown or mixed scope stays held. `--whole-source` explicitly includes
the entire selected source instead. Repository labels do not guarantee that
every sentence concerns only that project.

The default reviewed mode authorizes only the exact current session revisions.
The command leaves an inspectable normalized snapshot and reports its path.
First selection verifies your publishing identity with the server; later policy
narrowing can work offline. Add `--mode automatic --include-future` to authorize
future revisions and sessions within that scope. With automatic mode,
`--backfill none --include-future` excludes the current baseline.

An already-enabled local daemon discovers the saved policy within 30 seconds and
retries offline work in the background. Sharing does not enable a disabled
daemon. `remote sync` performs a bounded foreground batch and reports pending
work; call it again for a larger backlog when no daemon is running.

```sh
ctx remote status team --online
ctx remote pause team
ctx remote pause team --resume
ctx remote remove team
```

Pause stops future upload attempts. Removing the connection deletes its pending
local upload state. Neither deletes history already accepted by the server.
Status separates upload backlog from observed sessions held for review or scope.
Stored and searchable progress are separate: a durable receipt does not claim
the search index has caught up. If you narrow sharing while a publish request is
unresolved, ctx settles that operation without resending its history. An already
accepted revision remains on the server unless you explicitly withdraw it.

## Search from another machine or an agent

```sh
ctx --server team search "why did we change the cache"
ctx --server team show event CITATION
ctx --server team show session SESSION_CITATION
ctx --server team mcp serve
```

Remote search uses the named connection and requires no local history index or
daemon. Without `--server`, commands stay local. Remote failure never silently
falls back to local results. Search results carry publisher, origin, collection
and revision evidence. Pass the returned opaque citation unchanged to show.
Search returns bounded excerpts and citations; `show event` retrieves the complete
retained event behind a citation. A shortened excerpt does not alter the stored
evidence. Remote session display is an event log, including retained tool activity.
`--mode log` is its default; local `lite`/`full` transcript selection is not
available remotely.

Remote MCP serves the same authorized history over stdio. Graph operations,
Blame, semantic search and local filesystem tools are not remotely hosted by
this server. Unsupported local-only query options fail explicitly.

Corrections replace the current searchable session. Earlier exact citations
remain available until the publication is explicitly withdrawn:

```sh
ctx server --remote team withdraw --publication PUBLICATION_ID
```

Withdrawal closes reads and stale upload retries for that publication. Retained
payload files are not physically erased; storage erasure and backup retention
remain separate operational decisions.

## Back up and recover the server

With the server stopped, create a coherent checkpoint, then back it up with
restic to your chosen repository:

```sh
ctx server backup --output ./server-checkpoint
restic backup ./server-checkpoint
```

Use a fresh output directory for each checkpoint. The checkpoint includes the
permission catalog and accepted history. It is plaintext until your backup tool
encrypts it. Copying an arbitrary live server directory is not a supported backup.
Keep the backup repository and its password somewhere that survives losing the
server.

Restore into a new server directory:

```sh
ctx server --root ./recovered-server restore ./server-checkpoint
ctx server --root ./recovered-server run
```

The restored history is private to a new recovery owner. All previous logins,
pending invitations and access grants are invalidated. ctx saves the new owner
credential privately and configures its local administrator connection. Original
transcript evidence and publisher provenance remain intact.

**Review recovered history before inviting people again.** An older backup does
not know about withdrawals or permission changes made after the backup. A new
owner can inspect the retained history and withdraw material before deliberately
issuing fresh invitations. Revoked users do not automatically regain access.

List retained publications without knowing a search term:

```sh
ctx server --root ./recovered-server publications
```

Follow `next_cursor` with `--after CURSOR` until it is empty. Add
`--collection COLLECTION_ID` to review another restored collection. Each page supplies
publication IDs, original publishers and exact citations for retained revisions.
Read access includes older retained revisions, even when search shows a newer
correction. Use a returned
publication ID with `server --root ./recovered-server withdraw --publication ID`;
include the same `--collection COLLECTION_ID` for a nondefault collection.
For full transcript inspection, save an owner connection and use a returned citation:

```sh
ctx remote connect http://127.0.0.1:7332 --name recovered \
  --token-file ./recovered-server/operator.json
ctx --server recovered show session SESSION_CITATION
```

Add `--collection COLLECTION_ID` when connecting to another restored collection.
Review each collection before granting access to it.

On previously connected clients, remove the old connection, connect using a new
invitation, then select history again:

```sh
ctx remote remove team
ctx remote connect https://history.example.com
ctx remote share team --profile-root /path/to/agent-profile --whole-source
```

Removing the connection discards its local backlog, not its local history or
server copies. Re-selecting history grants fresh permission to publish under
the new identity. Replacing a credential for the same authenticated publisher
preserves the existing policy and backlog.

A restore can lose uploads or corrections accepted after the checkpoint. This
version does not reconcile an old backup forward or provide multi-node
replication or a zero-data-loss guarantee. A backup kept on the same lost disk
is not disaster recovery.
