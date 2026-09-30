# Back up and share history

ctx works locally by default. You can also export a portable backup or run a
server that receives selected histories and lets authorized people search them.
Neither requires changing how ordinary local search works.

**Hosted history is an opt-in beta included in the normal ctx binary.** Installing
or upgrading ctx does not start a server, connect to one, or enable uploads.
The server commands and remote API may evolve during beta; keep a server
checkpoint before upgrading. Ordinary local search is unchanged.

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

## Server upgrades

Upgrade the server deliberately: stop it, make a checkpoint, install the chosen
version, and restart it. Keep the old executable and checkpoint until you have
verified access, search, and retrieval. Restoring a checkpoint uses a fresh root
and fresh owner access as described below; do not open a migrated catalog with
an older executable.

Use a separately managed executable for a server service if the same machine
also has an automatically updated desktop installation. Updating a remote
client does not replace or restart the server. The server does not run its own
automatic updater.

The HTTP API uses versioned routes, and the storage catalog and backup manifest
carry format versions. An unsupported version is an error, never a reason to
discard history. During beta, use matching client and server releases unless
the release notes explicitly qualify another combination.

Upgrading the earlier beta catalog preserves retained history, citations and
user IDs, but revokes its unscoped device credentials and pending invitations.
Only the original owner credential proven by the protected operator file is
preserved. Re-enroll members for their specific collections. If that owner file
is unavailable, restore a checkpoint into a fresh root to recover owner access.

Older reader connections may have no saved user identity, so a revoked old
credential cannot prove who they belonged to. Connect the fresh invitation with
a new `--name` in that case. You can deliberately remove an unused old reader
connection afterward; removal discards its local pending state. A connection
with a saved sharing policy and authenticated publisher can be re-enrolled in
place and keeps its upload queue.

## Invite someone

While the server is running:

```sh
ctx server invite
```

ctx saves a short-lived, single-use invitation in a protected file and explains
how to use it. Deliver that file securely. An optional label such as
`ctx server invite alice` helps you recognize the person; it is not a login name
or a source of permissions. Each new invitation creates a distinct user ID,
even when labels are identical. On the invited person's machine:

```sh
ctx remote connect https://history.example.com
```

Paste the invitation when prompted. For scripts, use `--enrollment-file PATH`
instead. ctx saves the credential privately and reports the connection name.
Connecting does not upload any history or enable local indexing. Device and
operator credentials stay valid until revoked by default; invitations expire
after 15 minutes by default and can be used only once. An administrator can choose a finite lifetime
when issuing a credential. A non-owner's invitation cannot outlive the credential
that issued it, and its new device credential cannot extend that expiry.

A collection is a sharing audience. Read, publish and manage are separate rights.
A member can publish without reading other people's history, read without
publishing, or do both. Use separate collections for separate audiences; there
is no implicit organization-wide read permission. Invitations normally grant
read and publish. Use `--read-only` for a reader, `--publish-only` for a device
that can back up history without reading the collection, or `--manage` to also
authorize collection administration.

An administrator can set exact rights or revoke collection access through a
saved connection:

```sh
ctx server --remote team grant --user PRINCIPAL_ID --publish
ctx server --remote team revoke --user PRINCIPAL_ID
```

Omitted grant flags remove those rights. Credential rights are intersected with
the current grant, so increasing rights may require issuing a new credential.
The `--remote` form above revokes membership in that connection's collection.
Revocation prevents future access; it cannot retract copies already retrieved.

Ordinary device credentials are bound to one collection. Giving the same person
access to another collection does not make an existing device credential work
there; issue a separate invitation for that collection. Collection management
does not grant server-owner authority.

## Add or replace a device

The server owner can list users and issue an invitation for an existing ID:

```sh
ctx server user list
ctx server invite --user USER_ID
```

The person can also use `ctx server --remote team invite --user USER_ID` with the
ID shown by `ctx remote status team`. Match their existing rights: add `--read-only`
for a reader or `--publish-only` for a publisher without read access.
Collection managers can invite new people, but cannot issue devices
as other existing users. An existing-user invitation preserves that person's
identity and grants; it does not broaden their access.

For a script, connect using the protected invitation file:

```sh
ctx remote connect https://history.example.com --name team \
  --enrollment-file ./invitation.json
```

Repeating a successfully saved invitation reports `already_connected` without
redeeming it again. This confirms the local setup, not that the server is online
or the credential is still valid; use `ctx remote status team --online` to check.
A fresh invitation for the same user rotates the saved credential while retaining
the sharing selection and upload queue. A different user or server is rejected
instead of inheriting that selection. Use a new connection name for another
identity or destination.

Replacing this client's credential does not revoke the old credential. Revoke
it separately when retiring or replacing a device.

`--read-only` removes local publishing access and pauses any saved sharing policy
without discarding queued work. Reconnecting with publishing access does not
silently resume a paused policy.

Server owners can inspect and revoke access while the server is running:

```sh
ctx server user credentials USER_ID
ctx server revoke --credential CREDENTIAL_ID
ctx server revoke --user USER_ID
```

The root-selected form revokes the user across the server. Through an explicitly
named owner connection, add `--server-wide` to
`ctx server --remote team revoke --user USER_ID`; without it, that command removes
only the collection's membership. Revoking a single credential leaves other
devices working. Revoking a user blocks all their devices and outstanding
invitations.

Inventory lists IDs, optional labels, scope, expiry and revocation state, never
secrets. Follow `next_cursor` with `--after CURSOR` to read subsequent pages.
If an invitation was consumed but its response could not be saved, run
`ctx server user credentials USER_ID --format=json`. Match `enrollment_id` to the
invitation's ID, revoke that row's `credential_id`, and issue a fresh invitation
for the same user. Invitations cannot be replayed to recover a secret.

Scripts and device-management systems can deliver invitation files and run the
same connect command. There is no shared fleet-wide enrollment secret, automatic
history selection, or built-in identity-provider integration in this beta.

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
