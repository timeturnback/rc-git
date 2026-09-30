---
description: how the highlighter's queries and capture table fit together, and what import links cost
paths: crates/rc-git-highlight/**
---

# rc-git-highlight

- A capture name a query uses must be in `CAPTURE_NAMES`. tree-sitter-highlight lets the last
  pattern on a node win and emits nothing when that winner is not configured, so a query
  added without its names strips color from nodes that had it. Add the name and its
  `CLASS_FOR_NAME` entry in the same change; `tree_sitter_javascript::JSX_HIGHLIGHT_QUERY`
  needs `tag` before it can be appended.
- A query built from two grammars' files puts the one that should win last. TypeScript's goes
  before JavaScript's (`typescript_query`).
- `import_links` parses on its own, because `Highlighter` keeps its tree. Do not call it from
  the highlight path or on file open: the app asks on the first pointer move over a file.
- Links stay byte ranges of the input. A new kind of link adds a range, never a string.
