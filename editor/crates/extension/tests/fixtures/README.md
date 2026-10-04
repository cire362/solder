# Zed extensions, for tests

Real extensions, so the host is tested against code built the way every Zed
extension is: a WebAssembly component made with `zed_extension_api`. Each is
`extension.wasm` and `extension.toml` as Zed's catalog serves them. The tests
give them a scripted world; nothing is downloaded.

One for each shape the API has had. The versions in between differ from
these by what the compiler checks.

| folder | version | built for API | license | source |
| --- | --- | --- | --- | --- |
| `pest` | 0.1.1 | 0.0.1 | MIT | https://github.com/pest-parser/zed-pest |
| `nginx` | 0.0.3 | 0.0.6 | Apache-2.0 | https://github.com/d1y/nginx-zed |
| `terraform` | 0.1.9 | 0.1.0 | Apache-2.0 | https://github.com/zed-extensions/terraform |
| `ledger` | 0.2.0 | 0.3.0 | Apache-2.0 | https://github.com/mrkstwrt/zed-ledger |
| `html` | 0.3.2 | 0.7.0 | Apache-2.0 | https://github.com/zed-industries/zed/tree/main/extensions/html |
