---
description: how the highlighter's queries and capture table fit together, and what import links cost
paths: crates/rc-git-highlight/**
---

# rc-git-highlight

- A capture name a query uses must resolve to a `CAPTURES` entry, name and class in one row.
  tree-sitter-highlight lets the last pattern on a node win and emits nothing when that
  winner is not configured, so a query added without its names strips color from nodes that
  had it; `every_query_capture_name_resolves` fails on it. Strip a capture that is no color
  from the query instead (`swift_query` drops `@spell`); mapping it to `None` still hides
  the node. `tree_sitter_javascript::JSX_HIGHLIGHT_QUERY` needs `tag` before it is appended.
- A query built from two grammars' files puts the one that should win last. TypeScript's goes
  before JavaScript's (`typescript_query`).
- `import_links` parses on its own, because `Highlighter` keeps its tree. Do not call it from
  the highlight path or on file open: the app asks on the first pointer move over a file.
- Links stay byte ranges of the input. A new kind of link adds a range, never a string.
