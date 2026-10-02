# Auto-reload

When another program changes the file of an open buffer, the buffer follows. A buffer without
unsaved changes reloads, and the status line says `src/main.rs reloaded`, or `3 buffers reloaded`
after, for example, a `git checkout`. The reload is a step of the buffer's history, so `u` takes
it back.

A buffer with unsaved changes isn't reloaded behind your back: a box asks what to do with it.

| Key | Does |
| --- | --- |
| `r` | Reloads the buffer. Its unsaved changes are a step back in its history, so `u` brings them back |
| `k`, `Esc` | Keeps the buffer as it is. The file counts as seen, so `:w` overwrites it without `!` |
| `R` | Reloads all the buffers asked about |
| `K` | Keeps all the buffers asked about |

Several buffers are asked about one after another, and `R` and `K` show up then. The box waits
while you type in insert mode, and an open prompt or picker gets the keys first, so keys typed for
something else never answer it. While it is up, other keys do nothing.

A file written anew with the text the buffer knows, like after `git stash` and `git stash pop`, or
by a formatter that changes nothing, asks nothing. A later change of the file asks again.

When the file of a buffer is deleted, the buffer keeps its text and the status line says
`src/main.rs was deleted on disk`. If the file comes back, the buffer follows it again.

## Git

When HEAD moves, with `git commit`, `git checkout` or `git reset`, the buffers in the repository
get their [diff gutter](./editor.md#editorguttersdiff-section) refreshed against the new HEAD and
the status line shows the new branch, also those whose files didn't change.

## Watching

Only the directories of the open files are watched, each without its subdirectories, along with
the few files in `.git` that move HEAD, so watching costs the same in a large project as in a
small one. Changed files are read and compared in the background, so reloading a large file
doesn't hold up the editor.

Some changes reach no watcher, like those on a network drive or through a hard link in another
directory. Helix checks every buffer again when the terminal gets the focus back, which needs
[focus event support](https://github.com/helix-editor/helix/wiki/Terminal-Support) from the
terminal. A file written over more than a tenth of a second can be reloaded more than once while
it is written, each time a step of the buffer's history.

Set `auto-reload = false` in the [`[editor]` section](./editor.md#editor-section) to turn all of
this off.
