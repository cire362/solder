# Zed's extension API

The interface between a Zed extension and its host, as Zed defines it. The
host in `src/host.rs` is generated from these files, so they must stay
exactly as published.

Each folder is a world: the API as it was from that version until the next
folder's. An extension built for 0.2.3 runs in `since_v0.2.0`; 0.7 added
nothing to `since_v0.6.0`.

All of them come from `zed_extension_api` 0.7.0 on crates.io (its `wit/`).
Apache-2.0 (`LICENSE-APACHE`), https://github.com/zed-industries/zed
