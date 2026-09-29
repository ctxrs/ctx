"""Private server recovery through the real CLI and HTTP interface."""

import json
from urllib.parse import quote


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def recovered_owner(h, checkpoint, label):
    root = h.root / label
    credential = h.token_file(label)
    h.admin("restore", checkpoint, "--credentials-out", credential, root=root)
    document = json.loads(credential.read_text())
    token = document["credential"]["secret"]
    h.secrets.add(token)
    h.tokens[label] = token
    h.users[label] = document["principal"]
    h.start_server(root)
    for old in ("alice", "bob", "carol", "dana", "erin", "erin-restricted"):
        for collection in (h.team, h.restricted):
            h.deny("GET", h.route(collection, "search?q=beacon"), h.tokens[old])
            h.deny("POST", h.route(collection, "uploads"), h.tokens[old],
                   {"sha256": "a" * 64, "bytes": 1})
    h.connect(label, "team", h.team)
    h.connect(label, "restricted", h.restricted)
    # Manager review is independent of query terms and index availability.
    for collection in (h.team, h.restricted):
        cursor, seen = None, set()
        while True:
            path = h.route(collection, "publications?limit=1")
            if cursor is not None:
                path += "&after=" + quote(cursor, safe="")
            page = h.http("GET", path, token)
            for publication in page["publications"]:
                key = (publication["publication"], publication["retained_revision"])
                require(key not in seen, "inventory duplicated a retained revision")
                seen.add(key)
                require(publication["owner"] != document["principal"],
                        "recovery replaced original publisher provenance")
            cursor = page["next_cursor"]
            if cursor is None:
                break
        require(seen, "restored audience has no manager review inventory")
    return root


def invite_reader(h, owner, actor):
    invitation = h.token_file(actor + "-invite")
    h.cli(owner, "server", "--remote", "team", "invite", actor,
          "--read-only", "--output", invitation, "--format=json")
    enrollment = json.loads(invitation.read_text())["enrollment"]["secret"]
    h.secrets.add(enrollment)
    document = h.http("POST", "/v1/enroll", body={"enrollment": enrollment})
    token = document["credential"]["secret"]
    h.tokens[actor], h.users[actor] = token, document["principal"]
    h.secrets.add(token)
    h.token_file(actor).write_text(json.dumps(document) + "\n")
    h.token_file(actor).chmod(0o600)
    h.connect(actor, "team", h.team, read_only=True)


def private_recovery(h, recovered, old_hit, new_hit, private_hit, sibling, restricted,
                     team_sequence, restricted_sequence):
    # Yesterday's backup cannot know today's revocations or withdrawals. Restore
    # it only for a new owner; that owner reviews content before new invitations.
    old_owner = "recovered-old-owner"
    recovered_owner(h, recovered / "before-removal", old_owner)
    status = h.http("GET", h.route(h.team, "status"), h.tokens[old_owner])
    h.wait_searchable(h.team, old_owner, status["stored_sequence"])
    exact = h.http("GET", h.route(h.team, "events/" + quote(new_hit["citation"], safe="")),
                   h.tokens[old_owner])
    require(exact == new_hit, "private restore changed original evidence or provenance")
    h.cli(old_owner, "server", "--remote", "team", "withdraw",
          "--publication", "bob-main", "--format=json")
    status = h.http("GET", h.route(h.team, "status"), h.tokens[old_owner])
    h.wait_searchable(h.team, old_owner, status["stored_sequence"])
    invite_reader(h, old_owner, "reviewed-reader")
    require(not h.search("reviewed-reader", h.team, "cedarcorrected"),
            "reviewed withdrawal was shared with the new reader")
    h.evidence("reviewed-reader", h.team, "oakbeacon", sibling, "bob")
    h.deny("GET", h.route(h.restricted, "search?q=violetbeacon"),
           h.tokens["reviewed-reader"])
    h.stop(h.server)

    owner = "recovered-current-owner"
    recovered_owner(h, recovered / "current", owner)
    h.wait_searchable(h.team, owner, team_sequence)
    h.wait_searchable(h.restricted, owner, restricted_sequence)
    h.evidence(owner, h.team, "oakbeacon", sibling, "bob")
    h.evidence(owner, h.restricted, "violetbeacon", restricted, "dana")
    h.evidence(owner, h.team, "allowedqueue", "allowedqueue may retry after reconnect.", "alice")
    h.mcp(owner, "restricted", "violetbeacon", private_hit)
    for hit in (old_hit, new_hit):
        h.deny("GET", h.route(h.team, "events/" + quote(hit["citation"], safe="")),
               h.tokens[owner])
    require(not h.search(owner, h.team, "cedarcorrected"),
            "current checkpoint resurrected its recorded withdrawal")
    invite_reader(h, owner, "new-reader")
    h.evidence("new-reader", h.team, "oakbeacon", sibling, "bob")
    h.deny("GET", h.route(h.restricted, "search?q=violetbeacon"), h.tokens["new-reader"])
    h.stop(h.server)
