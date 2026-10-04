//! The pictures next to file names, from the icon theme of an extension.
//! With no theme chosen there are none, and rows look as they always did.

use std::{path::Path, sync::Arc};

use extension::IconTheme;
use gpui::{App, Global, Img, img, prelude::*, px};

/// The icon theme in use. `ExtensionStore` sets it from the name in the
/// settings and the extensions that are installed.
#[derive(Default)]
pub struct FileIcons(pub Option<Arc<IconTheme>>);

impl Global for FileIcons {}

fn picture(path: &Arc<Path>) -> Img {
    // Drawn from the file by GPUI, which reads and keeps it off this thread.
    img(path.clone()).size(px(14.)).flex_none()
}

fn theme(cx: &App) -> Option<&Arc<IconTheme>> {
    cx.try_global::<FileIcons>()?.0.as_ref()
}

fn name(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()
}

/// The picture for the file at `path`.
pub fn file(path: &Path, cx: &App) -> Option<Img> {
    theme(cx)?.file(name(path)?).map(picture)
}

/// The picture for the folder at `path`, open or closed.
pub fn folder(path: &Path, open: bool, cx: &App) -> Option<Img> {
    theme(cx)?.folder(name(path)?, open).map(picture)
}
