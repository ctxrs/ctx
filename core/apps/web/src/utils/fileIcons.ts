// File extension to icon mapping, based on Zed's icon system
// Maps file extensions to icon names (without .svg extension)

const FILE_ICON_SVGS = import.meta.glob("../assets/file-icons/*.svg", {
  eager: true,
  query: "?raw",
  import: "default",
}) as Record<string, string>;

const svgToDataUrl = (svg: string) => `data:image/svg+xml,${encodeURIComponent(svg)}`;

const FILE_ICON_DATA_URLS: Record<string, string> = {};

for (const [path, svg] of Object.entries(FILE_ICON_SVGS)) {
  const name = path.split("/").pop()?.replace(/\.svg$/, "");
  if (!name) continue;
  FILE_ICON_DATA_URLS[name] = svgToDataUrl(svg);
}

const FALLBACK_ICON_URL = FILE_ICON_DATA_URLS.file ?? "";

const FILE_EXTENSION_TO_ICON: Record<string, string> = {
  // astro
  astro: "astro",

  // audio
  aac: "audio",
  flac: "audio",
  m4a: "audio",
  mka: "audio",
  mp3: "audio",
  ogg: "audio",
  opus: "audio",
  wav: "audio",
  wma: "audio",
  wv: "audio",

  // backup
  bak: "backup",

  // bicep
  bicep: "bicep",

  // bun
  lockb: "bun",

  // c
  c: "c",
  h: "c",

  // cairo
  cairo: "cairo",

  // code
  handlebars: "code",
  metadata: "code",
  rkt: "code",
  scm: "code",

  // coffeescript
  coffee: "coffeescript",

  // cpp
  "c++": "cpp",
  "h++": "cpp",
  cc: "cpp",
  cpp: "cpp",
  cxx: "cpp",
  hh: "cpp",
  hpp: "cpp",
  hxx: "cpp",
  inl: "cpp",
  ixx: "cpp",

  // crystal
  cr: "crystal",
  ecr: "crystal",

  // csharp
  cs: "file",

  // csproj
  csproj: "file",

  // css
  css: "css",
  pcss: "css",
  postcss: "css",

  // cue
  cue: "file",

  // dart
  dart: "dart",

  // diff
  diff: "diff",

  // document
  doc: "book",
  docx: "book",
  mdx: "book",
  odp: "book",
  ods: "book",
  odt: "book",
  pdf: "book",
  ppt: "book",
  pptx: "book",
  rtf: "book",
  txt: "book",
  xls: "book",
  xlsx: "book",

  // elixir
  eex: "elixir",
  ex: "elixir",
  exs: "elixir",
  heex: "elixir",

  // elm
  elm: "elm",

  // erlang
  erl: "erlang",
  escript: "erlang",
  hrl: "erlang",
  xrl: "erlang",
  yrl: "erlang",

  // eslint
  eslintrc: "eslint",

  // font
  otf: "font",
  ttf: "font",
  woff: "font",
  woff2: "font",

  // fsharp
  fs: "fsharp",

  // fsproj
  fsproj: "file",

  // gleam
  gleam: "gleam",

  // go
  go: "go",
  mod: "go",
  work: "go",

  // graphql
  gql: "graphql",
  graphql: "graphql",
  graphqls: "graphql",

  // haskell
  hs: "haskell",

  // hcl
  hcl: "hcl",

  // html
  htm: "html",
  html: "html",

  // image
  avif: "image",
  bmp: "image",
  gif: "image",
  heic: "image",
  heif: "image",
  ico: "image",
  j2k: "image",
  jfif: "image",
  jp2: "image",
  jpeg: "image",
  jpg: "image",
  jxl: "image",
  png: "image",
  psd: "image",
  qoi: "image",
  svg: "image",
  tiff: "image",
  webp: "image",

  // java
  java: "java",

  // javascript
  cjs: "javascript",
  js: "javascript",
  mjs: "javascript",

  // json
  json: "code",
  jsonc: "code",

  // julia
  jl: "julia",

  // kdl
  kdl: "kdl",

  // kotlin
  kt: "kotlin",

  // lock
  lock: "lock",

  // log
  log: "info",

  // lua
  lua: "lua",

  // luau
  luau: "luau",

  // markdown
  markdown: "book",
  md: "book",

  // metal
  metal: "metal",

  // nim
  nim: "nim",

  // nix
  nix: "nix",

  // ocaml
  ml: "ocaml",
  mli: "ocaml",

  // odin
  odin: "odin",

  // php
  php: "php",

  // prettier
  prettierrc: "prettier",

  // prisma
  prisma: "prisma",

  // puppet
  pp: "puppet",

  // python
  py: "python",

  // r
  r: "r",
  R: "r",

  // react
  cjsx: "react",
  ctsx: "react",
  jsx: "react",
  mjsx: "react",
  mtsx: "react",
  tsx: "react",

  // roc
  roc: "roc",

  // ruby
  rb: "ruby",

  // rust
  rs: "rust",

  // sass
  sass: "sass",
  scss: "sass",

  // scala
  scala: "scala",
  sc: "scala",

  // settings
  conf: "settings",
  ini: "settings",
  yaml: "settings",
  yml: "settings",

  // solidity
  sol: "file",

  // storage
  accdb: "database",
  csv: "database",
  dat: "database",
  db: "database",
  dbf: "database",
  dll: "database",
  fmp: "database",
  fp7: "database",
  frm: "database",
  gdb: "database",
  ib: "database",
  ldf: "database",
  mdb: "database",
  mdf: "database",
  myd: "database",
  myi: "database",
  pdb: "database",
  RData: "database",
  rdata: "database",
  sav: "database",
  sdf: "database",
  sql: "database",
  sqlite: "database",
  tsv: "database",

  // stylelint
  stylelintrc: "javascript",

  // surrealql
  surql: "surrealql",

  // svelte
  svelte: "html",

  // swift
  swift: "swift",

  // tcl
  tcl: "tcl",

  // template
  hbs: "html",
  plist: "html",
  xml: "html",

  // terminal
  bash: "terminal",
  fish: "terminal",
  nu: "terminal",
  ps1: "terminal",
  sh: "terminal",
  zsh: "terminal",

  // terraform
  tf: "terraform",
  tfvars: "terraform",

  // toml
  toml: "toml",

  // typescript
  cts: "typescript",
  mts: "typescript",
  ts: "typescript",

  // v
  v: "v",
  vsh: "v",
  vv: "v",

  // vcs/git
  gitattributes: "git",
  gitignore: "git",
  gitkeep: "git",
  gitmodules: "git",

  // vbproj
  vbproj: "file",

  // video
  avi: "video",
  m4v: "video",
  mkv: "video",
  mov: "video",
  mp4: "video",
  webm: "video",
  wmv: "video",

  // vs_sln
  sln: "file",

  // vs_suo
  suo: "file",

  // vue
  vue: "vue",

  // vyper
  vy: "vyper",
  vyi: "vyper",

  // wgsl
  wgsl: "wgsl",

  // zig
  zig: "zig",
};

/**
 * Get the icon name for a given file path or filename
 * @param path - File path or filename
 * @returns Icon name (without .svg extension) or 'file' as default
 */
export function getFileIcon(path: string): string {
  if (!path) return "file";

  // Extract filename from path
  const filename = path.split(/[\\/]/).pop() || path;

  // Get extension (everything after the last dot, lowercased)
  const lastDotIndex = filename.lastIndexOf(".");
  if (lastDotIndex === -1) {
    // No extension, check for special filenames
    const lower = filename.toLowerCase();
    if (lower.startsWith("dockerfile")) return "docker";
    if (lower === "podfile") return "ruby";
    if (lower === "procfile") return "heroku";
    return "file";
  }

  const extension = filename.slice(lastDotIndex + 1).toLowerCase();
  return FILE_EXTENSION_TO_ICON[extension] || "file";
}

/**
 * Get the full icon path for a given file
 * @param path - File path or filename
 * @returns Path to the icon SVG file
 */
export function getFileIconSrc(path: string): string {
  const iconName = getFileIcon(path);
  return FILE_ICON_DATA_URLS[iconName] ?? FALLBACK_ICON_URL;
}
