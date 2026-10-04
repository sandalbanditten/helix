# Auto-reload

When the file of an open buffer changes on disk, Helix reloads the buffer. The reload is a step in
the buffer's history, so `u` undoes it.

A buffer with unsaved changes isn't reloaded without asking:

| Key | Description |
| --- | --- |
| `r` | Reload the buffer, keeping its unsaved changes in its history |
| `k`, `Escape` | Keep the buffer as it is |
| `R` | Reload all the buffers asked about |
| `K` | Keep all the buffers asked about |

When the file of a buffer is deleted, the buffer keeps its text. When git's HEAD moves, the
[diff gutter](./editor.md#editorguttersdiff-section) of the buffers in the repository follows it.

Set `auto-reload = false` in the [`[editor]` section](./editor.md#editor-section) to turn this
off.
