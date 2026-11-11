//! The live [`TreeImportSource`] over a committed fixture tree: the resolver cases the Swift
//! `ModelImportTests` drove through `BrowserModel.openImport` over a committed tree.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helper outside #[test] fns unwraps opening the repository"
)]

use rc_git_core::{Oid, Repository};
use rc_git_kit::import_resolver::{ImportResolver, ImportSource, TreeImportSource};
use rc_git_testkit::Fixture;

fn committed() -> (Fixture, Repository, Oid) {
    let fixture = Fixture::standard();
    fixture.write("web/src/App.tsx", b"import Button from './Button';\n");
    fixture.write(
        "web/src/Button.tsx",
        b"export default function Button() {}\n",
    );
    fixture.write(
        "web/tsconfig.json",
        br#"{"compilerOptions": {"paths": {"@/*": ["./src/*"]}}}"#,
    );
    fixture.write("web/big/tsconfig.json", &vec![b' '; 300 * 1024]);
    fixture.write("web/big/x.ts", b"");
    fixture.commit("add a web app", "2024-01-05T00:00:00+0000");
    let repository = Repository::open(fixture.path()).unwrap();
    let root = repository.resolve_tree("HEAD").unwrap();
    (fixture, repository, root)
}

#[test]
fn a_relative_import_resolves_in_the_committed_tree() {
    let (_fixture, repository, root) = committed();
    let resolver = ImportResolver::new(TreeImportSource::new(&repository, root));
    assert_eq!(
        resolver.resolve("./Button", "web/src/App.tsx").unwrap(),
        Some("web/src/Button.tsx".to_owned())
    );
}

#[test]
fn an_aliased_import_resolves_through_the_committed_config() {
    let (_fixture, repository, root) = committed();
    let resolver = ImportResolver::new(TreeImportSource::new(&repository, root));
    assert_eq!(
        resolver.resolve("@/Button", "web/src/App.tsx").unwrap(),
        Some("web/src/Button.tsx".to_owned())
    );
}

#[test]
fn a_package_or_missing_import_resolves_to_nothing() {
    let (_fixture, repository, root) = committed();
    let resolver = ImportResolver::new(TreeImportSource::new(&repository, root));
    assert_eq!(resolver.resolve("react", "web/src/App.tsx").unwrap(), None);
    assert_eq!(
        resolver.resolve("./missing", "web/src/App.tsx").unwrap(),
        None
    );
    assert_eq!(
        resolver
            .resolve("./nope/deeper", "web/src/App.tsx")
            .unwrap(),
        None
    );
}

#[test]
fn the_live_source_lists_directories_and_reads_configs() {
    let (_fixture, repository, root) = committed();
    let source = TreeImportSource::new(&repository, root);
    let names = |directory: &str| -> Option<Vec<String>> {
        source.list(directory).unwrap().map(|entries| {
            entries
                .iter()
                .map(|entry| entry.name_lossy().into_owned())
                .collect()
        })
    };
    assert_eq!(
        names("web/src"),
        Some(vec!["App.tsx".to_owned(), "Button.tsx".to_owned()])
    );
    assert_eq!(names("README.md"), None, "a file is not a directory");
    assert_eq!(names("nope"), None);
    assert!(
        names("").unwrap().contains(&"web".to_owned()),
        "the root lists"
    );
    let config = source.read("web/tsconfig.json").unwrap().unwrap();
    assert!(config.starts_with(b"{\"compilerOptions\""));
    assert_eq!(source.read("web/missing.json").unwrap(), None);
    assert_eq!(source.read("nope/tsconfig.json").unwrap(), None);
    assert_eq!(
        source.read("bin.dat").unwrap(),
        None,
        "a binary blob is not text"
    );
    assert_eq!(
        source.read("web/big/tsconfig.json").unwrap(),
        None,
        "a config over the size limit is not read"
    );
}

#[test]
fn reading_a_directory_as_a_config_fails() {
    let (_fixture, repository, root) = committed();
    let source = TreeImportSource::new(&repository, root);
    assert!(source.read("web/src").is_err(), "a tree is not a blob");
}
