# Desktop (Tauri v2) — Linux build dependencies

This app is built with **Tauri v2** and uses the system WebView stack on Linux (GTK + WebKitGTK). That means Linux builds depend on distro packages being present.

This document is the single source of truth for Linux system dependencies.

## Supported distros

- **Ubuntu/Debian (required)**: fully supported and what CI/dev should target.
- Fedora / Arch: best-effort notes (package names vary by version).

Linux sandbox product support:

- App install/update is expected to work broadly on Linux where the packaged app itself runs.
- Managed sandbox bootstrap is only a launch-quality commitment on Ubuntu/Debian.
- On non-Ubuntu/Debian Linux, `sandbox` requires a compatible runtime to already be present.
- Local Linux may stage sandbox-runtime downloads in the background on app launch.
- Remote Linux may stage sandbox-runtime downloads after daemon connect, and those downloads happen on the remote Linux machine.
- Privileged activation for sandbox runtime happens only when the user selects `sandbox`.
- User-facing setup failures should describe sandbox preparation in product language rather than exposing raw runtime commands or env vars.

## Canonical dev launch paths

Launch the desktop app from source:

```bash
pnpm -C core desktop:dev
```

Build the desktop app from source:

```bash
pnpm -C core desktop:build
```

For daemon and web workbench debugging without the desktop shell, run the daemon with Cargo and the web app with Vite as documented in the repository root README.

## Ubuntu / Debian (apt)

Install system dependencies:

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  pkg-config \
  curl \
  file \
  libgtk-3-dev \
  libglib2.0-dev \
  libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libssl-dev \
  libasound2-dev
```

On older Debian/Ubuntu releases where `libwebkit2gtk-4.1-dev` is unavailable, install `libwebkit2gtk-4.0-dev` instead. On releases that do not package Ayatana app indicators, use `libappindicator3-dev`.

These packages provide:

- Build tooling: `build-essential`, `pkg-config`, `curl`, `file`
- GTK + GLib headers: `libgtk-3-dev`, `libglib2.0-dev`
- WebKitGTK headers: `libwebkit2gtk-4.0-dev` (or `libwebkit2gtk-4.1-dev` on newer Ubuntu)
- App indicator (tray) headers: `libayatana-appindicator3-dev` (or `libappindicator3-dev` on some Ubuntu/Debian)
- SVG/icon support: `librsvg2-dev`
- TLS headers (common Rust deps): `libssl-dev`
- Audio headers (STT): `libasound2-dev`

## STT (Speech-to-Text) on Linux

Desktop STT uses `tauri-plugin-stt` (Vosk + cpal). For Linux shipping builds we **bundle the Vosk runtime**
(`libvosk.so`) with the app, and **download models on-demand** at runtime.

### What gets bundled vs downloaded

- Bundled on build (desktop, when `--features stt` is enabled):
  - Linux: `libvosk.so`
  - macOS: `libvosk.dylib`
  - Windows: `libvosk.dll` plus runtime deps (`libstdc++-6.dll`, `libwinpthread-1.dll`, `libgcc_s_seh-1.dll`)
- Downloaded on-demand (first use): Vosk model zip(s), stored under the app data dir (e.g. `<app_data_dir>/vosk-models`)

### Build behavior

When building the desktop binary with `--features stt` on desktop targets, the build script:

- Downloads a pinned Vosk release zip (currently `v0.3.42`) into `core/apps/desktop/src-tauri/target/vendor/vosk/...`
- Verifies the zip via SHA-256
- Extracts the Vosk runtime library for the platform
- Stages it into:
  - `core/apps/desktop/src-tauri/bin/libvosk.so` (so Tauri bundling includes it)
  - `core/apps/desktop/src-tauri/target/<profile>/libvosk.so` (so local runs can find it)

The produced desktop binary embeds a runtime search path that includes:

- `$ORIGIN` (local runs with `libvosk.so` next to the binary)
- `$ORIGIN/../lib/ctx/bin` and `$ORIGIN/../lib/ctx` (typical Tauri Linux bundle layouts)

On Windows, the build enables delay-loading for `libvosk.dll` and the app configures a DLL search path on startup.

### Offline / custom builds

If you want to supply your own `libvosk.so` (or build without network access), set:

```bash
CTX_VOSK_LIBVOSK_PATH=/absolute/path/to/libvosk.so
```

## Fedora (dnf) — optional

Package names can vary between Fedora releases.

Typical install set:

```bash
sudo dnf install -y \
  gcc gcc-c++ make pkgconf-pkg-config \
  glib2-devel gtk3-devel webkit2gtk4.0-devel \
  libappindicator-gtk3-devel \
  librsvg2-devel \
  openssl-devel
```

If `libappindicator-gtk3-devel` is unavailable on your Fedora version, look for an Ayatana alternative (e.g. `libayatana-appindicator-gtk3-devel`) or skip it if you don’t need tray integration.

## Arch (pacman) — optional

```bash
sudo pacman -S --needed \
  base-devel pkgconf \
  glib2 gtk3 webkit2gtk \
  libappindicator-gtk3 \
  librsvg \
  openssl
```

## Troubleshooting (symptom → fix)

Most failures show up as `pkg-config` errors during `cargo`/`tauri` builds.

| Symptom (snippet) | Missing package(s) (Ubuntu) |
| --- | --- |
| `Package glib-2.0 was not found in the pkg-config search path` | `libglib2.0-dev`, `pkg-config` |
| `Package gtk+-3.0 was not found` | `libgtk-3-dev`, `pkg-config` |
| `Package webkit2gtk-4.0 was not found` | `libwebkit2gtk-4.0-dev` |
| `Package webkit2gtk-4.1 was not found` | `libwebkit2gtk-4.1-dev` |
| `Package ayatana-appindicator3-0.1 was not found` | `libayatana-appindicator3-dev` (or `libappindicator3-dev` on some distros) |
| `Package librsvg-2.0 was not found` | `librsvg2-dev` |

If you’re still blocked, verify what `pkg-config` can see:

```bash
pkg-config --exists glib-2.0 && echo "glib OK"
pkg-config --exists gtk+-3.0 && echo "gtk OK"
pkg-config --exists webkit2gtk-4.0 && echo "webkit OK"
```

## Next steps (repo)

From the repo root:

```bash
pnpm -C core install --frozen-lockfile
pnpm -C core desktop:dev
```
