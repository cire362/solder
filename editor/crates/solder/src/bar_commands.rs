//! A bar button uses the same commands as the palette and key bindings.

use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, SharedString, Task, Window, div, prelude::*,
};

use crate::{
    command_palette::humanize_action_name,
    fuzzy,
    layout::{self, BarEnd, Command, Layout},
    picker::{Picker, PickerDelegate, highlighted_text},
    plugin_store::PluginStore,
    theme::{ActiveTheme, UI_FONT_SIZE},
};

pub fn run(command: &Command, window: &mut Window, cx: &mut App) {
    let action = match command {
        Command::Action(name) => cx.build_action(name, None),
        Command::Args((name, args)) => cx.build_action(name, Some(args.clone())),
        Command::Plugin { plugin, command } => {
            PluginStore::global(cx)
                .read(cx)
                .run_command(plugin, command);
            return;
        }
    };
    if let Ok(action) = action {
        window.dispatch_action(action, cx);
    }
}

pub struct Commands {
    end: BarEnd,
    commands: Vec<(String, Command)>,
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
}

impl Commands {
    pub fn new(end: BarEnd, plugins: Entity<PluginStore>, window: &Window, cx: &App) -> Self {
        let mut commands: Vec<_> = window
            .available_actions(cx)
            .into_iter()
            .filter(|action| {
                !matches!(
                    action.name().split("::").next(),
                    Some("picker" | "search_bar" | "inline_edit")
                )
            })
            .filter(|action| cx.build_action(action.name(), None).is_ok())
            .map(|action| {
                (
                    humanize_action_name(action.name()),
                    Command::Action(action.name().into()),
                )
            })
            .collect();
        commands.extend(
            plugins
                .read(cx)
                .commands()
                .into_iter()
                .map(|(plugin, command, title)| (title, Command::Plugin { plugin, command })),
        );
        commands.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            end,
            commands,
            matches: Vec::new(),
            selected: 0,
        }
    }
}

impl PickerDelegate for Commands {
    fn placeholder(&self) -> SharedString {
        "Choose a command for the button".into()
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
            (0..self.commands.len())
                .map(|ix| (ix, Vec::new()))
                .collect()
        } else {
            fuzzy::fuzzy_match(
                self.commands.iter().map(|(name, _)| name.as_str()),
                query,
                100,
                false,
            )
            .into_iter()
            .map(|m| {
                (
                    m.index,
                    fuzzy::positions(&self.commands[m.index].0, query, false),
                )
            })
            .collect()
        };
        self.selected = 0;
        Task::ready(())
    }

    fn confirm(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some((ix, _)) = self.matches.get(self.selected) else {
            return;
        };
        let (title, command) = &self.commands[*ix];
        // A button is narrow: "Toggle sidebar", not "Workspace: Toggle
        // sidebar". Its tooltip and the file still say which it is.
        let label = title.rsplit_once(": ").map_or(title.as_str(), |(_, l)| l);
        layout::put(
            Layout::get(cx)
                .clone()
                .add_button(label.to_owned(), command.clone(), self.end),
            cx,
        );
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        _: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> AnyElement {
        let (ix, positions) = &self.matches[ix];
        let theme = cx.theme();
        div()
            .w_full()
            .text_size(UI_FONT_SIZE)
            .text_color(theme.fg)
            .child(highlighted_text(
                &self.commands[*ix].0,
                positions,
                theme.fg,
                theme.accent,
            ))
            .into_any_element()
    }
}
