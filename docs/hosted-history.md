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

Initialize an explicit server directory and save the operator credential to a
private file:

```sh
ctx server --root ./history-server init team --credentials-out ./operator.json \
  --authority-file ./history-authority.json
ctx server --root ./history-server run
```

Keep `history-authority.json` outside server checkpoints and retain it separately;
it prevents a restored backup from undoing later access changes. Losing the
server disk and this file together leaves a restore closed to serving. See
[recovery](#recover-the-server-safely) below.

Initialization prints the collection ID. The default listener is
`127.0.0.1:7332`. For access from other machines, put the listener behind an HTTPS
reverse proxy. `--trusted-ingress` explicitly permits a non-loopback bind for that
deployment; it does not configure TLS or trust client-supplied identity headers.
Keep the backend listener inaccessible to untrusted networks.

The process runs in the foreground. Use your existing service manager to start
it on boot. It does not install a service or auto-upgrade itself.

Connect the operator's client, replacing `COLLECTION_ID` with the printed ID:

```sh
ctx remote connect team --url http://127.0.0.1:7332 \
  --collection COLLECTION_ID --token-file ./operator.json
ctx remote status team --online
```

HTTP is accepted only for numeric loopback addresses. Other destinations require
HTTPS. Connecting saves the destination and credential; it uploads no history.

## Invite people and choose an audience

A collection is a sharing audience. Read, publish and manage are separate rights.
A member can publish without reading anyone else's history, read without
publishing, or do both. Create separate collections for separate audiences.
There is no implicit organization-wide read permission.

Invite a team member while the server is running:

```sh
ctx server --remote team user invite alice --output ./alice-enrollment.json
```

Deliver that private file securely. On Alice's machine:

```sh
ctx remote connect team --url https://history.example.com \
  --collection COLLECTION_ID --enrollment-file ./alice-enrollment.json
```

The enrollment is short-lived and single-use. Ordinary invitations grant read
and publish; add `--read-only` for a reader or `--manage` for an administrator.
Tokens belong in protected files or piped stdin, not command-line arguments.

An administrator can change a member's exact rights or revoke collection access:

```sh
ctx server --remote team grant --user PRINCIPAL_ID --publish
ctx server --remote team revoke --user PRINCIPAL_ID
```

Omitted grant flags remove those rights. Credentials can only exercise rights
they were issued with, intersected with the member's current grants. Increasing
rights may therefore require issuing a new credential. Revocation prevents future
access; it cannot retract copies someone already retrieved.

## Select history explicitly

Authorize an existing provider profile and project scope:

```sh
ctx remote share team --profile-root /path/to/agent-profile \
  --mode automatic --backfill all --include-future \
  --work-root /path/to/team-project
ctx remote sync team
```

After registering a new profile with `ctx sources add`, run `ctx import --all`
to commit its source membership before selecting it for sharing.

Use the profile root registered with ctx, or select exact indexed sources with
repeatable `--source` digests. Work-directory evidence must fit the selected
roots; unknown or mixed scope stays held. `--whole-source` explicitly includes
the entire selected source instead. Repository labels do not guarantee that
every sentence concerns only that project.

`--backfill none --include-future` excludes the current baseline. Reviewed mode
uses `--mode reviewed --backfill all` without `--include-future` and authorizes
only the exact current session revisions. The command leaves an inspectable
normalized snapshot and reports its path.

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

## Recover the server safely

With the server stopped, create a coherent checkpoint and back it up with restic:

```sh
ctx server --root ./history-server backup --output ./server-checkpoint
restic backup ./server-checkpoint
```

A checkpoint includes the permission catalog and accepted payload closure.
Copying an arbitrary live server directory is not a supported checkpoint.

An old backup must not restore access that was revoked after it was made. For
recoverable serving, configure `--authority-file /independent/location/current.json`
when initializing the server and retain that file independently and continuously.
The server records its location and advances it before acknowledging permission,
withdrawal, publishing-policy changes, or explicit settlement of an uncertain
upload. Ordinary uploads do not advance it.
It is a current security floor, not a file to roll back alongside the database.

```sh
ctx server --root ./recovered-server restore ./server-checkpoint \
  --authority-file /independent/location/current.json
```

Only a checkpoint matching that current security authority can reopen. It may
omit uploads or content corrections accepted afterward: those are the backup
data-loss window. A checkpoint predating later security changes, or an ordinary
restore without matching authority, remains closed to serving.
Keeping the authority file on the same lost disk does not provide disaster
recovery. This version does not reconcile older checkpoints forward or provide
multi-node replication, enterprise identity federation, or a zero-data-loss SLA.
