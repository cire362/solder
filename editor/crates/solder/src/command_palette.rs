use gpui::{
    Action, AnyElement, Context, DismissEvent, Entity, FocusHandle, SharedString, Task, Window,
    div, prelude::*,
};

use crate::{
    fuzzy,
    picker::{Picker, PickerDelegate, format_binding, highlighted_text},
    plugin_store::PluginStore,
    theme::{ActiveTheme, UI_FONT_SIZE},
};

struct Command {
    name: String,
    run: Run,
    binding: Option<String>,
}

enum Run {
    Action(Box<dyn Action>),
    /// A running plugin's command: the plugin and the command's id.
    Plugin(String, String),
}

pub struct CommandPalette {
    plugins: Entity<PluginStore>,
    commands: Vec<Command>,
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
    /// Where the palette was opened from. Actions run there.
    target: Option<FocusHandle>,
}

/// Actions that only make sense inside a specific widget.
const HIDDEN_NAMESPACES: &[&str] = &["picker", "search_bar", "inline_edit"];

/// `editor::MoveLineUp` becomes `Editor: Move line up`.
pub fn humanize_action_name(name: &str) -> String {
    let (namespace, action) = name.rsplit_once("::").unwrap_or(("", name));
    let mut words = String::new();
    for (i, c) in action.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            words.push(' ');
            words.extend(c.to_lowercase());
        } else {
            words.push(c);
        }
    }
    let namespace = namespace.replace('_', " ");
    let mut ns = namespace.chars();
    let namespace = match ns.next() {
        Some(first) => first.to_uppercase().chain(ns).collect::<String>(),
        None => String::new(),
    };
    if namespace.is_empty() {
        words
    } else {
        format!("{namespace}: {words}")
    }
}

impl CommandPalette {
    /// Must be called while the palette's target still has focus, so the
    /// available actions and their key bindings are the target's.
    pub fn new(plugins: Entity<PluginStore>, window: &mut Window, cx: &mut gpui::App) -> Self {
        let mut commands: Vec<Command> = window
            .available_actions(cx)
            .into_iter()
            .filter(|a| {
                let ns = a.name().split("::").next().unwrap_or("");
                !HIDDEN_NAMESPACES.contains(&ns)
            })
            .map(|action| Command {
                name: humanize_action_name(action.name()),
                binding: window
                    .highest_precedence_binding_for_action(action.as_ref())
                    .map(|b| format_binding(&b)),
                run: Run::Action(action),
            })
            .collect();
        commands.extend(
            plugins
                .read(cx)
                .commands()
                .into_iter()
                .map(|(plugin, id, title)| Command {
                    name: title,
                    run: Run::Plugin(plugin, id),
                    binding: None,
                }),
        );
        commands.sort_by(|a, b| a.name.cmp(&b.name));
        commands.dedup_by(|a, b| a.name == b.name);
        Self {
            plugins,
            commands,
            matches: Vec::new(),
            selected: 0,
            target: window.focused(cx),
        }
    }
}

impl PickerDelegate for CommandPalette {
    fn placeholder(&self) -> SharedString {
        "Run a command".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Context<Picker<Self>>) {
        self.selected = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let query = query.trim();
        self.matches = if query.is_empty() {
            (0..self.commands.len()).map(|i| (i, Vec::new())).collect()
        } else {
            fuzzy::fuzzy_match(
                self.commands.iter().map(|c| c.name.as_str()),
                query,
                100,
                false,
            )
            .into_iter()
            .map(|m| {
                let positions = fuzzy::positions(&self.commands[m.index].name, query, false);
                (m.index, positions)
            })
            .collect()
        };
        self.selected = 0;
        Task::ready(())
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((ix, _)) = self.matches.get(self.selected) else {
            return;
        };
        if let Some(target) = &self.target {
            window.focus(target);
        }
        match &self.commands[*ix].run {
            // Dispatches to whatever has focus now, which is the target again.
            Run::Action(action) => window.dispatch_action(action.boxed_clone(), cx),
            Run::Plugin(plugin, id) => self.plugins.read(cx).run_command(plugin, id),
        }
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (command_ix, positions) = &self.matches[ix];
        let command = &self.commands[*command_ix];
        div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .text_size(UI_FONT_SIZE)
            .child(
                div()
                    .truncate()
                    .text_color(theme.fg)
                    .child(highlighted_text(
                        &command.name,
                        positions,
                        theme.fg,
                        theme.accent,
                    )),
            )
            .children(command.binding.clone().map(|b| {
                div()
                    .flex_none()
                    .px_1p5()
                    .rounded(theme.shape.token)
                    .bg(theme.bg_sunken)
                    .text_size(crate::theme::text(11.))
                    .text_color(theme.fg_muted)
                    .child(b)
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::humanize_action_name;

    #[test]
    fn humanizes_names() {
        assert_eq!(
            humanize_action_name("editor::MoveLineUp"),
            "Editor: Move line up"
        );
        assert_eq!(
            humanize_action_name("project_search::Deploy"),
            "Project search: Deploy"
        );
    }
}
