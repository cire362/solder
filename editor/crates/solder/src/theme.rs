//! Colors mirror the website tokens in `src/app/globals.css`: a zinc base with a
//! single molten-orange accent. Shape rule carried over from the site:
//! panels 16px, controls 8px, inline tokens 6px.

use gpui::{App, Global, Hsla, WindowAppearance, px, rgb, rgba};
use syntax::HighlightKind;

#[derive(Clone)]
pub struct Theme {
    pub bg: Hsla,
    pub bg_elev: Hsla,
    pub bg_sunken: Hsla,
    pub fg: Hsla,
    pub fg_muted: Hsla,
    pub fg_subtle: Hsla,
    pub line: Hsla,
    pub accent: Hsla,
    pub accent_soft: Hsla,
    pub accent_fg: Hsla,
    pub selection: Hsla,
    pub active_line: Hsla,
    pub search_match: Hsla,
    pub search_active: Hsla,
    pub bracket: Hsla,
    pub error: Hsla,
    pub warning: Hsla,
    pub syntax: SyntaxColors,
}

#[derive(Clone)]
pub struct SyntaxColors {
    pub keyword: Hsla,
    pub string: Hsla,
    pub function: Hsla,
    pub r#type: Hsla,
    pub comment: Hsla,
    pub number: Hsla,
    pub punctuation: Hsla,
    pub variable: Hsla,
    pub tag: Hsla,
}

impl SyntaxColors {
    pub fn color(&self, kind: HighlightKind) -> Hsla {
        use HighlightKind::*;
        match kind {
            Keyword | Operator | Attribute => self.keyword,
            String => self.string,
            Function => self.function,
            Type | Property => self.r#type,
            Comment => self.comment,
            Number | Constant => self.number,
            Punctuation => self.punctuation,
            Variable => self.variable,
            Tag => self.tag,
        }
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg: rgb(0x0c0c0e).into(),
            bg_elev: rgb(0x141417).into(),
            bg_sunken: rgb(0x0f0f12).into(),
            fg: rgb(0xededef).into(),
            fg_muted: rgb(0xa1a1aa).into(),
            fg_subtle: rgb(0x71717a).into(),
            line: rgba(0xffffff12).into(),
            accent: rgb(0xe8743f).into(),
            accent_soft: rgba(0xe8743f1a).into(),
            accent_fg: rgb(0x1a0c05).into(),
            selection: rgba(0xe8743f38).into(),
            active_line: rgba(0xffffff08).into(),
            search_match: rgba(0xe8743f26).into(),
            search_active: rgba(0xe8743f59).into(),
            bracket: rgba(0xffffff1f).into(),
            error: rgb(0xf87171).into(),
            warning: rgb(0xfbbf24).into(),
            syntax: SyntaxColors {
                keyword: rgb(0xe8743f).into(),
                string: rgb(0xd9b8a3).into(),
                function: rgb(0xf4f4f5).into(),
                r#type: rgb(0xd4d4d8).into(),
                comment: rgb(0x62626c).into(),
                number: rgb(0xf0a57f).into(),
                punctuation: rgb(0x8b8b95).into(),
                variable: rgb(0xc4c4cc).into(),
                tag: rgb(0xe8743f).into(),
            },
        }
    }

    pub fn light() -> Self {
        Self {
            bg: rgb(0xfafafa).into(),
            bg_elev: rgb(0xffffff).into(),
            bg_sunken: rgb(0xf1f1f3).into(),
            fg: rgb(0x18181b).into(),
            fg_muted: rgb(0x5b5b66).into(),
            fg_subtle: rgb(0x8a8a94).into(),
            line: rgba(0x18181b17).into(),
            accent: rgb(0xb24a1c).into(),
            accent_soft: rgba(0xb24a1c17).into(),
            accent_fg: rgb(0xfff7f2).into(),
            selection: rgba(0xb24a1c2e).into(),
            active_line: rgba(0x18181b08).into(),
            search_match: rgba(0xb24a1c1f).into(),
            search_active: rgba(0xb24a1c47).into(),
            bracket: rgba(0x18181b1a).into(),
            error: rgb(0xdc2626).into(),
            warning: rgb(0xb45309).into(),
            syntax: SyntaxColors {
                keyword: rgb(0xb24a1c).into(),
                string: rgb(0x87573a).into(),
                function: rgb(0x18181b).into(),
                r#type: rgb(0x3f3f46).into(),
                comment: rgb(0x8f8f99).into(),
                number: rgb(0xa8552b).into(),
                punctuation: rgb(0x71717a).into(),
                variable: rgb(0x3f3f46).into(),
                tag: rgb(0xb24a1c).into(),
            },
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }
}

impl Global for Theme {}

pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

/// Monospace family per platform until Geist Mono is bundled with the app.
pub const CODE_FONT: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "DejaVu Sans Mono"
};

pub const UI_FONT: &str = if cfg!(target_os = "macos") {
    ".SystemUIFont"
} else if cfg!(target_os = "windows") {
    "Segoe UI"
} else {
    "Cantarell"
};

pub const UI_FONT_SIZE: gpui::Pixels = px(12.5);
