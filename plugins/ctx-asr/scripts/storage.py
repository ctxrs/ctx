"""One locked append stream per native session; bounded private index sidecar."""

import datetime
import fcntl
import json
import os
import stat

MAX_STATE = 512 * 1024
MAX_STREAM = 16 * 1024 * 1024


def decode(data):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError("duplicate member")
            result[key] = value
        return result

    def invalid(_):
        raise ValueError("non-JSON number")

    return json.loads(data, object_pairs_hook=pairs, parse_constant=invalid)


def encode(value):
    return (json.dumps(value, ensure_ascii=True, allow_nan=False,
                       separators=(",", ":")) + "\n").encode()


def checked(fd, directory=False):
    info = os.fstat(fd)
    expected = stat.S_ISDIR if directory else stat.S_ISREG
    if (not expected(info.st_mode) or info.st_uid != os.getuid()
            or info.st_mode & 0o077 or (not directory and info.st_nlink != 1)):
        raise ValueError("output must be private and owned by this user")
    return info


def open_file(root, name, flags):
    fd = os.open(name, flags | os.O_NOFOLLOW | os.O_NONBLOCK,
                 0o600, dir_fd=root)
    try:
        checked(fd)
    except BaseException:
        os.close(fd)
        raise
    return fd


def capture(base, session, event, parent):
    from projection import project

    os.makedirs(base, mode=0o700, exist_ok=True)
    root = os.open(base, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    lock = stream = None
    try:
        checked(root, directory=True)
        lock = open_file(root, session + ".lock", os.O_CREAT | os.O_RDWR)
        # No retry loop: concurrent capture loses this event rather than
        # delaying the agent. The stream declares this possibility up front.
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        stream = open_file(root, session + ".asr.jsonl",
                           os.O_CREAT | os.O_RDWR | os.O_APPEND)
        size = os.fstat(stream).st_size
        try:
            index = open_file(root, session + ".state.json", os.O_RDONLY)
        except FileNotFoundError:
            if size:
                return
            state = {"seq": 0, "size": 0, "instance": os.urandom(16).hex(),
                     "pathKey": os.urandom(32).hex(), "roots": {},
                     "calls": {}, "children": {}, "requests": {}}
        else:
            try:
                raw = os.read(index, MAX_STATE + 1)
                if len(raw) > MAX_STATE:
                    return
                state = decode(raw)
            finally:
                os.close(index)
        # A crash between the two writes leaves a valid prefix. Never guess
        # identity/revisions from stale state or rewrite complete ASR lines.
        if state["size"] != size or state.get("sealed"):
            return
        observed = datetime.datetime.now(datetime.timezone.utc).isoformat(
            timespec="microseconds").replace("+00:00", "Z")
        rows = project(session, parent, event, state, observed)
        data = b"".join(encode(row) for row in rows)
        if size + len(data) > MAX_STREAM or len(encode(state)) > MAX_STATE - 1024:
            data = encode({"type": "gap", "seq": state["seq"],
                           "id": "r" + str(state["seq"]), "idScope": "capture",
                           "reason": "conversion_loss", "observedAt": observed,
                           "_meta": {"rs.ctx.claude-asr/limit": True}})
            state["seq"] += 1
            state["sealed"] = True
            # Drop the in-memory indexes after sealing, to keep state bounded.
            state["calls"] = state["children"] = state["requests"] = {}
            state["roots"] = {}
        state["size"] = size + len(data)
        if data and os.write(stream, data) != len(data):
            return
        name = session + ".state.tmp"
        index = open_file(root, name, os.O_CREAT | os.O_WRONLY)
        try:
            os.ftruncate(index, 0)
            raw = encode(state)
            if os.write(index, raw) != len(raw):
                return
        finally:
            os.close(index)
        os.replace(name, session + ".state.json", src_dir_fd=root, dst_dir_fd=root)
    finally:
        if stream is not None:
            os.close(stream)
        if lock is not None:
            os.close(lock)
        os.close(root)
