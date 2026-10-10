//! Small monochrome icons, embedded in the app. Paths belong to the
//! asset source, never to the user's machine; drawing inherits the
//! surrounding text color and follows the interface font size.

use std::borrow::Cow;

use gpui::{App, AssetSource, IntoElement, RenderOnce, SharedString, Svg, Window, prelude::*, svg};

use crate::layout::{Item, Panel};

pub struct Assets;

const PICTURES: &[(&str, &[u8])] = &[
    (
        "icons/arrow-down.svg",
        include_bytes!("../assets/icons/phosphor/arrow-down.svg"),
    ),
    (
        "icons/arrow-left.svg",
        include_bytes!("../assets/icons/phosphor/arrow-left.svg"),
    ),
    (
        "icons/arrow-right.svg",
        include_bytes!("../assets/icons/phosphor/arrow-right.svg"),
    ),
    (
        "icons/arrow-up.svg",
        include_bytes!("../assets/icons/phosphor/arrow-up.svg"),
    ),
    (
        "icons/arrows-clockwise.svg",
        include_bytes!("../assets/icons/phosphor/arrows-clockwise.svg"),
    ),
    (
        "icons/arrows-left-right.svg",
        include_bytes!("../assets/icons/phosphor/arrows-left-right.svg"),
    ),
    (
        "icons/arrows-out.svg",
        include_bytes!("../assets/icons/phosphor/arrows-out.svg"),
    ),
    (
        "icons/bug.svg",
        include_bytes!("../assets/icons/phosphor/bug.svg"),
    ),
    (
        "icons/chat-circle.svg",
        include_bytes!("../assets/icons/phosphor/chat-circle.svg"),
    ),
    (
        "icons/check.svg",
        include_bytes!("../assets/icons/phosphor/check.svg"),
    ),
    (
        "icons/code.svg",
        include_bytes!("../assets/icons/phosphor/code.svg"),
    ),
    (
        "icons/copy.svg",
        include_bytes!("../assets/icons/phosphor/copy.svg"),
    ),
    (
        "icons/cursor-text.svg",
        include_bytes!("../assets/icons/phosphor/cursor-text.svg"),
    ),
    (
        "icons/database.svg",
        include_bytes!("../assets/icons/phosphor/database.svg"),
    ),
    (
        "icons/dots-three.svg",
        include_bytes!("../assets/icons/phosphor/dots-three.svg"),
    ),
    (
        "icons/eye.svg",
        include_bytes!("../assets/icons/phosphor/eye.svg"),
    ),
    (
        "icons/eye-slash.svg",
        include_bytes!("../assets/icons/phosphor/eye-slash.svg"),
    ),
    (
        "icons/file.svg",
        include_bytes!("../assets/icons/phosphor/file.svg"),
    ),
    (
        "icons/floppy-disk.svg",
        include_bytes!("../assets/icons/phosphor/floppy-disk.svg"),
    ),
    (
        "icons/folder.svg",
        include_bytes!("../assets/icons/phosphor/folder.svg"),
    ),
    (
        "icons/gauge.svg",
        include_bytes!("../assets/icons/phosphor/gauge.svg"),
    ),
    (
        "icons/gear.svg",
        include_bytes!("../assets/icons/phosphor/gear.svg"),
    ),
    (
        "icons/git-branch.svg",
        include_bytes!("../assets/icons/phosphor/git-branch.svg"),
    ),
    (
        "icons/keyboard.svg",
        include_bytes!("../assets/icons/phosphor/keyboard.svg"),
    ),
    (
        "icons/list.svg",
        include_bytes!("../assets/icons/phosphor/list.svg"),
    ),
    (
        "icons/magnifying-glass.svg",
        include_bytes!("../assets/icons/phosphor/magnifying-glass.svg"),
    ),
    (
        "icons/minus.svg",
        include_bytes!("../assets/icons/phosphor/minus.svg"),
    ),
    (
        "icons/palette.svg",
        include_bytes!("../assets/icons/phosphor/palette.svg"),
    ),
    (
        "icons/pause.svg",
        include_bytes!("../assets/icons/phosphor/pause.svg"),
    ),
    (
        "icons/play.svg",
        include_bytes!("../assets/icons/phosphor/play.svg"),
    ),
    (
        "icons/plugs.svg",
        include_bytes!("../assets/icons/phosphor/plugs.svg"),
    ),
    (
        "icons/plus.svg",
        include_bytes!("../assets/icons/phosphor/plus.svg"),
    ),
    // Drawn here: the set has none that says "kept in its place".
    (
        "icons/push-pin.svg",
        include_bytes!("../assets/icons/own/push-pin.svg"),
    ),
    (
        "icons/puzzle-piece.svg",
        include_bytes!("../assets/icons/phosphor/puzzle-piece.svg"),
    ),
    (
        "icons/robot.svg",
        include_bytes!("../assets/icons/phosphor/robot.svg"),
    ),
    (
        "icons/sparkle.svg",
        include_bytes!("../assets/icons/phosphor/sparkle.svg"),
    ),
    (
        "icons/spinner-gap.svg",
        include_bytes!("../assets/icons/phosphor/spinner-gap.svg"),
    ),
    (
        "icons/stack.svg",
        include_bytes!("../assets/icons/phosphor/stack.svg"),
    ),
    (
        "icons/stop.svg",
        include_bytes!("../assets/icons/phosphor/stop.svg"),
    ),
    (
        "icons/table.svg",
        include_bytes!("../assets/icons/phosphor/table.svg"),
    ),
    (
        "icons/terminal-window.svg",
        include_bytes!("../assets/icons/phosphor/terminal-window.svg"),
    ),
    (
        "icons/text-indent.svg",
        include_bytes!("../assets/icons/phosphor/text-indent.svg"),
    ),
    (
        "icons/trash.svg",
        include_bytes!("../assets/icons/phosphor/trash.svg"),
    ),
    (
        "icons/warning-circle.svg",
        include_bytes!("../assets/icons/phosphor/warning-circle.svg"),
    ),
    (
        "icons/x.svg",
        include_bytes!("../assets/icons/phosphor/x.svg"),
    ),
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(PICTURES
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(PICTURES
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

#[derive(IntoElement)]
pub struct Icon(&'static str);

pub fn draw(name: &'static str) -> Icon {
    Icon(name)
}

impl Icon {
    fn picture(self, window: &Window) -> Svg {
        // Svg paints only an explicit color. Read the parent's text style
        // during layout, after its active and hover styles are applied.
        svg()
            .path(format!("icons/{}.svg", self.0))
            .size(crate::theme::text(14.))
            .text_color(window.text_style().color)
            .flex_none()
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        self.picture(window)
    }
}

pub fn panel(panel: Panel) -> &'static str {
    match panel {
        Panel::Files => "folder",
        Panel::Search => "magnifying-glass",
        Panel::Git => "git-branch",
        Panel::Services => "stack",
        Panel::Database => "database",
        Panel::Api => "plugs",
        Panel::Ai => "sparkle",
        Panel::Extensions => "puzzle-piece",
        Panel::ExtensionViews => "puzzle-piece",
        Panel::Chat => "chat-circle",
        Panel::Agent => "robot",
        Panel::Terminal => "terminal-window",
        Panel::Debug => "bug",
        Panel::Response => "arrows-left-right",
        Panel::Results => "table",
    }
}

pub fn named(name: &str) -> Option<&'static str> {
    PICTURES
        .iter()
        .filter_map(|(path, _)| path.strip_prefix("icons/")?.strip_suffix(".svg"))
        .find(|icon| *icon == name)
}

pub fn item(item: &Item) -> &'static str {
    match item {
        Item::Project => "folder",
        Item::File => "file",
        Item::Branch => "git-branch",
        Item::Position => "cursor-text",
        Item::Indent => "text-indent",
        Item::Language => "code",
        Item::Problems => "warning-circle",
        Item::Activity => "spinner-gap",
        Item::Connection => "database",
        Item::Extensions => "plugs",
        Item::Plugins => "puzzle-piece",
        Item::Performance => "gauge",
        Item::Button { .. } => "gear",
    }
}

pub fn button(label: &str) -> Option<&'static str> {
    Some(match label {
        "Run" | "Start" | "Query" | "Send" => "play",
        "Stop" => "stop",
        "Pause" => "pause",
        "Save" | "Save all" => "floppy-disk",
        "Copy" => "copy",
        "Remove" | "Delete" => "trash",
        "Close" | "Cancel" => "x",
        "New" | "Add" => "plus",
        "Refresh" | "Restart" => "arrows-clockwise",
        "Settings" => "gear",
        "Install" => "puzzle-piece",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn embedded_icons_render_without_files_or_fonts(cx: &mut gpui::TestAppContext) {
        let assets = Assets;
        for path in assets.list("icons/").unwrap() {
            let bytes = assets.load(&path).unwrap().unwrap();
            let image = gpui::Image::from_bytes(gpui::ImageFormat::Svg, bytes.to_vec());
            let rendered = cx
                .update(|cx| image.to_image_data(cx.svg_renderer()))
                .unwrap();
            assert!(
                rendered
                    .as_bytes(0)
                    .unwrap()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 0),
                "empty {path}"
            );
        }
        assert_eq!(assets.load("../file.svg").unwrap(), None);
        assert_eq!(assets.load("icons/missing.svg").unwrap(), None);
        for name in Panel::ALL
            .into_iter()
            .map(panel)
            .chain(Item::ALL.iter().map(item))
            .chain(
                ["Run", "Stop", "Save", "Copy", "Remove", "Install"]
                    .into_iter()
                    .filter_map(button),
            )
        {
            assert!(
                assets.load(&format!("icons/{name}.svg")).unwrap().is_some(),
                "missing {name}"
            );
        }
        assert_eq!(button("A command of my own"), None);
    }

    #[gpui::test]
    fn an_icon_copies_its_parents_text_color(cx: &mut gpui::TestAppContext) {
        use gpui::{Context, Hsla, Render, div};
        use std::{cell::Cell, rc::Rc};

        struct Parent(Hsla, Rc<Cell<bool>>);
        #[derive(IntoElement)]
        struct Probe(Hsla, Rc<Cell<bool>>);

        impl Render for Parent {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .text_color(self.0)
                    .child(Probe(self.0, self.1.clone()))
            }
        }
        impl RenderOnce for Probe {
            fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
                let mut icon = draw("file").picture(window);
                assert_eq!(
                    icon.style().text.as_ref().and_then(|text| text.color),
                    Some(self.0)
                );
                self.1.set(true);
                div()
            }
        }
        let theme = crate::theme::Theme::dark();
        for color in [theme.fg, theme.fg_subtle, theme.accent_fg] {
            let drawn = Rc::new(Cell::new(false));
            let (_, visual) = cx.add_window_view(|_, _| Parent(color, drawn.clone()));
            visual.run_until_parked();
            assert!(drawn.get());
        }
    }
}
