# Undo tree

Helix keeps every change in a tree: undoing a few changes and making a new one starts a branch,
and the undone changes stay on theirs. `u` and `U` go along a branch, to the parent and back to
the child last visited; `Alt-u` and `Alt-U` (`:earlier` and `:later`) go through all revisions in
the order they were made, across branches. `:earlier 2f` and `:later 1f` go by file writes, like
in Vim: with changes since the last write, `:earlier 1f` goes back to that write.

The undo tree shows that tree in a panel on the right of the editor. `Space u` focuses it on the
history of the focused buffer, `Space U` keeps it shown, following the focused buffer as you edit.

```
●     6 now   x = 2
○     5 10s   dbg!(x);
│ ○   4 40s   // todo
│ │ ○ 3 1m    count
│ ├─┘
│ ○   2 2m  S println!("{x}");
├─┘
○     1 3m    let x = 1;
○     0       original
```

Each row is a revision: its number, how long ago it was made, `S` for the revision written last
and `s` for those written before, and the first line it inserted (green), or deleted (red, after a
`-`) when it inserted nothing. The newest revision is on top; a branch joins the revision it started from
(`├─┘`). `●` marks the revision the buffer is at, or was at when the tree got focus.

While the tree has focus, moving through it takes the buffer to the revision under the cursor,
the view gliding to the change with [smooth scrolling](./editor.md#editorsmooth-scroll-section) on,
and the [diff gutter](./editor.md#editorguttersdiff-section) shows the changes against the
revision you started from. `Enter` keeps the revision reached, `Escape` goes back to the start;
any other key of the editor keeps the revision and runs in the editor. Afterwards, `U` follows the
branch you went into.

| Key                  | Description |
| -----                | ----------- |
| `j`, `Down`          | Older revision, the row below |
| `k`, `Up`            | Newer revision, the row above |
| `h`, `Left`          | Newer revision branching off the same one |
| `l`, `Right`         | Older revision branching off the same one |
| `u`                  | Undo: the parent |
| `U`                  | Redo: the child last visited |
| `[`, `]`             | Older, newer written revision |
| `Ctrl-d`, `Ctrl-u`   | Move half a page down, up |
| `PageDown`, `PageUp` | Move a page down, up |
| `gg`, `Home`         | Go to the newest revision |
| `ge`, `End`          | Go to the oldest revision |
| `zz`, `zc`           | Align the cursor row to the center |
| `zt`, `zb`           | Align the cursor row to the top, bottom |
| `/`                  | Search the text revisions inserted or deleted, with a regex |
| `n`, `N`             | Go to the next, previous match |
| `d`                  | Toggle the diff against the start |
| `J`, `K`             | Scroll the diff half a page down, up |
| `+`, `-`             | Widen, narrow the undo tree |
| `=`                  | Fit the width to the widest row |
| `\|`                 | Toggle the widest and narrowest width |
| `?`                  | Show these keys |
| `Enter`              | Keep the revision and return focus to the editor |
| `Escape`             | Go back to the start and return focus to the editor |

The search is the editor's: a regex typed in the command line, which recalls and completes
earlier searches (`Ctrl-p`, `Ctrl-n`, `Tab`) and ignores case unless the regex has capitals, as
the [`[editor.search]`](./editor.md#editorsearch-section) settings say. As you type, it takes the
buffer to the first revision below the cursor whose change matches and highlights the matching
changes; `Enter` keeps the search and `Escape` goes back. Searches are shared: `n` and `N` in the
tree go to the changes matching the last search, typed in the tree or in the editor, and the
editor's `n` and `N` look for the tree's.

The panel fits its width to the widest row when it appears, within limits; after `+`, `-` or `|`
it keeps the width you set. With [`[editor.undo] float`](./editor.md#editorundo-section) it floats
over the right edge of the editor instead of making room beside it, so the text keeps its width
(and soft-wraps no differently) while the panel covers what is under it.

## Diff

With [`[editor.undo] diff`](./editor.md#editorundo-section), the bottom of the panel shows what the
revision under the cursor changed, like vim-mundo's preview: the diff from its parent, under a row
like `─ 1 → 2 ──`. `d` switches to the diff from the revision browsing started from to the one
under the cursor, and back. `"difftastic"` runs [`difft`](https://difftastic.wilfred.me.uk/), which
diffs the syntax of the file's language, named by the file's path; `"builtin"` shows a unified
diff of the lines. The diff is worked out in the background a quarter of a second after the cursor
stops, so moving through the tree stays fast.

The diff part takes the keys like a split below the tree: whatever moves to the split below in the
editor, like `Ctrl-w j` or `Space w j`, moves there, and to the split above back to the tree. There
the editor's motions scroll the diff, whose lines are cut at the edge rather than wrapped: `j` and
`k` a line, `h` and `l` half the width, `Ctrl-d`, `Ctrl-u`, `Ctrl-f` and `Ctrl-b` by pages, `gg`
and `ge` to the top and the bottom, `gh` and `gl` to the leftmost and rightmost columns, with
counts and as remapped. `d`, `+`, `-`, `=`, `|`, `Enter` and `Escape` work as in the tree, and `?`
shows the keys. `J` and `K` scroll the diff from the tree.

## Undo files

With [`[editor.undo] persist`](./editor.md#editorundo-section), the history of a file outlives
Helix: it is kept in an undo file when the file is written and comes back when the file is opened
again, so `u` goes back past the session. Ages shown in the tree count from when each change was
made; changes older than a week show their date.
