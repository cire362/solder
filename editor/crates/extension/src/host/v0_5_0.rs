//! The world of API 0.5.0: the configuration of context servers.

use super::*;

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/since_v0.5.0",
        world: "extension",
        with: {
            "worktree": super::super::Worktree,
            "project": super::super::Project,
            "key-value-store": super::super::KeyValueStore,
            "zed:extension/http-client/http-response-stream": super::super::ResponseStream,
        },
    });
}

use bindings::zed::extension as api;

types_only!(api: common, lsp, slash_command, context_server);
tools!(api, releases_by_tag);
http!(api);
process!(api);
worktree!(bindings, located);
key_value_store!(bindings);
project!(bindings);
world_functions!(bindings);
start!(bindings);
