//! The context servers the agent can use: Model Context Protocol servers
//! named in `settings.json`. Each is a program the editor starts and that
//! gives the model tools of its own.
//!
//! Nothing starts with the editor. Servers are started when an agent task
//! begins, and stay for the next one. A tool of a server runs outside the
//! agent's sandbox, as the server sees fit, so the agent asks before the
//! first call of each tool (`agent_task.rs`).

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use ai::{
    mcp::{Launch, Server, Tool},
    tools::ToolSpec,
};
use futures::FutureExt;
use gpui::{App, AppContext, Context, Entity, Global, SharedString, Task};

use crate::settings::{ContextServer, Settings};

/// How long a server may take to answer its greeting. One started with
/// `npx` downloads itself the first time.
const START: Duration = Duration::from_secs(90);
const LIST: Duration = Duration::from_secs(20);
/// How long one call of a tool may take.
pub const CALL: Duration = Duration::from_secs(120);

pub enum State {
    Stopped,
    Starting,
    Running(Arc<Server>, Vec<Tool>),
    Failed(SharedString),
}

pub struct Entry {
    pub name: String,
    /// What the settings say about it, to tell when they changed.
    config: ContextServer,
    pub state: State,
    starting: Option<futures::future::Shared<Task<()>>>,
}

/// A tool of a running server, as the agent offers it to the model.
#[derive(Clone)]
pub struct McpTool {
    /// Under a name of its own among the agent's tools.
    pub spec: ToolSpec,
    pub server: Arc<Server>,
    pub server_name: String,
    /// The tool's name at the server.
    pub tool: String,
}

impl McpTool {
    /// The call as the user is shown it before allowing it.
    pub fn shown(&self, input: &serde_json::Value) -> String {
        let mut arguments = input.to_string();
        if arguments.len() > 400 {
            let mut cut = 400;
            while !arguments.is_char_boundary(cut) {
                cut -= 1;
            }
            arguments.truncate(cut);
            arguments.push_str("...");
        }
        format!("{} of {}: {arguments}", self.tool, self.server_name)
    }
}

pub struct McpStore {
    pub servers: Vec<Entry>,
    /// The environment servers start with, in place of the user's shell's:
    /// tests give their own and ask no shell.
    pub env: Option<Vec<(String, String)>>,
}

struct GlobalMcpStore(Entity<McpStore>);

impl Global for GlobalMcpStore {}

/// A name a model may call a tool by: letters, digits, `_` and `-`, and
/// not longer than providers take.
fn tool_name(server: &str, tool: &str) -> String {
    let clean = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let mut name = format!("mcp_{}_{}", clean(server), clean(tool));
    name.truncate(64);
    name
}

/// The program, arguments and environment of a server the settings name:
/// the user's own environment, where Node and the rest are, with the
/// server's variables on top. Blocking the first time: the shell is asked.
fn launch(
    config: &ContextServer,
    root: &Path,
    env: Option<Vec<(String, String)>>,
) -> Result<Launch, String> {
    let Some(program) = config.program() else {
        return Err("It has no command in settings.json".into());
    };
    let mut env: BTreeMap<String, String> = env
        .unwrap_or_else(extension::world::user_env)
        .into_iter()
        .collect();
    env.extend(config.variables());
    Ok(Launch {
        program,
        args: config.arguments(),
        env: env.into_iter().collect(),
        cwd: Some(root.to_path_buf()),
    })
}

impl McpStore {
    pub fn global(cx: &mut App) -> Entity<McpStore> {
        if let Some(store) = cx.try_global::<GlobalMcpStore>() {
            return store.0.clone();
        }
        let store = cx.new(|_| McpStore {
            servers: Vec::new(),
            env: None,
        });
        cx.set_global(GlobalMcpStore(store.clone()));
        store
    }

    /// The store, if anything asked for it yet.
    #[cfg(test)]
    pub fn global_if_any(cx: &App) -> Option<Entity<McpStore>> {
        cx.try_global::<GlobalMcpStore>()
            .map(|store| store.0.clone())
    }

    /// Brings the list in line with the settings. A server that is gone
    /// from them, turned off or changed is stopped; the rest keep running.
    fn sync(&mut self, cx: &App) {
        let wanted: BTreeMap<String, ContextServer> = cx
            .try_global::<Settings>()
            .map(|settings| settings.context_servers.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, config)| config.enabled)
            .collect();
        self.servers
            .retain(|entry| wanted.get(&entry.name) == Some(&entry.config));
        for (name, config) in wanted {
            if !self.servers.iter().any(|entry| entry.name == name) {
                self.servers.push(Entry {
                    name,
                    config,
                    state: State::Stopped,
                    starting: None,
                });
            }
        }
        self.servers.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Starts every server of the settings that is not running, for the
    /// project in `root`. The task ends when each has started or failed.
    pub fn start_all(&mut self, root: &Path, cx: &mut Context<Self>) -> Task<()> {
        self.sync(cx);
        let mut waiting = Vec::new();
        for index in 0..self.servers.len() {
            let entry = &mut self.servers[index];
            if matches!(entry.state, State::Stopped | State::Failed(_)) {
                entry.state = State::Starting;
                let (name, config, root) =
                    (entry.name.clone(), entry.config.clone(), root.to_path_buf());
                let env = self.env.clone();
                let started = cx.background_executor().spawn(async move {
                    let server = Server::start(&launch(&config, &root, env)?, START)?;
                    let tools = server.tools(LIST)?;
                    Ok::<_, String>((Arc::new(server), tools))
                });
                let task = cx.spawn(async move |this, cx| {
                    let started = started.await;
                    this.update(cx, |this, cx| {
                        // The settings may have changed while it started.
                        let Some(entry) = this.servers.iter_mut().find(|e| e.name == name) else {
                            return;
                        };
                        entry.starting = None;
                        entry.state = match started {
                            Ok((server, tools)) => State::Running(server, tools),
                            Err(error) => State::Failed(error.into()),
                        };
                        cx.notify();
                    })
                    .ok();
                });
                self.servers[index].starting = Some(task.shared());
            }
            if let Some(starting) = &self.servers[index].starting {
                waiting.push(starting.clone());
            }
        }
        cx.notify();
        if waiting.is_empty() {
            return Task::ready(());
        }
        cx.background_executor().spawn(async move {
            futures::future::join_all(waiting).await;
        })
    }

    /// The tools of the servers that run, each under a name of its own.
    pub fn tools(&self) -> Vec<McpTool> {
        let mut out: Vec<McpTool> = Vec::new();
        for entry in &self.servers {
            let State::Running(server, tools) = &entry.state else {
                continue;
            };
            for tool in tools {
                let name = tool_name(&entry.name, &tool.name);
                // Two long names cut to the same one: the first stands.
                if out.iter().any(|known| known.spec.name == name) {
                    continue;
                }
                out.push(McpTool {
                    spec: ToolSpec {
                        name,
                        description: format!("{} (from {})", tool.description, entry.name),
                        parameters: tool.schema.clone(),
                    },
                    server: server.clone(),
                    server_name: entry.name.clone(),
                    tool: tool.name.clone(),
                });
            }
        }
        out
    }

    /// The servers that did not start, and why.
    pub fn failures(&self) -> Vec<(String, SharedString)> {
        self.servers
            .iter()
            .filter_map(|entry| match &entry.state {
                State::Failed(why) => Some((entry.name.clone(), why.clone())),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_get_names_a_model_may_call() {
        assert_eq!(tool_name("postgres", "query"), "mcp_postgres_query");
        assert_eq!(
            tool_name("my server", "get/issue.v2"),
            "mcp_my_server_get_issue_v2"
        );
        assert_eq!(tool_name("a", &"x".repeat(100)).len(), 64);
    }
}
