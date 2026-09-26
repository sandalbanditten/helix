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
It should be toggleable, configurable in `config.toml`, and be left (default) or right dockable.
It should render and work much like `grove.hx` a steel plugin with (modified by me) source at `~/.local/share/steel/cogs/grove`.
It should be idiomatic rust and helix-like, but as performant as possible.
It should be able to use `LS_COLORS` like grove, and have the same feature-set.
Like `grove` it should render the tree much like `eza -aoTg`.
The theming should be helix-like, e.g. the scroll bar and pane separator.
Unlike `grove.hx`, the viewport should end _at_ the statusline, rather that pushing the statusline to the right, e.g. the statusline should continue to span the entire width of helix' viewport.

Consider these wanted features when making design and implementation decisions:

It should preserve the following features from `grove`:
- Opening `helix` on multiple buffers should have the relevant directories expanded on start.
- A fit-width `=` keybinding (to fit the longest file exactly in the view), like my local changed `grove`
  - On startup the viewport width should always be fitted
- `+`/`-` for growing/shrinking the view.
- The `eza`-like styling and `LS_COLORS`/`EZA_COLORS` support.
- Aggregation of long single-chain paths like `src/main/java/project/framework`, instead of `src\n\tmain\n\t\tjava\n\t\t\tproject\n\t\t\t\tframework`

Expanding on `grove` it should have the following features:
- Trying to go up from the top item should cycle you to the bottom and vice-versa.
- The view should be scrollable with <C-d>, <C-u>, and `zz`/`zb`/`zt` like for example LSP-popups.
- Add a viewport only `?` keybind to list possible keybinds in the viewport
- Add a `o` keybind to run `xdg-open` on a file. Switch `enter` and `o` so `enter` is `xdg-open` and `o` will open in a buffer.
- Add an element showing which of the files currently are open in different buffers, with a differently colored `*` to the one showing the currently _focused_ buffer.
- The keybinds to toggle and focus it should be configurable.
- Add a config option for when to start with the file tree open:
  - Never (`"never"`)
  - Always (`"always"`)
  - On opening helix on multiple buffers (`"multiple"`)
- For renaming files we want two keybinds:
  - `r` to change just the name and extension of the file, e.g. `r` on `~/Code/test/src/test.rs` should expose only `test.rs` for editing and change only that. The view for this should be inline in the viewport.
  - `R` to change the path after the workspace, e.g. `R` on `~/Code/test/src/test.rs` should expose `src/test.rs`.
  - `<C-r>` to change the _full_ path , e.g. `<C-r>` on `~/Code/test/src/test.rs` should expose `/home/<USER>/Code/test/src/test.rs`.
- At the top should be a fuzzy search, working much like helix' existing file picker.
  - No file preview
  - Reuse as much of the code for fuzzy searching and the like as possible.
  - It should be opened with `/` for search and `?` for rev-search.
  - A config option should be exposed

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
