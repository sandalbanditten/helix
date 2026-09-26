# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

## File Tree
We want a filetree.
It should be toggleable, configurable in `config.toml` (`[editor.file-tree]`), and be left (default) or right dockable.
It should render and work much like `grove.hx` a steel plugin with (modified by me) source at `~/.local/share/steel/cogs/grove`.
It should be idiomatic rust and helix-like, but as performant as possible.
It should be able to use `LS_COLORS` like grove, and have the same feature-set.
Like `grove` it should render the tree much like `eza -aoTg`.
The theming should be helix-like: the pane separator is a `│` rail in `ui.window` (like split separators) that carries the scrollbar thumb in `ui.menu.scroll` (like popups).
The panel starts at the top row (the bufferline only spans the editor columns) and ends _above_ the statusline, so the bottom statusline and the command line continue to span the entire width of helix' viewport.

Consider these wanted features when making design and implementation decisions:

It should preserve the following features from `grove`:
- Opening `helix` on multiple buffers should have the relevant directories expanded on start.
- A fit-width `=` keybinding (to fit the longest file exactly in the view), like my local changed `grove`
  - On startup (first time the tree is shown) the viewport width should always be fitted
- `+`/`-` for growing/shrinking the view.
- The `eza`-like styling and `LS_COLORS`/`EZA_COLORS` support.
  - Icons are `eza`'s, except that an expanded directory always shows the open folder `` (U+F115), even one with an icon of its own; collapsed directories keep theirs.
- Aggregation of long single-chain paths like `src/main/java/project/framework`, instead of `src\n\tmain\n\t\tjava\n\t\t\tproject\n\t\t\t\tframework`
- The rest of grove: git marks, unsaved `+` marks, icons, guides, pinned ancestor rows, splits (`C-s`/`C-v`), delete (`d`), mouse support.
  - `d` asks on the command line: `Delete <path>? (y/n):`, where `y` and Enter delete.
- Prompts and messages are styled like helix's: prompt labels are lowercase like `rename-to:` (`move-to:`), sentences start with a capital letter.

Expanding on `grove` it should have the following features:
- Trying to go up from the top item should cycle you to the bottom and vice-versa.
- The view should be scrollable with <C-d>, <C-u>, and `zz`/`zb`/`zt` like for example LSP-popups.
- Add a viewport only `?` keybind to list possible keybinds in the viewport
- `enter` runs `xdg-open` on the entry (files and directories), `o` opens a file in a buffer (and toggles a directory). `space` is not bound in the tree.
- Add an element showing which of the files currently are open in different buffers, with a differently colored `*` to the one showing the currently _focused_ buffer.
- The keybinds to toggle and focus it should be configurable: commands `focus_file_tree` (default `space e`) and `toggle_file_tree` (default `space E`).
  - Toggle switches between "always shown" and "shown only while focused". Focusing a hidden tree shows it until focus leaves it.
- While the tree is focused, a key it does not bind returns focus to the editor and runs there (the toggle/focus commands still work from the tree).
- Add a config option for when to start with the file tree open:
  - Never (`"never"`, default)
  - Always (`"always"`)
  - On opening helix on multiple buffers (`"multiple"`: two or more files on the command line)
  - `hx <dir>` shows the tree (unfocused) and still opens the file picker.
- For renaming files we want three keybinds:
  - `r` to change just the name and extension of the file, e.g. `r` on `~/Code/test/src/test.rs` should expose only `test.rs` for editing and change only that. The view for this should be inline in the viewport.
  - `R` to change the path after the workspace, e.g. `R` on `~/Code/test/src/test.rs` should expose `src/test.rs`, in the bottom command-line prompt.
  - `<C-r>` to change the _full_ path , e.g. `<C-r>` on `~/Code/test/src/test.rs` should expose `/home/<USER>/Code/test/src/test.rs`, in the bottom command-line prompt. Moves across filesystems fall back to copy+delete.
  - Open buffers follow the rename.
- `a` creates a file and `A` a directory, inline: the input row appears inside the target directory (the parent of the file under the cursor, or the directory under the cursor, expanded). For `a` it sits after that directory's subdirectories and before its files; for `A` it sits above the existing subdirectories. With alphabetical sorting it is the first child.
- At the top should be a fuzzy search, working much like helix' existing file picker.
  - No file preview
  - Reuse as much of the code for fuzzy searching and the like as possible.
  - `/` opens the search prompt at the top of the tree. As you type, the cursor jumps to the next match below it in tree order (wrapping), expanding directories to reveal it. `n`/`N` go to the next/previous match. The candidates are all workspace files (the file picker's walker).
  - No reverse search (`?` is the help key) and no search config options for now.
- External changes are picked up by watching the loaded directories and the repository's `.git` directory (`notify`).

Note that all these keybinds, except for the two to toggle and focus the file tree should only work when the file tree is focused

## Spellchecking
It should use helix/spellbook and work like `https://github.com/helix-editor/helix/pull/15910`.
Implement that PR again, but into this extended fork.
Make sure the implemented code is performant and idiomatic.
Remember to focus on code reuse and clean architecture.
Read through the design decisions by the original authors and consider them when planning/implementing.

<!-- MAYBE -->
## Emacs' `dired` Style Feature
Possibly a `dired` style view in the editor.
Consider how it should integrate with the file-tree.
Either zero integration, i.e. it opens in a buffer unrelated to the file-tree.
Maybe full integration, where the filetree _is_ a `dired` buffer.
