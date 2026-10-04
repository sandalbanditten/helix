# File tree

The file tree lists the files of the working directory beside the editor. Entries show their git
status on the left, a `*` when they are open in a buffer and a `+` when they have unsaved changes.

`Space e` focuses the tree on the current file and `Space E` toggles it. While the tree is focused
it takes the keys listed in the [keymap](./keymap.md#file-tree); any other key returns focus to
the editor. Opening a directory, like `hx .`, shows the tree.

Besides opening files, the tree creates, renames, moves, copies and deletes them. `e` edits the
directory under the cursor in [dired](./dired.md).

See [`[editor.file-tree]`](./editor.md#editorfile-tree-section) for its options.
