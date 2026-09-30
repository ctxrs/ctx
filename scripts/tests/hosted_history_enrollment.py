"""Device enrollment and collection boundaries in the hosted acceptance run."""

import json


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def setup_multicollection_reader(h):
    """One person receives separate credentials for two explicit audiences."""
    h.deny("GET", h.route(h.restricted, "status"), h.tokens["erin"])
    actor = "erin-restricted"
    invitation_path = h.token_file(actor + "-enrollment")
    h.cli("alice", "server", "--remote", "restricted", "invite",
          "--user", h.users["erin"], "--read-only", "--output", invitation_path,
          "--format=json")
    invitation = json.loads(invitation_path.read_text())
    enrollment = invitation["enrollment"]["secret"]
    h.secrets.add(enrollment)
    document = h.http("POST", "/v1/enroll", body={"enrollment": enrollment})
    require(document["principal"] == h.users["erin"],
            "additional collection enrollment replaced the person's identity")
    require(document["collection"] == h.restricted,
            "additional collection enrollment changed its audience")
    h.users[actor] = document["principal"]
    h.tokens[actor] = document["credential"]["secret"]
    h.secrets.add(h.tokens[actor])
    h.token_file(actor).write_text(json.dumps(document) + "\n")
    h.token_file(actor).chmod(0o600)
    h.deny("POST", "/v1/enroll", None, {"enrollment": enrollment})
    h.deny("GET", h.route(h.team, "status"), h.tokens[actor])
    h.connect(actor, "restricted", h.restricted, read_only=True)
    require(len(set(h.users.values())) == 5,
            "a second collection credential created an extra identity")


def device_lifecycle(h):
    """Exercise ordinary enrollment, reruns, rotation and online revocation."""
    endpoint = f"http://127.0.0.1:{h.port}"

    def enroll(actor, label, principal=None, name="team", collection=None):
        collection = collection or h.team
        path = h.token_file(label)
        args = ["--user", principal] if principal else []
        invitation = h.cli("alice", "server", "--remote", name, "invite",
                           *args, "--output", path, "--format=json")
        packet = json.loads(path.read_text())
        h.secrets.add(packet["enrollment"]["secret"])
        result = h.cli(actor, "remote", "connect", endpoint, "--name", name,
                       "--enrollment-file", path, "--format=json")
        settings_path = h.root / actor / "data/sharing" / name / "settings.json"
        before = settings_path.read_bytes()
        settings = json.loads(before)
        token = settings["credentials"]["publish"]
        h.secrets.add(token)
        identity = h.http("GET", h.route(collection, "whoami"), token)
        require(identity["principal"] == invitation["user"],
                "enrollment changed the invited user identity")
        repeated = h.cli(actor, "remote", "connect", endpoint, "--name", name,
                         "--enrollment-file", path, "--format=json")
        require(repeated["already_connected"] is True,
                "repeated successful enrollment did not report already connected")
        require(settings_path.read_bytes() == before,
                "repeated successful enrollment rewrote saved state")
        require(result["local"]["enabled"] is False,
                "enrollment enabled sharing without a selected source")
        require(not (h.root / actor / "data/search").exists(),
                "enrollment created a local history index")
        h.deny("POST", "/v1/enroll", None,
               {"enrollment": packet["enrollment"]["secret"]})
        return token, identity

    first, identity = enroll("laptop-one", "first-device")
    principal = identity["principal"]
    second, second_identity = enroll("laptop-two", "second-device", principal)
    require(identity["credential_id"] != second_identity["credential_id"],
            "two devices share one credential")
    h.cli("alice", "server", "--remote", "restricted", "grant", "--user",
          principal, "--read", "--publish", "--format=json")
    for token in (first, second):
        h.deny("GET", h.route(h.restricted, "status"), token)
    restricted, _ = enroll("laptop-two", "restricted-device", principal,
                           "restricted", h.restricted)
    h.http("GET", h.route(h.restricted, "status"), restricted)
    h.cli("alice", "server", "--remote", "team", "revoke", "--user",
          principal, "--format=json")
    h.deny("GET", h.route(h.team, "status"), second)
    h.http("GET", h.route(h.restricted, "status"), restricted)
    h.cli("alice", "server", "--remote", "team", "grant", "--user",
          principal, "--read", "--publish", "--format=json")
    h.cli("alice", "server", "--remote", "team", "revoke", "--credential",
          identity["credential_id"], "--format=json")
    h.deny("GET", h.route(h.team, "status"), first)
    rotated, rotated_identity = enroll("laptop-one", "rotated-device", principal)
    require(rotated_identity["principal"] == principal
            and rotated_identity["credential_id"] != identity["credential_id"],
            "credential rotation changed the user or reused a credential")
    inventory = h.cli("alice", "server", "--remote", "team", "user",
                      "credentials", principal, "--format=json")
    entries = {row["credential_id"]: row for row in inventory["credentials"]}
    require(all(record["credential_id"] in entries
                and entries[record["credential_id"]]["enrollment_id"] == record["enrollment_id"]
                for record in (identity, second_identity, rotated_identity)),
            "credential inventory omitted an enrolled device")
    for token in (second, rotated):
        h.http("GET", h.route(h.team, "status"), token)

    # Simulate a committed exchange whose result the device could not save.
    lost_path = h.token_file("lost-response")
    lost = h.cli("alice", "server", "--remote", "team", "invite", "--user",
                 principal, "--output", lost_path, "--format=json")
    secret = json.loads(lost_path.read_text())["enrollment"]["secret"]
    h.secrets.add(secret)
    orphan = h.http("POST", "/v1/enroll", body={"enrollment": secret})
    orphan_token = orphan["credential"]["secret"]
    h.secrets.add(orphan_token)
    h.deny("POST", "/v1/enroll", None, {"enrollment": secret})
    inventory = h.cli("alice", "server", "--remote", "team", "user",
                      "credentials", principal, "--format=json")
    orphan_rows = [row for row in inventory["credentials"]
                   if row["enrollment_id"] == lost["id"]]
    require(len(orphan_rows) == 1
            and orphan_rows[0]["credential_id"] == orphan["credential"]["id"],
            "lost response cannot be correlated without its credential secret")
    h.cli("alice", "server", "--remote", "team", "revoke", "--credential",
          orphan_rows[0]["credential_id"], "--format=json")
    h.deny("GET", h.route(h.team, "status"), orphan_token)
    recovered, recovered_identity = enroll("recovered-device", "reissue", principal)
    require(recovered_identity["principal"] == principal,
            "lost-response recovery created another user")
    h.http("GET", h.route(h.team, "status"), second)
    h.cli("alice", "server", "--remote", "team", "revoke", "--user",
          principal, "--server-wide", "--format=json")
    for token in (second, rotated, recovered):
        h.deny("GET", h.route(h.team, "status"), token)
    h.deny("GET", h.route(h.restricted, "status"), restricted)
    h.http("GET", h.route(h.team, "status"), h.tokens["alice"])
    h.phase("device access: two audiences, rerun, revoked-token rotation, lost-response reissue and online revocation")
