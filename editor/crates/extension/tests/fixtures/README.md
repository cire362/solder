# Zed extensions, for tests

Real extensions, so the host is tested against code built the way every Zed
extension is: a WebAssembly component made with `zed_extension_api`. Each is
`extension.wasm` and `extension.toml` as Zed's catalog serves them. The tests
give them a scripted world; nothing is downloaded.

One for each shape the API has had; the versions in between differ from
these by what the compiler checks. Vue's is here as the extension that sets
up a server it does not bring: it adds its plugin to the TypeScript server. Ruby's
is here for its debug adapter, `rdbg`. The Postgres context server is here as an
extension that brings one, and reads the user's settings to start it.

| folder | version | built for API | license | source |
| --- | --- | --- | --- | --- |
| `pest` | 0.1.1 | 0.0.1 | MIT | https://github.com/pest-parser/zed-pest |
| `nginx` | 0.0.3 | 0.0.6 | Apache-2.0 | https://github.com/d1y/nginx-zed |
| `terraform` | 0.1.9 | 0.1.0 | Apache-2.0 | https://github.com/zed-extensions/terraform |
| `ledger` | 0.2.0 | 0.3.0 | Apache-2.0 | https://github.com/mrkstwrt/zed-ledger |
| `html` | 0.3.2 | 0.7.0 | Apache-2.0 | https://github.com/zed-industries/zed/tree/main/extensions/html |
| `vue` | 0.4.0 | 0.7.0 | Apache-2.0 | https://github.com/zed-extensions/vue |
| `ruby` | 0.16.21 | 0.7.0 | Apache-2.0 | https://github.com/zed-extensions/ruby |
| `postgres-context-server` | 0.0.5 | 0.7.0 | Apache-2.0 | https://github.com/zed-extensions/postgres-context-server |
