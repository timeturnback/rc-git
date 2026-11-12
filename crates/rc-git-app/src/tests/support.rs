//! What the app's tests share: documents, the palette, a shell on fakes, and an ink counter.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, HeadlessAppContext, TestAppContext};
use rc_git_core::{EntryKind, Head, RefKind, RefListing, TreeEntry};
use rc_git_kit::fake::{FakeRepository, branch, oid, text_blob};
use rc_git_kit::platform::credentials::MemoryCredentialStore;
use rc_git_kit::platform::host_keys::MemoryHostKeyStore;
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::watch::ManualWatchers;
use rc_git_kit::text_document::TextDocument;

use crate::app::{Opened, Services, Shell};
use crate::palette::Palette;
use crate::window::RepoWindow;

/// An app that lays text out through real CoreText and draws off screen, with the app's icons.
pub fn headless_app() -> HeadlessAppContext {
    let text_system = Arc::new(gpui_apple::AppleTextSystem::new());
    let mut cx =
        HeadlessAppContext::with_platform(text_system, Arc::new(crate::icons::Assets), || {
            gpui_platform::current_headless_renderer()
        });
    cx.update(gpui_component::init);
    cx
}

/// Byte offsets where lines start, by the core's rule: none for empty input, and a trailing
/// newline does not start a line.
pub fn line_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    if bytes.is_empty() {
        return starts;
    }
    starts.push(0);
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' && index + 1 < bytes.len() {
            starts.push(index + 1);
        }
    }
    starts
}

/// A valid UTF-8 document of `text`.
pub fn document(text: &str) -> Arc<TextDocument> {
    let bytes = text.as_bytes().to_vec();
    let starts = line_starts(&bytes);
    Arc::new(TextDocument::new(bytes, &starts, true))
}

/// The default theme's palette.
pub fn palette() -> Palette {
    let cx = TestAppContext::single();
    cx.update(|cx| {
        gpui_component::init(cx);
        Palette::current(cx)
    })
}

/// Pixels dark enough to be glyphs, and pixels saturated enough to be highlight colors.
pub fn count_ink(pixels: impl IntoIterator<Item = [u8; 4]>) -> (usize, usize) {
    let mut ink = 0usize;
    let mut colored = 0usize;
    for pixel in pixels {
        let [r, g, b, _] = pixel;
        if r < 200 || g < 200 || b < 200 {
            ink += 1;
        }
        if r.abs_diff(g) > 60 || g.abs_diff(b) > 60 {
            colored += 1;
        }
    }
    (ink, colored)
}

pub const MAIN_COMMIT: u8 = 0x11;
pub const MAIN_TREE: u8 = 0x12;
pub const FEATURE_COMMIT: u8 = 0x21;
pub const FEATURE_TREE: u8 = 0x22;
pub const SRC_TREE: u8 = 0x30;
pub const README: u8 = 0x40;
pub const GIT_DIR_TREE: u8 = 0x50;

fn tree(name: &str, seed: u8, kind: EntryKind) -> TreeEntry {
    TreeEntry {
        name: name.into(),
        oid: oid(seed),
        kind,
    }
}

/// A bare repository: `main` and `feature`, a `src` folder, a README and a hidden `.github`.
pub fn repo() -> FakeRepository {
    let repo = FakeRepository::new();
    {
        let mut state = repo.state();
        state.listing = RefListing {
            refs: vec![
                branch("feature", oid(FEATURE_COMMIT), RefKind::LocalBranch),
                branch("main", oid(MAIN_COMMIT), RefKind::LocalBranch),
                branch("v1", oid(MAIN_COMMIT), RefKind::Tag),
            ],
            broken: Vec::new(),
        };
        state.head = Head {
            ref_name: Some("refs/heads/main".to_owned()),
            target: Some(oid(MAIN_COMMIT)),
            exact_ref_name: true,
        };
        state.tree_of.insert(oid(MAIN_COMMIT), oid(MAIN_TREE));
        state.tree_of.insert(oid(FEATURE_COMMIT), oid(FEATURE_TREE));
        state.trees.insert(
            oid(MAIN_TREE),
            vec![
                tree(".git", GIT_DIR_TREE, EntryKind::Tree),
                tree("src", SRC_TREE, EntryKind::Tree),
                tree("README.md", README, EntryKind::Blob),
            ],
        );
        state.trees.insert(
            oid(FEATURE_TREE),
            vec![tree("feature.txt", README, EntryKind::Blob)],
        );
        state.trees.insert(
            oid(SRC_TREE),
            vec![tree("main.rs", README, EntryKind::Blob)],
        );
        state.blobs.insert(oid(README), text_blob("# hi\n\nbody\n"));
    }
    repo
}

/// Runs the app until `done` holds. Repository reads run on real threads, which
/// `run_until_parked` does not wait for, so a test polls the state it waits on; the deadline
/// only bounds a hang and never decides an outcome.
pub fn wait_until(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the app to settle"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Opens `path` and waits until its window shows a root tree.
pub fn open_settled(harness: &Harness, cx: &mut TestAppContext, path: &str) -> Entity<RepoWindow> {
    let id = harness.shell.update(cx, |shell, cx| shell.open(path, cx));
    assert!(id.is_some(), "{path} opens");
    let window = harness.shell.read_with(cx, |shell, _| {
        shell
            .windows()
            .iter()
            .find(|tab| Some(tab.id()) == id)
            .map(|tab| tab.view.clone())
    });
    let window = window.unwrap_or_else(|| panic!("{path} has a window"));
    wait_until(cx, |cx| {
        cx.read(|cx| {
            let tree = window.read(cx).sidebar().read(cx).tree().clone();
            tree.read(cx).file_tree().node_count() > 0
        })
    });
    window
}

/// A shell on fakes, plus the repositories its opener knows by path and its settings.
pub struct Harness {
    pub shell: Entity<Shell>,
    pub watchers: Arc<ManualWatchers>,
}

/// A shell whose opener answers each of `repos` by path and fails every other path.
pub fn harness(cx: &mut TestAppContext, repos: &[(&str, FakeRepository)]) -> Harness {
    harness_with(cx, repos, &Arc::new(MemorySettings::new()))
}

/// [`harness`] over `settings`, for a test that saves tabs before the shell starts.
pub fn harness_with(
    cx: &mut TestAppContext,
    repos: &[(&str, FakeRepository)],
    settings: &Arc<MemorySettings>,
) -> Harness {
    cx.executor().allow_parking();
    cx.update(gpui_component::init);
    let known: HashMap<String, FakeRepository> = repos
        .iter()
        .map(|(path, repo)| ((*path).to_owned(), repo.clone()))
        .collect();
    let watchers = Arc::new(ManualWatchers::new());
    let services = Services {
        settings: settings.clone(),
        opener: Rc::new(move |path: &str| {
            let repo = known
                .get(path)
                .ok_or_else(|| format!("{path} is not a repository"))?;
            Ok(Opened {
                reading: Arc::new(repo.clone()),
                key: format!("{path}/.git"),
                tab_path: path.to_owned(),
                dirs: Some((format!("{path}/.git"), format!("{path}/.git"))),
                actions: None,
            })
        }),
        watchers: watchers.clone(),
        credentials: Arc::new(MemoryCredentialStore::new()),
        host_keys: Arc::new(MemoryHostKeyStore::new()),
        home: "/Users/me".to_owned(),
        app_name: "rc-git".to_owned(),
    };
    let shell = cx.new(|cx| Shell::new(services, cx));
    Harness { shell, watchers }
}
