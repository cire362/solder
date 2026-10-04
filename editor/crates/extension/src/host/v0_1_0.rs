//! The world of API 0.1.0: HTTP, releases by tag, slash commands and
//! documentation indexing arrive.

use super::*;

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/since_v0.1.0",
        world: "extension",
        with: {
            "worktree": super::super::Worktree,
            "key-value-store": super::super::KeyValueStore,
            "zed:extension/http-client/http-response-stream": super::super::ResponseStream,
        },
    });
}

use bindings::zed::extension as api;

types_only!(api: common, lsp, slash_command);
tools!(api, releases_by_tag);
http!(api);
worktree!(bindings, located);
key_value_store!(bindings);
world_functions!(bindings);
start!(bindings);
