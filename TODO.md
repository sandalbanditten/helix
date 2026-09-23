# TODO

## Smooth Scrolling
We want smooth scrolling for <C-d>, <C-u>, `zz`, `zb`, and `zt`.
Scrolling should be smooth, but it should take approximately the same time to scroll really far and very short.
It should respect `editor.scrolloff` and be configurable as `editor.smooth-scroll = true`.

## Breadcrumb Trail
We want a simple breacrumb trail as a configurable statusline element.
It should use treesitter queries that live in `runtime/queries/<lang>/` along with the other queries, and show up on using `helix --health [CATEGORY]`.
It should be much like `context.hx`, a steel plugin with source code at `~/.local/share/steel/cogs/context`.
It should be simple and use as much existing code as possible.
It should be idiomatic rust and follow the way helix does things.

## File Tree
We want a filetree.
It should be toggleable, configurable in `config.toml`, and be left or right dockable.
It should render and work like `grove.hx` a steel plugin with source at `~/.local/share/steel/cogs/grove`.
It should be idiomatic rust and helix-like, but as performant as possible.
It should be able to use `LS_COLORS` like grove, and have the same feature-set.

Expanding on grove it should have the following features:
- Add a viewport only `?` keybind to list possible keybinds in the viewport
- Add a `o` keybind to run `xdg-open` on a file. Possibly switch `enter` and `o`.
- Add an element showing which of the files currently are open in different buffers, possibly a differently colored `*` to the one showing the currently focused buffer.
- Add a config option for only opening the file tree when opening helix on multiple buffers. 
- Renaming/moving file should not modify the canonical path of the file.
