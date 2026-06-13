# tauri-plugin-automation

Enable cross-platform integration tests for Tauri apps using CrabNebula Webdriver for Tauri.

## Installation

**DO NOT** include the automation plugin for production builds.
This kind of instrumentation should only be available for test-only builds.

- Install the plugin crate:

```sh
cd src-tauri
cargo add tauri-plugin-automation
```

- Register the plugin:

Make sure you register the plugin as early as possible using [`tauri::Builder::plugin`](https://docs.rs/tauri/latest/tauri/struct.Builder.html#method.plugin) instead of [`AppHandle#plugin`](https://docs.rs/tauri/latest/tauri/struct.AppHandle.html#method.plugin) so it is ready when your app starts.

ALWAYS use a conditional compilation check to make sure the automation plugin is not added for production builds.

```rust
let mut builder = tauri::Builder::default();
#[cfg(debug_assertions)] // alternatively: #[cfg(feature = "automation")]
{
  builder = builder.plugin(tauri_plugin_automation::init());
}

builder
  .run(tauri::generate_context!())
  .expect("error while running tauri application");
```
