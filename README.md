# branchdiff

A terminal UI (TUI) to explore the diff between your **working tree** (the
current folder) and a git branch you pick, GitHub-PR style: a collapsible tree
of changed files on the left, and the selected file's diff on the right —
viewable **unified (in-place)** or **side-by-side**.

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
 branchdiff   main … working tree   (4 files)
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

### Releases

Pushing a version tag triggers the `release` GitHub Actions workflow
(`.github/workflows/release.yml`), which builds binaries for Linux, macOS
(x86_64 + arm64), and Windows and attaches them (with sha256 checksums) to a new
GitHub Release:

```sh
git tag v0.1.0
git push origin v0.1.0
```

## Usage

```sh
branchdiff <branch> [-C <repo-path>]

# examples
branchdiff main                 # working tree vs. main
branchdiff origin/main          # working tree vs. origin/main
branchdiff v1.0 -C ~/code/project
```

`branch` is any ref (branch, tag, or commit) you want to compare the current
folder against. The comparison is `git diff <branch>` — the branch on the left,
your **working tree** (all tracked changes on disk, staged or not) on the right.
This is handy for reviewing everything you've changed relative to, say, `main`
before you commit or open a PR.

> Untracked files are not shown (that's how `git diff <branch>` behaves); `git
> add` them first if you want them in the diff.

## Keys

| Key | Action |
|-----|--------|
| `↑`/`↓` or `k`/`j` | move (tree navigation jumps file-to-file, skipping folders) / scroll |
| `→`/`Enter` | open the file (focus diff) |
| `Tab` | switch focus between tree and diff |
| `s` | toggle unified ⇄ side-by-side |
| `t` | hide / show the file tree |
| `e` | edit the current file (embeds real vim, right panel) |
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

## Editing (embedded real vim)

Press `e` to turn the right panel into a **real terminal** running `vim` on the
selected file's working-tree path, attached to a pseudo-tty (`portable-pty`)
whose output is parsed by `vt100` and rendered in place as a `tui-term` widget
— so it's the actual editor, not an emulation, complete with its own modes,
plugins, and status/command line. Every keystroke is forwarded straight to it
(with `Ctrl`/arrows/function keys translated to the byte sequences a real
terminal would send), and the pane is kept sized to match the panel. It
returns to the diff view automatically as soon as vim exits (`:wq`, `:q!`,
...).

This is always `vim`, not `$EDITOR` — the environment's default editor varies
(many Linux distros/containers set `EDITOR=nano`) and behaves completely
differently (non-modal), which would silently break the experience this
feature is built around.

Requires `vim` to be installed and on `PATH`.

> Note: the diff view compares `branch` against your **working tree**, so edits
> you save from the embedded editor show up in the diff on the next auto-refresh
> — no commit required.

## How it works

- Shells out to `git diff` (no `libgit2` build dependency).
- `src/git.rs` — enumerate changed files and fetch per-file unified diffs.
- `src/diff.rs` — parse unified-diff text into hunks/lines and derive side-by-side rows.
- `src/tree.rs` — build the collapsible file tree from changed paths.
- `src/app.rs` — application state and the input event loop.
- `src/pty.rs` — the embedded real-editor PTY session (`portable-pty` + `vt100`).
- `src/highlight.rs` — syntect syntax highlighting (per-character foreground colours).
- `src/ui.rs` — all rendering (ratatui), incl. the delta-style diff builders.
