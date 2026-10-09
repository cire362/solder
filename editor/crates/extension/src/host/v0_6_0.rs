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
start!(bindings, sets_up_others, {
    fn debug_adapter(
        &self,
        store: &mut Store<State>,
        launch: &DebugLaunch,
        worktree: Resource<Worktree>,
    ) -> wasmtime::Result<Result<DebugAdapter, String>> {
        use api::dap;
        // The extension first turns what the editor knows (a program and
        // where to run it) into the adapter's own configuration, then says
        // how to start the adapter for it.
        let config = dap::DebugConfig {
            label: launch.label.clone(),
            adapter: launch.adapter.clone(),
            request: dap::DebugRequest::Launch(dap::LaunchRequest {
                program: launch.program.clone(),
                cwd: launch.cwd.clone(),
                args: launch.args.clone(),
                envs: launch.env.clone(),
            }),
            stop_on_entry: None,
        };
        let scenario = match self.call_dap_config_to_scenario(&mut *store, &config)? {
            Ok(scenario) => scenario,
            Err(error) => return Ok(Err(error)),
        };
        let definition = dap::DebugTaskDefinition {
            label: scenario.label,
            adapter: scenario.adapter.clone(),
            config: scenario.config,
            tcp_connection: scenario.tcp_connection,
        };
        let binary =
            self.call_get_dap_binary(store, &scenario.adapter, &definition, None, worktree)?;
        Ok(binary.map(|binary| DebugAdapter {
            command: binary.command,
            args: binary.arguments,
            env: binary.envs,
            cwd: binary.cwd,
            connection: binary
                .connection
                .map(|tcp| (std::net::Ipv4Addr::from(tcp.host), tcp.port, tcp.timeout)),
            attach: matches!(
                binary.request_args.request,
                dap::StartDebuggingRequestArgumentsRequest::Attach
            ),
            configuration: binary.request_args.configuration,
        }))
    }
});

impl api::dap::Host for State {
    /// Fills in what an adapter that listens leaves open: this machine,
    /// and a port nothing else has.
    fn resolve_tcp_template(
        &mut self,
        template: api::dap::TcpArgumentsTemplate,
    ) -> Result<api::dap::TcpArguments, String> {
        let port = match template.port {
            Some(port) => port,
            None => std::net::TcpListener::bind("127.0.0.1:0")
                .and_then(|listener| listener.local_addr())
                .map(|address| address.port())
                .map_err(|e| format!("No free port for the debug adapter: {e}"))?,
        };
        Ok(api::dap::TcpArguments {
            port,
            host: template
                .host
                .unwrap_or(u32::from(std::net::Ipv4Addr::LOCALHOST)),
            timeout: template.timeout,
        })
    }
}
