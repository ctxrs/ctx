"""Real HTTP terminal-cancellation cases for the hosted acceptance harness."""

import hashlib
import json
from urllib.parse import quote


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def settlement_request(publisher, request):
    # Transport encoding only: Rust hashes a serde tuple, not sorted JSON.
    # Operation/ArchiveIdentity/SessionMember fields follow their struct order.
    # Nested member identities retain the order emitted directly by the archive
    # inventory serializer; do not round-trip them through a sorted JSON map.
    operation = {key: request["operation"][key] for key in (
        "idempotency_key", "publication", "writer_epoch", "policy_revision",
        "expected_revision", "expected_sequence", "revision")}
    identity = {key: request["identity"][key] for key in ("origin", "view")}
    member = {key: request["member"][key] for key in (
        "source", "session_id", "path", "sha256", "bytes", "records")}
    encoded = json.dumps(["publish", publisher, operation, identity, member],
                         ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8")
    return {"publisher": publisher, "operation": operation,
            "fingerprint": hashlib.sha256(encoded).hexdigest()}


def cancel_before_publish(h, actor, collection, archive, request):
    """Fence an unused key; the caller then publishes these bytes with a fresh key."""
    operation = dict(request["operation"],
                     idempotency_key=request["operation"]["idempotency_key"] + "-cancelled")
    require(operation["expected_revision"] is None and operation["expected_sequence"] is None,
            "cancel-before-publish case needs a new publication")
    upload, path, body, split = h.stage(actor, collection, archive, request["member"])
    h.http("PUT", path + f"?offset={split}", h.tokens[actor], body[split:])
    cancelled = dict(request, operation=operation, upload=upload)
    packet = settlement_request(h.users[actor], cancelled)
    token = h.tokens[actor]
    cancel_route = h.route(collection, "operations/cancel")
    revision_route = h.route(collection, "revisions")
    before = h.http("GET", h.route(collection, "status"), token)
    expected = {"outcome": dict(packet, status="cancelled"), "publication": None}
    require(h.http("POST", cancel_route, token, packet) == expected,
            "HTTP cancellation did not fence the unpublished operation")
    # A matching cancellation retry and matching publish retries are terminal.
    require(h.http("POST", cancel_route, token, packet) == expected,
            "HTTP cancellation retry changed its terminal outcome")
    for _ in range(2):
        require(h.http("POST", revision_route, token, cancelled, statuses=(409,))
                == {"error": "operation_cancelled"},
                "cancelled publish did not return the specific terminal HTTP error")
    receipt_route = h.route(collection, "receipts/" + quote(operation["idempotency_key"], safe=""))
    require(h.http("GET", receipt_route, token, statuses=(409,))
            == {"error": "operation_cancelled"}, "cancelled receipt lookup lost its terminal state")
    h.http("GET", h.route(collection, "publications/" + quote(operation["publication"], safe="")),
           token, statuses=(404,))
    after = h.http("GET", h.route(collection, "status"), token)
    require(after["stored_sequence"] == before["stored_sequence"],
            "cancellation allocated an accepted publication sequence")


def settle_accepted(h, actor, collection, request, receipt, current_receipt):
    """Publish won; settlement returns its old receipt plus actual current state."""
    token = h.tokens[actor]
    packet = settlement_request(h.users[actor], request)
    publication_route = h.route(collection, "publications/" + quote(
        request["operation"]["publication"], safe=""))
    current = h.http("GET", publication_route, token)
    require(current["sequence"] == current_receipt["sequence"]
            and current["revision"] == current_receipt["operation"]["revision"]
            and current["owner"] == h.users[actor] and not current["withdrawn"],
            "publication observation does not match current accepted history")
    expected = {"outcome": {"status": "accepted", "receipt": receipt}, "publication": current}
    for _ in range(2):
        require(h.http("POST", h.route(collection, "operations/cancel"), token, packet) == expected,
                "publish-wins HTTP settlement lost the historical receipt or current state")
    require(h.http("POST", h.route(collection, "revisions"), token, request) == receipt,
            "settling an accepted operation changed its idempotent publish receipt")
    require(h.http("GET", publication_route, token) == current,
            "settling an old acceptance changed current publication history")
