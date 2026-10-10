//! The context servers the agent can use: Model Context Protocol servers
//! named in `settings.json`. Each is a program the editor starts, or one
//! somewhere else that is reached over HTTP, and gives the model tools of
//! its own. What a server has to read (its resources) is two more tools:
//! one that lists them and one that reads one. Its prompts are for the
//! user: texts the server writes to be sent as a task.
//!
//! Nothing starts with the editor. Servers are started when an agent task
//! begins, and stay for the next one. A tool of a server runs outside the
//! agent's sandbox, as the server sees fit, so the agent asks before the
//! first call of each tool (`agent_task.rs`).

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use ai::{
    mcp::{Launch, Prompt, Remote, Resource, Server, Tool},
    tools::ToolSpec,
};
use futures::FutureExt;
use gpui::{App, AppContext, Context, Entity, Global, SharedString, Task};

use crate::{
    extension_store::ExtensionStore,
    settings::{ContextServer, ServerCommand, Settings},
};

/// How long a server may take to answer its greeting. One started with
/// `npx` downloads itself the first time.
const START: Duration = Duration::from_secs(90);
const LIST: Duration = Duration::from_secs(20);
/// How long one call of a tool may take.
pub const CALL: Duration = Duration::from_secs(120);

pub enum State {
    Stopped,
    Starting,
    Running(Arc<Server>, Offer),
    Failed(SharedString),
}

/// What a running server has, as it said when it started.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Offer {
    pub tools: Vec<Tool>,
    pub prompts: Vec<Prompt>,
    pub resources: Vec<Resource>,
}

/// A prompt of a running server, for the user to choose.
#[derive(Clone)]
pub struct ServerPrompt {
    pub server: Arc<Server>,
    pub server_name: String,
    pub prompt: Prompt,
}

impl ServerPrompt {
    /// The text the server writes from what it was told. Blocking.
    pub fn text(&self, told: &[(String, String)]) -> Result<String, String> {
        self.server.prompt(&self.prompt.name, told, CALL)
    }
}

/// What a tool the agent is offered does at its server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Calls the server's tool of that name.
    Tool,
    /// Lists what the server has to read.
    Resources,
    /// Reads one of those, by its address.
    Read,
}

pub struct Entry {
    pub name: String,
    /// The extension that brings it, for one that is not the user's own
    /// command.
    pub extension: Option<String>,
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
    /// The tool's name at the server, or what it does there in words.
    pub tool: String,
    pub kind: Kind,
}

impl McpTool {
    /// Runs it: the answer as text, and whether it failed. Blocking.
    pub fn run(&self, input: serde_json::Value) -> (String, bool) {
        let ran = match self.kind {
            Kind::Tool => self.server.call(&self.tool, input, CALL),
            Kind::Resources => self.server.resources(LIST).map(|resources| {
                let lines: Vec<String> = resources
                    .iter()
                    .map(|r| match (r.name == r.uri, r.description.is_empty()) {
                        (true, true) => r.uri.clone(),
                        (true, false) => format!("{}: {}", r.uri, r.description),
                        (false, true) => format!("{} ({})", r.uri, r.name),
                        (false, false) => format!("{} ({}): {}", r.uri, r.name, r.description),
                    })
                    .collect();
                match lines.is_empty() {
                    true => ("It has nothing to read now".to_string(), false),
                    false => (lines.join("\n"), false),
                }
            }),
            Kind::Read => match input["uri"].as_str() {
                Some(uri) => self.server.read(uri, CALL).map(|text| (text, false)),
                None => Ok(("Give the address of what to read as uri".into(), true)),
            },
        };
        ran.unwrap_or_else(|error| (error, true))
    }

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

    /// Every context server there is to use, by name: what the settings
    /// say about it, and the extension that brings it, if one does. A
    /// server of the settings that has a command or an address is the
    /// user's own, even
    /// under a name an extension also uses. One of an extension needs no
    /// entry in the settings; an entry may turn it off or give it what it
    /// reads.
    pub fn listed(cx: &App) -> Vec<(String, ContextServer, Option<String>)> {
        let mut listed: BTreeMap<String, (ContextServer, Option<String>)> = cx
            .try_global::<Settings>()
            .map(|settings| settings.context_servers.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|(name, config)| (name, (config, None)))
            .collect();
        let brought = ExtensionStore::try_global(cx)
            .map(|store| store.read(cx).context_servers())
            .unwrap_or_default();
        for (extension, server) in brought {
            let entry = listed.entry(server).or_default();
            if entry.0.program().is_none() && entry.0.address().is_none() {
                entry.1 = Some(extension);
            }
        }
        listed
            .into_iter()
            .map(|(name, (config, extension))| (name, config, extension))
            .collect()
    }

    /// Brings the list in line with the settings and the extensions. A
    /// server that is gone, turned off or changed is stopped; the rest
    /// keep running.
    fn sync(&mut self, cx: &App) {
        let wanted: Vec<(String, ContextServer, Option<String>)> = Self::listed(cx)
            .into_iter()
            .filter(|(_, config, _)| config.enabled)
            .collect();
        self.servers.retain(|entry| {
            wanted.iter().any(|(name, config, extension)| {
                *name == entry.name && *config == entry.config && *extension == entry.extension
            })
        });
        for (name, config, extension) in wanted {
            if !self.servers.iter().any(|entry| entry.name == name) {
                self.servers.push(Entry {
                    name,
                    extension,
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
                let (name, mut config, root) =
                    (entry.name.clone(), entry.config.clone(), root.to_path_buf());
                let env = self.env.clone();
                // A server of an extension: the extension says how to
                // start it, and may install it first.
                let asked = entry.extension.clone().and_then(|extension| {
                    let store = ExtensionStore::try_global(cx)?;
                    Some(store.update(cx, |store, cx| {
                        store.context_server_command(&extension, &name, cx)
                    }))
                });
                let background = cx.background_executor().clone();
                let started = cx.background_executor().spawn(async move {
                    if let Some(asked) = asked {
                        let command = asked.await?;
                        config.command = Some(ServerCommand::Table {
                            path: command.command,
                            args: command.args,
                            env: command.env.into_iter().collect(),
                        });
                        config.args.clear();
                    }
                    // Starting waits for the server's answer: on a thread
                    // that may, not on the one that waited above.
                    background
                        .spawn(async move {
                            let server = match config.address() {
                                Some(url) => {
                                    let remote = Remote {
                                        url: url.to_string(),
                                        headers: config.headers.clone().into_iter().collect(),
                                    };
                                    Server::connect(&remote, START)?
                                }
                                None => Server::start(&launch(&config, &root, env)?, START)?,
                            };
                            let offer = Offer {
                                tools: server.tools(LIST)?,
                                // A server that fails at these still has
                                // its tools.
                                prompts: server.prompts(LIST).unwrap_or_default(),
                                resources: server.resources(LIST).unwrap_or_default(),
                            };
                            Ok::<_, String>((Arc::new(server), offer))
                        })
                        .await
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
                            Ok((server, offer)) => State::Running(server, offer),
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
            let State::Running(server, offer) = &entry.state else {
                continue;
            };
            let mut offered: Vec<(String, &str, serde_json::Value, String, Kind)> = offer
                .tools
                .iter()
                .map(|tool| {
                    (
                        tool.name.clone(),
                        tool.description.as_str(),
                        tool.schema.clone(),
                        tool.name.clone(),
                        Kind::Tool,
                    )
                })
                .collect();
            // What the server has to read, as two tools of the editor's
            // making: the model looks through the list and reads what it
            // needs, and the user is asked as for any other tool.
            if server.has_resources {
                offered.push((
                    "list_resources".into(),
                    "Lists what this server has to read, each with the address to read it by",
                    serde_json::json!({ "type": "object", "properties": {} }),
                    "list resources".into(),
                    Kind::Resources,
                ));
                offered.push((
                    "read_resource".into(),
                    "Reads one of the things this server has, by its address",
                    serde_json::json!({
                        "type": "object",
                        "properties": { "uri": { "type": "string" } },
                        "required": ["uri"],
                    }),
                    "read resource".into(),
                    Kind::Read,
                ));
            }
            for (called, description, parameters, tool, kind) in offered {
                let name = tool_name(&entry.name, &called);
                // Two long names cut to the same one, or a tool of the
                // server's own called as one of these: the first stands.
                if out.iter().any(|known| known.spec.name == name) {
                    continue;
                }
                out.push(McpTool {
                    spec: ToolSpec {
                        name,
                        description: format!("{description} (from {})", entry.name),
                        parameters,
                    },
                    server: server.clone(),
                    server_name: entry.name.clone(),
                    tool,
                    kind,
                });
            }
        }
        out
    }

    /// The prompts of the servers that run.
    pub fn prompts(&self) -> Vec<ServerPrompt> {
        let mut out = Vec::new();
        for entry in &self.servers {
            if let State::Running(server, offer) = &entry.state {
                out.extend(offer.prompts.iter().map(|prompt| ServerPrompt {
                    server: server.clone(),
                    server_name: entry.name.clone(),
                    prompt: prompt.clone(),
                }));
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
