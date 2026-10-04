# Zed's HTML extension, for tests

A real extension, so the host is tested against code built the way every
Zed extension is: a WebAssembly component made with `zed_extension_api`.
The tests give it a scripted world; nothing is downloaded.

- `extension.wasm`, `extension.toml`: HTML 0.3.2 as Zed's catalog serves it.
  Apache-2.0, https://github.com/zed-industries/zed/tree/main/extensions/html
