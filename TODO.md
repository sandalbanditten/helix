# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

## File Tree
We want a copy keybind (`y`) which copies a file and a (`p`) to paste it, or similar interface.
This keybind should also put the path relative to workspace root in system clipboard.

There is a bug where sometimes when opening popups like `space k` for documentation; the popups show up where they should be.
If the file tree _wasn't_ there, e.g. opening a popup on the first column of the editor viewport shows the popup on the first column on the terminal.

There is also a bug which causes cursor flickering, which I believe was introduced with the file tree feature.
The flickering may happen even when there is no file tree open, and it usually happens in insert mode
This should be fixed.

<!-- MAYBE -->
## File watching with `notify` crate
Configurable to either:
- Popup saying file changed, if it already had unwritten changes
- Just reload the changes
