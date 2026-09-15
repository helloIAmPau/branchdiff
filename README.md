# branchdiff

A terminal UI (TUI) to explore the diff between two git branches, GitHub-PR style:
a collapsible tree of changed files on the left, and the selected file's diff on the
right — viewable **unified (in-place)** or **side-by-side**.

Diffs are rendered in a **git-delta–style**: two-column line numbers, full-width
add/remove backgrounds (green = added / red = removed, Claude Code shades),
brighter **word-level highlighting** of
the exact changed span within a line, styled hunk-header dividers, and
**`syntect` syntax highlighting** of the code using a bundled **GitHub Dark
theme** (`assets/github-dark.tmTheme`) over a GitHub-Dark diff palette. The
syntect build uses the pure-Rust `fancy-regex` backend, so no C toolchain /
oniguruma is required.

The view **auto-refreshes** when the repository changes on disk (new commits,
edited files): a filesystem watcher (`notify`) feeds a **300 ms trailing
debounce**, so a burst of writes coalesces into a single redraw instead of
flickering. Refresh is deferred while the built-in editor is open, and the tree
selection is preserved across refreshes.

```
 branchdiff   main … feature   (4 files)
┌ Files ──────────────────┐┌ src/main.rs ───────────────────────── unified ┐
│▾ src/                   ││@@ -1,3 +1,5 @@                                 │
│  ▾ util/                ││   1    1   fn main() {                         │
│      M math.rs          ││   2      -     println!("hello");             │
│    M main.rs            ││        2 +     println!("hello, world");      │
│  A NEWFILE.txt          ││        3 +     let x = 42;                     │
│  D README.md            ││   3    5   }                                  │
└─────────────────────────┘└───────────────────────────────────────────────┘
 ↑↓ move · →/⏎ open  [Tab] focus  [s] split  [t] tree  [C] collapse  [q] quit
```

## Install / build

Requires a Rust toolchain and `git` on your `PATH`.

```sh
cargo build --release
# binary at target/release/branchdiff
```

## Usage

```sh
branchdiff <base> [head] [-C <repo-path>]

# examples
branchdiff main my-feature      # explicit base and head
branchdiff main                 # base vs. the CURRENT branch
branchdiff origin/main HEAD
branchdiff v1.0 v2.0 -C ~/code/project
```

`base` is the branch you would merge *into* (e.g. `main`); `head` is the branch with
your changes. When `head` is omitted it defaults to the **current branch**. The comparison uses three-dot (`base...head`) semantics — exactly what a
pull request shows: everything on `head` since it diverged from `base`.

## Keys

| Key | Action |
|-----|--------|
| `↑`/`↓` or `k`/`j` | move (tree navigation jumps file-to-file, skipping folders) / scroll |
| `→`/`Enter` | open the file (focus diff) |
| `Tab` | switch focus between tree and diff |
| `s` | toggle unified ⇄ side-by-side |
| `t` | hide / show the file tree |
| `e` | edit the current file (vim-style, right panel) |
| `←`/`→` (in diff) | pan horizontally on long lines |
| `PgUp`/`PgDn`, `Ctrl-u`/`Ctrl-d` | page the diff |
| `g`/`G` | top / bottom |
| `C` | toggle collapse/expand all folders |
| `q` / `Ctrl-c` | quit |

## Mouse

| Action | Effect |
|--------|--------|
| Click a file | select it and show its diff |
| Click a folder | expand / collapse it |
| Click the diff panel | focus the diff |
| Wheel over the tree | move the selection up/down |
| Wheel over the diff | scroll the diff up/down |

## Editing (vim-style)

Press `e` to turn the right panel into a modal editor for the selected file. It
loads the working-tree copy (falling back to the `head` revision) and, on `:w`,
writes back to the file on disk. The buffer is syntax-highlighted with the same
GitHub Dark theme.

- **Modes:** Normal · Insert · Command (`:`). `Esc` returns to Normal.
- **Enter insert:** `i` `a` `I` `A` `o` `O`
- **Motions:** `h` `j` `k` `l`, `0` `$` `^`, `w` `b`, `gg` `G` (arrows/Home/End too)
- **Edits:** `x` (char), `dd` (line), `D` (to end of line)
- **Commands:** `:w` write · `:q` quit (blocked if unsaved) · `:q!` force-quit ·
  `:wq` / `:x` write & quit

The bottom row shows the current mode, cursor `line:col`, and messages.

> Note: the diff view compares two **commits** (`base...head`), so working-tree
> edits you save won't change that diff until they're committed on `head`.

## How it works

- Shells out to `git diff` (no `libgit2` build dependency).
- `src/git.rs` — enumerate changed files and fetch per-file unified diffs.
- `src/diff.rs` — parse unified-diff text into hunks/lines and derive side-by-side rows.
- `src/tree.rs` — build the collapsible file tree from changed paths.
- `src/app.rs` — application state and the input event loop.
- `src/editor.rs` — the modal vim-style text editor.
- `src/highlight.rs` — syntect syntax highlighting (per-character foreground colours).
- `src/ui.rs` — all rendering (ratatui), incl. the delta-style diff builders.
