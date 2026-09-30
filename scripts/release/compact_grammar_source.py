"""Stage pinned compact tables for the explicit legacy-compatible release profile.

Normal Cargo/Bazel builds use upstream tables. This bridge trades parse speed for
size; it changes neither features nor the locked dependency graph.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import tarfile
import tomllib

from compact_grammar import compact
from compact_grammar_verify import compare


PINS = Path(__file__).with_name("compact_grammar_pins.json")
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
NOTICE = "NOTICE.ctx-compact-grammar"
PROFILE = {"opt_level": "s", "lto": "thin", "codegen_units": 1}


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def pins():
    return json.loads(PINS.read_text(encoding="utf-8"))


def crates(manifest):
    result = {}
    for parser in manifest["parsers"]:
        result.setdefault(parser["crate"], []).append(parser)
    return result


def identity():
    return {
        "kind": "ctx-compact-grammar-v1",
        "manifest_sha256": digest(PINS),
        "scripts_sha256": {
            name: digest(Path(__file__).with_name(name)) for name in (
                "compact_grammar.py", "compact_grammar_verify.py", "compact_grammar_source.py",
            )
        },
        "dense_state_count": 2,
        "release_profile": PROFILE,
    }


def verify_files(root, expected):
    for relative, checksum in expected.items():
        path = root / relative
        if path.is_symlink() or digest(path) != checksum:
            raise ValueError(f"pinned grammar source mismatch: {relative}")


def tree_digest(root):
    files = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("grammar source contains a symlink")
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError("grammar source is not a regular file")
        if path != root / NOTICE:
            files[path.relative_to(root).as_posix()] = digest(path)
    return hashlib.sha256(canonical(files).encode()).hexdigest()


def verify_lock(lock_file, manifest):
    packages = tomllib.loads(lock_file.read_text(encoding="utf-8"))["package"]
    for spec in [manifest["runtime"], *manifest["parsers"]]:
        name, version = spec["crate"].rsplit("-", 1)
        matches = [p for p in packages if p["name"] == name and p["version"] == version]
        if (len(matches) != 1 or matches[0].get("source") != REGISTRY
                or matches[0].get("checksum") != spec["archive_sha256"]):
            raise ValueError(f"compact grammar lock pin mismatch: {spec['crate']}")


def notice(record, transform):
    return (
        "ctx modifies only generated parse-table storage for the explicit\n"
        "legacy-compatible release profile. State 0 and 1 remain dense; all\n"
        "state/action IDs and lookahead order are preserved. Compact lookup can\n"
        "be slower. Upstream manifests, scanners, headers and licenses are unchanged.\n"
        "Source-tree SHA-256 is over canonical sorted relative-path/file-SHA-256 JSON\n"
        "with a trailing newline, excluding this notice.\n"
        + canonical({"transform": transform, "package": record})
    )


def config_path(output, notify_source=None):
    return output / ("config-notify.toml" if notify_source is not None else "config.toml")


def prepare(archives, lock_file, output, notify_source=None):
    output = output.resolve()
    manifest = pins()
    verify_lock(lock_file, manifest)
    selected = crates(manifest)
    for crate, parsers in selected.items():
        if digest(archives / f"{crate}.crate") != parsers[0]["archive_sha256"]:
            raise ValueError(f"compact grammar archive checksum mismatch: {crate}")
    output.mkdir()  # Fresh build-owned staging only; never a Cargo registry tree.
    records, paths = [], []
    transform = identity()
    for crate, parsers in selected.items():
        with tarfile.open(archives / f"{crate}.crate", "r:gz") as bundle:
            names = set()
            for member in bundle.getmembers():
                path = Path(member.name)
                if (not member.isfile() or path.is_absolute() or ".." in path.parts
                        or len(path.parts) < 2 or path.parts[0] != crate
                        or path.as_posix() in names or path.name == NOTICE):
                    raise ValueError("unexpected compact grammar archive member")
                names.add(path.as_posix())
            bundle.extractall(output, filter="data")
        source = output / crate
        name, version = crate.rsplit("-", 1)
        record = {
            "name": name, "version": version, "source": REGISTRY,
            "archive_sha256": parsers[0]["archive_sha256"],
            "original_source_sha256": tree_digest(source), "parsers": [],
        }
        # Check all pins before rewriting either parser in a multi-parser crate.
        for parser in parsers:
            verify_files(source, parser["files"])
        for parser in parsers:
            path = source / parser["parser"]
            original = path.read_bytes().decode("utf-8")
            transformed, stats = compact(original)
            proof = compare(original, transformed)
            path.write_bytes(transformed.encode("utf-8"))
            record["parsers"].append({
                "id": parser["id"], "path": parser["parser"],
                "original_sha256": parser["files"][parser["parser"]],
                "transformed_sha256": digest(path), **stats, **proof,
            })
        record["source_sha256"] = tree_digest(source)
        (source / NOTICE).write_text(notice(record, transform), encoding="utf-8")
        records.append(record)
        paths.append(str(source.resolve()))
    provenance = {"transform": transform, "packages": records}
    (output / "provenance.json").write_text(canonical(provenance), encoding="utf-8")
    config_path(output).write_text("paths = " + json.dumps(paths) + "\n", encoding="utf-8")
    if notify_source is not None:
        config_path(output, notify_source).write_text(
            "paths = " + json.dumps([*paths, str(notify_source.resolve())]) + "\n",
            encoding="utf-8",
        )
    return output


def bind_metadata(metadata, output):
    """Bind advisory identities to selected staged bytes, with portable provenance."""
    manifest = pins()
    provenance = json.loads((output / "provenance.json").read_text(encoding="utf-8"))
    if provenance["transform"] != identity():
        raise ValueError("compact grammar transform identity mismatch")
    selected = crates(manifest)
    records = provenance["packages"]
    if [f"{r['name']}-{r['version']}" for r in records] != list(selected):
        raise ValueError("compact grammar staged package set mismatch")
    for record in records:
        crate = f"{record['name']}-{record['version']}"
        source = output / crate
        packages = [p for p in metadata["packages"] if p["name"] == record["name"]]
        if (len(packages) != 1 or packages[0]["version"] != record["version"]
                or packages[0]["source"] is not None
                or Path(packages[0]["manifest_path"]).resolve() != (source / "Cargo.toml").resolve()):
            raise ValueError(f"Cargo did not select compact grammar source: {crate}")
        if (record["archive_sha256"] != selected[crate][0]["archive_sha256"]
                or record["source"] != REGISTRY or tree_digest(source) != record["source_sha256"]):
            raise ValueError(f"compact grammar staged source mismatch: {crate}")
        if (source / NOTICE).read_text(encoding="utf-8") != notice(record, provenance["transform"]):
            raise ValueError(f"compact grammar notice mismatch: {crate}")
        packages[0]["source"] = REGISTRY
    runtime = manifest["runtime"]
    name, version = runtime["crate"].rsplit("-", 1)
    packages = [p for p in metadata["packages"] if p["name"] == name]
    if (len(packages) != 1 or packages[0]["version"] != version or packages[0]["source"] != REGISTRY):
        raise ValueError("compact grammar runtime pin mismatch")
    verify_files(Path(packages[0]["manifest_path"]).parent, runtime["files"])
    metadata["source_transforms"] = provenance


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("archives")
    stage = commands.add_parser("prepare")
    stage.add_argument("--archives", required=True, type=Path)
    stage.add_argument("--lock-file", required=True, type=Path)
    stage.add_argument("--output", required=True, type=Path)
    stage.add_argument("--notify-source", type=Path)
    args = parser.parse_args()
    if args.command == "archives":
        for crate, parsers in crates(pins()).items():
            print(crate, parsers[0]["archive_sha256"])
    else:
        print(prepare(args.archives, args.lock_file, args.output, args.notify_source))


if __name__ == "__main__":
    main()
