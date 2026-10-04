# Undo tree

Helix keeps every change in a tree: undoing a few changes and making a new one starts a branch,
and the undone changes stay on theirs. `u` and `U` go along a branch, while `Alt-u` and `Alt-U`
(`:earlier` and `:later`) go through all revisions in the order they were made. `:earlier 2f` and
`:later 1f` go by file writes.

The undo tree shows this tree in a panel beside the editor. `Space u` focuses it on the history of
the current buffer, and `Space U` toggles it.

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

Each row is a revision: its number, its age, `S` for the revision written last and `s` for those
written before, and the first line it inserted or deleted. `●` marks the revision the buffer is
at.

While the tree is focused, moving through it takes the buffer to the revision under the cursor.
`Enter` keeps that revision and `Escape` goes back to where you started.

| Key                  | Description |
| -----                | ----------- |
| `j`, `Down`          | Move to the older revision below |
| `k`, `Up`            | Move to the newer revision above |
| `h`, `Left`          | Move to the newer revision branching off the same one |
| `l`, `Right`         | Move to the older revision branching off the same one |
| `u`                  | Undo |
| `U`                  | Redo |
| `[`, `]`             | Move to the older, newer written revision |
| `Ctrl-d`, `Ctrl-u`   | Move half a page down, up |
| `PageDown`, `PageUp` | Move a page down, up |
| `gg`, `Home`         | Go to the newest revision |
| `ge`, `End`          | Go to the oldest revision |
| `zz`, `zc`           | Align the cursor row to the center |
| `zt`, `zb`           | Align the cursor row to the top, bottom |
| `/`                  | Search the changes of the revisions |
| `n`, `N`             | Go to the next, previous match |
| `d`                  | Toggle the diff against the start |
| `J`, `K`             | Scroll the diff half a page down, up |
| `+`, `-`             | Widen, narrow the undo tree |
| `=`                  | Fit the width to the widest row |
| `\|`                 | Toggle the widest and narrowest width |
| `?`                  | Show these keys |
| `Enter`              | Keep the revision and return focus to the editor |
| `Escape`             | Go back to the start and return focus to the editor |

With [`[editor.undo] diff`](./editor.md#editorundo-section), the bottom of the panel shows what
the revision under the cursor changed, or with `d` what changed since the revision you started
from. `Ctrl-w j` moves into the diff to scroll it, and `Ctrl-w k` back.

## Undo files

With [`[editor.undo] persist`](./editor.md#editorundo-section), the history of a file is kept in
an undo file when the file is written, and comes back when the file is opened again.
