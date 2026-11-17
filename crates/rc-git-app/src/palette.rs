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
    comment: Hsla,
    link: Hsla,
    green: Hsla,
    yellow: Hsla,
    blue: Hsla,
    magenta: Hsla,
    cyan: Hsla,
    selection: Hsla,
}

impl Palette {
    /// The palette of the theme active in `cx`.
    #[must_use]
    pub fn current(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            foreground: theme.foreground,
            muted: theme.muted_foreground,
            comment: crate::theme::tertiary(cx),
            link: theme.link,
            green: theme.green,
            yellow: theme.yellow,
            blue: theme.blue,
            magenta: theme.magenta,
            cyan: theme.cyan,
            selection: theme.selection,
        }
    }

    /// The color text of `class` is drawn in: keywords magenta, strings green, functions blue,
    /// types and properties cyan, constants and numbers yellow, comments in the tertiary text
    /// tone.
    #[must_use]
    pub fn color(&self, class: Class) -> Hsla {
        match class {
            Class::Attribute | Class::Constant | Class::Number => self.yellow,
            Class::Comment => self.comment,
            Class::Emphasis | Class::Keyword | Class::Strong => self.magenta,
            Class::Function => self.blue,
            Class::Heading | Class::Property | Class::Type => self.cyan,
            Class::Link => self.link,
            Class::Operator | Class::Punctuation | Class::Variable => self.foreground,
            Class::String => self.green,
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

    /// A link: an import's hover underline.
    #[must_use]
    pub fn link(&self) -> Hsla {
        self.link
    }

    /// The selection's background, drawn translucent over the text.
    #[must_use]
    pub fn selection(&self) -> Hsla {
        self.selection
    }

    /// A find match's background, drawn translucent over the text.
    #[must_use]
    pub fn find(&self) -> Hsla {
        self.yellow
    }
}
