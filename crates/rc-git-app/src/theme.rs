//! The app's own light and dark themes, embedded from `assets/theme.json` and installed over
//! gpui-component's defaults at launch. Views still read every color from `cx.theme()`; this
//! module only decides which colors the theme holds.

use std::rc::Rc;

use anyhow::Context as _;
use gpui::App;
use gpui_component::{Theme, ThemeConfig, ThemeRegistry};

/// The embedded theme file: one light and one dark `ThemeConfig`.
const THEME_JSON: &str = include_str!("../assets/theme.json");
/// The light theme's name in [`THEME_JSON`].
const LIGHT: &str = "RCGit Light";
/// The dark theme's name in [`THEME_JSON`].
const DARK: &str = "RCGit Dark";

/// The app's two themes, parsed.
pub struct Themes {
    /// Used while the system appearance is light.
    pub light: Rc<ThemeConfig>,
    /// Used while the system appearance is dark.
    pub dark: Rc<ThemeConfig>,
}

/// Parses the embedded theme file.
///
/// # Errors
///
/// Fails when `assets/theme.json` is not a theme set, or lacks the light or the dark theme, or
/// either theme carries the wrong mode.
pub fn parse() -> anyhow::Result<Themes> {
    let mut registry = ThemeRegistry::default();
    registry
        .load_themes_from_str(THEME_JSON)
        .context("parsing crates/rc-git-app/assets/theme.json as a theme set")?;
    let find = |name: &str, dark: bool| -> anyhow::Result<Rc<ThemeConfig>> {
        let theme = registry.themes().get(name).cloned().with_context(|| {
            format!("crates/rc-git-app/assets/theme.json has no theme named {name:?}")
        })?;
        anyhow::ensure!(
            theme.mode.is_dark() == dark,
            "theme {name:?} in crates/rc-git-app/assets/theme.json has the wrong mode"
        );
        Ok(theme)
    };
    Ok(Themes {
        light: find(LIGHT, false)?,
        dark: find(DARK, true)?,
    })
}

/// Installs the app's themes for both appearances. Call after `gpui_component::init` and before
/// `Theme::sync_system_appearance`, which applies the one for the current appearance. The
/// syntax-highlight section is carried over from gpui-component's defaults, which the
/// theme file leaves alone.
pub fn install(cx: &mut App) {
    #[expect(
        clippy::expect_used,
        reason = "the theme file is embedded at build time; one that does not parse is a build \
                  defect the first launch must stop on"
    )]
    let themes = parse()
        .inspect_err(|error| crate::log::error(&format!("{error:#}")))
        .expect("the embedded theme file crates/rc-git-app/assets/theme.json must parse");
    let registry = ThemeRegistry::global(cx);
    let light = with_highlight(&themes.light, registry.default_light_theme());
    let dark = with_highlight(&themes.dark, registry.default_dark_theme());
    let theme = Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
}

/// `config` with `fallback`'s syntax-highlight section when it has none of its own.
fn with_highlight(config: &Rc<ThemeConfig>, fallback: &Rc<ThemeConfig>) -> Rc<ThemeConfig> {
    if config.highlight.is_some() {
        return Rc::clone(config);
    }
    let mut config = ThemeConfig::clone(config);
    config.highlight.clone_from(&fallback.highlight);
    Rc::new(config)
}

#[cfg(test)]
mod tests {
    use gpui::{Hsla, Rgba};
    use gpui_component::{Theme, ThemeColor, ThemeConfig};

    use super::parse;

    /// A color as the 8-bit channels and alpha the screen gets.
    fn bytes(color: Hsla) -> [f64; 4] {
        let rgba = Rgba::from(color);
        [rgba.r, rgba.g, rgba.b, rgba.a].map(|channel| (f64::from(channel) * 255.).round())
    }

    /// `top` painted over the opaque `ground`, rounded to 8 bits as the screen holds it.
    fn over(top: Hsla, ground: [f64; 3]) -> [f64; 3] {
        let [r, g, b, a] = bytes(top);
        let alpha = a / 255.;
        [r, g, b]
            .iter()
            .zip(ground)
            .map(|(channel, under)| (channel * alpha + under * (1. - alpha)).round())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap_or([0.; 3])
    }

    fn opaque(color: Hsla) -> [f64; 3] {
        let [r, g, b, _] = bytes(color);
        [r, g, b]
    }

    fn luminance(color: [f64; 3]) -> f64 {
        let linear = |channel: f64| {
            let value = channel / 255.;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color[0]) + 0.7152 * linear(color[1]) + 0.0722 * linear(color[2])
    }

    fn contrast(text: [f64; 3], ground: [f64; 3]) -> f64 {
        let (a, b) = (luminance(text), luminance(ground));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn applied(config: &std::rc::Rc<ThemeConfig>) -> Theme {
        let mut theme = Theme::default();
        theme.apply_config(config);
        theme
    }

    /// Every ground a row draws on: the content, the sidebar and a panel, each bare, hovered
    /// and selected.
    fn row_grounds(theme: &Theme) -> Vec<(&'static str, [f64; 3])> {
        let mut grounds = Vec::new();
        for (name, base) in [
            ("background", theme.background),
            ("sidebar", theme.sidebar),
            ("popover", theme.popover),
        ] {
            let base = opaque(base);
            grounds.push((name, base));
            grounds.push((name, over(theme.list_hover, base)));
            grounds.push((name, over(theme.list_active, base)));
        }
        grounds
    }

    fn assert_readable(mode: &str, label: &str, text: Hsla, grounds: &[(&str, [f64; 3])]) {
        for (ground, color) in grounds {
            let ratio = contrast(over(text, *color), *color);
            assert!(
                ratio >= 4.5,
                "{mode}: {label} on {ground} {color:?} is {ratio:.2}:1, under 4.5:1"
            );
        }
    }

    #[test]
    fn the_theme_file_holds_a_light_and_a_dark_theme() {
        let themes = parse().unwrap();
        assert!(!themes.light.mode.is_dark());
        assert!(themes.dark.mode.is_dark());
    }

    #[test]
    fn every_color_the_app_reads_is_set_and_parses() {
        let themes = parse().unwrap();
        for config in [&themes.light, &themes.dark] {
            let colors = &config.colors;
            let theme = applied(config);
            for (key, value, resolved) in [
                ("background", &colors.background, theme.background),
                ("foreground", &colors.foreground, theme.foreground),
                (
                    "muted.foreground",
                    &colors.muted_foreground,
                    theme.muted_foreground,
                ),
                (
                    "table.head.foreground",
                    &colors.table_head_foreground,
                    theme.table_head_foreground,
                ),
                ("border", &colors.border, theme.border),
                ("sidebar.background", &colors.sidebar, theme.sidebar),
                ("title_bar.background", &colors.title_bar, theme.title_bar),
                ("popover.background", &colors.popover, theme.popover),
                ("primary.background", &colors.primary, theme.primary),
                ("link", &colors.link, theme.link),
                ("ring", &colors.ring, theme.ring),
                (
                    "table.even.background",
                    &colors.table_even,
                    theme.table_even,
                ),
            ] {
                let value = value
                    .as_ref()
                    .unwrap_or_else(|| panic!("{}: {key} is not set", config.name));
                let parsed = gpui_component::try_parse_color(value).unwrap();
                assert_eq!(
                    parsed, resolved,
                    "{}: {key} {value} did not apply",
                    config.name
                );
            }
        }
    }

    #[test]
    fn the_base_colors_replace_the_defaults() {
        let themes = parse().unwrap();
        for (config, defaults) in [
            (&themes.light, ThemeColor::light()),
            (&themes.dark, ThemeColor::dark()),
        ] {
            let theme = applied(config);
            for (label, ours, theirs) in [
                ("red", theme.red, defaults.red),
                ("green", theme.green, defaults.green),
                ("yellow", theme.yellow, defaults.yellow),
                ("blue", theme.blue, defaults.blue),
                ("magenta", theme.magenta, defaults.magenta),
                ("cyan", theme.cyan, defaults.cyan),
            ] {
                assert_ne!(ours, theirs, "{}: base.{label} did not apply", config.name);
            }
        }
    }

    #[test]
    fn rows_have_no_zebra() {
        let themes = parse().unwrap();
        for config in [&themes.light, &themes.dark] {
            let theme = applied(config);
            assert_eq!(theme.table_even, theme.table, "{}", config.name);
            assert_eq!(theme.list_even, theme.colors.list, "{}", config.name);
        }
    }

    #[test]
    fn text_reads_at_four_and_a_half_to_one_on_every_row_ground() {
        let themes = parse().unwrap();
        for config in [&themes.light, &themes.dark] {
            let theme = applied(config);
            let mode = config.name.as_ref();
            let rows = row_grounds(&theme);
            assert_readable(mode, "foreground", theme.foreground, &rows);
            assert_readable(mode, "muted foreground", theme.muted_foreground, &rows);
            let bare = [
                ("background", opaque(theme.background)),
                ("sidebar", opaque(theme.sidebar)),
                ("popover", opaque(theme.popover)),
            ];
            assert_readable(mode, "tertiary", theme.table_head_foreground, &bare);
            assert_readable(mode, "link", theme.link, &bare);
            for (label, color) in [
                ("red", theme.red),
                ("green", theme.green),
                ("yellow", theme.yellow),
                ("blue", theme.blue),
                ("magenta", theme.magenta),
                ("cyan", theme.cyan),
            ] {
                assert_readable(mode, label, color, &bare);
            }
            let button = contrast(opaque(theme.primary_foreground), opaque(theme.primary));
            assert!(
                button >= 4.5,
                "{mode}: primary button text is {button:.2}:1"
            );
        }
    }
}
