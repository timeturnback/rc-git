//! The one place a highlight class becomes a color, for every surface that draws highlighted
//! text (source mode, fenced code in the markdown preview).

use gpui::{App, Hsla};
use gpui_component::ActiveTheme;
use rc_git_highlight::Class;

/// Highlight colors resolved from the active theme. Build one per paint, so a light/dark switch
/// recolors on the next frame.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    foreground: Hsla,
    muted: Hsla,
    link: Hsla,
    red: Hsla,
    yellow: Hsla,
    blue: Hsla,
    magenta: Hsla,
    cyan: Hsla,
}

impl Palette {
    /// The palette of the theme active in `cx`.
    #[must_use]
    pub fn current(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            foreground: theme.foreground,
            muted: theme.muted_foreground,
            link: theme.link,
            red: theme.red,
            yellow: theme.yellow,
            blue: theme.blue,
            magenta: theme.magenta,
            cyan: theme.cyan,
        }
    }

    /// The color text of `class` is drawn in. Mirrors the `AppKit` app's system-color choices
    /// with the nearest theme color: purple is magenta, orange and brown are yellow, teal is
    /// cyan, indigo is blue.
    #[must_use]
    pub fn color(&self, class: Class) -> Hsla {
        match class {
            Class::Attribute | Class::Constant | Class::Number => self.yellow,
            Class::Comment => self.muted,
            Class::Emphasis | Class::Keyword | Class::Strong => self.magenta,
            Class::Function => self.cyan,
            Class::Heading | Class::Property | Class::Type => self.blue,
            Class::Link => self.link,
            Class::Operator | Class::Punctuation | Class::Variable => self.foreground,
            Class::String => self.red,
        }
    }

    /// Plain text.
    #[must_use]
    pub fn foreground(&self) -> Hsla {
        self.foreground
    }

    /// Secondary text: the line-number gutter, break markers.
    #[must_use]
    pub fn muted(&self) -> Hsla {
        self.muted
    }
}
