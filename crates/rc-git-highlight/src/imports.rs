//! Import links: the regions of a JavaScript or TypeScript file that name another module.
//!
//! Pure, like the highlighter: bytes in, links out. A link holds no string. Its specifier is
//! a byte range of the same input, so a link is sixteen bytes however long the path is, and
//! the caller slices out the one specifier the user clicks.

use std::collections::HashMap;
use std::ops::Range;

use tree_sitter::Node;

use crate::{Error, Language};

/// One clickable region and the module it leads to. Byte offsets into the input, half-open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportLink {
    /// First byte of the clickable region.
    pub start: u32,
    /// One past its last byte.
    pub end: u32,
    /// First byte of the module specifier, as written between the quotes.
    pub specifier_start: u32,
    /// One past the specifier's last byte.
    pub specifier_end: u32,
}

/// A half-open byte range of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bytes {
    start: u32,
    end: u32,
}

impl Bytes {
    fn of(node: Node<'_>) -> Self {
        let range = node.byte_range();
        Self {
            start: u32::try_from(range.start).unwrap_or(u32::MAX),
            end: u32::try_from(range.end).unwrap_or(u32::MAX),
        }
    }
}

/// Finds every import link in `data`, ordered by start and never overlapping.
///
/// A link is the specifier of an `import`, a re-export, an `import()` or a `require()`; a
/// name one of those binds, where it is imported; or a JSX element whose name is such a
/// binding, so a component leads to the file it came from. Names are matched by spelling
/// alone: a local that shadows an import is linked like the import.
///
/// A specifier written with an escape sequence or as a template string is not linked. A
/// language without modules of this kind has no links.
///
/// # Errors
/// [`Error::Grammar`] when the language cannot be loaded, [`Error::Imports`] when the parser
/// returns no tree. A broken region inside an otherwise fine file is not an error; the
/// intact parts keep their links.
pub fn import_links(data: &[u8], language: Language) -> Result<Vec<ImportLink>, Error> {
    let grammar: tree_sitter::Language = match language {
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Bash
        | Language::C
        | Language::Json
        | Language::Markdown
        | Language::Python
        | Language::Rust
        | Language::Swift
        | Language::Toml
        | Language::Yaml => return Ok(Vec::new()),
    };
    if !mentions_a_module(data) {
        return Ok(Vec::new());
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|_| Error::Grammar { language })?;
    let tree = parser
        .parse(data, None)
        .ok_or(Error::Imports { language })?;
    let mut collector = Collector::new(data);
    collector.walk(&tree);
    Ok(collector.finish())
}

/// Whether the bytes hold a keyword every link needs: `import`, `require`, or the `from` of
/// a re-export. A file with none has no links, so it is never parsed.
fn mentions_a_module(data: &[u8]) -> bool {
    data.windows(6).any(|window| window == b"import")
        || data.windows(7).any(|window| window == b"require")
        || data.windows(4).any(|window| window == b"from")
}

/// The nodes that may sit between a declared name and the `import()` or `require()` that
/// gives it a module, as in `const Page = lazy(() => import('./Page'))`. Anything else in
/// between, a block or a JSX attribute for instance, means the name is not the module.
const BINDING_PATH: [&str; 9] = [
    "arguments",
    "arrow_function",
    "as_expression",
    "await_expression",
    "call_expression",
    "member_expression",
    "non_null_expression",
    "parenthesized_expression",
    "satisfies_expression",
];

struct Collector<'a> {
    data: &'a [u8],
    links: Vec<ImportLink>,
    /// Local names an import binds, each with the specifier it came from.
    bindings: HashMap<&'a [u8], Bytes>,
    /// JSX element names and the identifier each one starts with. Resolved against the
    /// bindings once the whole file is read, since a binding may follow its first use.
    elements: Vec<(Bytes, Range<usize>)>,
}

impl<'a> Collector<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            links: Vec::new(),
            bindings: HashMap::new(),
            elements: Vec::new(),
        }
    }

    /// Visits every node once. Iterative, with the ancestors kept alongside the cursor, so
    /// deep nesting costs heap instead of stack and a binding is found without `parent()`.
    fn walk(&mut self, tree: &tree_sitter::Tree) {
        let mut cursor = tree.walk();
        let mut ancestors: Vec<Node<'_>> = Vec::new();
        'walk: loop {
            let node = cursor.node();
            if self.visit(node, &ancestors) && cursor.goto_first_child() {
                ancestors.push(node);
                continue 'walk;
            }
            loop {
                if cursor.goto_next_sibling() {
                    continue 'walk;
                }
                if !cursor.goto_parent() {
                    break 'walk;
                }
                ancestors.pop();
            }
        }
    }

    /// Records what `node` contributes and says whether its children are worth visiting.
    fn visit(&mut self, node: Node<'_>, ancestors: &[Node<'_>]) -> bool {
        match node.kind() {
            "import_statement" => {
                self.import_statement(node);
                false
            }
            "export_statement" => {
                let Some(source) = node.child_by_field_name("source") else {
                    return true;
                };
                self.reexport(node, source);
                false
            }
            "call_expression" => {
                let Some(specifier) = self.module_call(node) else {
                    return true;
                };
                self.link(specifier, specifier);
                if let Some(name) = self.declared_name(ancestors) {
                    self.bindings.entry(name).or_insert(specifier);
                }
                false
            }
            "jsx_opening_element" | "jsx_closing_element" | "jsx_self_closing_element" => {
                self.element(node);
                true
            }
            _ => true,
        }
    }

    fn link(&mut self, region: Bytes, specifier: Bytes) {
        self.links.push(ImportLink {
            start: region.start,
            end: region.end,
            specifier_start: specifier.start,
            specifier_end: specifier.end,
        });
    }

    fn text(&self, node: Node<'_>) -> &'a [u8] {
        self.data.get(node.byte_range()).unwrap_or_default()
    }

    /// The specifier inside a string literal: its one fragment, without the quotes. `None`
    /// for an empty string and for one with an escape sequence, whose bytes are not the path.
    fn specifier(string: Node<'_>) -> Option<Bytes> {
        if string.kind() != "string" || string.named_child_count() != 1 {
            return None;
        }
        let fragment = string.named_child(0)?;
        (fragment.kind() == "string_fragment").then(|| Bytes::of(fragment))
    }

    /// `import a, { b as c } from 'x'`, `import * as n from 'x'`, `import 'x'`, and
    /// TypeScript's `import a = require('x')`.
    fn import_statement(&mut self, node: Node<'_>) {
        let mut source = node.child_by_field_name("source");
        let mut clause = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "import_clause" => clause = Some(child),
                "import_require_clause" => {
                    source = child.child_by_field_name("source");
                    clause = Some(child);
                }
                _ => {}
            }
        }
        let Some(specifier) = source.and_then(Self::specifier) else {
            return;
        };
        self.link(specifier, specifier);
        let Some(clause) = clause else { return };
        let mut cursor = clause.walk();
        for child in clause.named_children(&mut cursor) {
            match child.kind() {
                "identifier" => self.bind(child, child, specifier),
                "namespace_import" => {
                    if let Some(name) = child.named_child(0) {
                        self.bind(child, name, specifier);
                    }
                }
                "named_imports" => self.named_imports(child, specifier),
                _ => {}
            }
        }
    }

    fn named_imports(&mut self, list: Node<'_>, specifier: Bytes) {
        let mut cursor = list.walk();
        for item in list.named_children(&mut cursor) {
            if item.kind() != "import_specifier" {
                continue;
            }
            let local = item
                .child_by_field_name("alias")
                .or_else(|| item.child_by_field_name("name"));
            match local {
                Some(local) => self.bind(item, local, specifier),
                None => self.link(Bytes::of(item), specifier),
            }
        }
    }

    /// Links `region` and, when `name` is a plain identifier, binds it to the specifier.
    fn bind(&mut self, region: Node<'_>, name: Node<'_>, specifier: Bytes) {
        self.link(Bytes::of(region), specifier);
        if name.kind() == "identifier" {
            self.bindings.entry(self.text(name)).or_insert(specifier);
        }
    }

    /// `export { a, b as c } from 'x'` and `export * from 'x'`. Nothing is bound: a
    /// re-export gives the file no local name.
    fn reexport(&mut self, node: Node<'_>, source: Node<'_>) {
        let Some(specifier) = Self::specifier(source) else {
            return;
        };
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() != "export_clause" {
                continue;
            }
            let mut items = child.walk();
            for item in child.named_children(&mut items) {
                if item.kind() == "export_specifier" {
                    self.link(Bytes::of(item), specifier);
                }
            }
        }
        self.link(specifier, specifier);
    }

    /// The specifier of `import('x')` or `require('x')`, when `node` is one.
    fn module_call(&self, node: Node<'_>) -> Option<Bytes> {
        let function = node.child_by_field_name("function")?;
        let loads_a_module = match function.kind() {
            "import" => true,
            "identifier" => self.text(function) == b"require",
            _ => false,
        };
        if !loads_a_module {
            return None;
        }
        let first = node.child_by_field_name("arguments")?.named_child(0)?;
        Self::specifier(first)
    }

    /// The name a module call is assigned to, reading outward from the call through
    /// [`BINDING_PATH`] to a `const name = ...`.
    fn declared_name(&self, ancestors: &[Node<'_>]) -> Option<&'a [u8]> {
        for ancestor in ancestors.iter().rev() {
            let kind = ancestor.kind();
            if kind == "variable_declarator" {
                let name = ancestor.child_by_field_name("name")?;
                return (name.kind() == "identifier").then(|| self.text(name));
            }
            if !BINDING_PATH.contains(&kind) {
                return None;
            }
        }
        None
    }

    /// Remembers a JSX element name that may be a component: `<Panel>` and `<ui.Panel>`,
    /// never `<div>`, since a lowercase name with no dot is an intrinsic element.
    fn element(&mut self, node: Node<'_>) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let mut root = name;
        while root.kind() == "member_expression" {
            let Some(object) = root.child_by_field_name("object") else {
                return;
            };
            root = object;
        }
        if root.kind() != "identifier" {
            return;
        }
        let intrinsic = root.id() == name.id()
            && self
                .text(root)
                .first()
                .is_some_and(|byte| !byte.is_ascii_uppercase());
        if !intrinsic {
            self.elements.push((Bytes::of(name), root.byte_range()));
        }
    }

    /// Turns the remembered elements into links and restores start order.
    fn finish(mut self) -> Vec<ImportLink> {
        let elements = std::mem::take(&mut self.elements);
        for (region, root) in elements {
            let Some(name) = self.data.get(root) else {
                continue;
            };
            if let Some(specifier) = self.bindings.get(name).copied() {
                self.link(region, specifier);
            }
        }
        self.links.sort_unstable_by_key(|link| link.start);
        self.links
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every link as the text the user clicks and the specifier it leads to.
    fn links(source: &str, language: Language) -> Vec<(&str, &str)> {
        let found = import_links(source.as_bytes(), language).unwrap();
        for pair in found.windows(2) {
            assert!(
                pair[0].end <= pair[1].start,
                "links overlap or are out of order: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
        found
            .iter()
            .map(|link| {
                (
                    &source[link.start as usize..link.end as usize],
                    &source[link.specifier_start as usize..link.specifier_end as usize],
                )
            })
            .collect()
    }

    #[test]
    fn import_statements_link_their_names_and_their_specifier() {
        let source = "import Button, { useState as useS, type FC } from './ui';\n\
                      import * as path from \"node:path\";\n\
                      import './side-effect.css';\n";
        assert_eq!(
            links(source, Language::Tsx),
            vec![
                ("Button", "./ui"),
                ("useState as useS", "./ui"),
                ("type FC", "./ui"),
                ("./ui", "./ui"),
                ("* as path", "node:path"),
                ("node:path", "node:path"),
                ("./side-effect.css", "./side-effect.css"),
            ]
        );
    }

    #[test]
    fn type_imports_and_import_require_link_in_typescript() {
        let source = "import type { Shape } from '@/shapes';\nimport fs = require('fs');\n";
        assert_eq!(
            links(source, Language::TypeScript),
            vec![
                ("Shape", "@/shapes"),
                ("@/shapes", "@/shapes"),
                ("fs", "fs"),
                ("fs", "fs"),
            ]
        );
    }

    #[test]
    fn reexports_link_their_names_and_their_specifier() {
        let source = "export { a, b as c } from './re';\nexport * from './all';\n";
        assert_eq!(
            links(source, Language::JavaScript),
            vec![
                ("a", "./re"),
                ("b as c", "./re"),
                ("./re", "./re"),
                ("./all", "./all"),
            ]
        );
    }

    #[test]
    fn a_local_export_is_not_a_link() {
        let source = "import a from './a';\nexport { a };\nexport default a;\n";
        assert_eq!(
            links(source, Language::TypeScript),
            vec![("a", "./a"), ("./a", "./a")]
        );
    }

    #[test]
    fn dynamic_imports_and_requires_link_their_specifier() {
        let source = "const legacy = require('../legacy');\n\
                      import('./later').then((module) => module.run());\n\
                      load(require);\n";
        assert_eq!(
            links(source, Language::JavaScript),
            vec![("../legacy", "../legacy"), ("./later", "./later")]
        );
    }

    #[test]
    fn components_link_to_the_module_they_were_imported_from() {
        let source = "import Panel from './Panel';\n\
                      import { Row as Line, ui } from '@/kit';\n\
                      const view = (\n  <Panel title=\"a\">\n    <Line />\n    <ui.Cell>x</ui.Cell>\n  </Panel>\n);\n";
        let found = links(source, Language::Tsx);
        let elements: Vec<_> = found.iter().skip(5).copied().collect();
        assert_eq!(
            elements,
            vec![
                ("Panel", "./Panel"),
                ("Line", "@/kit"),
                ("ui.Cell", "@/kit"),
                ("ui.Cell", "@/kit"),
                ("Panel", "./Panel"),
            ]
        );
    }

    #[test]
    fn intrinsic_elements_and_local_components_are_not_links() {
        let source = "import div from './div';\n\
                      const Local = () => <span />;\n\
                      const view = <div><Local /><></></div>;\n";
        assert_eq!(
            links(source, Language::Tsx),
            vec![("div", "./div"), ("./div", "./div")]
        );
    }

    #[test]
    fn a_lazy_component_links_even_when_declared_after_its_use() {
        let source = "const App = () => <Page />;\n\
                      const Page = lazy(() => import('./pages/Page'));\n\
                      const Chart = dynamic(() => import('./Chart').then((m) => m.Chart), { ssr: false });\n\
                      const late = <Chart />;\n";
        assert_eq!(
            links(source, Language::Tsx),
            vec![
                ("Page", "./pages/Page"),
                ("./pages/Page", "./pages/Page"),
                ("./Chart", "./Chart"),
                ("Chart", "./Chart"),
            ]
        );
    }

    #[test]
    fn a_module_loaded_inside_a_component_does_not_name_the_component() {
        let source = "const Page = () => { const data = require('./data.json'); return <p />; };\n\
                      const Card = () => <Icon src={require('./icon.png')} />;\n\
                      const view = <Page><Card /></Page>;\n";
        assert_eq!(
            links(source, Language::Tsx),
            vec![("./data.json", "./data.json"), ("./icon.png", "./icon.png"),]
        );
    }

    #[test]
    fn specifiers_that_are_not_plain_strings_are_skipped() {
        let source = "import a from 'pkg\\u0041';\n\
                      import b from '';\n\
                      const c = require(`./${name}`);\n\
                      const d = require(name);\n\
                      import e from './e';\n";
        assert_eq!(
            links(source, Language::JavaScript),
            vec![("e", "./e"), ("./e", "./e")]
        );
    }

    #[test]
    fn offsets_are_bytes_so_text_before_a_link_may_be_multibyte() {
        let source = "// caf\u{e9} \u{1f642}\nimport a from './a';\n";
        assert_eq!(
            links(source, Language::TypeScript),
            vec![("a", "./a"), ("./a", "./a")]
        );
    }

    #[test]
    fn broken_input_keeps_the_links_of_its_intact_parts() {
        let source = "import a from './a';\nconst x = = 3 +;\nimport b from './b';\n";
        let found = links(source, Language::TypeScript);
        assert!(found.contains(&("./a", "./a")));
        assert!(found.contains(&("./b", "./b")));
    }

    #[test]
    fn files_without_modules_and_other_languages_have_no_links() {
        assert_eq!(links("const x = 1;\n", Language::TypeScript), vec![]);
        assert_eq!(links("", Language::Tsx), vec![]);
        assert_eq!(links("use std::fmt; // import\n", Language::Rust), vec![]);
        assert_eq!(
            links("const important = 'require';\n", Language::JavaScript),
            vec![],
            "the keywords alone, outside an import, make no link"
        );
    }
}
