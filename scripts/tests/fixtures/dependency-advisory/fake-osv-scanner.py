#!/usr/bin/env python3

import json
import os
from pathlib import Path
import sys


if sys.argv[1:] == ["--version"]:
    print("osv-scanner version: 2.4.0")
    print("osv-scalibr version: fixture")
    raise SystemExit(0)

exit_code = int(os.environ.get("FAKE_OSV_EXIT", "0"))
if exit_code not in {0, 1}:
    raise SystemExit(exit_code)
fixture = Path(os.environ["FAKE_OSV_FIXTURE"])
value = json.loads(fixture.read_text(encoding="utf-8"))
lockfiles = [
    str(Path(sys.argv[index + 1]).resolve())
    for index, argument in enumerate(sys.argv)
    if argument == "-L"
]
if len(lockfiles) != len(value["results"]):
    raise SystemExit(64)
for result, lockfile in zip(value["results"], lockfiles, strict=True):
    result["source"]["path"] = lockfile
print(json.dumps(value))
raise SystemExit(exit_code)
