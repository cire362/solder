//! The world of API 0.0.6: the functions are grouped into interfaces, a
//! server is asked about by its id and can be given settings.

use super::*;

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/since_v0.0.6",
        world: "extension",
        with: {
            "worktree": super::super::Worktree,
        },
    });
}

use bindings::zed::extension as api;

types_only!(api: lsp);
tools!(api);
worktree!(bindings, located);
world_functions!(bindings);
start!(bindings, before_label_details);
