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
| open text file | header read plus inflate | 1 MiB inflates in a few ms; no decode above the limit |
| log page | under 2 ms per 100 commits | walk plus commit decode; commit-graph speeds ancestry |
| tree diff | scales with the difference | unchanged subtrees share an id and are skipped whole |
| blob diff | two blob reads plus the diff | sizes checked against the limit before decoding |
| working-tree listing | one `read_dir` + one tree object | content hashed only when sizes match |
| working-tree status | under 50 ms per 10 000 files; capped at 200 000 scanned entries, background | one `read_dir` per directory, ignored directories pruned unopened, content hashed only when sizes match; the cap bounds the walk and how long it holds a pool handle, and truncation is reported |
| blame | grows with commits touching the file | on demand for the open file, never in bulk |
| highlight | after first paint, cached by blob id | capped at 2 MiB; plain text never waits |

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
  fragments. Do not build per-line views.
- **Do not shell out.** A `git` process costs more than any operation above.
- **Do not build a parallel index.** Packs, `commit-graph`, and the multi-pack index are the
  index. A repository missing `objects/info/commit-graph` walks history slower; suggest
  `git commit-graph write --reachable` to the user rather than writing it. The one sanctioned
  exception is path search: it flattens the selected root once, in the background, keyed by
  the root tree id, cancellable, and capped at 200 000 entries.
- **Decorate after painting.** Syntax highlighting and blame load after the content is on
  screen and never block it; a stale result is dropped by generation, like every other load.

## Measuring

```sh
cargo build --release -p rc-git-cli
target/release/rc-git --time ls   <repo>            # open + root tree
target/release/rc-git --time refs <repo>
target/release/rc-git --time cat  <repo> HEAD path/to/file
target/release/rc-git --time log  <repo> -n 100
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
