# Performance

The goal is that nothing the user does costs time proportional to repository size, only to
what appears on screen. These are the rules the code follows and the numbers it is held to.

## Budgets

Measured with `rc-git --time` on a warm page cache, release build, Apple silicon.

| Operation | Budget | Why it fits |
|---|---|---|
| open repository | under 5 ms | reads `HEAD`, `config`, index headers; packs mmap lazily |
| list refs | under 1 ms per 1 000 refs | one `packed-refs` scan plus loose files |
| switch ref | under 1 ms | one object read to peel to a tree |
| expand folder | under 1 ms per 1 000 entries | one tree object decode plus sort |
| open text file | header read plus inflate | 1 MiB inflates in a few ms; no decode above the limit; UTF-8 validity and line starts computed once in the core, decode and line-start mapping run off the main actor |
| log page | under 2 ms per 100 commits | walk plus commit decode; commit-graph speeds ancestry |
| tree diff | scales with the difference | unchanged subtrees share an id and are skipped whole |
| blob diff | two blob reads plus the diff | sizes checked against the limit before decoding |
| working-tree listing | one `read_dir` + one tree object | content hashed only when sizes match and the index cannot vouch for the file |
| working-tree status | under 150 ms per 10 000 files; capped at 200 000 scanned entries, background | one `read_dir` per directory, ignored directories pruned unopened, one `lstat` per tracked file through the index's stat cache and a hash only for what it cannot settle; the cap bounds the walk and how long it holds a pool handle, and truncation is reported |
| working-tree file list | under 60 ms per 10 000 files; capped at 200 000 scanned entries, background | one `read_dir` and one `lstat` per entry, one tree object per tracked directory, ignored directories the tree does not track pruned unopened, nothing hashed; runs once per opening of path search in the working tree |
| blame | grows with the history between tip and the commit that introduced the oldest surviving line — up to the whole ancestry for a file with a line from the first commit | on demand for the open file, never in bulk; a file over the limit at tip costs one header read |
| markdown preview | about 20 ms per MiB, off the main actor | one byte scan of the source, no grapheme walk and no intermediate tree; capped at 8 MiB, above which the file opens in source mode |
| highlight | after first paint, cached by blob id | capped at 2 MiB; plain text never waits; colors land as temporary attributes per visible chunk on scroll, never a whole-file pass on the main actor; the span cache is LRU-bounded |
| import links | about 100 ms per MiB, on the first pointer move over a file, off the main actor | one parse of its own, skipped when the bytes hold no `import`, `require` or `from`; 16 bytes a link, cached with the blob's highlight spans; hover is a binary search and one temporary attribute |
| follow an import | one tree object per directory on the way, plus the config files above the importing file when the specifier is not relative | resolved on click, through the shared tree cache; nothing is indexed and nothing is kept |

First paint after choosing a repository should land under 30 ms: open, `HEAD`, root tree,
refs. Everything else is on demand.

## Rules

- **Read the header before the body.** `blob_size` and `read_blob` check size from the object
  header; a 2 GiB blob costs a header read.
- **One tree object per expand.** Never walk a subtree to answer a question about a folder.
- **Never load the whole log.** `log(tip, skip, limit)` pages; the UI asks for the next page
  when scrolled. Deep pages re-walk from the tip, so keep `skip` small and cache pages.
- **Key every cache on an object id.** Content addressing makes invalidation unnecessary.
- **Render only the viewport.** The text view lays out lazily; the ruler draws visible
  fragments from line starts precomputed with the document; syntax colors apply per visible
  chunk as the user scrolls. Do not build per-line views, and do not walk the whole document
  on the main actor.
- **Let the index answer for the working tree.** A status walk hashes a file only when the
  blob header's size matches *and* the index cannot vouch for it: an entry whose recorded
  `lstat` still matches disk, whose id is the tree's, and that is not racy means the file was
  not touched. Every other case reads the file, so the cache can drop a hash but never a
  change. Hashing every size-matched file instead costs an order of magnitude on a large
  checkout.
- **Coalesce repeated requests, never queue them.** `ChangesModel` keeps one scan in flight
  and at most one queued re-run, because a working tree can change faster than it can be
  walked. Spawning a task per filesystem event builds a backlog that never drains — it cost
  190 000 suspended tasks and 638 MB once.
- **Do not shell out.** A `git` process costs more than any operation above.
- **Do not build a parallel index.** Packs, `commit-graph`, and the multi-pack index are the
  index. A repository missing `objects/info/commit-graph` walks history slower; suggest
  `git commit-graph write --reachable` to the user rather than writing it. The one sanctioned
  exception is path search: it flattens the selected root once, in the background, keyed by
  the root tree id, cancellable, and capped at 200 000 entries. In the working tree it walks
  disk instead, once per opening of the panel, capped the same way, with ignored folders the
  tree does not track pruned unopened.
- **Decorate after painting.** Syntax highlighting and blame load after the content is on
  screen and never block it; a stale result is dropped by generation, like every other load.
- **Parse for a feature when the user reaches for it.** Import links need a second parse of
  the file, which costs about as much as highlighting it, so it runs on the first pointer
  move over the text. A file opened from the sidebar and read costs nothing extra.

## Measuring

```sh
cargo build --release -p rc-git-cli
target/release/rc-git --time ls   <repo>            # open + root tree
target/release/rc-git --time refs <repo>
target/release/rc-git --time cat  <repo> HEAD path/to/file
target/release/rc-git --time log  <repo> -n 100
target/release/rc-git --time changes <repo>         # the full working-tree status walk
target/release/rc-git --time files <repo>           # the working-tree walk path search uses
```

`--time` reports the open and the command separately on stderr, so the two can be tracked
apart. For statistics over many runs use `hyperfine` around the same commands. Compare against
`git ls-tree`, `git cat-file -p`, and `git log -n 100` on the same repository to keep the
numbers honest.

## Known costs

- Cold page cache: the first open of a large pack pays for disk reads; nothing to do in code.
- Long delta chains: a blob at the end of a chain inflates every base. gix caches bases per
  handle, which is why handles are pooled instead of created per call.
- Annotated tags: `refs()` peels each one through the object database unless `packed-refs`
  carries peeled entries, which `git pack-refs` writes.
