# OMP 18.0.10 `parentSession` fixtures

The native JSONL files under `native/` are deterministic sanitizations of
isolated sessions written by OMP 18.0.10. `ordinary-id-fork` records the
parent's native session ID. `path-branch` records the absolute native session
filename used by OMP's RPC branch flow.

The sanitization retains native filename structure, JSONL framing, record
order, file sizes, copied entry chains, and the 256-byte title slots. Paths and
identifiers are deterministic fixture values. Tests stage these files in a
temporary OMP session root and substitute that root for the fixture path before
import.
