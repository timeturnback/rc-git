//! Every icon the app draws (`Icons.swift`), as Lucide icons from `gpui-kit-assets`: the one
//! place a meaning becomes a symbol. Folders and refs take the accent color, files stay muted,
//! so a long listing does not turn into a wall of color; the tint is resolved at render.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};
use gpui_kit_assets::{IconName, icon_assets};
use rc_git_core::{EntryKind, RefKind};
use rc_git_kit::branch_banner::Symbol;

icon_assets!(
    ExtraIcons,
    [
        GitBranch, Cloud, Tag, Circle, CircleDot, FolderCog, Link, TextWrap, Maximize2, Minimize2
    ]
);

/// What an icon stands for. Each view names the meaning; [`icon`] picks the symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    /// A tree entry, by kind.
    Entry(EntryKind),
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

/// The Lucide icon for `glyph`.
#[must_use]
pub fn icon(glyph: Glyph) -> IconName {
    match glyph {
        Glyph::Entry(EntryKind::Tree) => IconName::Folder,
        Glyph::Entry(EntryKind::Submodule) => IconName::FolderCog,
        Glyph::Entry(EntryKind::Symlink) => IconName::Link,
        Glyph::Entry(EntryKind::Blob | EntryKind::ExecutableBlob) => IconName::File,
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
        | Glyph::CloseTab
        | Glyph::OpenTab
        | Glyph::WrapLines
        | Glyph::ExpandDiff
        | Glyph::RestoreDiff => false,
    }
}

/// The app's asset source: the icons above that the component bundle lacks, then the
/// component bundle, which the widgets draw their own chevrons and checks from.
#[derive(Debug, Clone, Copy, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = ExtraIcons.list(path)?;
        paths.extend(gpui_kit_assets::Assets.list(path)?);
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY: [Glyph; 28] = [
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
}
