# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

## Typst Inline Preview
We want inline preview of math symbols in typst, using virtual text.
An example would be `$2 alpha^2$` should show as `$2 α^2$` until the cursor is in on or right next to "α".
Should work for all Typst symbols.

## File Tree
There is a bug where sometimes when opening popups like `space k` for documentation; the popups show up where they should be.
If the file tree _wasn't_ there, e.g. opening a popup on the first column of the editor viewport shows the popup on the first column on the terminal.

<!-- MAYBE -->
### Emacs' `dired` Style Feature
Possibly a `dired` style view in the editor.
Either zero integration, i.e. it opens in a buffer unrelated to the file-tree.
Maybe full integration, where the filetree _is_ a `dired` buffer.
I.e. hitting a keybind on the filetree fullscreens it and shows the same output as `eza -l`.
Another keybind for just the currently hovered directory, or item in said directory.

<!-- MAYBE -->
## File watching with `notify` crate
Configurable to either:
- Popup saying file changed, if it already had unwritten changes
- Just reload the changes
