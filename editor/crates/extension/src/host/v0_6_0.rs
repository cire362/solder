//! The world of API 0.6.0, which 0.7 kept: debug adapters.

use super::*;

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/since_v0.6.0",
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

impl api::dap::Host for State {
    fn resolve_tcp_template(
        &mut self,
        _: api::dap::TcpArgumentsTemplate,
    ) -> Result<api::dap::TcpArguments, String> {
        Err("Solder does not run the debug adapters of extensions".into())
    }
}
