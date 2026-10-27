#!/usr/bin/env python3
"""Deterministically sanitize selected native JSON/JSONL captures, without reserializing.

Only for transcript semantics: byte offsets and acquisition timestamps are not
qualified by this transform. Review input/output independently before publishing.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re

IDENTIFIER = re.compile(
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
    r"|(?:call_|chatcmpl-)[A-Za-z0-9_-]+|\b[0-9a-f]{32,64}\b"
)


def sanitize(source, destination, replacements):
    files = sorted(p for p in source.rglob('*') if p.is_file())
    mapping = dict(replacements)
    for p in files:
        text = p.read_text()
        for match in IDENTIFIER.finditer(str(p.relative_to(source)) + '\n' + text):
            value = match.group()
            digest = hashlib.sha256(('asr-fixture:' + value).encode()).hexdigest()
            if value.startswith(('call_', 'chatcmpl-')):
                prefix = 'call_' if value.startswith('call_') else 'chatcmpl-'
                mapping[value] = prefix + digest[:len(value) - len(prefix)]
            elif '-' not in value:
                mapping[value] = digest[:len(value)]
            else:
                mapping[value] = '-'.join([digest[:8], digest[8:12], digest[12:16], digest[16:20], digest[20:32]])
    pattern = re.compile('|'.join(re.escape(v) for v in sorted(mapping, key=lambda v: (-len(v), v))))
    replace = lambda text: pattern.sub(lambda match: mapping[match.group()], text)
    manifest = []
    for p in files:
        relative = replace(str(p.relative_to(source)))
        data = replace(p.read_bytes().decode()).encode()
        # Validate framing without changing formatting or record order.
        if p.suffix == '.jsonl':
            for line in data.splitlines():
                json.loads(line)
        elif p.suffix == '.json':
            json.loads(data)
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        manifest.append({'path': relative, 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()})
    return manifest


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    parser.add_argument('--replacements', type=Path, required=True,
                        help='Private JSON object mapping reviewed literal values to replacements')
    args = parser.parse_args()
    print(json.dumps(sanitize(args.source, args.destination, json.loads(args.replacements.read_text())), indent=2))
