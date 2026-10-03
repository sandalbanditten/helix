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
and `s` for those written before, and the first line it inserted, or deleted (after a `-`) when it
inserted nothing. The newest revision is on top; a branch joins the revision it started from
(`├─┘`). `●` marks the revision the buffer is at, or was at when the tree got focus.

While the tree has focus, moving through it takes the buffer to the revision under the cursor,
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
| `/`                  | Search the text revisions inserted or deleted |
| `n`, `N`             | Go to the next, previous match |
| `+`, `-`             | Widen, narrow the undo tree |
| `=`                  | Fit the width to the widest row |
| `?`                  | Show these keys |
| `Enter`              | Keep the revision and return focus to the editor |
| `Escape`             | Go back to the start and return focus to the editor |

The search, typed in the command line, takes the buffer to the first revision below the cursor
whose change contains the text as you type, ignoring case unless the text has capitals, and
highlights the matching changes; `Enter` keeps it and `Escape` goes back.

The panel fits its width to the widest row when it appears, within limits; after `+` or `-` it
keeps the width you set.

## Undo files

With [`[editor.undo] persist`](./editor.md#editorundo-section), the history of a file outlives
Helix: it is kept in an undo file when the file is written and comes back when the file is opened
again, so `u` goes back past the session. Ages shown in the tree count from when each change was
made; changes older than a week show their date (UTC).
