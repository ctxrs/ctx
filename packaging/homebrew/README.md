# Homebrew cask

A Homebrew cask for installing the ctx desktop app on macOS.

```ruby
Casks/ctx.rb
```

The cask installs the signed `.dmg` published to the stable release channel
(`https://api.ctx.rs/functions/v1/releases/stable/latest.json`), selecting the
Apple Silicon or Intel build automatically. The bundled app self-updates, so the
cask is marked `auto_updates true`; `brew upgrade` is only needed when the cask
metadata itself changes.

## Installing

Until the cask is published to a tap, install it directly from a checkout:

```sh
brew install --cask ./packaging/homebrew/Casks/ctx.rb
```

## Publishing to a tap (maintainers)

Homebrew discovers casks through taps, not application repositories. To make
`brew install --cask ctx` work for everyone, publish this file to a tap repo
named `ctxrs/homebrew-tap`:

```sh
brew tap-new ctxrs/tap
cp packaging/homebrew/Casks/ctx.rb "$(brew --repository ctxrs/tap)/Casks/ctx.rb"
# commit and push the tap repo
```

Users then install with:

```sh
brew install --cask ctxrs/tap/ctx
```

## Keeping the cask current

`version` and the per-architecture `sha256` values must match the current
stable release. The `livecheck` block reads `latest_version` from the release
manifest, so `brew livecheck ctx` reports when a newer version is available. To
bump:

1. Read the latest version and checksums from the release manifest:
   ```sh
   curl -fsSL https://api.ctx.rs/functions/v1/releases/stable/latest.json
   ```
   Each platform entry carries the published `sha256` for its `.dmg`.
2. Update `version` and both `sha256` values in `Casks/ctx.rb`.

## Notes

- macOS only. Linux builds ship as an AppImage and are installed with the
  script at `https://ctx.rs/install`; Homebrew casks do not support Linux.
- The `zap` stanza removes the local data directory (`~/.ctx`) and app caches on
  `brew uninstall --zap ctx`.
