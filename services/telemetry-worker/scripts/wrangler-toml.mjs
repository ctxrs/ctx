import fs from "node:fs";

export function readWranglerConfig(configPath) {
  return parseWranglerToml(fs.readFileSync(configPath, "utf8"), configPath);
}

export function parseWranglerToml(text, configPath = "wrangler.toml") {
  const root = {
    name: "",
    vars: {},
    triggers: {crons: []},
    ratelimits: [],
    queues: {consumers: [], producers: []},
    env: {},
  };
  let section = [];
  for (const rawLine of text.split(/\r?\n/u)) {
    const line = stripTomlComment(rawLine).trim();
    if (!line) continue;
    const arraySectionMatch = line.match(/^\[\[([^\]]+)\]\]$/u);
    if (arraySectionMatch) {
      section = arraySectionMatch[1].split(".");
      if (section.length === 1 && section[0] === "ratelimits") {
        root.ratelimits.push({});
      } else if (section.length === 2 && section[0] === "queues") {
        const kind = section[1];
        if (kind === "producers" || kind === "consumers") root.queues[kind].push({});
      } else if (section.length === 3 && section[0] === "env" && section[2] === "ratelimits") {
        const environment = ensureEnvironment(root, section[1]);
        environment.ratelimits.push({});
      } else if (
        section.length === 4
        && section[0] === "env"
        && section[2] === "queues"
      ) {
        const environment = ensureEnvironment(root, section[1]);
        const kind = section[3];
        if (kind === "producers" || kind === "consumers") environment.queues[kind].push({});
      }
      continue;
    }
    const sectionMatch = line.match(/^\[([^\]]+)\]$/u);
    if (sectionMatch) {
      section = sectionMatch[1].split(".");
      continue;
    }
    const assignment = line.match(/^([A-Za-z0-9_-]+)\s*=\s*(.+)$/u);
    if (!assignment) continue;
    const key = assignment[1];
    const value = parseTomlScalar(assignment[2].trim());
    if (section.length === 0) {
      if (key === "name" && typeof value === "string") root.name = value;
      continue;
    }
    if (section.length === 1 && section[0] === "vars") {
      root.vars[key] = value;
      continue;
    }
    if (section.length === 1 && section[0] === "triggers" && key === "crons") {
      root.triggers.crons = Array.isArray(value) ? value : [];
      continue;
    }
    if (section.length === 1 && section[0] === "ratelimits") {
      Object.assign(root.ratelimits.at(-1), {[key]: value});
      continue;
    }
    if (section.length === 2 && section[0] === "queues") {
      const kind = section[1];
      if (kind === "producers" || kind === "consumers") {
        Object.assign(root.queues[kind].at(-1), {[key]: value});
      }
      continue;
    }
    if (section.length === 3 && section[0] === "env" && section[2] === "vars") {
      ensureEnvironment(root, section[1]).vars[key] = value;
      continue;
    }
    if (section.length === 3 && section[0] === "env" && section[2] === "triggers" && key === "crons") {
      ensureEnvironment(root, section[1]).triggers.crons = Array.isArray(value) ? value : [];
      continue;
    }
    if (section.length === 3 && section[0] === "env" && section[2] === "ratelimits") {
      Object.assign(ensureEnvironment(root, section[1]).ratelimits.at(-1), {[key]: value});
      continue;
    }
    if (section.length === 4 && section[0] === "env" && section[2] === "queues") {
      const kind = section[3];
      if (kind === "producers" || kind === "consumers") {
        Object.assign(ensureEnvironment(root, section[1]).queues[kind].at(-1), {[key]: value});
      }
      continue;
    }
    if (section.length === 2 && section[0] === "env" && key === "name" && typeof value === "string") {
      ensureEnvironment(root, section[1]).name = value;
    }
  }
  return {...root, path: configPath};
}

function ensureEnvironment(root, name) {
  root.env[name] ??= {
    name: "",
    vars: {},
    triggers: {crons: []},
    ratelimits: [],
    queues: {consumers: [], producers: []},
  };
  return root.env[name];
}

function stripTomlComment(line) {
  let inString = false;
  let quote = "";
  for (let index = 0; index < line.length; index += 1) {
    const character = line[index];
    if ((character === "\"" || character === "'") && line[index - 1] !== "\\") {
      if (!inString) {
        inString = true;
        quote = character;
      } else if (quote === character) {
        inString = false;
        quote = "";
      }
    }
    if (character === "#" && !inString) return line.slice(0, index);
  }
  return line;
}

function parseTomlScalar(value) {
  if (value.startsWith("[") && value.endsWith("]")) {
    try {
      const parsed = JSON.parse(value);
      if (Array.isArray(parsed) && parsed.every((entry) => typeof entry === "string")) return parsed;
    } catch {
      return value;
    }
  }
  if ((value.startsWith("\"") && value.endsWith("\"")) || (value.startsWith("'") && value.endsWith("'"))) {
    return value.slice(1, -1);
  }
  if (value === "true") return true;
  if (value === "false") return false;
  return value;
}
