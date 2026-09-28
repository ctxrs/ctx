// Authored model of the released 1.x/2.0 config mutation, not a native capture.
const fs = require('node:fs');
const path = require('node:path');
const [operation, root] = process.argv.slice(2);
const config = path.join(root, 'config.toml');
const before = fs.existsSync(config) ? fs.readFileSync(config, 'utf8') : '';
const section = (name) => new RegExp(`^\\[${name}\\][^\\n]*\\n([\\s\\S]*?)(?=^\\[|$(?![\\s\\S]))`, 'm').exec(before)?.[1] ?? '';
if (operation === 'read') {
  const explicit = /^mode\s*=\s*["'](auto|manual)["']/m.exec(section('indexing'))?.[1];
  const mode = explicit ?? (/^enabled\s*=\s*false/m.test(section('daemon')) ? 'manual' : 'auto');
  process.stdout.write(JSON.stringify({ schema_version: 1, indexing: { mode }, read_only: true }) + '\n');
} else if (operation === 'disable') {
  let current = '';
  let wrote = false;
  const lines = before.split('\n').map((line) => {
    if (line.startsWith('[')) current = line.trim();
    if (current === '[daemon]' && /^enabled\s*=/.test(line)) return null;
    if (current === '[indexing]' && /^mode\s*=/.test(line)) { wrote = true; return 'mode = "manual"'; }
    return line;
  }).filter((line) => line !== null);
  if (!wrote) lines.push('[indexing]', 'mode = "manual"', '');
  fs.mkdirSync(root, { recursive: true });
  fs.writeFileSync(config, lines.join('\n'));
} else { throw new Error('unknown legacy fixture operation'); }
