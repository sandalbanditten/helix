# TODO

Remember to be idiomatic, focus on clean code and architecture.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.

## Breadcrumb Trail
We want a simple breadcrumb trail as a configurable statusline element.
It should use treesitter queries that live in `runtime/queries/<lang>/` along with the other queries, and show up on using `helix --health [CATEGORY]`.
It should be much like `context.hx`, a steel plugin with source code at `~/.local/share/steel/cogs/context`.
Take the queries from there.
By default it should be enabled and be placed after the file name.
A config option for a leading "> " should be present and enabled by default.
It should be simple and use as much existing code as possible.
It should be idiomatic rust and follow the way helix does things.

## Simple Code Folding Plugin
We want a simple code folding plugin with a keybind to fold/unfold currently selected function, method, class, block, module, etc.
We want two keybinds to fold all and unfold all.
We want a config option for starting with everything folded.
It should be performant, consider the specialized data structures in use by the helix editor.
It should, if possible, use tree-sitter for performant code analysis.

## File Tree
We want a filetree.
It should be toggleable, configurable in `config.toml`, and be left or right dockable.
It should render and work like `grove.hx` a steel plugin with source at `~/.local/share/steel/cogs/grove`.
It should be idiomatic rust and helix-like, but as performant as possible.
It should be able to use `LS_COLORS` like grove, and have the same feature-set.
The theming should be helix-like, e.g. the scroll bar and pane separator.
Unlike `grove.hx`, the viewport should end _at_ the statusline, rather that pushing the statusline to the right, e.g. the statusline should continue to span the entire width of helix' viewport.

Implement the base file tree before the following expansions, but consider them when making design and implementation decisions.
Expanding on grove it should have the following features:
- Trying to go up from the top item should cycle you to the bottom and vice-versa.
- The view should be scrollable with <C-d> and <C-u>, like for example LSP popups.
- Add a viewport only `?` keybind to list possible keybinds in the viewport
- Add a `m` keybind which "grabs" the file/folder and allows the user to move it up and down, including into other directories.
- Add a `o` keybind to run `xdg-open` on a file. Possibly switch `enter` and `o`.
- Add an element showing which of the files currently are open in different buffers, possibly a differently colored `*` to the one showing the currently focused buffer.
- Add a config option for only opening the file tree when opening helix on multiple buffers. 
- Renaming/moving file should not modify the canonical path of the file.
