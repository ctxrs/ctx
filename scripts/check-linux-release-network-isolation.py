#!/usr/bin/env python3
"""Fail closed unless a Linux release builder has no external IP path."""

from __future__ import annotations

import argparse
import errno
import fcntl
import ipaddress
import json
import socket
import struct
import sys
from pathlib import Path
from typing import Any


LOOPBACK_TYPE = 772
ALLOWED_INERT_INTERFACES = {
    "tunl0": 768,  # ARPHRD_TUNNEL / built-in IPv4 IPIP fallback device
    "ip6tnl0": 769,  # ARPHRD_TUNNEL6 / built-in IPv6 tunnel fallback device
    "sit0": 776,  # ARPHRD_SIT / built-in IPv6-in-IPv4 fallback device
}
ACTIVE_FLAGS = 0x1 | 0x40 | 0x10000  # IFF_UP | IFF_RUNNING | IFF_LOWER_UP
SAFE_ROUTE_ERRORS = {
    errno.EAFNOSUPPORT,
    errno.EADDRNOTAVAIL,
    errno.EHOSTUNREACH,
    errno.ENETUNREACH,
    errno.ENODEV,
    errno.EPROTONOSUPPORT,
}


class IsolationError(RuntimeError):
    pass


def read_text(path: Path, *, optional: bool = False) -> str | None:
    try:
        return path.read_text(encoding="utf-8").strip()
    except OSError:
        if optional:
            return None
        raise IsolationError(f"required network state is unreadable: {path}")


def ipv4_address(name: str) -> list[str]:
    request = struct.pack("256s", name.encode("utf-8")[:15])
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        try:
            result = fcntl.ioctl(probe.fileno(), 0x8915, request)  # SIOCGIFADDR
        except OSError as error:
            if error.errno in {errno.EADDRNOTAVAIL, errno.ENODEV, errno.ENXIO}:
                return []
            raise IsolationError(f"could not inspect IPv4 address for {name}: {error}") from error
    return [socket.inet_ntoa(result[20:24])]


def ipv6_addresses(path: Path) -> dict[str, list[str]]:
    addresses: dict[str, list[str]] = {}
    text = read_text(path)
    assert text is not None
    for line in text.splitlines():
        fields = line.split()
        if len(fields) != 6:
            raise IsolationError(f"malformed IPv6 interface state: {path}")
        try:
            address = str(ipaddress.IPv6Address(int(fields[0], 16)))
        except ValueError as error:
            raise IsolationError(f"malformed IPv6 address state: {path}") from error
        addresses.setdefault(fields[5], []).append(address)
    return addresses


def routes(path: Path, family: str) -> list[dict[str, str]]:
    text = read_text(path)
    assert text is not None
    result: list[dict[str, str]] = []
    lines = text.splitlines()
    if family == "ipv4":
        if not lines or not lines[0].split() or lines[0].split()[0] != "Iface":
            raise IsolationError(f"malformed IPv4 route state: {path}")
        for line in lines[1:]:
            fields = line.split()
            if len(fields) < 11:
                raise IsolationError(f"malformed IPv4 route state: {path}")
            result.append(
                {"family": family, "interface": fields[0], "destination": fields[1]}
            )
        return result

    for line in lines:
        fields = line.split()
        if len(fields) != 10:
            raise IsolationError(f"malformed IPv6 route state: {path}")
        result.append(
            {"family": family, "interface": fields[9], "destination": fields[0]}
        )
    return result


def route_probe(family: int, target: tuple[Any, ...]) -> str:
    # UDP connect performs a local route selection; it sends no datagram.
    with socket.socket(family, socket.SOCK_DGRAM) as probe:
        try:
            probe.connect(target)
        except OSError as error:
            if error.errno in SAFE_ROUTE_ERRORS:
                return "unreachable"
            return f"indeterminate_errno_{error.errno}"
        return "routable"


def collect_state() -> dict[str, Any]:
    sys_net = Path("/sys/class/net")
    proc_net = Path("/proc/net")
    ipv6_by_name = ipv6_addresses(proc_net / "if_inet6")
    interfaces: list[dict[str, Any]] = []
    try:
        names = [name for _, name in socket.if_nameindex()]
    except OSError as error:
        raise IsolationError(f"could not enumerate network interfaces: {error}") from error
    for name in names:
        interface = sys_net / name
        type_text = read_text(interface / "type")
        flags_text = read_text(interface / "flags")
        operstate = read_text(interface / "operstate")
        carrier_text = read_text(interface / "carrier", optional=True)
        assert type_text is not None and flags_text is not None and operstate is not None
        try:
            interface_type = int(type_text, 10)
            flags = int(flags_text, 0)
            carrier = None if carrier_text is None else int(carrier_text, 10)
        except ValueError as error:
            raise IsolationError(f"malformed sysfs network state for {name}") from error
        interfaces.append(
            {
                "name": name,
                "type": interface_type,
                "operstate": operstate,
                "carrier": carrier,
                "flags": flags,
                "ipv4": ipv4_address(name),
                "ipv6": ipv6_by_name.get(name, []),
            }
        )
    return {
        "interfaces": interfaces,
        "routes": routes(proc_net / "route", "ipv4")
        + routes(proc_net / "ipv6_route", "ipv6"),
        "route_probes": {
            "ipv4": route_probe(socket.AF_INET, ("192.0.2.1", 9)),
            "ipv6": route_probe(socket.AF_INET6, ("2001:db8::1", 9, 0, 0)),
        },
    }


def validate_state(state: dict[str, Any]) -> None:
    interfaces = state.get("interfaces")
    if not isinstance(interfaces, list) or not interfaces:
        raise IsolationError("network interface inventory is missing")
    by_name: dict[str, dict[str, Any]] = {}
    for interface in interfaces:
        if not isinstance(interface, dict) or not isinstance(interface.get("name"), str):
            raise IsolationError("network interface inventory is malformed")
        name = interface["name"]
        if name in by_name:
            raise IsolationError(f"duplicate network interface: {name}")
        by_name[name] = interface

    loopback = by_name.get("lo")
    if loopback is None or loopback.get("type") != LOOPBACK_TYPE:
        raise IsolationError("canonical loopback interface is missing or has the wrong type")
    for family in ("ipv4", "ipv6"):
        addresses = loopback.get(family)
        if not isinstance(addresses, list):
            raise IsolationError(f"loopback {family} address state is malformed")
        for address in addresses:
            try:
                is_loopback = ipaddress.ip_address(address).is_loopback
            except ValueError as error:
                raise IsolationError(f"malformed {family} address on lo: {address}") from error
            if not is_loopback:
                raise IsolationError(f"non-loopback {family} address is assigned to lo: {address}")

    for name, interface in by_name.items():
        if name == "lo":
            continue
        expected_type = ALLOWED_INERT_INTERFACES.get(name)
        if expected_type is None:
            raise IsolationError(f"unsupported non-loopback interface is present: {name}")
        if interface.get("type") != expected_type:
            raise IsolationError(f"inert interface {name} has the wrong link type")
        if interface.get("operstate") != "down":
            raise IsolationError(f"inert interface {name} is not down")
        if interface.get("carrier") not in (None, 0):
            raise IsolationError(f"inert interface {name} has carrier")
        flags = interface.get("flags")
        if not isinstance(flags, int) or flags & ACTIVE_FLAGS:
            raise IsolationError(f"inert interface {name} has active flags")
        for family in ("ipv4", "ipv6"):
            addresses = interface.get(family)
            if not isinstance(addresses, list):
                raise IsolationError(f"inert interface {name} has malformed {family} state")
            if addresses:
                raise IsolationError(f"inert interface {name} has a {family} address")

    known_names = set(by_name)
    route_state = state.get("routes")
    if not isinstance(route_state, list):
        raise IsolationError("route inventory is missing")
    for route in route_state:
        if not isinstance(route, dict) or not isinstance(route.get("interface"), str):
            raise IsolationError("route inventory is malformed")
        interface = route["interface"]
        if interface not in known_names:
            raise IsolationError(f"route references an unknown interface: {interface}")
        if interface != "lo":
            raise IsolationError(f"non-loopback route is present on {interface}")

    probes = state.get("route_probes")
    if not isinstance(probes, dict):
        raise IsolationError("route probe state is missing")
    for family in ("ipv4", "ipv6"):
        if probes.get(family) != "unreachable":
            raise IsolationError(f"{family} route probe is not conclusively unreachable")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixture", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        if args.fixture is None:
            state = collect_state()
        else:
            with args.fixture.open(encoding="utf-8") as source:
                state = json.load(source)
        if not isinstance(state, dict):
            raise IsolationError("network isolation state is malformed")
        validate_state(state)
    except (IsolationError, OSError, json.JSONDecodeError) as error:
        print(f"offline release network isolation failed: {error}", file=sys.stderr)
        return 1
    names = sorted(interface["name"] for interface in state["interfaces"])
    print(f"offline release network isolation ok: interfaces={','.join(names)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
