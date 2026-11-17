---
title: "A gpui-component ThemeConfig changes or drops colors without a word: alpha clamps, silent keys, private base fields"
date: 2026-11-17
category: ui-bugs
module: rc-git-app
problem_type: best_practice
component: frontend
severity: medium
framework_version: "gpui-component 0.9.0"
applies_when:
  - "Editing crates/rc-git-app/assets/theme.json or adding a color the app reads from cx.theme()"
  - "Choosing a selected-row, table-row or text-selection fill for the theme file"
  - "Writing a test that proves a theme key applied, or measuring contrast without an App"
  - "Wrapping a gpui-component TabBar in a bordered container"
related_components: [gpui-component, theme, tab-bar]
tags: [gpui-component, theme, themeconfig, contrast, list-active, tab-bar]
---

# A gpui-component ThemeConfig changes or drops colors without a word

## Context

The app's look comes from `crates/rc-git-app/assets/theme.json`, one `ThemeConfig` per mode,
which `theme::install` puts into `Theme.light_theme` and `Theme.dark_theme` at launch.
gpui-component applies the file in `Theme::apply_config` (`gpui-component-0.9.0/src/theme/schema.rs`).
Several of its behaviors change or drop what the file says without an error, a log line, or a
failing test. They only show up as a wrong look, and the app is never launched to check one.

## Guidance

- **Selected fills are alpha-clamped.** `apply_config` caps `list.active.background` and
  `table.active.background` at alpha 0.2 and `selection.background` at 0.3 (`schema.rs`, the
  `clamp_alpha` calls after the color list). An opaque `#26282e` selected row paints as 20% of
  itself. Write these fills translucent (`#ffffff14` dark, `#00000012` light) and measure them
  painted over each ground, as `theme.rs`'s contrast test does.
- **A bad value or an unknown key is dropped.** A color string that fails `try_parse_color`
  falls back to the default color, and serde ignores a key the schema does not name. The
  default theme itself uses `link.foreground`, which the schema does not read; the key is
  `link`. `every_color_the_app_reads_is_set_and_parses` in `crates/rc-git-app/src/theme.rs`
  asserts each key the app reads is set and equals the resolved color; add a key there when a
  view starts reading a new field.
- **`base.*` config fields are private.** `ThemeConfigColors.red`, `green` and the rest cannot
  be read from outside the crate, so a test proves they applied by comparing the resolved
  `Theme` with `ThemeColor::light()` / `ThemeColor::dark()`
  (`the_base_colors_replace_the_defaults`).
- **No `serde_json` and no `App` are needed.** `ThemeRegistry::default().load_themes_from_str`
  parses a theme set into a private registry, and `Theme::default().apply_config(&config)`
  resolves it headless, so contrast tests are plain unit tests.
- **The theme has no third text tone.** The tertiary tone lives in `table.head.foreground`;
  views read it through `theme::tertiary(cx)`, never the field directly.
- **`TabBar` draws its own bottom border** in `theme.border` (`src/tab/tab_bar.rs`, the
  `border-b` child). A border on a wrapper around it draws a second line.

## Why This Matters

Every one of these fails silent: the build, clippy and the ink tests stay green while the
window shows a washed-out selection, a default color in place of the one written, or a doubled
divider. The tests in `theme.rs` catch the dropped-key and contrast cases; the alpha clamp and
the TabBar border are caught only by knowing them.

## When to Apply

Before changing a value in `theme.json`, adding a theme field a view reads, or styling a
gpui-component widget whose chrome already reads the theme.

## Examples

```jsonc
// Washes out to 20%: the clamp keeps the color and cuts its alpha.
"list.active.background": "#26282e",
// Paints as written over every ground.
"list.active.background": "#ffffff14",
```
