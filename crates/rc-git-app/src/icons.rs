//! Every icon the app draws (`Icons.swift`): the one place a meaning becomes a symbol. Controls,
//! folders and refs are Lucide icons from `gpui-kit-assets`; a file whose type the Seti UI
//! mapping knows takes that type's Seti icon and color, as VS Code draws it, and any other file
//! the plain Lucide one. Folders and refs take the accent tone, unknown files the muted one;
//! every tint is resolved at render.

use std::borrow::Cow;

use gpui::{App, AssetSource, Hsla, SharedString, Styled as _};
use gpui_component::{ActiveTheme, Colorize as _, Icon, Theme};
use gpui_kit_assets::{IconName, IconNamed, icon_assets};
use rc_git_core::{EntryKind, RefKind};
use rc_git_kit::branch_banner::Symbol;
use rc_git_kit::file_icons::{FileIcon, FileTint, file_icon};

icon_assets!(
    ExtraIcons,
    [
        GitBranch, Cloud, Tag, Circle, CircleDot, FolderCog, Link, TextWrap, Maximize2, Minimize2,
        FolderGit2
    ]
);

/// Declares [`SETI`] from the icon stems listed, each embedded from `vendor/seti-ui/icons/`.
macro_rules! seti_icons {
    ($($name:literal),* $(,)?) => {
        /// Each bundled Seti icon as (stem, asset path, SVG bytes), sorted by stem.
        const SETI: &[(&str, &str, &[u8])] = &[$((
            $name,
            concat!("icons/seti/", $name, ".svg"),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../vendor/seti-ui/icons/",
                $name,
                ".svg"
            )),
        )),*];
    };
}

seti_icons!(
    "R",
    "argdown",
    "asm",
    "audio",
    "babel",
    "bazel",
    "bicep",
    "bower",
    "bsl",
    "c",
    "c-sharp",
    "cake",
    "cake_php",
    "clock",
    "clojure",
    "code-climate",
    "code-search",
    "coffee",
    "coldfusion",
    "config",
    "cpp",
    "crystal",
    "crystal_embedded",
    "css",
    "csv",
    "cu",
    "d",
    "dart",
    "db",
    "default",
    "docker",
    "ejs",
    "elixir",
    "elixir_script",
    "elm",
    "eslint",
    "ethereum",
    "f-sharp",
    "favicon",
    "firebase",
    "firefox",
    "font",
    "git",
    "github",
    "gitlab",
    "go",
    "go2",
    "godot",
    "gradle",
    "grails",
    "graphql",
    "grunt",
    "gulp",
    "hacklang",
    "haml",
    "happenings",
    "haskell",
    "haxe",
    "heroku",
    "hex",
    "html",
    "html_erb",
    "ignored",
    "illustrator",
    "image",
    "info",
    "ionic",
    "jade",
    "java",
    "javascript",
    "jenkins",
    "jinja",
    "json",
    "julia",
    "karma",
    "kotlin",
    "less",
    "license",
    "liquid",
    "livescript",
    "lock",
    "lua",
    "makefile",
    "markdown",
    "maven",
    "mdo",
    "mustache",
    "nim",
    "notebook",
    "npm",
    "npm_ignored",
    "nunjucks",
    "ocaml",
    "odata",
    "pddl",
    "pdf",
    "perl",
    "photoshop",
    "php",
    "pipeline",
    "plan",
    "platformio",
    "powershell",
    "prisma",
    "prolog",
    "pug",
    "puppet",
    "purescript",
    "python",
    "react",
    "reasonml",
    "rescript",
    "rollup",
    "ruby",
    "rust",
    "salesforce",
    "sass",
    "sbt",
    "scala",
    "shell",
    "slim",
    "smarty",
    "spring",
    "stylelint",
    "stylus",
    "sublime",
    "svelte",
    "svg",
    "swift",
    "terraform",
    "tex",
    "todo",
    "tsconfig",
    "twig",
    "typescript",
    "vala",
    "video",
    "vite",
    "vue",
    "wasm",
    "wat",
    "webpack",
    "wgt",
    "windows",
    "word",
    "xls",
    "xml",
    "yarn",
    "yml",
    "zig",
    "zip",
);

/// What an icon stands for. Each view names the meaning; [`icon`] picks the symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    /// A tree entry, by kind.
    Entry(EntryKind),
    /// A file whose type the Seti mapping knows.
    File(FileIcon),
    /// A ref, by namespace.
    Ref(RefKind),
    /// A ref whose target could not be read; also the restore notice's warning.
    Warning,
    /// The working tree, in the ref list and on the banner.
    WorkingTree,
    /// One commit, or a detached `HEAD`.
    Commit,
    /// Toolbar: step back.
    Back,
    /// Toolbar: step forward.
    Forward,
    /// Toolbar: show or hide the sidebar.
    Sidebar,
    /// Toolbar: back to the default branch.
    BackToDefault,
    /// Toolbar: sync.
    Sync,
    /// A spinner while a sync runs.
    Busy,
    /// A collapsed folder's disclosure.
    Collapsed,
    /// An expanded folder's disclosure.
    Expanded,
    /// The banner's disclosure, which opens the ref picker.
    Disclosure,
    /// A tick box with everything at or below its row ticked.
    TickOn,
    /// A tick box with only some of what is below its row ticked.
    TickMixed,
    /// A window tab: the repository it shows.
    Repository,
    /// A window tab's close button.
    CloseTab,
    /// The tab strip's button that opens a repository in a new tab.
    OpenTab,
    /// A diff's path bar: wrap long lines.
    WrapLines,
    /// A diff's path bar: show the diff across the whole window.
    ExpandDiff,
    /// A diff's path bar: put the panes beside the diff back.
    RestoreDiff,
}

/// The symbol an icon draws: a Lucide icon or a bundled Seti one, by asset path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppIcon {
    /// A Lucide icon from `gpui-kit-assets`.
    Lucide(IconName),
    /// A Seti icon's asset path in [`Assets`].
    Seti(&'static str),
}

impl IconNamed for AppIcon {
    fn path(self) -> SharedString {
        match self {
            Self::Lucide(name) => name.path(),
            Self::Seti(path) => path.into(),
        }
    }
}

/// The glyph for a tree entry named `name`: its file type's icon when the mapping knows it,
/// else the entry kind's.
#[must_use]
pub fn entry_glyph(kind: EntryKind, name: &str) -> Glyph {
    match kind {
        EntryKind::Blob | EntryKind::ExecutableBlob => {
            file_icon(name).map_or(Glyph::Entry(kind), Glyph::File)
        }
        EntryKind::Tree | EntryKind::Submodule | EntryKind::Symlink => Glyph::Entry(kind),
    }
}

/// The symbol for `glyph`.
#[must_use]
pub fn icon(glyph: Glyph) -> AppIcon {
    if let Glyph::File(file) = glyph
        && let Some((_, path, _)) = seti_entry(file.name)
    {
        return AppIcon::Seti(path);
    }
    AppIcon::Lucide(lucide(glyph))
}

/// The bundled Seti icon `stem` as (stem, asset path, SVG bytes), `None` when it is not
/// bundled.
fn seti_entry(stem: &str) -> Option<(&'static str, &'static str, &'static [u8])> {
    let at = SETI.binary_search_by_key(&stem, |(name, _, _)| name).ok()?;
    SETI.get(at).copied()
}

/// The Lucide icon for every glyph but a bundled file type, which [`icon`] draws from Seti.
fn lucide(glyph: Glyph) -> IconName {
    match glyph {
        Glyph::Entry(EntryKind::Tree) => IconName::Folder,
        Glyph::Entry(EntryKind::Submodule) => IconName::FolderCog,
        Glyph::Entry(EntryKind::Symlink) => IconName::Link,
        Glyph::Entry(EntryKind::Blob | EntryKind::ExecutableBlob) | Glyph::File(_) => {
            IconName::File
        }
        Glyph::Ref(RefKind::LocalBranch) => IconName::GitBranch,
        Glyph::Ref(RefKind::RemoteBranch) => IconName::Cloud,
        Glyph::Ref(RefKind::Tag) => IconName::Tag,
        Glyph::Ref(RefKind::Other) => IconName::Circle,
        Glyph::Warning => IconName::TriangleAlert,
        Glyph::WorkingTree => IconName::HardDrive,
        Glyph::Commit => IconName::CircleDot,
        Glyph::Back => IconName::ChevronLeft,
        Glyph::Forward | Glyph::Collapsed => IconName::ChevronRight,
        Glyph::Expanded => IconName::ChevronDown,
        Glyph::Sidebar => IconName::PanelLeft,
        Glyph::BackToDefault => IconName::Undo2,
        Glyph::Sync => IconName::RefreshCw,
        Glyph::Busy => IconName::LoaderCircle,
        Glyph::Disclosure => IconName::ChevronsUpDown,
        Glyph::TickOn => IconName::Check,
        Glyph::TickMixed => IconName::Minus,
        Glyph::Repository => IconName::FolderGit2,
        Glyph::CloseTab => IconName::Close,
        Glyph::OpenTab => IconName::Plus,
        Glyph::WrapLines => IconName::TextWrap,
        Glyph::ExpandDiff => IconName::Maximize2,
        Glyph::RestoreDiff => IconName::Minimize2,
    }
}

/// The banner's glyph, `None` while nothing is browsed.
#[must_use]
pub fn banner_glyph(symbol: Symbol) -> Option<Glyph> {
    match symbol {
        Symbol::Ref(kind) => Some(Glyph::Ref(kind)),
        Symbol::Commit | Symbol::Detached => Some(Glyph::Commit),
        Symbol::WorkingTree => Some(Glyph::WorkingTree),
        Symbol::Nothing => None,
    }
}

/// `glyph` as a row icon where files and folders are listed, in [`listing_tint`].
#[must_use]
pub fn listing_icon(glyph: Glyph, cx: &App) -> Icon {
    Icon::new(icon(glyph))
        .size(crate::style::ICON_SIZE)
        .text_color(listing_tint(glyph, cx.theme()))
}

/// The color `glyph` draws in where files and folders are listed: a file type's own tint, the
/// secondary text tone for an accented glyph, else the tertiary one.
fn listing_tint(glyph: Glyph, theme: &Theme) -> Hsla {
    if let Glyph::File(file) = glyph {
        return file_tint(file.tint, theme);
    }
    if is_accented(glyph) {
        theme.muted_foreground
    } else {
        crate::theme::tertiary_of(theme)
    }
}

/// The shade of `tint` in `theme`. Seti's orange and pink have no theme color of their own, so
/// they are mixed halfway (in Oklab) between the two theme colors around them, and follow the
/// appearance as those do. Seti's greys take the tertiary tone.
#[must_use]
pub fn file_tint(tint: FileTint, theme: &Theme) -> Hsla {
    match tint {
        FileTint::Blue => theme.blue,
        FileTint::Green => theme.green,
        FileTint::Yellow => theme.yellow,
        FileTint::Red => theme.red,
        FileTint::Purple => theme.magenta,
        FileTint::Orange => theme.red.mix_oklab(theme.yellow, 0.5),
        FileTint::Pink => theme.red.mix_oklab(theme.magenta, 0.5),
        FileTint::Muted => crate::theme::tertiary_of(theme),
    }
}

/// Whether `glyph` takes the secondary text tone rather than the tertiary one: folders and
/// refs do, files and controls do not.
#[must_use]
pub fn is_accented(glyph: Glyph) -> bool {
    match glyph {
        Glyph::Entry(EntryKind::Tree | EntryKind::Submodule)
        | Glyph::Ref(_)
        | Glyph::WorkingTree
        | Glyph::Commit => true,
        Glyph::Entry(EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink)
        | Glyph::File(_)
        | Glyph::Warning
        | Glyph::Back
        | Glyph::Forward
        | Glyph::Sidebar
        | Glyph::BackToDefault
        | Glyph::Sync
        | Glyph::Busy
        | Glyph::Collapsed
        | Glyph::Expanded
        | Glyph::Disclosure
        | Glyph::TickOn
        | Glyph::TickMixed
        | Glyph::Repository
        | Glyph::CloseTab
        | Glyph::OpenTab
        | Glyph::WrapLines
        | Glyph::ExpandDiff
        | Glyph::RestoreDiff => false,
    }
}

/// The app's asset source: the Lucide icons above that the component bundle lacks, the Seti
/// icons, then the component bundle, which the widgets draw their own chevrons and checks from.
#[derive(Debug, Clone, Copy, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        let stem = path
            .strip_prefix("icons/seti/")
            .and_then(|rest| rest.strip_suffix(".svg"));
        if let Some((_, _, bytes)) = stem.and_then(seti_entry) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = ExtraIcons.list(path)?;
        for (_, seti, _) in SETI {
            if seti.starts_with(path) {
                paths.push((*seti).into());
            }
        }
        paths.extend(gpui_kit_assets::Assets.list(path)?);
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY: [Glyph; 29] = [
        Glyph::File(FileIcon {
            name: "rust",
            tint: FileTint::Muted,
        }),
        Glyph::Entry(EntryKind::Tree),
        Glyph::Entry(EntryKind::Submodule),
        Glyph::Entry(EntryKind::Symlink),
        Glyph::Entry(EntryKind::Blob),
        Glyph::Entry(EntryKind::ExecutableBlob),
        Glyph::Ref(RefKind::LocalBranch),
        Glyph::Ref(RefKind::RemoteBranch),
        Glyph::Ref(RefKind::Tag),
        Glyph::Ref(RefKind::Other),
        Glyph::Warning,
        Glyph::WorkingTree,
        Glyph::Commit,
        Glyph::Back,
        Glyph::Forward,
        Glyph::Sidebar,
        Glyph::BackToDefault,
        Glyph::Sync,
        Glyph::Busy,
        Glyph::Collapsed,
        Glyph::Expanded,
        Glyph::Disclosure,
        Glyph::Entry(EntryKind::Blob),
        Glyph::Ref(RefKind::Tag),
        Glyph::TickOn,
        Glyph::TickMixed,
        Glyph::WrapLines,
        Glyph::ExpandDiff,
        Glyph::RestoreDiff,
    ];

    /// An icon the asset source cannot load draws nothing, so a missing one would ship as a
    /// blank where a symbol belongs.
    #[test]
    fn every_glyph_loads_from_the_app_assets() {
        for glyph in EVERY {
            let path = icon(glyph).path();
            let loaded = Assets.load(&path);
            assert!(
                matches!(loaded, Ok(Some(ref bytes)) if !bytes.is_empty()),
                "{glyph:?} -> {path} did not load"
            );
        }
    }

    /// An icon the asset source cannot load draws a blank where a file's icon belongs.
    #[test]
    fn every_bundled_seti_icon_loads() {
        for (stem, path, _) in SETI {
            let loaded = Assets.load(path);
            assert!(
                matches!(loaded, Ok(Some(ref bytes)) if bytes.starts_with(b"<svg")),
                "{stem} -> {path} did not load as an SVG"
            );
        }
    }

    /// The mapping names each icon by stem, and one the app does not bundle would fall back to
    /// the plain file icon for every file of that type. `seti_entry` binary-searches the table,
    /// which finds nothing once it falls out of order; an unused entry is dead weight.
    #[test]
    fn the_seti_table_is_sorted_and_holds_only_used_icons() {
        let stems: Vec<&str> = SETI.iter().map(|(stem, _, _)| *stem).collect();
        assert_eq!(stems, rc_git_kit::file_icons::icon_names());
    }

    #[test]
    fn a_known_file_draws_its_seti_icon_and_others_the_plain_one() {
        assert_eq!(
            icon(entry_glyph(EntryKind::Blob, "main.rs")),
            AppIcon::Seti("icons/seti/rust.svg")
        );
        assert_eq!(
            icon(entry_glyph(EntryKind::ExecutableBlob, "run.sh")),
            AppIcon::Seti("icons/seti/shell.svg")
        );
        assert_eq!(
            icon(entry_glyph(EntryKind::Blob, "notes")),
            AppIcon::Lucide(IconName::File)
        );
        let missing = Glyph::File(FileIcon {
            name: "no-such-icon",
            tint: FileTint::Blue,
        });
        assert_eq!(icon(missing), AppIcon::Lucide(IconName::File));
    }

    #[test]
    fn folders_links_and_submodules_keep_their_kind_icon_whatever_the_name() {
        for kind in [EntryKind::Tree, EntryKind::Symlink, EntryKind::Submodule] {
            assert_eq!(entry_glyph(kind, "main.rs"), Glyph::Entry(kind));
        }
    }
}
