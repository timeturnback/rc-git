# Seti UI icons

File-type icons and their mapping from [Seti UI](https://github.com/jesseweed/seti-ui), the
icon theme VS Code ships as its default. MIT licensed; see `LICENSE.md`.

Copied unchanged from commit `2d6c5e68b4ded73c92dac291845ee44e1182d511` (2025-10-28):

- `mapping.less`: upstream `styles/components/icons/mapping.less`. `rc-git-kit`'s
  `file_icons` module parses it at first use.
- `icons/`: the upstream `icons/*.svg` files that `mapping.less` names, and no others.
  `rc-git-app`'s `icons.rs` embeds each one by name.

To update, copy both from a newer upstream checkout, then run `cargo test -p rc-git-kit -p
rc-git-app`. The tests fail on a rule line that no longer parses, on an icon the mapping names
that is not in `icons/` or not listed in `icons.rs`, and on a listed icon the mapping no longer
uses.
