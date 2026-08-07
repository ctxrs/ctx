#!/usr/bin/env python3
"""Deterministic subprocess fixture for the Linux PTY observer tests."""

from __future__ import annotations

import json
import os
import signal
import socket
import sys
import time
from pathlib import Path


def controls() -> None:
    size = os.get_terminal_size(0)
    tty = [os.isatty(descriptor) for descriptor in (0, 1, 2)]
    os.write(
        1,
        (
            f"geometry:{size.columns}x{size.lines} tty:{tty}\n"
            "\x1b[31mred 日本語✓\x1b[0m\n"
            "\x1b]0;fixture title\x07"
            "\x1b[?25lcontrol\x1b[?25h\n"
        ).encode("utf-8"),
    )
    os.write(2, b"stderr\n")


def environment() -> None:
    names = [
        "HOME",
        "XDG_CONFIG_HOME",
        "CTX_DATA_ROOT",
        "TMPDIR",
        "CTX_FIXTURE_VALUE",
    ]
    print(json.dumps({name: os.environ.get(name) for name in names}, sort_keys=True))
    print(f"ambient-secret:{os.environ.get('CLI_UX_AMBIENT_SECRET', 'absent')}")


def socket_denied() -> None:
    probes = [
        ("unix", lambda: socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)),
        ("socketpair", socket.socketpair),
    ]
    for name, create in probes:
        try:
            descriptor = create()
        except OSError as error:
            print(f"{name}:{error.errno}")
        else:
            if isinstance(descriptor, tuple):
                for item in descriptor:
                    item.close()
            else:
                descriptor.close()
            print(f"{name}:open")


def local_unix() -> None:
    root = Path(os.environ["CTX_DATA_ROOT"])
    socket_path = root / "fixture.sock"
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        server.bind(str(socket_path))
        server.listen(1)
        client.connect(str(socket_path))
        accepted, _ = server.accept()
        try:
            client.sendall(b"local")
            print(f"unix:{accepted.recv(5).decode('ascii')}")
        finally:
            accepted.close()
    finally:
        client.close()
        server.close()

    families = [
        ("inet-stream", socket.AF_INET, socket.SOCK_STREAM),
        ("inet-dgram", socket.AF_INET, socket.SOCK_DGRAM),
        ("inet6-stream", socket.AF_INET6, socket.SOCK_STREAM),
    ]
    if hasattr(socket, "AF_NETLINK"):
        families.append(("netlink", socket.AF_NETLINK, socket.SOCK_RAW))
    for name, family, kind in families:
        try:
            descriptor = socket.socket(family, kind)
        except OSError as error:
            print(f"{name}:{error.errno}")
        else:
            descriptor.close()
            print(f"{name}:open")


def orphan(pid_file: Path) -> None:
    ready_reader, ready_writer = os.pipe()
    child = os.fork()
    if child == 0:
        os.close(ready_reader)
        os.setsid()
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        for descriptor in (0, 1, 2):
            try:
                os.close(descriptor)
            except OSError:
                pass
        os.write(ready_writer, b"1")
        os.close(ready_writer)
        time.sleep(30)
        os._exit(0)
    os.close(ready_writer)
    try:
        if os.read(ready_reader, 1) != b"1":
            raise RuntimeError("orphan did not become ready")
    finally:
        os.close(ready_reader)
    pid_file.write_text(f"{child}\n", encoding="ascii")
    print(f"spawned:{child}", flush=True)


def exited_orphan() -> None:
    child = os.fork()
    if child == 0:
        os.setsid()
        time.sleep(0.05)
        os._exit(0)
    print(f"exited:{child}", flush=True)


def main() -> None:
    mode = sys.argv[1]
    if mode == "controls":
        controls()
    elif mode == "environment":
        environment()
    elif mode == "stdin":
        print(f"stdin:{sys.stdin.read()!r}")
    elif mode == "exit":
        print("failure", flush=True)
        raise SystemExit(7)
    elif mode == "signal":
        print("signaled", flush=True)
        os.kill(os.getpid(), signal.SIGTERM)
    elif mode == "timeout":
        print("waiting", flush=True)
        time.sleep(30)
    elif mode == "invalid-utf8":
        os.write(1, b"\xff")
    elif mode == "socket-denied":
        socket_denied()
    elif mode == "local-unix":
        local_unix()
    elif mode == "orphan":
        orphan(Path(sys.argv[2]))
    elif mode == "exited-orphan":
        exited_orphan()
    else:
        raise SystemExit(f"unknown fixture mode: {mode}")


if __name__ == "__main__":
    main()
