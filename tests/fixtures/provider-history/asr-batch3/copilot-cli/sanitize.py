#!/usr/bin/env python3
"""Sanitize the scheduler capture; excludes byte-offset/literal-prompt claims."""
import argparse
import datetime
import json
import re
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('source', type=Path)
parser.add_argument('destination', type=Path)
parser.add_argument('--private-prefix', required=True)
args = parser.parse_args()
rows = [json.loads(line) for line in args.source.read_text().splitlines()]
uuids = {}
pattern = re.compile(r'\b[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\b')
start = datetime.datetime.fromisoformat(rows[0]['timestamp'].replace('Z', '+00:00'))
shift = datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc) - start

def uuid(match):
    original = match.group()
    return uuids.setdefault(original, f'00000000-0000-4000-8000-{len(uuids)+1:012d}')

def sanitize(value):
    if isinstance(value, str):
        value = pattern.sub(uuid, value.replace(args.private_prefix, '/synthetic/copilot'))
        # Embedded wall-clock strings are not used as identity or schedule syntax.
        return re.sub(r'<current_datetime>.*?</current_datetime>',
                      '<current_datetime>2026-01-01T00:00:10Z</current_datetime>', value)
    if isinstance(value, list):
        return [sanitize(item) for item in value]
    if isinstance(value, dict):
        result = {key: sanitize(item) for key, item in value.items()}
        for key in ['timestamp', 'startTime']:
            if isinstance(value.get(key), str):
                result[key] = (datetime.datetime.fromisoformat(value[key].replace('Z', '+00:00')) + shift).isoformat(timespec='milliseconds').replace('+00:00', 'Z')
        if isinstance(value.get('sessionStartTime'), int):
            result['sessionStartTime'] = value['sessionStartTime'] + round(shift.total_seconds() * 1000)
        return result
    return value

output = []
for row in rows:
    row = sanitize(row)
    if row['type'] == 'system.message':
        # Native prompt template is not needed for this structural capture.
        row['data']['content'] = 'Synthetic system instructions.'
        for block in row['data'].get('contentBlocks', []):
            block['content'] = 'Synthetic system instruction block.'
    output.append(json.dumps(row, ensure_ascii=False, separators=(',', ':')) + '\n')
args.destination.write_text(''.join(output))
