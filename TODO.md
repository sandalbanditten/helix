# TODO

Remember to be idiomatic, focus on clean code and architecture.
Follow design principles like loose coupling and high cohesion.
Read the files in `docs/`, especially `architecture.md` and `vision.md` and do things the rust and helix way.
Take your time and ask about specification details rather than guessing.
Look online for examples of similar features and implementations.
Ask when in doubt about specification or implementation, don't guess.
Performance is very important, especially for large files, directories, and projects.

## Code folding
Folding Java style doc-comments should look like `/** … */`.
Folded C-style (in all languages) brackets should look like `{ … }`.
We can do the second by making `editor.folding.placeholder` a string instead of a char, and defaulting to `" … "`.

## File Tree
There is a bug where sometimes when opening popups like `space k` for documentation; the popups show up where they should be,
if the file tree _wasn't_ there, e.g. opening a popup on the first column of the editor viewport shows the
popup on the first column on the termina. 

<!-- MAYBE -->
### Emacs' `dired` Style Feature
Possibly a `dired` style view in the editor.
Either zero integration, i.e. it opens in a buffer unrelated to the file-tree.
Maybe full integration, where the filetree _is_ a `dired` buffer.
I.e. hitting a keybind on the filetree fullscreens it and shows the same output as `eza -l`.

<!-- MAYBE -->
## File watching with `notify` crate
